//! Señalización: salas en memoria sobre la sesión WebTransport `/signal`, ligadas a cuentas.
//!
//! El cliente abre la sesión (`CONNECT` extendido con `:protocol = webtransport`), abre un stream
//! bidireccional y envía `Auth` con su token de sesión y la clave pública de su dispositivo: el
//! servidor busca el token (por su SHA-256) y exige que el dispositivo del token sea esa clave.
//! En modo desarrollo (`COROOM_DEV_TOKEN`) acepta también ese token, sin cuenta. El
//! servidor responde `Observed` con la dirección de origen de la conexión QUIC —que sale del mismo
//! socket que los medios del cliente—, la presencia de sus amigos conectados (`Presence`) y después
//! atiende `CreateRoom`, `JoinRoom`, `Candidates`, `Mls`, `Kick` e `Invite`. Reenvía `Mls` y
//! `Candidates` sin interpretarlos y expide un `RelayToken` a cada par (o `QuotaExceeded` /
//! `RelayDisabled` si la sala no tiene relay).
//!
//! Con cuentas, la identidad de `CreateRoom` y `JoinRoom` debe ser la del dispositivo
//! autenticado, así que la credencial MLS de cada miembro es una clave vinculada a su cuenta.
//! Las salas siguen en memoria mientras el anfitrión está conectado; el anfitrión es un usuario y
//! los invitados también (`PeerJoined` y `Joined` llevan usuario y nombre visible).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use coroom_transport::proto::PROTOCOL_VERSION;
use coroom_transport::proto::codec::{self, FrameBuffer};
use coroom_transport::proto::control::Role;
use coroom_transport::proto::signal::{
    Activity, ErrorCode, HOST_PEER_ID, PeerId, ROOM_CODE_ALPHABET, ROOM_CODE_LEN, SignalMessage,
    UserId, is_valid_room_code,
};

use crate::auth::{constant_time_eq, token_hash};
use crate::db::Db;
use crate::quota::Quota;
use crate::relay::{Grant, Relay};

/// Ruta de la sesión WebTransport.
pub const SIGNAL_PATH: &str = "/signal";

/// Espera máxima del `Auth` inicial.
const AUTH_TIMEOUT: Duration = Duration::from_secs(10);

/// Mensajes en cola por sesión; si un cliente no lee, se le desconecta.
const SESSION_QUEUE: usize = 256;

/// Longitud máxima del nombre de una sala.
const MAX_ROOM_NAME: usize = 64;

/// Nombre visible de las sesiones del modo desarrollo.
const DEV_NAME: &str = "desarrollo";

/// Cuerpo máximo de un mensaje de señalización. Lo más grande que viaja es el `Welcome` MLS con el
/// árbol de la sala (7,5 KB con 32 invitados, test `the_largest_signalling_messages_fit`) y la
/// clave X-Wing firmada (1 281 B): 256 KiB deja margen y acota a 64 MiB lo que puede quedar en la
/// cola de una sesión que no lee (`docs/seguridad.md`, hallazgo S-2).
pub const MAX_SIGNAL_MESSAGE: usize = 256 * 1024;

/// Códigos de sala que no existen admitidos por cuenta (o por sesión, sin cuentas) en
/// [`JOIN_FAILURE_WINDOW`]: con 30 bits por código, adivinar uno queda fuera de alcance
/// (`docs/seguridad.md`, hallazgo S-3).
pub const MAX_JOIN_FAILURES: usize = 10;

/// Ventana de [`MAX_JOIN_FAILURES`].
pub const JOIN_FAILURE_WINDOW: Duration = Duration::from_secs(60);

/// Configuración de la señalización.
#[derive(Debug, Clone)]
pub struct HubConfig {
    /// Token de desarrollo; si está, se acepta además de las cuentas.
    pub dev_token: Option<String>,
    /// Invitados como mucho por sala.
    pub max_guests: u32,
    /// Si el servidor tiene relay.
    pub relay_enabled: bool,
}

impl HubConfig {
    /// Configuración de los tests: sin token de desarrollo, 8 invitados y relay.
    pub fn new(max_guests: u32) -> Self {
        Self {
            dev_token: None,
            max_guests,
            relay_enabled: true,
        }
    }
}

type SessionId = u64;

/// Quién está detrás de una sesión.
#[derive(Debug, Clone)]
struct Account {
    /// Usuario (`None` en modo desarrollo).
    user: Option<i64>,
    /// Nombre visible.
    name: String,
    /// Versión de su avatar (`None`: no tiene).
    avatar: Option<u64>,
    /// Clave del dispositivo autenticado (`None` en modo desarrollo: no se comprueba).
    device_pk: Option<[u8; 32]>,
}

impl Account {
    fn user_id(&self) -> UserId {
        self.user.and_then(|id| u64::try_from(id).ok()).unwrap_or(0)
    }
}

#[derive(Debug)]
struct Member {
    session: SessionId,
    identity_pk: [u8; 32],
    account: Account,
}

/// Una misma persona no puede pedir entrar en la misma sala más de una vez cada tanto.
const JOIN_REQUEST_EVERY: Duration = Duration::from_secs(5);
/// Cuánto vive una petición de entrada sin respuesta: lo mismo que muestra la aplicación, para que
/// «Ahora no» siga teniendo a quién responder aunque entre tanto otros hayan pedido entrar.
const JOIN_REQUEST_TTL: Duration = Duration::from_secs(60);

/// Si la sala tiene relay y, si no, por qué.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelayState {
    Available,
    Disabled,
    Quota,
}

#[derive(Debug)]
struct Room {
    code: String,
    host: Member,
    guests: BTreeMap<PeerId, Member>,
    next_peer: PeerId,
    relay: RelayState,
}

/// Lo que cambia cuando se cierra una sesión de señalización.
#[derive(Debug, Default)]
struct Left {
    /// El usuario, si era su última sesión (toca avisar «desconectado»).
    last: Option<i64>,
    /// Usuarios cuya actividad cambió (salieron de una sala, o la sala se cerró).
    changed: Vec<i64>,
}

#[derive(Debug)]
struct Session {
    tx: mpsc::Sender<SignalMessage>,
    close: Option<oneshot::Sender<()>>,
    member: Option<(u64, PeerId)>,
    account: Account,
}

/// A quién se le cuentan los códigos de sala fallidos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum FailureKey {
    User(i64),
    Session(SessionId),
}

#[derive(Debug, Default)]
struct HubInner {
    sessions: HashMap<SessionId, Session>,
    rooms: HashMap<u64, Room>,
    codes: HashMap<String, u64>,
    online: HashMap<i64, HashSet<SessionId>>,
    join_failures: HashMap<FailureKey, VecDeque<Instant>>,
    /// Última petición de entrar de cada (quien pide, anfitrión): una cada [`JOIN_REQUEST_EVERY`].
    join_requests: HashMap<(i64, i64), Instant>,
    next_session: SessionId,
    next_room: u64,
}

