//! Relay de reenvío ([`coroom_transport::proto::relay`]).
//!
//! El relay **no termina QUIC**: el anfitrión y cada invitado hablan su conexión de pares de
//! punta a punta (TLS con sus identidades) y el relay solo copia sus paquetes UDP de uno a otro,
//! como un servidor TURN (RFC 8656). No descifra ni vuelve a cifrar nada, y no ve ni las
//! cabeceras de los medios: por un camino lento o una CPU sin cifrado por hardware, el coste por
//! paquete es el de recibirlo y mandarlo.
//!
//! Las tramas comparten el socket UDP del servidor con HTTP/3: el [`Relay`] es el gancho de ese
//! socket ([`SegmentHook`]) y atiende cada trama en la tarea que recibe, sin pasar por `quinn`.
//!
//! - **Ligadura**: cada par se liga con el token que le dio la señalización sin mandarlo:
//!   `Allocate` con su identificador, una galleta atada a la dirección de origen (`Challenge`,
//!   sin estado: prueba que el par recibe ahí), la galleta firmada con el token (`Bind`) y la
//!   confirmación firmada por el relay (`Bound`, con el par). El relay olvida la ligadura tras
//!   [`BINDING_LIFETIME`] sin tráfico.
//! - **Reenvío**: un `Data` de una dirección ligada se reescribe con el par de origen y sale hacia
//!   la dirección del par de destino. Solo entre el anfitrión y los invitados de la misma sala
//!   (nunca entre invitados ni hacia fuera): no es un relay abierto. Los lotes que van al mismo
//!   destino salen juntos con GSO.
//! - **Cuentas y cuotas**: cada token va ligado a la sala, al par y al **usuario anfitrión** de
//!   la sala ([`Grant::host_user`]); los bytes que reenvía la sala se cargan a ese usuario
//!   ([`Quota`]) y, si agota su cuota mensual, el relay suelta la sala y lo avisa a la
//!   señalización ([`Relay::set_exceeded_sender`]). `COROOM_RELAY_MAX_KBPS` limita lo que el
//!   anfitrión manda a cada invitado y lo que cada invitado manda al anfitrión con un cubo de
//!   fichas (lo que pasa se descarta y se cuenta); el límite viaja en el `RelayToken` para que el
//!   anfitrión no apunte por encima.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use coroom_transport::proto::PROTOCOL_VERSION;
use coroom_transport::proto::control::Role;
use coroom_transport::proto::relay::{
    COOKIE_LEN, ErrorCode, Frame, TOKEN_ID_LEN, data_peer, is_relay_frame, set_data_peer,
};
use coroom_transport::proto::signal::{HOST_PEER_ID, PeerId};
use coroom_transport::quic::canonical;
use coroom_transport::relay::{BINDING_LIFETIME, bind_mac, bound_mac, mac, mac_eq, token_id};
use coroom_transport::socket::{Segment, SegmentHook, UdpIo};

use crate::quota::Quota;

/// Token opaco del relay.
pub type Token = [u8; 32];

/// Duración de cada época de las galletas: una vale en su época y en la siguiente.
const COOKIE_EPOCH: Duration = Duration::from_secs(30);

/// Cada cuánto se barren las ligaduras vencidas.
const SWEEP_INTERVAL: Duration = Duration::from_secs(10);

/// Margen del cubo de cada sentido sobre `COROOM_RELAY_MAX_KBPS`: el límite es del vídeo con su
/// reparación (lo que el anfitrión respeta), y por el mismo camino van el audio, la voz, el
/// control y las cabeceras de QUIC y del relay.
const BUCKET_HEADROOM: f64 = 1.05;
const BUCKET_EXTRA_KBPS: f64 = 512.0;

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

/// Cubo de fichas de un sentido de un invitado (`COROOM_RELAY_MAX_KBPS`).
#[derive(Debug)]
struct Bucket {
    rate: f64,
    burst: f64,
    tokens: f64,
    last: Instant,
}

