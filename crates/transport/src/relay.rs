//! Ligadura con el relay de reenvío ([`coroom_proto::relay`]): firmas que comparten cliente y
//! servidor, el gancho del socket del cliente ([`RelayHook`]) y la ligadura viva
//! ([`RelayBinding`]).
//!
//! Ligado, el relay es una ruta más del `Endpoint`: cada par de la sala tiene una dirección
//! virtual ([`virtual_addr`]) a la que `quinn` conecta como a cualquier otra, y el socket mete
//! esos paquetes en tramas [`Frame::Data`] hacia el relay y saca los que llegan de él. La conexión
//! QUIC va de punta a punta entre los pares (TLS con sus identidades, como en directo): el relay
//! no tiene sus claves y solo ve paquetes cifrados.

use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use aws_lc_rs::{digest, hmac};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use coroom_proto::PROTOCOL_VERSION;
use coroom_proto::relay::{
    COOKIE_LEN, DATA_HEADER, ErrorCode, Frame, MAC_LEN, TOKEN_ID_LEN, bind_message, bound_message,
    is_relay_frame,
};

use crate::TransportError;
use crate::quic::canonical;
use crate::socket::{Divert, Segment, SegmentHook, UdpIo, virtual_addr, virtual_peer};

/// Primera espera de cada paso de la ligadura; se dobla en cada reintento hasta [`MAX_RETRY`].
const FIRST_RETRY: Duration = Duration::from_millis(200);

/// Espera máxima entre reintentos de la ligadura.
const MAX_RETRY: Duration = Duration::from_secs(1);

/// Plazo de la ligadura entera.
pub const BIND_TIMEOUT: Duration = Duration::from_secs(5);

/// Cada cuánto se avisa al relay de que la ligadura sigue (aunque no haya datos: el anfitrión de
/// una sala vacía). El relay la suelta tras [`BINDING_LIFETIME`] sin nada.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(10);

/// Lo que el relay guarda una ligadura sin tráfico.
pub const BINDING_LIFETIME: Duration = Duration::from_secs(40);

/// Identificador público de un token: lo que viaja en vez del token.
pub fn token_id(token: &[u8]) -> [u8; TOKEN_ID_LEN] {
    let mut input = b"coroom relay id".to_vec();
    input.extend_from_slice(token);
    let hash = digest::digest(&digest::SHA256, &input);
    let mut id = [0; TOKEN_ID_LEN];
    id.copy_from_slice(&hash.as_ref()[..TOKEN_ID_LEN]);
    id
}

/// HMAC-SHA256 de `message` con `key`, recortado a [`MAC_LEN`].
pub fn mac(key: &[u8], message: &[u8]) -> [u8; MAC_LEN] {
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), message);
    let mut out = [0; MAC_LEN];
    out.copy_from_slice(&tag.as_ref()[..MAC_LEN]);
    out
}

/// Compara dos firmas en tiempo constante.
pub fn mac_eq(a: &[u8; MAC_LEN], b: &[u8; MAC_LEN]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Firma del par en un [`Frame::Bind`].
pub fn bind_mac(
    token: &[u8],
    token_id: &[u8; TOKEN_ID_LEN],
    cookie: &[u8; COOKIE_LEN],
) -> [u8; MAC_LEN] {
    mac(token, &bind_message(token_id, cookie))
}

/// Firma del relay en un [`Frame::Bound`].
pub fn bound_mac(
    token: &[u8],
    cookie: &[u8; COOKIE_LEN],
    peer: u32,
    lifetime_s: u16,
) -> [u8; MAC_LEN] {
    mac(token, &bound_message(cookie, peer, lifetime_s))
}

/// Ruta hacia el relay del gancho del cliente.
#[derive(Debug, Clone, Copy)]
struct Route {
    /// Dirección del relay (canónica).
    relay: SocketAddr,
    /// Si el relay confirmó la ligadura (hasta entonces los paquetes a pares virtuales se tiran).
    bound: bool,
}

/// Gancho del socket de un `Endpoint` cliente: las tramas del relay ligado.
#[derive(Debug, Default)]
pub struct RelayHook {
    route: RwLock<Option<Route>>,
    frames: std::sync::Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
}

impl RelayHook {
    fn route(&self) -> Option<Route> {
        self.route.read().ok().and_then(|route| *route)
    }

    fn set_route(&self, route: Option<Route>) {
        if let Ok(mut current) = self.route.write() {
            *current = route;
        }
    }

    /// Dirección del relay ligado, si lo hay.
    pub fn relay(&self) -> Option<SocketAddr> {
        self.route()
            .filter(|route| route.bound)
            .map(|route| route.relay)
    }
}

impl SegmentHook for RelayHook {
    fn inspects(&self, from: SocketAddr) -> bool {
        self.route()
            .is_some_and(|route| route.relay == canonical(from))
    }

    fn segment(
        &self,
        io: &UdpIo,
        _from: SocketAddr,
        _to: Option<std::net::IpAddr>,
        datagram: &mut [u8],
    ) -> Segment {
        if !is_relay_frame(datagram) {
            // La señalización con el mismo servidor.
            return Segment::Quic;
        }
        match Frame::parse(datagram) {
            Some(Frame::Data { peer, packet }) if !packet.is_empty() => Segment::Deliver {
                from: io.map(virtual_addr(peer)),
                skip: DATA_HEADER,
            },
            Some(Frame::Data { .. }) | None => Segment::Consumed,
            Some(_) => {
                if let Ok(frames) = self.frames.lock()
                    && let Some(frames) = frames.as_ref()
                {
                    let _ = frames.send(datagram.to_vec());
                }
                Segment::Consumed
            }
        }
    }

    fn divert(&self, destination: SocketAddr) -> Divert {
        let Some(peer) = virtual_peer(destination) else {
            return Divert::Direct;
        };
        match self.route() {
            Some(Route { relay, bound: true }) => Divert::Relay { relay, peer },
            _ => Divert::Drop,
        }
    }
}

/// Datos de una ligadura.
#[derive(Debug, Clone)]
pub struct RelayParams {
    /// Dirección UDP del relay (la del servidor de señalización).
    pub addr: SocketAddr,
    /// Token del `RelayToken`.
    pub token: Vec<u8>,
}

/// Estado de una ligadura.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingState {
    /// Ligado como el par `peer` de la sala.
    Bound {
        /// Par.
        peer: u32,
    },
    /// El relay la rechazó o la soltó y no se pudo rehacer.
    Lost(ErrorCode),
}