impl HubInner {
    /// Qué hace `user` ahora: anfitrión de una sala, invitado en una, o nada.
    fn activity_of(&self, user: i64) -> Activity {
        let mut activity = Activity::Idle;
        for room in self.rooms.values() {
            if room.host.account.user == Some(user) {
                return Activity::Hosting;
            }
            if room
                .guests
                .values()
                .any(|member| member.account.user == Some(user))
            {
                activity = Activity::Playing;
            }
        }
        activity
    }

    /// La sala que `user` tiene abierta como anfitrión.
    fn hosted_room(&self, user: i64) -> Option<&Room> {
        self.rooms
            .values()
            .find(|room| room.host.account.user == Some(user))
    }

    /// Fallos recientes de `key`, olvidando los de fuera de la ventana.
    fn recent_failures(&mut self, key: FailureKey, now: Instant) -> usize {
        let Some(failures) = self.join_failures.get_mut(&key) else {
            return 0;
        };
        while failures
            .front()
            .is_some_and(|at| now.duration_since(*at) >= JOIN_FAILURE_WINDOW)
        {
            failures.pop_front();
        }
        let count = failures.len();
        if count == 0 {
            self.join_failures.remove(&key);
        }
        count
    }

    fn note_failure(&mut self, key: FailureKey, now: Instant) {
        let failures = self.join_failures.entry(key).or_default();
        failures.push_back(now);
        while failures.len() > MAX_JOIN_FAILURES {
            failures.pop_front();
        }
    }
}

/// Una sala activa, sin identificarla.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoomSummary {
    /// Identificador interno (para cruzarlo con los contadores del relay).
    pub id: u64,
    /// Invitados.
    pub guests: usize,
    /// Si tiene relay.
    pub relay: bool,
}

/// Salas y sesiones de señalización.
#[derive(Debug)]
pub struct Hub {
    config: HubConfig,
    relay: Arc<Relay>,
    db: Option<Db>,
    quota: Option<Arc<Quota>>,
    inner: Mutex<HubInner>,
    /// Pone en fila los avisos de avatar ([`Hub::avatar_changed`]).
    avatar_order: tokio::sync::Mutex<()>,
}

/// Resultado de una operación: mensajes para quien la pidió o un error.
type Reply = Result<Vec<SignalMessage>, (ErrorCode, String)>;

fn error(code: ErrorCode, message: impl Into<String>) -> SignalMessage {
    SignalMessage::Error {
        code,
        message: message.into(),
    }
}

fn poisoned() -> (ErrorCode, String) {
    (ErrorCode::Internal, "estado envenenado".to_owned())
}

fn random_code() -> Option<String> {
    let mut bytes = [0u8; ROOM_CODE_LEN];
    getrandom::fill(&mut bytes).ok()?;
    Some(
        bytes
            .iter()
            .map(|b| char::from(ROOM_CODE_ALPHABET[usize::from(b & 31)]))
            .collect(),
    )
}

/// Texto de `QuotaExceeded`.
pub const QUOTA_MESSAGE: &str = "el anfitrión agotó su cuota mensual del relay del servidor: la \
                                 sala solo funciona en directo";

/// Texto de `RelayDisabled` en los avisos.
pub const RELAY_DISABLED_MESSAGE: &str = "el servidor no tiene relay: la sala solo funciona en \
                                          directo";

impl Hub {
    /// Señalización sin cuentas (solo con el token de desarrollo) que expide tokens de `relay`.
    pub fn new(config: HubConfig, relay: Arc<Relay>) -> Arc<Self> {
        Arc::new(Self {
            config,
            relay,
            db: None,
            quota: None,
            inner: Mutex::new(HubInner::default()),
            avatar_order: tokio::sync::Mutex::new(()),
        })
    }

