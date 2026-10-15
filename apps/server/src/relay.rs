//! Relay de medios (ALPN `coroom-relay/1`).
//!
//! Cada par conecta por QUIC y se autentica en un primer stream (`0x03` + `Hello` con el token que
//! le dio la señalización). Después:
//!
//! - los datagramas del anfitrión se reenvían a todos los invitados de la sala (**abanico**) y los
//!   de cada invitado al anfitrión;
//! - cada stream de control que abre un invitado se empalma con uno nuevo que el relay abre en la
//!   conexión del anfitrión (con el `Hello` del invitado, cuya identidad fija el token);
//! - el relay solo lee la [`ObjectHeader`] de los datagramas y guarda los objetos de origen de los
//!   últimos 16 grupos de vídeo por sala para responder a los `Nack` de los invitados sin molestar
//!   al anfitrión (los del stream de control y sus copias en datagramas de la pista `Control`).
//!   Nunca ve el contenido: va cifrado con SFrame.
//! - la voz de cada invitado sube al anfitrión como cualquier datagrama suyo; el
//!   anfitrión la reenvía sin descifrar y el relay la abre en abanico a todos los invitados (el que
//!   habló descarta la suya por el emisor de su SFrame).
//! - **Cuentas y cuotas**: cada token va ligado a la sala, al par y al **usuario
//!   anfitrión** de la sala ([`Grant::host_user`]); los bytes de datagramas que reenvía la sala se
//!   cargan a ese usuario ([`Quota`]) y, si agota su cuota mensual, el relay corta la sala y lo
//!   avisa a la señalización ([`Relay::set_exceeded_sender`]). `COROOM_RELAY_MAX_KBPS` limita cada
//!   sala con un cubo de fichas: lo que pasa del límite se descarta y se cuenta.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use coroom_transport::connect::STREAM_RELAY_AUTH;
use coroom_transport::proto::PROTOCOL_VERSION;
use coroom_transport::proto::codec::{self, FrameBuffer};
use coroom_transport::proto::control::{self, ControlMessage, Hello, Role};
use coroom_transport::proto::media::{ObjectHeader, Track};
use coroom_transport::proto::signal::PeerId;

use crate::quota::Quota;

/// Token opaco del relay.
pub type Token = [u8; 32];

/// Grupos de la caché de `Nack` de cada sala.
pub const CACHE_GROUPS: usize = 16;

/// Espera máxima del stream de autenticación.
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);

/// Espera máxima al anfitrión cuando un invitado abre su control antes de que llegue.
const HOST_WAIT: Duration = Duration::from_secs(30);

/// Tipos de stream de `coroom-transport` (primer byte).
const STREAM_CONTROL: u8 = 0x01;
const STREAM_INPUT: u8 = 0x02;

/// Credencial que la señalización expide para un par.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grant {
    /// Sala.
    pub room: u64,
    /// Par dentro de la sala.
    pub peer: PeerId,
    /// Papel.
    pub role: Role,
    /// Identidad del par.
    pub identity_pk: [u8; 32],
    /// Usuario anfitrión de la sala, al que se cargan los bytes (`None` en modo desarrollo).
    pub host_user: Option<i64>,
}

/// Cubo de fichas del límite por sala (`COROOM_RELAY_MAX_KBPS`).
#[derive(Debug)]
struct Bucket {
    rate: f64,
    burst: f64,
    tokens: f64,
    last: Instant,
}

impl Bucket {
    fn new(kbps: u32) -> Self {
        let rate = f64::from(kbps) * 125.0;
        // Medio segundo de ráfaga (un intra cabe), y al menos 64 KiB.
        let burst = (rate / 2.0).max(65_536.0);
        Self {
            rate,
            burst,
            tokens: burst,
            last: Instant::now(),
        }
    }

    fn allow(&mut self, bytes: usize, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.rate).min(self.burst);
        let bytes = bytes as f64;
        if self.tokens >= bytes {
            self.tokens -= bytes;
            true
        } else {
            false
        }
    }
}