/// Ligadura viva con el relay; al soltarla, los pares virtuales dejan de ser alcanzables.
#[derive(Debug)]
pub struct RelayBinding {
    hook: Arc<RelayHook>,
    state: watch::Receiver<BindingState>,
    task: JoinHandle<()>,
}

impl RelayBinding {
    /// Estado actual.
    pub fn state(&self) -> BindingState {
        *self.state.borrow()
    }

    /// Cambios de estado (el relay la suelta por cuota, expulsión o sala cerrada).
    pub fn watch(&self) -> watch::Receiver<BindingState> {
        self.state.clone()
    }

    /// Dirección del relay.
    pub fn relay(&self) -> Option<SocketAddr> {
        self.hook.relay()
    }
}

impl Drop for RelayBinding {
    fn drop(&mut self) {
        self.task.abort();
        self.hook.set_route(None);
        if let Ok(mut frames) = self.hook.frames.lock() {
            *frames = None;
        }
    }
}

/// Liga el socket de `io`/`hook` al relay con `params` y la mantiene viva en segundo plano.
pub(crate) async fn bind(
    io: Arc<dyn SendRaw>,
    hook: Arc<RelayHook>,
    params: RelayParams,
) -> Result<RelayBinding, TransportError> {
    let relay = canonical(params.addr);
    let (tx, mut frames) = mpsc::unbounded_channel();
    if let Ok(mut current) = hook.frames.lock() {
        *current = Some(tx);
    }
    hook.set_route(Some(Route {
        relay,
        bound: false,
    }));
    let peer = match handshake(&*io, &mut frames, relay, &params).await {
        Ok(peer) => peer,
        Err(err) => {
            hook.set_route(None);
            return Err(err);
        }
    };
    hook.set_route(Some(Route { relay, bound: true }));
    debug!(%relay, peer, "ligado al relay");
    let (state_tx, state) = watch::channel(BindingState::Bound { peer });
    let task_hook = Arc::clone(&hook);
    let task = tokio::spawn(async move {
        keep_alive(&*io, &task_hook, &mut frames, relay, &params, &state_tx).await;
    });
    Ok(RelayBinding { hook, state, task })
}

/// Envío de tramas fuera de `quinn` por el socket del `Endpoint`.
pub(crate) trait SendRaw: Send + Sync + 'static {
    fn send_raw(&self, to: SocketAddr, frame: &[u8]);
}