impl Bucket {
    fn new(kbps: u32) -> Self {
        let rate = (f64::from(kbps) * BUCKET_HEADROOM + BUCKET_EXTRA_KBPS) * 125.0;
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

/// Un par ligado.
#[derive(Debug)]
struct Binding {
    grant: Grant,
    /// Dirección local a la que llega (la de origen de lo que se le reenvía).
    local: Option<IpAddr>,
    last: Instant,
    /// Lo que el anfitrión le manda (si es invitado).
    downlink: Option<Bucket>,
    /// Lo que manda él.
    uplink: Option<Bucket>,
}

/// Contadores de una sala del relay.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoomCounters {
    /// Bytes reenviados.
    pub bytes: u64,
    /// Paquetes descartados por el límite de la sala.
    pub dropped: u64,
}

/// Paquetes por reenviar al mismo destino, para un solo envío con GSO.
#[derive(Debug, Default)]
struct Batch {
    to: Option<(SocketAddr, Option<IpAddr>)>,
    segment: usize,
    count: usize,
    /// Si el último es más corto que `segment` (después de él no cabe ninguno).
    closed: bool,
    buf: Vec<u8>,
}

#[derive(Debug)]
struct RelayInner {
    /// Tokens por su identificador.
    grants: HashMap<[u8; TOKEN_ID_LEN], (Token, Grant)>,
    /// Ligaduras por dirección (canónica).
    bindings: HashMap<SocketAddr, Binding>,
    /// Dirección de cada par ligado.
    peers: HashMap<(u64, PeerId), SocketAddr>,
    rooms: HashMap<u64, RoomCounters>,
    /// Bytes reenviados por sala que todavía no se cargaron a la cuota.
    uncharged: HashMap<u64, (Option<i64>, u64)>,
    batch: Batch,
    last_sweep: Instant,
}

impl Default for RelayInner {
    fn default() -> Self {
        Self {
            grants: HashMap::new(),
            bindings: HashMap::new(),
            peers: HashMap::new(),
            rooms: HashMap::new(),
            uncharged: HashMap::new(),
            batch: Batch::default(),
            last_sweep: Instant::now(),
        }
    }
}

impl RelayInner {
    fn unbind(&mut self, addr: &SocketAddr) {
        if let Some(binding) = self.bindings.remove(addr) {
            let key = (binding.grant.room, binding.grant.peer);
            if self.peers.get(&key) == Some(addr) {
                self.peers.remove(&key);
            }
        }
    }

    fn sweep(&mut self, now: Instant) {
        if now.saturating_duration_since(self.last_sweep) < SWEEP_INTERVAL {
            return;
        }
        self.last_sweep = now;
        let expired: Vec<SocketAddr> = self
            .bindings
            .iter()
            .filter(|(_, binding)| now.saturating_duration_since(binding.last) > BINDING_LIFETIME)
            .map(|(addr, _)| *addr)
            .collect();
        for addr in expired {
            debug!(%addr, "relay: ligadura vencida");
            self.unbind(&addr);
        }
        let live: std::collections::HashSet<u64> =
            self.grants.values().map(|(_, grant)| grant.room).collect();
        self.rooms.retain(|room, _| live.contains(room));
    }
}

/// Estado del relay del servidor y gancho de su socket UDP.
#[derive(Debug)]
pub struct Relay {
    inner: Mutex<RelayInner>,
    /// Clave de las galletas (aleatoria en cada arranque).
    secret: [u8; 32],
    started: Instant,
    enabled: bool,
    total_bytes: AtomicU64,
    quota: Option<Arc<Quota>>,
    max_kbps: u32,
    exceeded: Mutex<Option<mpsc::UnboundedSender<u64>>>,
}

fn random_bytes<const N: usize>() -> Option<[u8; N]> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).ok()?;
    Some(bytes)
}

impl Relay {
    /// Relay vacío, sin cuotas ni límite por invitado.
    pub fn new() -> Arc<Self> {
        Self::with_limits(None, 0)
    }

    /// Relay que carga los bytes a `quota` y limita cada invitado a `max_kbps` (`0` = sin
    /// límite).
    pub fn with_limits(quota: Option<Arc<Quota>>, max_kbps: u32) -> Arc<Self> {
        Self::build(quota, max_kbps, true)
    }

    /// Relay apagado (`COROOM_RELAY_ENABLED=false`): el socket no mira ninguna trama.
    pub fn disabled(quota: Option<Arc<Quota>>) -> Arc<Self> {
        Self::build(quota, 0, false)
    }