#[derive(Debug)]
struct CachedGroup {
    track: Track,
    group: u32,
    sources: Vec<Option<Bytes>>,
}

/// Objetos de origen de los últimos [`CACHE_GROUPS`] grupos.
#[derive(Debug, Default)]
struct GroupCache {
    groups: VecDeque<CachedGroup>,
}

impl GroupCache {
    fn insert(&mut self, header: &ObjectHeader, datagram: Bytes) {
        // Solo el vídeo usa `Nack`; el audio (100 objetos por segundo y pista) echaría de
        // la caché los fotogramas que sí se piden.
        if header.is_repair() || header.track != Track::Video {
            return;
        }
        let index = match self
            .groups
            .iter()
            .position(|cached| cached.track == header.track && cached.group == header.group)
        {
            Some(index) => index,
            None => {
                self.groups.push_back(CachedGroup {
                    track: header.track,
                    group: header.group,
                    sources: vec![None; usize::from(header.object_count)],
                });
                while self.groups.len() > CACHE_GROUPS {
                    self.groups.pop_front();
                }
                self.groups.len() - 1
            }
        };
        if let Some(slot) = self.groups[index]
            .sources
            .get_mut(usize::from(header.object))
        {
            *slot = Some(datagram);
        }
    }

    /// Los objetos que pide un `Nack`, si están todos en la caché.
    fn lookup(&self, track: Track, group: u32, missing: &[u16]) -> Option<Vec<Bytes>> {
        let cached = self
            .groups
            .iter()
            .find(|cached| cached.track == track && cached.group == group)?;
        if missing.is_empty() {
            cached.sources.iter().cloned().collect()
        } else {
            missing
                .iter()
                .map(|index| cached.sources.get(usize::from(*index)).cloned().flatten())
                .collect()
        }
    }
}

#[derive(Debug)]
struct RelayRoom {
    host: watch::Sender<Option<quinn::Connection>>,
    guests: HashMap<PeerId, quinn::Connection>,
    cache: GroupCache,
    bytes: u64,
    bucket: Option<Bucket>,
    dropped: u64,
}

impl RelayRoom {
    fn new(max_kbps: u32) -> Self {
        Self {
            host: watch::channel(None).0,
            guests: HashMap::new(),
            cache: GroupCache::default(),
            bytes: 0,
            bucket: (max_kbps > 0).then(|| Bucket::new(max_kbps)),
            dropped: 0,
        }
    }

    /// Pasa el límite de la sala; si no, cuenta el descarte.
    fn admit(&mut self, bytes: usize) -> bool {
        let allowed = self
            .bucket
            .as_mut()
            .is_none_or(|bucket| bucket.allow(bytes, Instant::now()));
        if !allowed {
            self.dropped += 1;
        }
        allowed
    }
}

/// Contadores de una sala del relay.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoomCounters {
    /// Bytes reenviados.
    pub bytes: u64,
    /// Datagramas descartados por el límite de la sala.
    pub dropped: u64,
}

#[derive(Debug, Default)]
struct RelayInner {
    grants: HashMap<Token, Grant>,
    rooms: HashMap<u64, RelayRoom>,
}

/// Estado del relay del servidor.
#[derive(Debug, Default)]
pub struct Relay {
    inner: Mutex<RelayInner>,
    total_bytes: AtomicU64,
    nack_hits: AtomicU64,
    quota: Option<Arc<Quota>>,
    max_kbps: u32,
    exceeded: Mutex<Option<mpsc::UnboundedSender<u64>>>,
}

fn random_token() -> Option<Token> {
    let mut token = [0u8; 32];
    getrandom::fill(&mut token).ok()?;
    Some(token)
}

impl Relay {
    /// Relay vacío, sin cuotas ni límite por sala.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Relay que carga los bytes a `quota` y limita cada sala a `max_kbps` (`0` = sin límite).
    pub fn with_limits(quota: Option<Arc<Quota>>, max_kbps: u32) -> Arc<Self> {
        Arc::new(Self {
            quota,
            max_kbps,
            ..Self::default()
        })
    }