/// Allocate → Challenge → Bind → Bound, con reintentos; el par que dio el relay.
async fn handshake(
    io: &dyn SendRaw,
    frames: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    relay: SocketAddr,
    params: &RelayParams,
) -> Result<u32, TransportError> {
    let id = token_id(&params.token);
    let deadline = tokio::time::Instant::now() + BIND_TIMEOUT;
    let allocate = Frame::Allocate {
        version: PROTOCOL_VERSION,
        token_id: id,
    }
    .encode();
    let mut retry = FIRST_RETRY;
    let mut request = allocate.clone();
    let mut cookie: Option<[u8; COOKIE_LEN]> = None;
    loop {
        io.send_raw(relay, &request);
        let wait = tokio::time::Instant::now() + retry;
        loop {
            let frame = tokio::select! {
                frame = frames.recv() => frame,
                () = tokio::time::sleep_until(wait.min(deadline)) => break,
            };
            let Some(frame) = frame else {
                return Err(TransportError::Protocol("ligadura cancelada".to_owned()));
            };
            match Frame::parse(&frame) {
                Some(Frame::Challenge {
                    token_id: echoed,
                    cookie: fresh,
                }) if echoed == id => {
                    cookie = Some(fresh);
                    request = Frame::Bind {
                        version: PROTOCOL_VERSION,
                        token_id: id,
                        cookie: fresh,
                        mac: bind_mac(&params.token, &id, &fresh),
                    }
                    .encode();
                    retry = FIRST_RETRY;
                    break;
                }
                Some(Frame::Bound {
                    cookie: echoed,
                    peer,
                    lifetime_s,
                    mac,
                }) if Some(echoed) == cookie => {
                    if mac_eq(&mac, &bound_mac(&params.token, &echoed, peer, lifetime_s)) {
                        return Ok(peer);
                    }
                    warn!("respuesta del relay con una firma que no corresponde");
                }
                Some(Frame::Error(ErrorCode::BadSignature)) if cookie.is_some() => {
                    // La galleta venció entre los dos pasos: se vuelve a pedir.
                    cookie = None;
                    request = allocate.clone();
                    break;
                }
                // La respuesta a un `Data` que salió antes de ligarse (al rehacerla): no es de
                // esta ligadura.
                Some(Frame::Error(ErrorCode::NotBound)) => {}
                Some(Frame::Error(code)) => return Err(binding_error(code)),
                _ => {}
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(TransportError::Timeout("ligadura con el relay"));
        }
        retry = (retry * 2).min(MAX_RETRY);
    }
}

fn binding_error(code: ErrorCode) -> TransportError {
    TransportError::Protocol(match code {
        ErrorCode::Version => "el relay habla otra versión del protocolo".to_owned(),
        ErrorCode::UnknownToken => "el relay no reconoce el token".to_owned(),
        ErrorCode::BadSignature => "el relay rechazó la firma de la ligadura".to_owned(),
        ErrorCode::NotBound => "el relay no tiene la ligadura".to_owned(),
        ErrorCode::Quota => "la sala agotó su cuota del relay".to_owned(),
        ErrorCode::Other(code) => format!("el relay rechazó la ligadura (código {code})"),
    })
}

/// Refresca la ligadura; si el relay la perdió (se reinició o venció), la rehace.
async fn keep_alive(
    io: &dyn SendRaw,
    hook: &RelayHook,
    frames: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    relay: SocketAddr,
    params: &RelayParams,
    state: &watch::Sender<BindingState>,
) {
    let refresh = Frame::Refresh.encode();
    let mut tick = tokio::time::interval(REFRESH_INTERVAL);
    tick.tick().await;
    loop {
        let lost = tokio::select! {
            _ = tick.tick() => {
                io.send_raw(relay, &refresh);
                None
            }
            frame = frames.recv() => match frame.as_deref().and_then(Frame::parse) {
                Some(Frame::Error(code)) => Some(code),
                Some(_) => None,
                None => return,
            },
        };
        let Some(code) = lost else {
            continue;
        };
        if matches!(code, ErrorCode::NotBound) {
            // El relay la olvidó: se rehace con el mismo token, sin cortar las conexiones.
            match handshake(io, frames, relay, params).await {
                Ok(peer) => {
                    hook.set_route(Some(Route { relay, bound: true }));
                    let _ = state.send(BindingState::Bound { peer });
                    continue;
                }
                Err(err) => debug!(error = %err, "no se pudo rehacer la ligadura con el relay"),
            }
        }
        warn!(?code, "el relay soltó la ligadura");
        hook.set_route(Some(Route {
            relay,
            bound: false,
        }));
        let _ = state.send(BindingState::Lost(code));
        return;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_depend_on_the_token_and_the_cookie() {
        let token = [7u8; 32];
        let id = token_id(&token);
        assert_ne!(id, token_id(&[8u8; 32]));
        let a = bind_mac(&token, &id, &[1; COOKIE_LEN]);
        assert!(mac_eq(&a, &bind_mac(&token, &id, &[1; COOKIE_LEN])));
        assert!(!mac_eq(&a, &bind_mac(&token, &id, &[2; COOKIE_LEN])));
        assert!(!mac_eq(&a, &bind_mac(&[8u8; 32], &id, &[1; COOKIE_LEN])));
        // La del relay no sirve como la del par.
        assert!(!mac_eq(&a, &bound_mac(&token, &[1; COOKIE_LEN], 0, 40)));
    }
}