    fn build(quota: Option<Arc<Quota>>, max_kbps: u32, enabled: bool) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(RelayInner::default()),
            // Sin aleatoriedad del sistema no hay tokens (`grant` falla) y nada se liga.
            secret: random_bytes().unwrap_or([0; 32]),
            started: Instant::now(),
            enabled,
            total_bytes: AtomicU64::new(0),
            quota,
            max_kbps,
            exceeded: Mutex::new(None),
        })
    }

    /// Límite de cada invitado en kbit/s (`0` = sin límite).
    pub fn max_kbps(&self) -> u32 {
        self.max_kbps
    }

    /// A dónde se avisa (con el id de la sala) cuando una sala se corta por la cuota.
    pub fn set_exceeded_sender(&self, sender: mpsc::UnboundedSender<u64>) {
        if let Ok(mut exceeded) = self.exceeded.lock() {
            *exceeded = Some(sender);
        }
    }

    /// Contadores de cada sala activa.
    pub fn counters(&self) -> HashMap<u64, RoomCounters> {
        self.inner
            .lock()
            .map(|inner| inner.rooms.clone())
            .unwrap_or_default()
    }

    /// Expide un token para `grant`; `None` si el sistema no da aleatoriedad.
    pub fn grant(&self, grant: Grant) -> Option<Token> {
        let token: Token = random_bytes()?;
        if let Ok(mut inner) = self.inner.lock() {
            inner.grants.insert(token_id(&token), (token, grant));
            inner.rooms.entry(grant.room).or_default();
        }
        Some(token)
    }

    /// Retira los tokens de un par y suelta su ligadura: lo que mande o le manden deja de
    /// pasar.
    pub fn revoke_peer(&self, room: u64, peer: PeerId) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner
            .grants
            .retain(|_, (_, grant)| !(grant.room == room && grant.peer == peer));
        if let Some(addr) = inner.peers.get(&(room, peer)).copied() {
            inner.unbind(&addr);
        }
    }

    /// Retira todos los tokens de una sala y suelta sus ligaduras.
    pub fn close_room(&self, room: u64) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        Self::close_room_locked(&mut inner, room);
    }

    fn close_room_locked(inner: &mut RelayInner, room: u64) {
        inner.grants.retain(|_, (_, grant)| grant.room != room);
        let addrs: Vec<SocketAddr> = inner
            .bindings
            .iter()
            .filter(|(_, binding)| binding.grant.room == room)
            .map(|(addr, _)| *addr)
            .collect();
        for addr in addrs {
            inner.unbind(&addr);
        }
        inner.rooms.remove(&room);
        inner.uncharged.remove(&room);
    }

    /// Bytes reenviados por el relay desde el arranque (todas las salas).
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes.load(Ordering::Relaxed)
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

    /// Pares ligados ahora (todas las salas).
    pub fn bound_peers(&self) -> usize {
        self.inner
            .lock()
            .map(|inner| inner.bindings.len())
            .unwrap_or(0)
    }

    /// Galleta que vale ahora para `from` y `token_id` (la del `Challenge`; para el arnés del
    /// fuzzer, que no puede leer la respuesta).
    #[cfg(feature = "fuzzing")]
    pub(crate) fn current_cookie(
        &self,
        from: SocketAddr,
        token_id: &[u8; TOKEN_ID_LEN],
    ) -> [u8; COOKIE_LEN] {
        self.cookie(canonical(from), token_id, self.epoch(Instant::now()))
    }

    /// Galleta de `from` para `token_id` en la época `epoch`.
    fn cookie(
        &self,
        from: SocketAddr,
        token_id: &[u8; TOKEN_ID_LEN],
        epoch: u64,
    ) -> [u8; COOKIE_LEN] {
        let mut message = format!("{from}").into_bytes();
        message.extend_from_slice(token_id);
        message.extend_from_slice(&epoch.to_be_bytes());
        mac(&self.secret, &message)
    }

    fn epoch(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.started).as_secs() / COOKIE_EPOCH.as_secs()
    }

    fn reply(io: &UdpIo, to: SocketAddr, local: Option<IpAddr>, frame: Frame<'_>) {
        if let Err(err) = io.send(to, local, &frame.encode(), None) {
            debug!(%to, error = %err, "relay: respuesta sin enviar");
        }
    }

    fn allocate(
        &self,
        io: &UdpIo,
        from: SocketAddr,
        local: Option<IpAddr>,
        version: u16,
        token_id: [u8; TOKEN_ID_LEN],
    ) {
        if version != PROTOCOL_VERSION {
            return Self::reply(io, from, local, Frame::Error(ErrorCode::Version));
        }
        let known = self
            .inner
            .lock()
            .is_ok_and(|inner| inner.grants.contains_key(&token_id));
        if !known {
            return Self::reply(io, from, local, Frame::Error(ErrorCode::UnknownToken));
        }
        let cookie = self.cookie(from, &token_id, self.epoch(Instant::now()));
        Self::reply(io, from, local, Frame::Challenge { token_id, cookie });
    }

    fn bind(
        &self,
        io: &UdpIo,
        from: SocketAddr,
        local: Option<IpAddr>,
        token_id: [u8; TOKEN_ID_LEN],
        cookie: [u8; COOKIE_LEN],
        signature: [u8; COOKIE_LEN],
    ) {
        let now = Instant::now();
        let epoch = self.epoch(now);
        let fresh = [epoch, epoch.saturating_sub(1)]
            .iter()
            .any(|epoch| mac_eq(&cookie, &self.cookie(from, &token_id, *epoch)));
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let Some((token, grant)) = inner.grants.get(&token_id).copied() else {
            drop(inner);
            return Self::reply(io, from, local, Frame::Error(ErrorCode::UnknownToken));
        };
        if !fresh || !mac_eq(&signature, &bind_mac(&token, &token_id, &cookie)) {
            drop(inner);
            return Self::reply(io, from, local, Frame::Error(ErrorCode::BadSignature));
        }
        // El mismo par desde otra dirección (otro puerto tras un cambio de red, o un segundo
        // intento): vale la última.
        if let Some(previous) = inner.peers.get(&(grant.room, grant.peer)).copied()
            && previous != from
        {
            inner.unbind(&previous);
        }
        inner.unbind(&from);
        let limited = (self.max_kbps > 0).then_some(self.max_kbps);
        let guest = grant.role == Role::Guest;
        inner.bindings.insert(
            from,
            Binding {
                grant,
                local,
                last: now,
                downlink: limited.filter(|_| guest).map(Bucket::new),
                uplink: limited.filter(|_| guest).map(Bucket::new),
            },
        );
        inner.peers.insert((grant.room, grant.peer), from);
        drop(inner);
        info!(room = grant.room, peer = grant.peer, role = ?grant.role, "relay: par ligado");
        let lifetime_s = BINDING_LIFETIME.as_secs() as u16;
        Self::reply(
            io,
            from,
            local,
            Frame::Bound {
                cookie,
                peer: grant.peer,
                lifetime_s,
                mac: bound_mac(&token, &cookie, grant.peer, lifetime_s),
            },
        );
    }

    fn refresh(&self, io: &UdpIo, from: SocketAddr, local: Option<IpAddr>) {
        let bound = self.inner.lock().is_ok_and(|mut inner| {
            inner.sweep(Instant::now());
            match inner.bindings.get_mut(&from) {
                Some(binding) => {
                    binding.last = Instant::now();
                    true
                }
                None => false,
            }
        });
        let frame = if bound {
            Frame::Refreshed
        } else {
            Frame::Error(ErrorCode::NotBound)
        };
        Self::reply(io, from, local, frame);
    }

    /// Reenvía un `Data` de `from` (ya ligado) hacia su destino; `false` si no está ligado.
    fn forward(&self, io: &UdpIo, from: SocketAddr, frame: &mut [u8]) -> bool {
        let Some(dest) = data_peer(frame) else {
            return true;
        };
        let now = Instant::now();
        let Ok(mut inner) = self.inner.lock() else {
            return true;
        };
        inner.sweep(now);
        let Some(source) = inner.bindings.get(&from) else {
            return false;
        };
        let grant = source.grant;
        // Solo anfitrión ↔ invitado de la misma sala.
        let allowed = match grant.role {
            Role::Host => dest != HOST_PEER_ID,
            Role::Guest => dest == HOST_PEER_ID,
        };
        let target = allowed
            .then(|| inner.peers.get(&(grant.room, dest)).copied())
            .flatten();
        let Some(target) = target else {
            return true;
        };
        let len = frame.len();
        let admitted = {
            let RelayInner { bindings, .. } = &mut *inner;
            let mut pass = true;
            if let Some(binding) = bindings.get_mut(&from) {
                binding.last = now;
                if let Some(bucket) = binding.uplink.as_mut() {
                    pass &= bucket.allow(len, now);
                }
            }
            let local = match bindings.get_mut(&target) {
                Some(binding) => {
                    if pass && let Some(bucket) = binding.downlink.as_mut() {
                        pass &= bucket.allow(len, now);
                    }
                    binding.local
                }
                None => None,
            };
            pass.then_some(local)
        };
        let counters = inner.rooms.entry(grant.room).or_default();
        let Some(local) = admitted else {
            counters.dropped += 1;
            return true;
        };
        counters.bytes += len as u64;
        let entry = inner
            .uncharged
            .entry(grant.room)
            .or_insert((grant.host_user, 0));
        entry.1 += len as u64;
        set_data_peer(frame, grant.peer);
        let batch = &mut inner.batch;
        let fits = batch.to == Some((target, local))
            && !batch.closed
            && len <= batch.segment
            && batch.count < io.max_gso_segments()
            && batch.buf.len() + len <= 64_000;
        if !fits {
            Self::send_batch(io, batch);
            batch.to = Some((target, local));
            batch.segment = len;
        }
        batch.buf.extend_from_slice(frame);
        batch.count += 1;
        batch.closed = len < batch.segment;
        self.total_bytes.fetch_add(len as u64, Ordering::Relaxed);
        true
    }

    fn send_batch(io: &UdpIo, batch: &mut Batch) {
        if let Some((to, local)) = batch.to.take()
            && !batch.buf.is_empty()
            && let Err(err) = io.send(to, local, &batch.buf, Some(batch.segment))
        {
            debug!(%to, error = %err, "relay: paquetes sin reenviar");
        }
        batch.buf.clear();
        batch.count = 0;
        batch.closed = false;
    }

    /// Carga a la cuota lo reenviado; corta las salas que la agotan.
    fn charge_pending(&self) {
        let pending: Vec<(u64, Option<i64>, u64)> = match self.inner.lock() {
            Ok(mut inner) => inner
                .uncharged
                .drain()
                .map(|(room, (user, bytes))| (room, user, bytes))
                .collect(),
            Err(_) => return,
        };
        let Some(quota) = &self.quota else {
            return;
        };
        for (room, user, bytes) in pending {
            let Some(user) = user else {
                continue;
            };
            if quota.charge(user, bytes) {
                continue;
            }
            warn!(room, "relay: cuota mensual agotada; se corta la sala");
            self.close_room(room);
            if let Ok(exceeded) = self.exceeded.lock()
                && let Some(sender) = exceeded.as_ref()
            {
                let _ = sender.send(room);
            }
        }
    }
}