    /// Señalización con cuentas en `db` y la cuota del relay `quota`.
    pub fn with_accounts(
        config: HubConfig,
        relay: Arc<Relay>,
        db: Db,
        quota: Arc<Quota>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            relay,
            db: Some(db),
            quota: Some(quota),
            inner: Mutex::new(HubInner::default()),
            avatar_order: tokio::sync::Mutex::new(()),
        })
    }

    /// Salas abiertas.
    pub fn rooms(&self) -> usize {
        self.inner
            .lock()
            .map(|inner| inner.rooms.len())
            .unwrap_or(0)
    }

    /// Salas abiertas, sin identificarlas.
    pub fn room_summaries(&self) -> Vec<RoomSummary> {
        self.inner
            .lock()
            .map(|inner| {
                inner
                    .rooms
                    .iter()
                    .map(|(id, room)| RoomSummary {
                        id: *id,
                        guests: room.guests.len(),
                        relay: room.relay == RelayState::Available,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Avisa a las sesiones de `to` de que `from` (con su nombre visible) le pide amistad.
    pub fn friend_requested(&self, to: i64, from: i64, from_name: &str) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let message = SignalMessage::FriendRequested {
            from_user: u64::try_from(from).unwrap_or(0),
            from_name: from_name.to_owned(),
        };
        Self::deliver_user(&mut inner, to, &message);
    }

    /// Si `user` tiene alguna sesión de señalización abierta.
    pub fn is_online(&self, user: i64) -> bool {
        self.inner
            .lock()
            .map(|inner| inner.online.get(&user).is_some_and(|set| !set.is_empty()))
            .unwrap_or(false)
    }

    /// Qué hace `user` ahora (para `GET /friends`).
    pub fn activity(&self, user: i64) -> Activity {
        self.inner
            .lock()
            .map(|inner| inner.activity_of(user))
            .unwrap_or_default()
    }

    /// Avisa a los amigos conectados de lo que hace `user` ahora (se recalcula bajo el cerrojo,
    /// como [`Hub::announce`]: el último aviso siempre es el estado real). Sin sesiones abiertas
    /// no se avisa: eso lo cuenta `Presence`.
    async fn announce_activity(&self, user: i64) {
        let friends = self.friend_ids(Some(user)).await;
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if !inner.online.get(&user).is_some_and(|set| !set.is_empty()) {
            return;
        }
        let message = SignalMessage::Activity {
            user: u64::try_from(user).unwrap_or(0),
            activity: inner.activity_of(user),
        };
        for friend in &friends {
            Self::deliver_user(&mut inner, *friend, &message);
        }
    }

    /// Como [`Hub::announce_activity`], para el usuario de `session`.
    async fn announce_session_activity(&self, session: SessionId) {
        let user = self.inner.lock().ok().and_then(|inner| {
            inner
                .sessions
                .get(&session)
                .and_then(|entry| entry.account.user)
        });
        if let Some(user) = user {
            self.announce_activity(user).await;
        }
    }

    /// Pide entrar en la sala que `to_user` tiene abierta: se le hace llegar al anfitrión.
    async fn join_request(&self, session: SessionId, to_user: UserId) -> Reply {
        let account = self.session_account(session)?;
        let (Some(db), Some(from)) = (&self.db, account.user) else {
            return Err((
                ErrorCode::BadRequest,
                "pedir entrar necesita una cuenta (no funciona en modo desarrollo)".into(),
            ));
        };
        let to = i64::try_from(to_user)
            .map_err(|_| (ErrorCode::BadRequest, "usuario no válido".to_owned()))?;
        if to == from {
            return Err((
                ErrorCode::BadRequest,
                "no puedes pedir entrar en tu propia sala".to_owned(),
            ));
        }
        let friends = db.are_friends(from, to).await.map_err(|err| {
            warn!(error = %err, "no se pudo comprobar la amistad");
            (ErrorCode::Internal, "error de la base de datos".to_owned())
        })?;
        if !friends {
            return Err((
                ErrorCode::NotFriend,
                "solo puedes pedir entrar en la sala de un amigo".to_owned(),
            ));
        }
        let mut inner = self.inner.lock().map_err(|_| poisoned())?;
        let now = Instant::now();
        if inner
            .join_requests
            .get(&(from, to))
            .is_some_and(|last| now.duration_since(*last) < JOIN_REQUEST_EVERY)
        {
            return Err((
                ErrorCode::BadRequest,
                "ya se lo has pedido: espera a que responda".to_owned(),
            ));
        }
        let host_session = inner.hosted_room(to).map(|room| room.host.session).ok_or((
            ErrorCode::RoomNotFound,
            "tu amigo no tiene ninguna sala abierta".to_owned(),
        ))?;
        inner.join_requests.insert((from, to), now);
        inner
            .join_requests
            .retain(|_, last| now.duration_since(*last) < JOIN_REQUEST_TTL);
        Self::deliver(
            &mut inner,
            host_session,
            SignalMessage::JoinRequested {
                from_user: account.user_id(),
                from_name: account.name.clone(),
            },
        );
        Ok(Vec::new())
    }

    /// El anfitrión no deja entrar a `to_user`: se le dice.
    fn join_decline(&self, session: SessionId, to_user: UserId) -> Reply {
        let account = self.session_account(session)?;
        let Some(from) = account.user else {
            return Err((
                ErrorCode::BadRequest,
                "pedir entrar necesita una cuenta (no funciona en modo desarrollo)".into(),
            ));
        };
        let to = i64::try_from(to_user)
            .map_err(|_| (ErrorCode::BadRequest, "usuario no válido".to_owned()))?;
        let mut inner = self.inner.lock().map_err(|_| poisoned())?;
        if inner.hosted_room(from).is_none() {
            return Err((
                ErrorCode::BadRequest,
                "solo el anfitrión de una sala puede responder".to_owned(),
            ));
        }
        // Solo se contesta a quien lo pidió y aún espera (la aplicación lo muestra el mismo tiempo).
        let asked = inner.join_requests.remove(&(to, from));
        if asked.is_none_or(|at| at.elapsed() >= JOIN_REQUEST_TTL) {
            return Err((
                ErrorCode::BadRequest,
                "ese usuario no ha pedido entrar".to_owned(),
            ));
        }
        Self::deliver_user(
            &mut inner,
            to,
            &SignalMessage::JoinDeclined {
                from_user: account.user_id(),
                from_name: account.name.clone(),
            },
        );
        Ok(Vec::new())
    }

    /// Cierra todas las sesiones de `user` (cuenta deshabilitada o contraseña cambiada).
    pub fn disconnect_user(&self, user: i64) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let sessions: Vec<SessionId> = inner
            .online
            .get(&user)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default();
        for session in sessions {
            Self::deliver(
                &mut inner,
                session,
                error(
                    ErrorCode::Unauthorized,
                    "el administrador ha cerrado tu sesión",
                ),
            );
            if let Some(entry) = inner.sessions.get_mut(&session)
                && let Some(close) = entry.close.take()
            {
                let _ = close.send(());
            }
        }
    }

    /// La sala `room` se quedó sin relay por la cuota: avisa a sus miembros.
    pub fn quota_cut(&self, room: u64) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let Some(entry) = inner.rooms.get_mut(&room) else {
            return;
        };
        entry.relay = RelayState::Quota;
        let sessions: Vec<SessionId> = std::iter::once(entry.host.session)
            .chain(entry.guests.values().map(|member| member.session))
            .collect();
        for session in sessions {
            Self::deliver(
                &mut inner,
                session,
                SignalMessage::QuotaExceeded {
                    message: QUOTA_MESSAGE.to_owned(),
                },
            );
        }
    }

    /// Atiende los cortes por cuota del relay hasta que se cierre el canal.
    pub async fn watch_quota(self: Arc<Self>, mut cuts: mpsc::UnboundedReceiver<u64>) {
        while let Some(room) = cuts.recv().await {
            self.quota_cut(room);
        }
    }

    fn register(
        &self,
        tx: mpsc::Sender<SignalMessage>,
        close: oneshot::Sender<()>,
        account: Account,
    ) -> Option<(SessionId, bool)> {
        let mut inner = self.inner.lock().ok()?;
        let id = inner.next_session;
        inner.next_session += 1;
        let mut first = false;
        if let Some(user) = account.user {
            let set = inner.online.entry(user).or_default();
            first = set.is_empty();
            set.insert(id);
        }
        inner.sessions.insert(
            id,
            Session {
                tx,
                close: Some(close),
                member: None,
                account,
            },
        );
        Some((id, first))
    }

    /// Envía a una sesión sin esperar; si su cola está llena, la cierra.
    fn deliver(inner: &mut HubInner, session: SessionId, message: SignalMessage) {
        let Some(entry) = inner.sessions.get_mut(&session) else {
            return;
        };
        if entry.tx.try_send(message).is_err() {
            warn!(session, "sesión de señalización saturada; se cierra");
            if let Some(close) = entry.close.take() {
                let _ = close.send(());
            }
        }
    }

    /// Envía a todas las sesiones de `user`.
    fn deliver_user(inner: &mut HubInner, user: i64, message: &SignalMessage) -> usize {
        let sessions: Vec<SessionId> = inner
            .online
            .get(&user)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default();
        for session in &sessions {
            Self::deliver(inner, *session, message.clone());
        }
        sessions.len()
    }

    /// Avisa a los amigos conectados de que `user` entra o sale.
    ///
    /// Quien llama decide `online` bajo el cerrojo, pero avisa después de esperar a la base de
    /// datos (los amigos); entretanto otra sesión del mismo usuario puede haber entrado o salido
    /// (la sala de espera da paso a la sala del anfitrión). Por eso se vuelve a mirar el estado
    /// ahora, con el cerrojo que también cubre la entrega: un aviso que ya no es verdad se descarta
    /// y el último que llega a los amigos siempre es el estado real.
    fn announce(&self, user: i64, friends: &[i64], online: bool) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let now_online = inner.online.get(&user).is_some_and(|set| !set.is_empty());
        if now_online != online {
            debug!(user, online, "aviso de presencia caducado; se descarta");
            return;
        }
        let message = SignalMessage::Presence {
            user: u64::try_from(user).unwrap_or(0),
            online,
        };
        for friend in friends {
            Self::deliver_user(&mut inner, *friend, &message);
        }
    }

    /// `user` cambió de avatar (o lo quitó): lo apuntan sus sesiones y las salas en las que está
    /// (para los `Joined` y `PeerJoined` siguientes) y se avisa con `Avatar` a sus amigos
    /// conectados, a sus otras sesiones y a quien comparte sala con él. La versión se lee de la
    /// base en fila (`avatar_order`): dos cambios seguidos avisan siempre de la última.
    pub async fn avatar_changed(&self, user: i64) {
        let Some(db) = &self.db else {
            return;
        };
        let _order = self.avatar_order.lock().await;
        let avatar = match db.user_by_id(user).await {
            Ok(Some(row)) => row.avatar,
            Ok(None) => return,
            Err(err) => {
                warn!(user, %err, "no se pudo leer el avatar para avisar");
                return;
            }
        };
        let friends = self.friend_ids(Some(user)).await;
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        for session in inner.sessions.values_mut() {
            if session.account.user == Some(user) {
                session.account.avatar = avatar;
            }
        }
        // A sus amigos, a sus otras sesiones y a quien comparte sala con él (amigo o no).
        let mut targets: std::collections::BTreeSet<i64> = friends.into_iter().collect();
        targets.insert(user);
        for room in inner.rooms.values_mut() {
            let mut inside = false;
            for member in std::iter::once(&mut room.host).chain(room.guests.values_mut()) {
                if member.account.user == Some(user) {
                    member.account.avatar = avatar;
                    inside = true;
                }
            }
            if inside {
                let members = std::iter::once(&room.host).chain(room.guests.values());
                targets.extend(members.filter_map(|member| member.account.user));
            }
        }
        let message = SignalMessage::Avatar {
            user: u64::try_from(user).unwrap_or(0),
            avatar,
        };
        for target in targets {
            Self::deliver_user(&mut inner, target, &message);
        }
    }

    async fn friend_ids(&self, user: Option<i64>) -> Vec<i64> {
        match (&self.db, user) {
            (Some(db), Some(user)) => db.friend_ids(user).await.unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    fn relay_token(
        &self,
        grant: Grant,
        relay_addr: SocketAddr,
    ) -> Result<SignalMessage, (ErrorCode, String)> {
        let token = self.relay.grant(grant).ok_or_else(|| {
            (
                ErrorCode::Internal,
                "no se pudo generar el token del relay".to_owned(),
            )
        })?;
        Ok(SignalMessage::RelayToken {
            token: token.to_vec(),
            addr: relay_addr,
        })
    }

    /// Lo que recibe un par sobre el relay de su sala.
    fn relay_message(
        &self,
        state: RelayState,
        grant: Grant,
        relay_addr: SocketAddr,
    ) -> Result<SignalMessage, (ErrorCode, String)> {
        match state {
            RelayState::Available => self.relay_token(grant, relay_addr),
            RelayState::Disabled => Ok(SignalMessage::RelayDisabled),
            RelayState::Quota => Ok(SignalMessage::QuotaExceeded {
                message: QUOTA_MESSAGE.to_owned(),
            }),
        }
    }

    fn session_account(&self, session: SessionId) -> Result<Account, (ErrorCode, String)> {
        let inner = self.inner.lock().map_err(|_| poisoned())?;
        inner
            .sessions
            .get(&session)
            .map(|entry| entry.account.clone())
            .ok_or((ErrorCode::Internal, "sesión desconocida".to_owned()))
    }

    fn check_identity(
        account: &Account,
        identity_pk: &[u8; 32],
    ) -> Result<(), (ErrorCode, String)> {
        match account.device_pk {
            Some(device) if device != *identity_pk => Err((
                ErrorCode::BadRequest,
                "la identidad no es la del dispositivo con el que iniciaste sesión".to_owned(),
            )),
            _ => Ok(()),
        }
    }

    async fn create_room(
        &self,
        session: SessionId,
        identity_pk: [u8; 32],
        name: &str,
        relay_addr: SocketAddr,
    ) -> Reply {
        if name.chars().count() > MAX_ROOM_NAME {
            return Err((
                ErrorCode::BadRequest,
                format!("el nombre de la sala no puede pasar de {MAX_ROOM_NAME} caracteres"),
            ));
        }
        let account = self.session_account(session)?;
        Self::check_identity(&account, &identity_pk)?;
        let relay = if !self.config.relay_enabled {
            RelayState::Disabled
        } else {
            match (&self.quota, account.user) {
                (Some(quota), Some(user)) => {
                    if let Err(err) = quota.load(user).await {
                        warn!(error = %err, "no se pudo leer el uso del relay");
                    }
                    if quota.exceeded(user) {
                        RelayState::Quota
                    } else {
                        RelayState::Available
                    }
                }
                _ => RelayState::Available,
            }
        };
        let mut inner = self.inner.lock().map_err(|_| poisoned())?;
        if inner
            .sessions
            .get(&session)
            .is_some_and(|entry| entry.member.is_some())
        {
            return Err((ErrorCode::BadRequest, "ya estás en una sala".to_owned()));
        }
        let code = loop {
            let code = random_code().ok_or_else(|| {
                (
                    ErrorCode::Internal,
                    "no se pudo generar el código de sala".to_owned(),
                )
            })?;
            if !inner.codes.contains_key(&code) {
                break code;
            }
        };
        let room_id = inner.next_room;
        inner.next_room += 1;
        inner.codes.insert(code.clone(), room_id);
        let host_user = account.user;
        inner.rooms.insert(
            room_id,
            Room {
                code: code.clone(),
                host: Member {
                    session,
                    identity_pk,
                    account,
                },
                guests: BTreeMap::new(),
                next_peer: HOST_PEER_ID + 1,
                relay,
            },
        );
        if let Some(entry) = inner.sessions.get_mut(&session) {
            entry.member = Some((room_id, HOST_PEER_ID));
        }
        drop(inner);
        info!(room = room_id, relay = ?relay, "sala creada");
        let relay_message = self.relay_message(
            relay,
            Grant {
                room: room_id,
                peer: HOST_PEER_ID,
                role: Role::Host,
                identity_pk,
                host_user,
            },
            relay_addr,
        )?;
        Ok(vec![SignalMessage::RoomCreated { code }, relay_message])
    }

    fn join_room(
        &self,
        session: SessionId,
        code: &str,
        identity_pk: [u8; 32],
        key_package: Vec<u8>,
        relay_addr: SocketAddr,
    ) -> Reply {
        let code = code.trim().to_ascii_uppercase();
        if !is_valid_room_code(&code) {
            return Err((
                ErrorCode::RoomNotFound,
                "el código de sala no es válido".to_owned(),
            ));
        }
        let mut inner = self.inner.lock().map_err(|_| poisoned())?;
        let Some(account) = inner
            .sessions
            .get(&session)
            .map(|entry| entry.account.clone())
        else {
            return Err((ErrorCode::Internal, "sesión desconocida".to_owned()));
        };
        Self::check_identity(&account, &identity_pk)?;
        if inner
            .sessions
            .get(&session)
            .is_some_and(|entry| entry.member.is_some())
        {
            return Err((ErrorCode::BadRequest, "ya estás en una sala".to_owned()));
        }
        let failure_key = match account.user {
            Some(user) => FailureKey::User(user),
            None => FailureKey::Session(session),
        };
        let now = Instant::now();
        if inner.recent_failures(failure_key, now) >= MAX_JOIN_FAILURES {
            warn!(session, "demasiados códigos de sala que no existen");
            return Err((
                ErrorCode::RoomNotFound,
                "demasiados códigos de sala que no existen; espera un minuto y comprueba el código \
                 con el anfitrión"
                    .to_owned(),
            ));
        }
        let Some(&room_id) = inner.codes.get(&code) else {
            inner.note_failure(failure_key, now);
            return Err((ErrorCode::RoomNotFound, format!("no existe la sala {code}")));
        };
        let max_guests = self.config.max_guests as usize;
        let Some(room) = inner.rooms.get_mut(&room_id) else {
            return Err((ErrorCode::RoomNotFound, format!("no existe la sala {code}")));
        };
        if room.guests.len() >= max_guests {
            return Err((
                ErrorCode::RoomFull,
                format!("la sala ya tiene {max_guests} invitados"),
            ));
        }
        let peer_id = room.next_peer;
        room.next_peer += 1;
        let user = account.user_id();
        let name = account.name.clone();
        let avatar = account.avatar;
        room.guests.insert(
            peer_id,
            Member {
                session,
                identity_pk,
                account,
            },
        );
        let host_session = room.host.session;
        let host_pk = room.host.identity_pk;
        let host_user = room.host.account.user_id();
        let host_user_id = room.host.account.user;
        let host_name = room.host.account.name.clone();
        let host_avatar = room.host.account.avatar;
        let relay = room.relay;
        if let Some(entry) = inner.sessions.get_mut(&session) {
            entry.member = Some((room_id, peer_id));
        }
        Self::deliver(
            &mut inner,
            host_session,
            SignalMessage::PeerJoined {
                peer_id,
                identity_pk,
                key_package,
                user,
                name,
                avatar,
            },
        );
        drop(inner);
        info!(room = room_id, peer = peer_id, "invitado en la sala");
        let relay_message = self.relay_message(
            relay,
            Grant {
                room: room_id,
                peer: peer_id,
                role: Role::Guest,
                identity_pk,
                host_user: host_user_id,
            },
            relay_addr,
        )?;
        Ok(vec![
            SignalMessage::Joined {
                room_id,
                host_pk,
                your_peer_id: peer_id,
                host_user,
                host_name,
                host_avatar,
            },
            relay_message,
        ])
    }

    async fn invite(&self, session: SessionId, to_user: UserId, code: &str) -> Reply {
        let account = self.session_account(session)?;
        let (Some(db), Some(from)) = (&self.db, account.user) else {
            return Err((
                ErrorCode::BadRequest,
                "las invitaciones necesitan una cuenta (no funcionan en modo desarrollo)".into(),
            ));
        };
        let to = i64::try_from(to_user)
            .map_err(|_| (ErrorCode::BadRequest, "usuario no válido".to_owned()))?;
        if to == from {
            return Err((
                ErrorCode::BadRequest,
                "no puedes invitarte a ti mismo".to_owned(),
            ));
        }
        let code = code.trim().to_ascii_uppercase();
        {
            let inner = self.inner.lock().map_err(|_| poisoned())?;
            let room = inner
                .codes
                .get(&code)
                .and_then(|id| inner.rooms.get(id))
                .ok_or((ErrorCode::RoomNotFound, format!("no existe la sala {code}")))?;
            if room.host.account.user != Some(from) {
                return Err((
                    ErrorCode::BadRequest,
                    "solo el anfitrión puede invitar a su sala".to_owned(),
                ));
            }
        }
        let friends = db.are_friends(from, to).await.map_err(|err| {
            warn!(error = %err, "no se pudo comprobar la amistad");
            (ErrorCode::Internal, "error de la base de datos".to_owned())
        })?;
        if !friends {
            return Err((
                ErrorCode::NotFriend,
                "solo puedes invitar a tus amigos (o darle el código de la sala)".to_owned(),
            ));
        }
        let mut inner = self.inner.lock().map_err(|_| poisoned())?;
        let delivered = Self::deliver_user(
            &mut inner,
            to,
            &SignalMessage::Invited {
                from_user: account.user_id(),
                from_name: account.name.clone(),
                code,
            },
        );
        if delivered == 0 {
            return Err((
                ErrorCode::UserOffline,
                "tu amigo no está conectado: dale el código de la sala".to_owned(),
            ));
        }
        Ok(Vec::new())
    }

    /// Sesiones destino de un mensaje de `from` en su sala (`None` = todos los demás).
    fn targets(
        inner: &HubInner,
        session: SessionId,
        to: Option<PeerId>,
    ) -> Result<(PeerId, Vec<SessionId>), (ErrorCode, String)> {
        let (room_id, from) = inner
            .sessions
            .get(&session)
            .and_then(|entry| entry.member)
            .ok_or((ErrorCode::BadRequest, "no estás en ninguna sala".to_owned()))?;
        let room = inner
            .rooms
            .get(&room_id)
            .ok_or((ErrorCode::RoomClosed, "la sala se ha cerrado".to_owned()))?;
        let member_session = |peer: PeerId| {
            if peer == HOST_PEER_ID {
                Some(room.host.session)
            } else {
                room.guests.get(&peer).map(|member| member.session)
            }
        };
        let sessions = match to {
            Some(peer) if peer == from => {
                return Err((
                    ErrorCode::BadRequest,
                    "no puedes escribirte a ti mismo".into(),
                ));
            }
            Some(peer) => vec![member_session(peer).ok_or((
                ErrorCode::PeerNotFound,
                format!("el par {peer} no está en la sala"),
            ))?],
            None => std::iter::once(HOST_PEER_ID)
                .chain(room.guests.keys().copied())
                .filter(|peer| *peer != from)
                .filter_map(member_session)
                .collect(),
        };
        Ok((from, sessions))
    }

    fn forward(&self, session: SessionId, message: SignalMessage) -> Reply {
        let mut inner = self.inner.lock().map_err(|_| poisoned())?;
        let (to, build): (Option<PeerId>, Box<dyn Fn(PeerId) -> SignalMessage>) = match message {
            SignalMessage::Candidates { to_peer, addrs, .. } => (
                Some(to_peer),
                Box::new(move |from_peer| SignalMessage::Candidates {
                    from_peer,
                    to_peer,
                    addrs: addrs.clone(),
                }),
            ),
            SignalMessage::Mls {
                to_peer, payload, ..
            } => (
                to_peer,
                Box::new(move |from_peer| SignalMessage::Mls {
                    from_peer,
                    to_peer,
                    payload: payload.clone(),
                }),
            ),
            _ => return Err((ErrorCode::BadRequest, "mensaje no reenviable".to_owned())),
        };
        let (from, sessions) = Self::targets(&inner, session, to)?;
        for target in sessions {
            Self::deliver(&mut inner, target, build(from));
        }
        Ok(Vec::new())
    }

    /// Expulsa a `peer` de la sala del anfitrión `session`; devuelve también el usuario expulsado
    /// (su actividad cambia).
    fn kick(&self, session: SessionId, peer: PeerId) -> (Reply, Option<i64>) {
        let mut kicked = None;
        let reply = self.kick_inner(session, peer, &mut kicked);
        (reply, kicked)
    }

    fn kick_inner(&self, session: SessionId, peer: PeerId, kicked: &mut Option<i64>) -> Reply {
        let mut inner = self.inner.lock().map_err(|_| poisoned())?;
        let (room_id, from) = inner
            .sessions
            .get(&session)
            .and_then(|entry| entry.member)
            .ok_or((ErrorCode::BadRequest, "no estás en ninguna sala".to_owned()))?;
        if from != HOST_PEER_ID {
            return Err((
                ErrorCode::BadRequest,
                "solo el anfitrión puede expulsar".to_owned(),
            ));
        }
        let member = inner
            .rooms
            .get_mut(&room_id)
            .and_then(|room| room.guests.remove(&peer))
            .ok_or((
                ErrorCode::PeerNotFound,
                format!("el par {peer} no está en la sala"),
            ))?;
        *kicked = member.account.user;
        Self::deliver(
            &mut inner,
            member.session,
            error(ErrorCode::Kicked, "el anfitrión te ha expulsado de la sala"),
        );
        if let Some(entry) = inner.sessions.get_mut(&member.session) {
            entry.member = None;
            if let Some(close) = entry.close.take() {
                let _ = close.send(());
            }
        }
        drop(inner);
        self.relay.revoke_peer(room_id, peer);
        info!(room = room_id, peer, "invitado expulsado");
        Ok(vec![SignalMessage::PeerLeft { peer_id: peer }])
    }

    /// La sesión terminó: sale de su sala (y la cierra si era el anfitrión).
    fn leave(&self, session: SessionId) -> Left {
        let mut left = Left::default();
        let Ok(mut inner) = self.inner.lock() else {
            return left;
        };
        let Some(entry) = inner.sessions.remove(&session) else {
            return left;
        };
        inner.join_failures.remove(&FailureKey::Session(session));
        if let Some(user) = entry.account.user
            && let Some(set) = inner.online.get_mut(&user)
        {
            set.remove(&session);
            if set.is_empty() {
                inner.online.remove(&user);
                left.last = Some(user);
            }
        }
        let Some((room_id, peer)) = entry.member else {
            return left;
        };
        left.changed.extend(entry.account.user);
        if peer == HOST_PEER_ID {
            let Some(room) = inner.rooms.remove(&room_id) else {
                return left;
            };
            inner.codes.remove(&room.code);
            for member in room.guests.values() {
                left.changed.extend(member.account.user);
                Self::deliver(
                    &mut inner,
                    member.session,
                    error(ErrorCode::RoomClosed, "el anfitrión ha cerrado la sala"),
                );
                if let Some(entry) = inner.sessions.get_mut(&member.session) {
                    entry.member = None;
                }
            }
            drop(inner);
            self.relay.close_room(room_id);
            info!(room = room_id, "sala cerrada");
        } else {
            let host = inner.rooms.get_mut(&room_id).map(|room| {
                room.guests.remove(&peer);
                room.host.session
            });
            if let Some(host) = host {
                Self::deliver(&mut inner, host, SignalMessage::PeerLeft { peer_id: peer });
            }
            drop(inner);
            self.relay.revoke_peer(room_id, peer);
            info!(room = room_id, peer, "invitado fuera de la sala");
        }
        left
    }

    async fn handle(
        &self,
        session: SessionId,
        message: SignalMessage,
        relay_addr: SocketAddr,
    ) -> Reply {
        match message {
            SignalMessage::CreateRoom { identity_pk, name } => {
                let reply = self
                    .create_room(session, identity_pk, &name, relay_addr)
                    .await;
                if reply.is_ok() {
                    self.announce_session_activity(session).await;
                }
                reply
            }
            SignalMessage::JoinRoom {
                code,
                identity_pk,
                key_package,
            } => {
                let reply = self.join_room(session, &code, identity_pk, key_package, relay_addr);
                if reply.is_ok() {
                    self.announce_session_activity(session).await;
                }
                reply
            }
            message @ (SignalMessage::Candidates { .. } | SignalMessage::Mls { .. }) => {
                self.forward(session, message)
            }
            SignalMessage::Kick { peer_id } => {
                let (reply, kicked) = self.kick(session, peer_id);
                if let Some(user) = kicked {
                    self.announce_activity(user).await;
                }
                reply
            }
            SignalMessage::Invite { to_user, code } => self.invite(session, to_user, &code).await,
            SignalMessage::JoinRequest { to_user } => self.join_request(session, to_user).await,
            SignalMessage::JoinDecline { to_user } => self.join_decline(session, to_user),
            SignalMessage::Auth { .. } => {
                Err((ErrorCode::BadRequest, "ya estás autenticado".to_owned()))
            }
            _ => Err((
                ErrorCode::BadRequest,
                "mensaje no válido para el servidor".to_owned(),
            )),
        }
    }

    /// Comprueba el `Auth` y devuelve la cuenta, o el error que hay que enviar.
    async fn authenticate(&self, message: Option<SignalMessage>) -> Result<Account, SignalMessage> {
        let Some(SignalMessage::Auth {
            protocol_version,
            token,
            device_pk,
        }) = message
        else {
            return Err(error(
                ErrorCode::Unauthorized,
                "el primer mensaje debe ser Auth con el token de sesión",
            ));
        };
        if protocol_version != PROTOCOL_VERSION {
            return Err(error(
                ErrorCode::ProtocolVersion,
                format!(
                    "el servidor habla la versión {PROTOCOL_VERSION} del protocolo y el cliente \
                     la {protocol_version}; actualiza coroom"
                ),
            ));
        }
        if let Some(dev) = self.config.dev_token.as_deref()
            && constant_time_eq(&token, dev)
        {
            return Ok(Account {
                user: None,
                name: DEV_NAME.to_owned(),
                avatar: None,
                device_pk: None,
            });
        }
        let Some(db) = &self.db else {
            return Err(error(
                ErrorCode::Unauthorized,
                "token de desarrollo no válido (COROOM_DEV_TOKEN)",
            ));
        };
        let session = match db.session(&token_hash(&token)).await {
            Ok(Some(session)) => session,
            Ok(None) => {
                return Err(error(
                    ErrorCode::Unauthorized,
                    "la sesión no es válida o ha caducado: vuelve a iniciar sesión con \
                     `coroom login`",
                ));
            }
            Err(err) => {
                warn!(error = %err, "no se pudo leer la sesión");
                return Err(error(ErrorCode::Internal, "error de la base de datos"));
            }
        };
        if session.user.disabled {
            return Err(error(
                ErrorCode::Unauthorized,
                "tu cuenta está deshabilitada; habla con el administrador del servidor",
            ));
        }
        match &session.device {
            Some(device) if device.public_key == device_pk => {
                let _ = db.touch_device(device.id).await;
                Ok(Account {
                    user: Some(session.user.id),
                    name: session.user.display_name,
                    avatar: session.user.avatar,
                    device_pk: Some(device_pk),
                })
            }
            Some(_) => Err(error(
                ErrorCode::Unauthorized,
                "este dispositivo no es el vinculado al token: inicia sesión desde él con \
                 `coroom login`",
            )),
            None => Err(error(
                ErrorCode::Unauthorized,
                "el token de la interfaz web no abre la señalización: inicia sesión con \
                 `coroom login`",
            )),
        }
    }

    /// Atiende el stream de señalización de una sesión hasta que se cierra.
    pub async fn run<S>(self: Arc<Self>, stream: S, remote: SocketAddr, relay_addr: SocketAddr)
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let (tx, mut rx) = mpsc::channel::<SignalMessage>(SESSION_QUEUE);
        let writer_task = tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                let Ok(frame) = codec::encode(&message) else {
                    continue;
                };
                if writer.write_all(&frame).await.is_err() {
                    break;
                }
            }
            let _ = writer.shutdown().await;
        });
        let mut buffer = FrameBuffer::with_max(MAX_SIGNAL_MESSAGE);

        // Autenticación.
        let auth = tokio::time::timeout(AUTH_TIMEOUT, read_message(&mut reader, &mut buffer))
            .await
            .ok()
            .flatten();
        let account = match self.authenticate(auth).await {
            Ok(account) => account,
            Err(rejection) => {
                debug!(%remote, "señalización rechazada");
                let _ = tx.send(rejection).await;
                drop(tx);
                let _ = writer_task.await;
                return;
            }
        };

        let (close_tx, mut close_rx) = oneshot::channel();
        let user = account.user;
        let Some((session, first)) = self.register(tx.clone(), close_tx, account) else {
            return;
        };
        let _ = tx.send(SignalMessage::Observed { addr: remote }).await;
        let friends = self.friend_ids(user).await;
        if let Some(user) = user {
            if first {
                self.announce(user, &friends, true);
            }
            for friend in &friends {
                if self.is_online(*friend) {
                    let user = u64::try_from(*friend).unwrap_or(0);
                    let _ = tx
                        .send(SignalMessage::Presence { user, online: true })
                        .await;
                    let activity = self.activity(*friend);
                    if activity != Activity::Idle {
                        let _ = tx.send(SignalMessage::Activity { user, activity }).await;
                    }
                }
            }
        }
        debug!(%remote, session, ?user, "sesión de señalización abierta");

        loop {
            let message = tokio::select! {
                message = read_message(&mut reader, &mut buffer) => message,
                _ = &mut close_rx => None,
            };
            let Some(message) = message else { break };
            match self.handle(session, message, relay_addr).await {
                Ok(replies) => {
                    for reply in replies {
                        if tx.send(reply).await.is_err() {
                            break;
                        }
                    }
                }
                Err((code, message)) => {
                    if tx.send(error(code, message)).await.is_err() {
                        break;
                    }
                }
            }
        }
        let left = self.leave(session);
        if let Some(user) = left.last {
            let friends = self.friend_ids(Some(user)).await;
            self.announce(user, &friends, false);
        }
        for user in left.changed {
            self.announce_activity(user).await;
        }
        drop(tx);
        let _ = tokio::time::timeout(Duration::from_secs(2), writer_task).await;
        debug!(session, "sesión de señalización cerrada");
    }
}