    /// A dónde se avisa (con el id de la sala) cuando una sala se corta por la cuota.
    pub fn set_exceeded_sender(&self, sender: mpsc::UnboundedSender<u64>) {
        if let Ok(mut exceeded) = self.exceeded.lock() {
            *exceeded = Some(sender);
        }
    }

    /// Carga `bytes` de la sala a su anfitrión; `false` si agotó la cuota (y corta la sala).
    fn charge(&self, room: u64, host_user: Option<i64>, bytes: u64) -> bool {
        let (Some(quota), Some(user)) = (&self.quota, host_user) else {
            return true;
        };
        if quota.charge(user, bytes) {
            return true;
        }
        warn!(room, "relay: cuota mensual agotada; se corta la sala");
        self.close_room(room);
        if let Ok(exceeded) = self.exceeded.lock()
            && let Some(sender) = exceeded.as_ref()
        {
            let _ = sender.send(room);
        }
        false
    }

    /// Contadores de cada sala activa.
    pub fn counters(&self) -> HashMap<u64, RoomCounters> {
        self.inner
            .lock()
            .map(|inner| {
                inner
                    .rooms
                    .iter()
                    .map(|(id, room)| {
                        (
                            *id,
                            RoomCounters {
                                bytes: room.bytes,
                                dropped: room.dropped,
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Expide un token para `grant`; `None` si el sistema no da aleatoriedad.
    pub fn grant(&self, grant: Grant) -> Option<Token> {
        let token = random_token()?;
        if let Ok(mut inner) = self.inner.lock() {
            inner.grants.insert(token, grant);
        }
        Some(token)
    }

    /// Retira los tokens de un par y cierra sus conexiones al relay.
    pub fn revoke_peer(&self, room: u64, peer: PeerId) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner
            .grants
            .retain(|_, grant| !(grant.room == room && grant.peer == peer));
        if let Some(relay_room) = inner.rooms.get_mut(&room) {
            if let Some(conn) = relay_room.guests.remove(&peer) {
                conn.close(quinn::VarInt::from_u32(2), b"fuera de la sala");
            }
            if peer == coroom_transport::proto::signal::HOST_PEER_ID
                && let Some(conn) = relay_room.host.send_replace(None)
            {
                conn.close(quinn::VarInt::from_u32(2), b"fuera de la sala");
            }
        }
    }

    /// Retira todos los tokens de una sala y cierra sus conexiones.
    pub fn close_room(&self, room: u64) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.grants.retain(|_, grant| grant.room != room);
        if let Some(relay_room) = inner.rooms.remove(&room) {
            if let Some(conn) = relay_room.host.send_replace(None) {
                conn.close(quinn::VarInt::from_u32(2), b"sala cerrada");
            }
            for conn in relay_room.guests.values() {
                conn.close(quinn::VarInt::from_u32(2), b"sala cerrada");
            }
        }
    }

    /// Bytes reenviados por el relay desde el arranque (todas las salas).
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes.load(Ordering::Relaxed)
    }

    /// `Nack` respondidos desde la caché sin pasar por el anfitrión.
    pub fn nack_hits(&self) -> u64 {
        self.nack_hits.load(Ordering::Relaxed)
    }

    /// Bytes reenviados en cada sala activa (sin identificar las salas).
    pub fn room_bytes(&self) -> Vec<u64> {
        let mut bytes: Vec<u64> = self
            .inner
            .lock()
            .map(|inner| inner.rooms.values().map(|room| room.bytes).collect())
            .unwrap_or_default();
        bytes.sort_unstable_by(|a, b| b.cmp(a));
        bytes
    }

    /// Atiende una conexión con ALPN `coroom-relay/1` hasta que se cierra.
    pub async fn handle(self: Arc<Self>, conn: quinn::Connection) {
        let remote = conn.remote_address();
        let grant = match tokio::time::timeout(AUTH_TIMEOUT, self.authenticate(&conn)).await {
            Ok(Some(grant)) => grant,
            _ => {
                debug!(%remote, "relay: autenticación fallida");
                conn.close(quinn::VarInt::from_u32(1), b"token");
                return;
            }
        };
        info!(room = grant.room, peer = grant.peer, role = ?grant.role, "relay: par conectado");
        match grant.role {
            Role::Host => self.run_host(conn, grant).await,
            Role::Guest => self.run_guest(conn, grant).await,
        }
    }

    async fn authenticate(&self, conn: &quinn::Connection) -> Option<Grant> {
        let (mut send, mut recv) = conn.accept_bi().await.ok()?;
        let mut kind = [0u8; 1];
        recv.read_exact(&mut kind).await.ok()?;
        if kind[0] != STREAM_RELAY_AUTH {
            return None;
        }
        let mut buffer = FrameBuffer::new();
        let hello = match read_message(&mut recv, &mut buffer).await? {
            ControlMessage::Hello(hello) => hello,
            _ => return None,
        };
        if hello.protocol_version != PROTOCOL_VERSION {
            return None;
        }
        let token: Token = hello.relay_token.as_deref()?.try_into().ok()?;
        let grant = {
            let mut inner = self.inner.lock().ok()?;
            let grant = *inner.grants.get(&token)?;
            if grant.role != hello.role {
                return None;
            }
            let max_kbps = self.max_kbps;
            let room = inner
                .rooms
                .entry(grant.room)
                .or_insert_with(|| RelayRoom::new(max_kbps));
            match grant.role {
                Role::Host => {
                    if let Some(old) = room.host.send_replace(Some(conn.clone())) {
                        old.close(quinn::VarInt::from_u32(3), b"reemplazada");
                    }
                }
                Role::Guest => {
                    if let Some(old) = room.guests.insert(grant.peer, conn.clone()) {
                        old.close(quinn::VarInt::from_u32(3), b"reemplazada");
                    }
                }
            }
            grant
        };
        let reply = ControlMessage::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            identity_pk: grant.identity_pk,
            role: grant.role,
            relay_token: None,
        });
        send.write_all(&codec::encode(&reply).ok()?).await.ok()?;
        // El stream de autenticación queda abierto mientras dure la conexión.
        tokio::spawn(async move {
            let _keep = send;
            while let Ok(Some(_)) = recv.read_chunk(1024, true).await {}
        });
        Some(grant)
    }

    async fn run_host(self: Arc<Self>, conn: quinn::Connection, grant: Grant) {
        while let Ok(datagram) = conn.read_datagram().await {
            let Ok((header, _)) = ObjectHeader::parse_datagram(&datagram) else {
                continue;
            };
            let targets: Vec<quinn::Connection> = match self.inner.lock() {
                Ok(mut inner) => match inner.rooms.get_mut(&grant.room) {
                    Some(room) => {
                        // El límite por sala recorta el vídeo, que es lo que lo llena; el audio,
                        // el cursor y los datagramas de control pasan (perderlos por una ráfaga de
                        // vídeo congelaba el audio y retrasaba los `Nack`). La cuota de bytes
                        // ([`Relay::charge`]) cuenta todo igual.
                        if header.track == Track::Video && !room.admit(datagram.len()) {
                            continue;
                        }
                        room.cache.insert(&header, datagram.clone());
                        room.bytes += (datagram.len() * room.guests.len()) as u64;
                        room.guests.values().cloned().collect()
                    }
                    None => break,
                },
                Err(_) => break,
            };
            let forwarded = (datagram.len() * targets.len()) as u64;
            self.total_bytes.fetch_add(forwarded, Ordering::Relaxed);
            if !self.charge(grant.room, grant.host_user, forwarded) {
                break;
            }
            for target in targets {
                let _ = target.send_datagram(datagram.clone());
            }
        }
        if let Ok(inner) = self.inner.lock()
            && let Some(room) = inner.rooms.get(&grant.room)
        {
            room.host.send_if_modified(|current| {
                if current
                    .as_ref()
                    .is_some_and(|host| host.stable_id() == conn.stable_id())
                {
                    *current = None;
                    true
                } else {
                    false
                }
            });
        }
        debug!(room = grant.room, "relay: anfitrión desconectado");
    }

    async fn run_guest(self: Arc<Self>, conn: quinn::Connection, grant: Grant) {
        let datagrams = {
            let relay = Arc::clone(&self);
            let conn = conn.clone();
            tokio::spawn(async move {
                while let Ok(datagram) = conn.read_datagram().await {
                    if !relay.admit(grant.room, datagram.len()) {
                        continue;
                    }
                    // Un datagrama de control lleva en `group` la etiqueta del invitado (su id de
                    // par), con la que el anfitrión atribuye cada `Feedback`. La pone el relay
                    // (la del grant, autenticado): si no, un invitado podría firmar como otro un
                    // `Feedback` con los contadores al máximo y dejarlo sin control de tasa (el
                    // anfitrión descartaría los suyos por viejos).
                    let datagram = retag_control(datagram, grant.peer);
                    // La copia en datagrama de un `Nack` se responde desde la caché como la del
                    // stream; si no está, sigue hacia el anfitrión.
                    if let Ok(ControlMessage::Nack {
                        track,
                        group,
                        missing,
                    }) = control::parse_control_datagram(&datagram)
                        && let Some(cached) = relay.cached(grant.room, track, group, &missing)
                    {
                        relay.nack_hits.fetch_add(1, Ordering::Relaxed);
                        for datagram in cached {
                            let _ = conn.send_datagram(datagram);
                        }
                        continue;
                    }
                    if let Some(host) = relay.host_of(grant.room) {
                        let len = datagram.len() as u64;
                        relay.total_bytes.fetch_add(len, Ordering::Relaxed);
                        let _ = host.send_datagram(datagram);
                        if !relay.charge(grant.room, grant.host_user, len) {
                            break;
                        }
                    }
                }
            })
        };
        while let Ok((send, recv)) = conn.accept_bi().await {
            let relay = Arc::clone(&self);
            let conn = conn.clone();
            tokio::spawn(async move { relay.splice_guest_stream(conn, grant, send, recv).await });
        }
        datagrams.abort();
        if let Ok(mut inner) = self.inner.lock()
            && let Some(room) = inner.rooms.get_mut(&grant.room)
            && room
                .guests
                .get(&grant.peer)
                .is_some_and(|current| current.stable_id() == conn.stable_id())
        {
            room.guests.remove(&grant.peer);
        }
        debug!(
            room = grant.room,
            peer = grant.peer,
            "relay: invitado desconectado"
        );
    }

    /// Un datagrama de un invitado pasa el límite de la sala (y suma a sus bytes).
    fn admit(&self, room: u64, bytes: usize) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        let Some(room) = inner.rooms.get_mut(&room) else {
            return false;
        };
        if !room.admit(bytes) {
            return false;
        }
        room.bytes += bytes as u64;
        true
    }

    fn host_of(&self, room: u64) -> Option<quinn::Connection> {
        let inner = self.inner.lock().ok()?;
        inner.rooms.get(&room)?.host.borrow().clone()
    }

    async fn wait_host(&self, room: u64) -> Option<quinn::Connection> {
        let mut watcher = {
            let inner = self.inner.lock().ok()?;
            inner.rooms.get(&room)?.host.subscribe()
        };
        let wait = async {
            loop {
                if let Some(host) = watcher.borrow_and_update().clone() {
                    return Some(host);
                }
                watcher.changed().await.ok()?;
            }
        };
        tokio::time::timeout(HOST_WAIT, wait).await.ok().flatten()
    }

    /// Empalma un stream que abre un invitado con uno nuevo en la conexión del anfitrión.
    async fn splice_guest_stream(
        &self,
        guest: quinn::Connection,
        grant: Grant,
        mut guest_send: quinn::SendStream,
        mut guest_recv: quinn::RecvStream,
    ) {
        let mut kind = [0u8; 1];
        if guest_recv.read_exact(&mut kind).await.is_err() {
            return;
        }
        let mut buffer = FrameBuffer::new();
        let first = match kind[0] {
            STREAM_CONTROL => {
                let Some(ControlMessage::Hello(hello)) =
                    read_message(&mut guest_recv, &mut buffer).await
                else {
                    return;
                };
                // La identidad la fija el token, no lo que diga el invitado.
                let hello = Hello {
                    identity_pk: grant.identity_pk,
                    role: Role::Guest,
                    relay_token: None,
                    ..hello
                };
                match codec::encode(&ControlMessage::Hello(hello)) {
                    Ok(frame) => Some(frame),
                    Err(_) => return,
                }
            }
            STREAM_INPUT => None,
            _ => return,
        };
        let Some(host) = self.wait_host(grant.room).await else {
            let _ = guest_send.reset(quinn::VarInt::from_u32(1));
            return;
        };
        let Ok((mut host_send, mut host_recv)) = host.open_bi().await else {
            return;
        };
        if host_send.write_all(&kind).await.is_err() {
            return;
        }
        if let Some(frame) = first
            && host_send.write_all(&frame).await.is_err()
        {
            return;
        }

        // Anfitrión → invitado: copia tal cual.
        let downstream = tokio::spawn(async move {
            while let Ok(Some(chunk)) = host_recv.read_chunk(64 * 1024, true).await {
                if guest_send.write_all(&chunk.bytes).await.is_err() {
                    break;
                }
            }
            let _ = guest_send.finish();
        });

        if kind[0] == STREAM_INPUT {
            while let Ok(Some(chunk)) = guest_recv.read_chunk(64 * 1024, true).await {
                if host_send.write_all(&chunk.bytes).await.is_err() {
                    break;
                }
            }
        } else {
            // Invitado → anfitrión: mensaje a mensaje, respondiendo los `Nack` desde la caché.
            'pump: loop {
                loop {
                    let body = match buffer.next_body() {
                        Ok(Some(body)) => body,
                        Ok(None) => break,
                        Err(_) => break 'pump,
                    };
                    if let Ok(ControlMessage::Nack {
                        track,
                        group,
                        missing,
                    }) = codec::decode::<ControlMessage>(&body)
                        && let Some(cached) = self.cached(grant.room, track, group, &missing)
                    {
                        self.nack_hits.fetch_add(1, Ordering::Relaxed);
                        for datagram in cached {
                            let _ = guest.send_datagram(datagram);
                        }
                        continue;
                    }
                    let mut frame = (body.len() as u32).to_be_bytes().to_vec();
                    frame.extend_from_slice(&body);
                    if host_send.write_all(&frame).await.is_err() {
                        break 'pump;
                    }
                }
                match guest_recv.read_chunk(codec::MAX_MESSAGE_LEN, true).await {
                    Ok(Some(chunk)) => buffer.extend(&chunk.bytes),
                    _ => break,
                }
            }
        }
        let _ = host_send.finish();
        let _ = downstream.await;
    }