impl SegmentHook for Relay {
    fn inspects(&self, _from: SocketAddr) -> bool {
        self.enabled
    }

    fn segment(
        &self,
        io: &UdpIo,
        from: SocketAddr,
        to: Option<IpAddr>,
        datagram: &mut [u8],
    ) -> Segment {
        if !is_relay_frame(datagram) {
            return Segment::Quic;
        }
        let from = canonical(from);
        if datagram.first() == Some(&coroom_transport::proto::relay::kind::DATA) {
            if !self.forward(io, from, datagram) {
                Self::reply(io, from, to, Frame::Error(ErrorCode::NotBound));
            }
            return Segment::Consumed;
        }
        match Frame::parse(datagram) {
            Some(Frame::Allocate { version, token_id }) => {
                self.allocate(io, from, to, version, token_id);
            }
            Some(Frame::Bind {
                version,
                token_id,
                cookie,
                mac,
            }) => {
                if version == PROTOCOL_VERSION {
                    self.bind(io, from, to, token_id, cookie, mac);
                } else {
                    Self::reply(io, from, to, Frame::Error(ErrorCode::Version));
                }
            }
            Some(Frame::Refresh) => self.refresh(io, from, to),
            _ => {}
        }
        Segment::Consumed
    }

    fn flush(&self, io: &UdpIo) {
        if let Ok(mut inner) = self.inner.lock() {
            Self::send_batch(io, &mut inner.batch);
        }
        self.charge_pending();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bucket_drops_over_the_limit_with_headroom() {
        let start = Instant::now();
        let mut bucket = Bucket::new(1_000);
        // (1000 × 1,05 + 512) kbit/s = 195 250 B/s; ráfaga de 97 625 B.
        assert!(bucket.allow(90_000, start));
        assert!(!bucket.allow(10_000, start));
        assert!(bucket.allow(10_000, start + Duration::from_millis(100)));
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

    #[test]
    fn cookies_are_bound_to_the_address_and_expire() {
        let relay = Relay::new();
        let id = [3; TOKEN_ID_LEN];
        let a: SocketAddr = "192.0.2.1:5000".parse().expect("dirección");
        let b: SocketAddr = "192.0.2.1:5001".parse().expect("dirección");
        assert_eq!(relay.cookie(a, &id, 4), relay.cookie(a, &id, 4));
        assert_ne!(relay.cookie(a, &id, 4), relay.cookie(b, &id, 4));
        assert_ne!(relay.cookie(a, &id, 4), relay.cookie(a, &id, 6));
        assert_ne!(
            relay.cookie(a, &id, 4),
            Relay::new().cookie(a, &id, 4),
            "otra clave en cada arranque"
        );
    }
}