/// Lee un mensaje completo; `None` si el stream se cierra o el mensaje no es válido.
async fn read_message<R>(reader: &mut R, buffer: &mut FrameBuffer) -> Option<SignalMessage>
where
    R: AsyncRead + Unpin,
{
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        match buffer.next_message::<SignalMessage>() {
            Ok(Some(message)) => return Some(message),
            Ok(None) => {}
            Err(err) => {
                debug!(error = %err, "mensaje de señalización no válido");
                return None;
            }
        }
        let read = reader.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend(&chunk[..read]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_codes_use_the_unambiguous_alphabet() {
        for _ in 0..100 {
            let code = random_code().expect("código");
            assert!(is_valid_room_code(&code), "{code}");
        }
    }

    #[test]
    fn dev_accounts_have_no_user_and_skip_the_identity_check() {
        let dev = Account {
            user: None,
            name: DEV_NAME.to_owned(),
            avatar: None,
            device_pk: None,
        };
        assert_eq!(dev.user_id(), 0);
        assert!(Hub::check_identity(&dev, &[1; 32]).is_ok());
        let real = Account {
            user: Some(7),
            name: "Ana".to_owned(),
            avatar: None,
            device_pk: Some([2; 32]),
        };
        assert_eq!(real.user_id(), 7);
        assert!(Hub::check_identity(&real, &[2; 32]).is_ok());
        assert!(Hub::check_identity(&real, &[1; 32]).is_err());
    }

    fn session(hub: &Hub, user: Option<i64>) -> SessionId {
        let (tx, _rx) = mpsc::channel(SESSION_QUEUE);
        let (close, _closed) = oneshot::channel();
        let account = Account {
            user,
            name: "Ana".to_owned(),
            avatar: None,
            device_pk: None,
        };
        hub.register(tx, close, account).expect("sesión").0
    }

    /// Hallazgo S-3: adivinar códigos de sala tiene un límite por cuenta que no se salta abriendo
    /// otra sesión, y un código bueno sigue fallando mientras dura el castigo.
    #[test]
    fn guessing_room_codes_is_rate_limited_per_account() {
        let hub = Hub::new(HubConfig::new(8), Relay::new());
        let host = session(&hub, Some(1));
        let addr: SocketAddr = ([127, 0, 0, 1], 4433).into();
        let created = block_on(hub.create_room(host, [1; 32], "sala", addr)).expect("sala creada");
        let Some(SignalMessage::RoomCreated { code }) = created.first().cloned() else {
            panic!("sin código: {created:?}");
        };
        let wrong = if code == "AAAAAA" { "BBBBBB" } else { "AAAAAA" };
        let guesser = session(&hub, Some(2));
        for _ in 0..MAX_JOIN_FAILURES {
            let err = hub
                .join_room(guesser, wrong, [2; 32], vec![1], addr)
                .expect_err("no existe");
            assert_eq!(err.0, ErrorCode::RoomNotFound);
            assert!(err.1.contains("no existe la sala"), "{}", err.1);
        }
        // Otra sesión de la misma cuenta hereda el límite, incluso con el código bueno.
        let again = session(&hub, Some(2));
        let err = hub
            .join_room(again, &code, [2; 32], vec![1], addr)
            .expect_err("limitado");
        assert!(err.1.contains("demasiados"), "{}", err.1);
        // Otra cuenta no.
        let other = session(&hub, Some(3));
        let err = hub
            .join_room(other, wrong, [3; 32], vec![1], addr)
            .expect_err("no existe");
        assert!(err.1.contains("no existe la sala"), "{}", err.1);
        // Pasada la ventana, se olvida.
        if let Ok(mut inner) = hub.inner.lock() {
            let past = Instant::now()
                .checked_sub(JOIN_FAILURE_WINDOW)
                .expect("reloj");
            for failures in inner.join_failures.values_mut() {
                for at in failures.iter_mut() {
                    *at = past;
                }
            }
        }
        assert!(hub.join_room(again, &code, [2; 32], vec![1], addr).is_ok());
    }

    /// La sala de espera da paso a la sala del anfitrión: el «desconectado» de la sesión que se
    /// cierra, decidido antes de que la otra entrase, llega tarde y no tapa el «conectado».
    #[test]
    fn a_late_offline_announcement_does_not_overtake_online() {
        let hub = Hub::new(HubConfig::new(8), Relay::new());
        let (tx, mut friend_rx) = mpsc::channel(SESSION_QUEUE);
        let (close, _closed) = oneshot::channel();
        let friend = Account {
            user: Some(1),
            name: "Bea".to_owned(),
            avatar: None,
            device_pk: None,
        };
        hub.register(tx, close, friend).expect("sesión del amigo");
        let lobby = session(&hub, Some(2));
        // La sala de espera se cierra: era la última sesión, toca «desconectado»…
        assert_eq!(hub.leave(lobby).last, Some(2));
        // …pero antes de avisar (se esperaba a la base de datos) entra la sala del anfitrión.
        let (host, first) = {
            let (tx, _rx) = mpsc::channel(SESSION_QUEUE);
            let (close, _closed) = oneshot::channel();
            let account = Account {
                user: Some(2),
                name: "Ana".to_owned(),
                avatar: None,
                device_pk: None,
            };
            hub.register(tx, close, account).expect("sesión de la sala")
        };
        assert!(first);
        hub.announce(2, &[1], true);
        hub.announce(2, &[1], false);
        let mut seen = Vec::new();
        while let Ok(message) = friend_rx.try_recv() {
            seen.push(message);
        }
        assert_eq!(
            seen,
            [SignalMessage::Presence {
                user: 2,
                online: true
            }],
            "el aviso caducado se descarta"
        );
        // Al cerrarse de verdad, sí llega.
        assert_eq!(hub.leave(host).last, Some(2));
        hub.announce(2, &[1], false);
        assert_eq!(
            friend_rx.try_recv().ok(),
            Some(SignalMessage::Presence {
                user: 2,
                online: false
            })
        );
    }

    /// Ejecuta un futuro hasta el final en un runtime de un hilo.
    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime")
            .block_on(future)
    }

    /// Hallazgo S-2: un mensaje que anuncia más de [`MAX_SIGNAL_MESSAGE`] corta la sesión sin
    /// esperar ni guardar su cuerpo.
    #[test]
    fn an_oversized_signalling_message_ends_the_session() {
        block_on(async {
            let (mut client, server) = tokio::io::duplex(1024);
            let announced = (MAX_SIGNAL_MESSAGE as u32 + 1).to_be_bytes();
            client.write_all(&announced).await.expect("prefijo");
            let mut reader = server;
            let mut buffer = FrameBuffer::with_max(MAX_SIGNAL_MESSAGE);
            let read = tokio::time::timeout(
                Duration::from_secs(1),
                read_message(&mut reader, &mut buffer),
            )
            .await
            .expect("sin esperar al cuerpo");
            assert!(read.is_none());
            assert!(buffer.pending() <= 4);
        });
    }

    /// Hallazgo S-2: lo más grande que viaja por la señalización cabe en [`MAX_SIGNAL_MESSAGE`]
    /// con mucho margen, incluso en una sala de 32 invitados.
    #[test]
    fn the_largest_signalling_messages_fit() {
        use coroom_transport::crypto::hpke::signed_pq_key;
        use coroom_transport::crypto::{Identity, KeyPackageBundle, RoomGroup};
        let host = Identity::generate().expect("identidad");
        let mut group = RoomGroup::create(&host).expect("grupo");
        let mut largest = signed_pq_key(&host).expect("clave X-Wing").len();
        for _ in 0..32 {
            let guest = Identity::generate().expect("identidad");
            let (key_package, _bundle) = KeyPackageBundle::generate(&guest).expect("KeyPackage");
            largest = largest.max(key_package.len());
            let change = group.add_member(&key_package).expect("alta");
            largest = largest.max(change.commit.len());
            largest = largest.max(change.welcome.as_ref().map_or(0, Vec::len));
        }
        println!("mensaje de señalización más grande con 32 invitados: {largest} B");
        assert!(largest * 4 < MAX_SIGNAL_MESSAGE, "{largest} B");
    }
}