    fn cached(&self, room: u64, track: Track, group: u32, missing: &[u16]) -> Option<Vec<Bytes>> {
        let inner = self.inner.lock().ok()?;
        inner.rooms.get(&room)?.cache.lookup(track, group, missing)
    }
}

/// Lee un mensaje de control completo de `recv`.
async fn read_message(
    recv: &mut quinn::RecvStream,
    buffer: &mut FrameBuffer,
) -> Option<ControlMessage> {
    loop {
        if let Some(message) = buffer.next_message::<ControlMessage>().ok()? {
            return Some(message);
        }
        let chunk = recv.read_chunk(codec::MAX_MESSAGE_LEN, true).await.ok()??;
        buffer.extend(&chunk.bytes);
    }
}

/// Si `datagram` es de la pista de control con una etiqueta distinta de `peer`, una copia con
/// `peer` en `group` (bytes 4-8 de la cabecera, ver `ObjectHeader::parse`); si no, tal cual.
fn retag_control(datagram: Bytes, peer: PeerId) -> Bytes {
    let Ok((header, _)) = ObjectHeader::parse_datagram(&datagram) else {
        return datagram;
    };
    if header.track != Track::Control || header.group == peer {
        return datagram;
    }
    let mut retagged = datagram.to_vec();
    retagged[4..8].copy_from_slice(&peer.to_be_bytes());
    Bytes::from(retagged)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn datagram(group: u32, object: u16, count: u16, repair: bool) -> (ObjectHeader, Bytes) {
        let mut flags = if repair { ObjectHeader::REPAIR } else { 0 };
        if !repair && object + 1 == count {
            flags |= ObjectHeader::LAST_OBJECT;
        }
        let header = ObjectHeader {
            track: Track::Video,
            flags,
            group,
            object,
            object_count: count,
            payload_len: 1,
            epoch_hint: 0,
        };
        let mut bytes = header.to_bytes().to_vec();
        bytes.push(object as u8);
        (header, Bytes::from(bytes))
    }

    #[test]
    fn cache_answers_complete_nacks_only() {
        let mut cache = GroupCache::default();
        for object in [0, 1, 3] {
            let (header, bytes) = datagram(5, object, 4, false);
            cache.insert(&header, bytes);
        }
        let (header, bytes) = datagram(5, 0, 4, true);
        cache.insert(&header, bytes);
        assert_eq!(
            cache.lookup(Track::Video, 5, &[1, 3]).map(|v| v.len()),
            Some(2)
        );
        assert!(cache.lookup(Track::Video, 5, &[2]).is_none());
        assert!(cache.lookup(Track::Video, 5, &[]).is_none(), "falta el 2");
        assert!(cache.lookup(Track::Audio, 5, &[1]).is_none());
        let (mut audio, bytes) = datagram(6, 0, 1, false);
        audio.track = Track::Audio;
        cache.insert(&audio, bytes);
        assert!(
            cache.lookup(Track::Audio, 6, &[]).is_none(),
            "el audio no se guarda"
        );
        let (header, bytes) = datagram(5, 2, 4, false);
        cache.insert(&header, bytes);
        assert_eq!(cache.lookup(Track::Video, 5, &[]).map(|v| v.len()), Some(4));
    }

    #[test]
    fn cache_keeps_the_last_sixteen_groups() {
        let mut cache = GroupCache::default();
        for group in 0..20 {
            let (header, bytes) = datagram(group, 0, 1, false);
            cache.insert(&header, bytes);
        }
        assert!(cache.lookup(Track::Video, 3, &[0]).is_none());
        assert!(cache.lookup(Track::Video, 4, &[0]).is_some());
        assert!(cache.lookup(Track::Video, 19, &[0]).is_some());
    }

    #[test]
    fn the_room_bucket_drops_over_the_limit() {
        let start = Instant::now();
        let mut bucket = Bucket::new(1_000);
        // 1000 kbit/s = 125 000 B/s; ráfaga de 65 536 B.
        assert!(bucket.allow(60_000, start));
        assert!(!bucket.allow(10_000, start));
        assert!(bucket.allow(10_000, start + Duration::from_millis(100)));
        let mut room = RelayRoom::new(1_000);
        assert!(room.admit(65_000));
        assert!(!room.admit(65_000));
        assert_eq!(room.dropped, 1);
        assert!(RelayRoom::new(0).admit(10_000_000), "sin límite");
    }

    #[test]
    fn tokens_are_unique_and_revocable() {
        let relay = Relay::new();
        let grant = Grant {
            room: 1,
            peer: 2,
            role: Role::Guest,
            identity_pk: [9; 32],
            host_user: Some(1),
        };
        let a = relay.grant(grant).expect("token");
        let b = relay.grant(grant).expect("token");
        assert_ne!(a, b);
        relay.revoke_peer(1, 2);
        assert!(relay.inner.lock().expect("estado").grants.is_empty());
    }
}
