//! QUIC entre pares: un [`Endpoint`] por proceso y una [`PeerConnection`] por par.
//!
//! El `Endpoint` envuelve un `quinn::Endpoint` sobre **un solo socket UDP** (`[::]:0` con doble pila
//! si el sistema lo permite, `0.0.0.0:0` si no) que actúa a la vez de cliente y de servidor: así la
//! apertura simultánea de NAT ([`crate::connect`]), la señalización ([`crate::signal`]) y el relay
//! salen del mismo puerto y el servidor ve la misma dirección reflejada para todo.
//!
//! TLS 1.3 con claves públicas crudas e intercambio X25519MLKEM768 ([`crate::tls`]); ALPN
//! `coroom-p2p/1` entre pares. Transporte: datagramas para medios (RFC 9221), `keep_alive` de 5 s,
//! `idle_timeout` de 15 s, migración de ruta activada y el controlador de congestión de medios
//! ([`crate::rate::MediaController`]) salvo que se pida BBR. La señalización (HTTP/3) no lleva
//! medios y usa el controlador por defecto de `quinn` ([`Congestion::Standard`]).
//!
//! Búferes: el socket pide [`SOCKET_BUFFER`] al sistema en los dos sentidos (un intra de cientos de
//! KB llega en pocos milisegundos y los 64 KB por defecto de Windows, o los ~208 KB de
//! `net.core.rmem_max` en Linux, lo descartaban entero); la cola de datagramas por enviar de cada
//! conexión guarda unos 250 ms al bitrate máximo ([`datagram_send_buffer`]): más sería vídeo viejo.
//!
//! Streams de cada conexión: el primer byte de cada stream bidireccional dice su tipo (control o
//! entrada). El **control** lo abre siempre el invitado con [`PeerConnection::open_control`] y lo
//! acepta el anfitrión con [`PeerConnection::accept_control`]; los dos se saludan con
//! [`Hello`] y después intercambian [`ControlMessage`] enmarcados por longitud. A través del relay
//! funciona igual: el relay abre en la conexión del anfitrión un stream de control por invitado.
//!
//! **Datagramas por pista**: una tarea por conexión lee todos los datagramas y los separa
//! por la pista de su cabecera: los de entrada ([`coroom_proto::input`], pista `Input`) van a
//! [`PeerConnection::read_input_datagram`], los de audio ([`coroom_proto::audio`], pistas
//! `Audio` y `Voice`) a [`PeerConnection::read_audio_datagram`], los de control (pista `Control`)
//! a [`PeerConnection::read_control_datagram`] y el resto a
//! [`PeerConnection::read_datagram`] (vídeo); así ni la entrada ni el audio pasan por el
//! reensamblador de medios con FEC y NACK. Cada cola guarda como mucho
//! [`DATAGRAM_QUEUE`] datagramas y, si se llena, descarta el más antiguo (como hace `quinn`).

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use tracing::debug;

use coroom_crypto::Identity;
use coroom_proto::Alpn;
use coroom_proto::codec::{self, FrameBuffer};
use coroom_proto::control::{ControlMessage, Hello};
use coroom_proto::media::ObjectHeader;

use crate::TransportError;
use crate::rate::{Congestion, CongestionShared};
use crate::tls::{self, Admitted, IdentityKey, PublicKey, ServerTrust};

/// Intervalo de `keep_alive` de todas las conexiones.
pub const KEEP_ALIVE: Duration = Duration::from_secs(5);

/// Retraso máximo de un ACK que se pide al par (extensión ACK frequency de QUIC). Sin ella el par
/// puede esperar 25 ms antes de confirmar y el PTO los suma: un `Nack` o un `KeyframeRequest` que
/// se pierde en el stream de control, con poco tráfico detrás que delate la pérdida, se reenviaba
/// 60-70 ms después con un RTT de 20 ms. Con 5 ms, unos 40 ms.
pub const MAX_ACK_DELAY: Duration = Duration::from_millis(5);

/// Tiempo sin tráfico tras el que una conexión se da por muerta.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(15);

/// Carga UDP inicial y mínima de las conexiones: la justa para datagramas de 1200 bytes
/// ([`ObjectHeader::MAX_PACKET`]) con la cabecera corta de QUIC.
///
/// Es también el `min_mtu` de `quinn`, el suelo de su detección de agujeros negros. Con el suelo
/// por defecto (1200) esa detección confunde la pérdida aleatoria de datagramas grandes con una
/// bajada de MTU (todos los paquetes de medios son "grandes"), baja el MTU a mitad de un fotograma
/// y deja sin enviar los objetos ya preparados y sus reenvíos por `Nack`. Con el suelo en 1252 los
/// datagramas de medios nunca cuentan como sospechosos. Contrapartida: un camino cuyo MTU no llegue
/// a 1280 bytes de IP no sirve; el respaldo es bajar esta constante a 1200 y los medios usarán el
/// `max_datagram_size` que quede ([`PeerConnection::max_packet`], 1162 bytes).
pub const INITIAL_MTU: u16 = 1252;

/// Espera máxima del saludo de un stream de control.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// Búfer de datagramas recibidos de cada conexión (y por enviar, en [`transport_config`]).
const DATAGRAM_BUFFER: usize = 4 * 1024 * 1024;

/// Medios por enviar que guarda como mucho la cola de datagramas de una conexión: lo que lleve más
/// de esto esperando ya es vídeo viejo, y `quinn` descarta el más antiguo cuando se llena.
pub const SEND_QUEUE_TIME: Duration = Duration::from_millis(250);

/// Suelo de la cola de datagramas por enviar ([`datagram_send_buffer`]): deja pasar el intra de un
/// bitrate bajo. El techo es [`DATAGRAM_BUFFER`].
const MIN_SEND_BUFFER: usize = 512 * 1024;

/// Búfer del socket UDP que se pide al sistema en cada sentido (`SO_RCVBUF` y `SO_SNDBUF`).
pub const SOCKET_BUFFER: usize = 8 * 1024 * 1024;

/// Por debajo de esto, el búfer de recepción que dio el sistema merece un aviso.
const SMALL_SOCKET_BUFFER: usize = 1024 * 1024;

/// Cola de datagramas por enviar de una conexión de medios: [`SEND_QUEUE_TIME`] a `max_kbps`,
/// acotada a [512 KB, 4 MB].
pub fn datagram_send_buffer(max_kbps: u32) -> usize {
    let bytes = (u64::from(max_kbps) * 1000 / 8) as f64 * SEND_QUEUE_TIME.as_secs_f64();
    (bytes as usize).clamp(MIN_SEND_BUFFER, DATAGRAM_BUFFER)
}

/// Datagramas que espera cada cola de una conexión (medios y entrada) antes de descartar el más
/// antiguo.
pub const DATAGRAM_QUEUE: usize = 4096;

/// Conexiones entrantes de pares no esperados que se guardan para [`Endpoint::accept_peer`].
const MAX_UNCLAIMED: usize = 16;

/// Mensajes de control en cola por conexión.
const CONTROL_QUEUE: usize = 1024;

/// Tipo de un stream bidireccional (primer byte).
pub(crate) const STREAM_CONTROL: u8 = 0x01;
pub(crate) const STREAM_INPUT: u8 = 0x02;

/// Ruta de una conexión.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Path {
    /// QUIC directo entre los dos pares.
    Direct,
    /// A través del relay del servidor.
    Relayed,
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Path::Direct => "directo",
            Path::Relayed => "relay",
        })
    }
}

/// Opciones del `Endpoint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointOptions {
    /// Controlador de congestión de las conexiones.
    pub congestion: Congestion,
    /// Bitrate objetivo inicial de cada conexión (para la ventana del controlador de medios).
    pub initial_target_kbps: u32,
}

impl EndpointOptions {
    /// Bitrate máximo de los medios de este `Endpoint` (el objetivo inicial que se le da es el
    /// máximo del usuario), con el que se dimensiona la cola de datagramas por enviar.
    pub fn max_kbps(&self) -> u32 {
        self.initial_target_kbps
    }
}

impl Default for EndpointOptions {
    fn default() -> Self {
        Self {
            congestion: Congestion::Media,
            initial_target_kbps: crate::rate::RateConfig::default().start_kbps,
        }
    }
}

/// Configuración de transporte común (pares, relay y señalización; el servidor también la usa):
/// `keep_alive`, `idle_timeout`, MTU inicial y mínimo, búferes de datagramas, controlador de
/// congestión y [`MAX_ACK_DELAY`].
pub fn transport_config(
    shared: &Arc<CongestionShared>,
    congestion: Congestion,
) -> quinn::TransportConfig {
    let mut config = quinn::TransportConfig::default();
    config
        .keep_alive_interval(Some(KEEP_ALIVE))
        .initial_mtu(INITIAL_MTU)
        .min_mtu(INITIAL_MTU)
        .datagram_receive_buffer_size(Some(DATAGRAM_BUFFER))
        .datagram_send_buffer_size(DATAGRAM_BUFFER)
        .congestion_controller_factory(congestion.factory(shared));
    let mut ack_frequency = quinn::AckFrequencyConfig::default();
    ack_frequency.max_ack_delay(Some(MAX_ACK_DELAY));
    config.ack_frequency_config(Some(ack_frequency));
    // 15 s caben de sobra en un `VarInt`.
    if let Ok(idle) = quinn::IdleTimeout::try_from(IDLE_TIMEOUT) {
        config.max_idle_timeout(Some(idle));
    }
    config
}

/// Configuración de transporte de las conexiones del `Endpoint`: la de [`transport_config`] con la
/// cola de datagramas por enviar de [`datagram_send_buffer`] para `max_kbps`.
fn endpoint_transport_config(
    shared: &Arc<CongestionShared>,
    congestion: Congestion,
    max_kbps: u32,
) -> quinn::TransportConfig {
    let mut config = transport_config(shared, congestion);
    config.datagram_send_buffer_size(datagram_send_buffer(max_kbps));
    config
}

/// Pide [`SOCKET_BUFFER`] de recepción y de envío al socket y anota lo que dio el sistema (Linux
/// lo recorta a `net.core.rmem_max`/`wmem_max` y devuelve el doble de lo que reserva).
fn grow_socket_buffers(socket: &socket2::Socket) {
    if let Err(err) = socket.set_recv_buffer_size(SOCKET_BUFFER) {
        tracing::warn!(error = %err, "no se pudo ampliar el búfer de recepción del socket UDP");
    }
    if let Err(err) = socket.set_send_buffer_size(SOCKET_BUFFER) {
        tracing::warn!(error = %err, "no se pudo ampliar el búfer de envío del socket UDP");
    }
    let recv = socket.recv_buffer_size().unwrap_or(0);
    let send = socket.send_buffer_size().unwrap_or(0);
    tracing::info!(recv, send, "búferes del socket UDP");
    if recv < SMALL_SOCKET_BUFFER {
        tracing::warn!(
            recv,
            "el sistema dio un búfer de recepción UDP pequeño: un fotograma clave grande puede \
             perderse entero (en Linux: sysctl -w net.core.rmem_max=8388608 \
             net.core.wmem_max=8388608)"
        );
    }
}

/// Abre el socket UDP: doble pila en `[::]` si se puede; si no, IPv4.
fn bind_socket(addr: Option<SocketAddr>) -> Result<std::net::UdpSocket, TransportError> {
    use socket2::{Domain, Protocol, Socket, Type};

    let bind = |addr: SocketAddr| -> std::io::Result<std::net::UdpSocket> {
        let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))?;
        if addr.is_ipv6() && addr.ip().is_unspecified() {
            socket.set_only_v6(false)?;
        }
        grow_socket_buffers(&socket);
        socket.bind(&addr.into())?;
        Ok(socket.into())
    };
    match addr {
        Some(addr) => Ok(bind(addr)?),
        None => match bind(SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))) {
            Ok(socket) => Ok(socket),
            Err(err) => {
                debug!(error = %err, "sin doble pila IPv6; el endpoint usa solo IPv4");
                Ok(bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))?)
            }
        },
    }
}

/// Normaliza direcciones IPv4 mapeadas en IPv6 (`::ffff:a.b.c.d`) a IPv4.
pub fn canonical(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

#[derive(Default)]
struct Dispatch {
    waiters: HashMap<PublicKey, mpsc::UnboundedSender<PeerConnection>>,
    unclaimed: VecDeque<PeerConnection>,
}

struct EndpointInner {
    quinn: quinn::Endpoint,
    key: IdentityKey,
    admitted: Admitted,
    options: EndpointOptions,
    dispatch: Arc<Mutex<Dispatch>>,
    unclaimed: Arc<Notify>,
    accept_task: JoinHandle<()>,
}

impl Drop for EndpointInner {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

/// Extremo QUIC de coroom: un socket UDP, cliente y servidor a la vez.
///
/// Se clona barato (todas las copias comparten el socket). Hay que crearlo dentro de un runtime
/// de `tokio`.
#[derive(Clone)]
pub struct Endpoint {
    inner: Arc<EndpointInner>,
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Endpoint")
            .field("local_addr", &self.local_addr().ok())
            .field("identity", &self.inner.key)
            .finish_non_exhaustive()
    }
}

/// Pide al sistema temporizadores de 1 ms. En Windows el reloj despierta cada 15,6 ms por defecto,
/// y con eso el pacer de QUIC (ráfagas de 2 ms) y los `sleep` de tokio llegan hasta 15 ms tarde:
/// el caudal se hunde y la latencia crece. Como hacen los juegos y los clientes de streaming, el
/// proceso pide 1 ms una sola vez; desde Windows 10 2004 solo afecta a este proceso y el sistema
/// lo libera al terminar. Windows 11 ignora además esa petición si la ventana del proceso está
/// oculta o minimizada (o si no tiene ventana), salvo que el proceso se salga de esa regulación con
/// `SetProcessInformation(ProcessPowerThrottling)` y `PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION`
/// (el bit en la máscara de control y la de estado a 0: «no regular la resolución»). En los demás
/// sistemas no hace falta.
// Únicas llamadas `unsafe` de esta crate: la API de Windows, con su invariante en el `SAFETY`.
#[allow(unsafe_code)]
fn high_resolution_timers() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::{
            GetCurrentProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION, PROCESS_POWER_THROTTLING_STATE,
            ProcessPowerThrottling, SetProcessInformation,
        };

        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let state = PROCESS_POWER_THROTTLING_STATE {
                Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
                ControlMask: PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
                StateMask: 0,
            };
            // SAFETY: `GetCurrentProcess` devuelve un pseudo-identificador que no hay que cerrar;
            // `state` es un `PROCESS_POWER_THROTTLING_STATE` válido que vive durante la llamada y
            // su tamaño es el que se pasa. Los Windows anteriores a 11 rechazan el bit con un
            // error, que solo se anota.
            let throttling = unsafe {
                SetProcessInformation(
                    GetCurrentProcess(),
                    ProcessPowerThrottling,
                    std::ptr::from_ref(&state).cast(),
                    std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
                )
            };
            if let Err(err) = throttling {
                tracing::debug!(error = %err, "sin exención de la regulación de temporizadores");
            }
            // SAFETY: `timeBeginPeriod` solo recibe el periodo en milisegundos y no toca memoria
            // nuestra; el periodo queda pedido hasta que el proceso termina.
            let result = unsafe { windows::Win32::Media::timeBeginPeriod(1) };
            if result != 0 {
                tracing::warn!(
                    result,
                    "no se pudo pedir resolución de 1 ms a los temporizadores"
                );
            }
        });
    }
}

impl Endpoint {
    /// Abre el socket (`addr`, o `[::]:0`/`0.0.0.0:0` si es `None`) con la identidad `identity`.
    pub fn bind(
        identity: &Identity,
        addr: Option<SocketAddr>,
        options: EndpointOptions,
    ) -> Result<Self, TransportError> {
        high_resolution_timers();
        let key = IdentityKey::new(identity)?;
        let admitted = Admitted::default();
        let crypto = Arc::new(
            QuicServerConfig::try_from(tls::peer_server_config(&key, admitted.clone())?)
                .map_err(|err| TransportError::Tls(err.to_string()))?,
        );
        let runtime = quinn::default_runtime().ok_or_else(|| {
            TransportError::Protocol("el Endpoint necesita un runtime de tokio".to_owned())
        })?;
        let socket = bind_socket(addr)?;
        let server_config = server_config(&crypto, options);
        let quinn = quinn::Endpoint::new(
            quinn::EndpointConfig::default(),
            Some(server_config.0),
            socket,
            runtime,
        )?;

        let dispatch = Arc::new(Mutex::new(Dispatch::default()));
        let unclaimed = Arc::new(Notify::new());
        let accept_task = tokio::spawn(accept_loop(
            quinn.clone(),
            crypto,
            options,
            Arc::clone(&dispatch),
            Arc::clone(&unclaimed),
        ));
        Ok(Self {
            inner: Arc::new(EndpointInner {
                quinn,
                key,
                admitted,
                options,
                dispatch,
                unclaimed,
                accept_task,
            }),
        })
    }

    /// Dirección local del socket.
    pub fn local_addr(&self) -> Result<SocketAddr, TransportError> {
        Ok(self.inner.quinn.local_addr()?)
    }

    /// Clave pública de la identidad local.
    pub fn public_key(&self) -> PublicKey {
        self.inner.key.public_key()
    }

    /// Opciones con que se creó.
    pub fn options(&self) -> EndpointOptions {
        self.inner.options
    }

    /// `quinn::Endpoint` subyacente (para la señalización y los tests).
    pub fn quinn(&self) -> &quinn::Endpoint {
        &self.inner.quinn
    }

    /// Admite a `key` como cliente entrante (un par de la sala).
    pub fn admit(&self, key: PublicKey) {
        self.inner.admitted.insert(key);
    }

    /// Deja de admitir a `key` (las conexiones ya abiertas siguen; ciérralas aparte).
    pub fn revoke(&self, key: &PublicKey) {
        self.inner.admitted.remove(key);
    }

    /// Conecta como cliente QUIC con el par `expected` en `addr` (ALPN `coroom-p2p/1`).
    ///
    /// Falla el handshake si la clave cruda del otro extremo no es `expected`, o si el otro
    /// extremo no nos tiene admitidos.
    pub async fn connect_peer(
        &self,
        addr: SocketAddr,
        expected: PublicKey,
    ) -> Result<PeerConnection, TransportError> {
        let crypto =
            QuicClientConfig::try_from(tls::peer_client_config(&self.inner.key, expected)?)
                .map_err(|err| TransportError::Tls(err.to_string()))?;
        let shared = CongestionShared::new(self.inner.options.initial_target_kbps);
        let mut config = quinn::ClientConfig::new(Arc::new(crypto));
        config.transport_config(Arc::new(endpoint_transport_config(
            &shared,
            self.inner.options.congestion,
            self.inner.options.max_kbps(),
        )));
        let connection = self
            .inner
            .quinn
            .connect_with(config, self.map_addr(addr), tls::PEER_SERVER_NAME)?
            .await?;
        let key = tls::peer_key_of(&connection)
            .ok_or_else(|| TransportError::Tls("el par no presentó clave cruda".to_owned()))?;
        if key != expected {
            connection.close(quinn::VarInt::from_u32(1), b"identidad");
            return Err(TransportError::Tls(
                "identidad del par inesperada".to_owned(),
            ));
        }
        Ok(PeerConnection::new(
            connection,
            Some(key),
            Path::Direct,
            shared,
        ))
    }

    /// Conecta con el servidor de coroom en `addr` con el ALPN `alpn` (señalización `h3` o relay).
    /// El relay lleva medios y usa el controlador del `Endpoint`; la señalización, el de `quinn`
    /// ([`Congestion::Standard`]): sus streams no tienen por qué ir al ritmo de un bitrate de vídeo.
    pub async fn connect_server(
        &self,
        addr: SocketAddr,
        server_name: &str,
        trust: &ServerTrust,
        alpn: Alpn,
    ) -> Result<(quinn::Connection, Arc<CongestionShared>), TransportError> {
        let crypto = QuicClientConfig::try_from(tls::server_client_config(trust, alpn)?)
            .map_err(|err| TransportError::Tls(err.to_string()))?;
        let shared = CongestionShared::new(self.inner.options.initial_target_kbps);
        let congestion = match alpn {
            Alpn::H3 => Congestion::Standard,
            Alpn::Relay | Alpn::P2p => self.inner.options.congestion,
        };
        let mut config = quinn::ClientConfig::new(Arc::new(crypto));
        config.transport_config(Arc::new(endpoint_transport_config(
            &shared,
            congestion,
            self.inner.options.max_kbps(),
        )));
        let connection = self
            .inner
            .quinn
            .connect_with(config, self.map_addr(addr), server_name)?
            .await?;
        Ok((connection, shared))
    }

    /// Recibe las conexiones entrantes del par `key` (las que ya llegaron también).
    ///
    /// Sustituye a una espera anterior del mismo par; [`Endpoint::forget`] la retira.
    pub fn incoming_from(&self, key: PublicKey) -> mpsc::UnboundedReceiver<PeerConnection> {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut dispatch) = self.inner.dispatch.lock() {
            let (matching, rest): (Vec<_>, Vec<_>) = dispatch
                .unclaimed
                .drain(..)
                .partition(|conn| conn.peer_key() == Some(key));
            dispatch.unclaimed = rest.into();
            for conn in matching {
                let _ = tx.send(conn);
            }
            dispatch.waiters.insert(key, tx);
        }
        rx
    }

    /// Retira la espera de [`Endpoint::incoming_from`].
    pub fn forget(&self, key: &PublicKey) {
        if let Ok(mut dispatch) = self.inner.dispatch.lock() {
            dispatch.waiters.remove(key);
        }
    }

    /// Siguiente conexión entrante de un par admitido que nadie esperaba con [`Endpoint::incoming_from`].
    pub async fn accept_peer(&self) -> PeerConnection {
        loop {
            let notified = self.inner.unclaimed.notified();
            if let Some(conn) = self
                .inner
                .dispatch
                .lock()
                .ok()
                .and_then(|mut dispatch| dispatch.unclaimed.pop_front())
            {
                return conn;
            }
            notified.await;
        }
    }

    /// Cierra el endpoint y todas sus conexiones.
    pub fn close(&self) {
        self.inner
            .quinn
            .close(quinn::VarInt::from_u32(0), b"cierre");
    }

    /// Con el socket en doble pila, las direcciones IPv4 se escriben mapeadas en IPv6.
    fn map_addr(&self, addr: SocketAddr) -> SocketAddr {
        match (addr, self.inner.quinn.local_addr()) {
            (SocketAddr::V4(v4), Ok(SocketAddr::V6(_))) => {
                SocketAddr::new(v4.ip().to_ipv6_mapped().into(), v4.port())
            }
            _ => addr,
        }
    }
}

struct ServerConfigWithShared(quinn::ServerConfig, Arc<CongestionShared>);

fn server_config(
    crypto: &Arc<QuicServerConfig>,
    options: EndpointOptions,
) -> ServerConfigWithShared {
    let shared = CongestionShared::new(options.initial_target_kbps);
    let mut config = quinn::ServerConfig::with_crypto(Arc::clone(crypto) as _);
    config.transport_config(Arc::new(endpoint_transport_config(
        &shared,
        options.congestion,
        options.max_kbps(),
    )));
    config.migration(true);
    ServerConfigWithShared(config, shared)
}

async fn accept_loop(
    quinn: quinn::Endpoint,
    crypto: Arc<QuicServerConfig>,
    options: EndpointOptions,
    dispatch: Arc<Mutex<Dispatch>>,
    unclaimed: Arc<Notify>,
) {
    while let Some(incoming) = quinn.accept().await {
        let crypto = Arc::clone(&crypto);
        let dispatch = Arc::clone(&dispatch);
        let unclaimed = Arc::clone(&unclaimed);
        tokio::spawn(async move {
            let remote = incoming.remote_address();
            // Un controlador de congestión con estado propio por conexión.
            let ServerConfigWithShared(config, shared) = server_config(&crypto, options);
            let connection = match incoming.accept_with(Arc::new(config)) {
                Ok(connecting) => match connecting.await {
                    Ok(connection) => connection,
                    Err(err) => {
                        debug!(%remote, error = %err, "handshake entrante fallido");
                        return;
                    }
                },
                Err(err) => {
                    debug!(%remote, error = %err, "conexión entrante rechazada");
                    return;
                }
            };
            let Some(key) = tls::peer_key_of(&connection) else {
                connection.close(quinn::VarInt::from_u32(1), b"identidad");
                return;
            };
            let peer = PeerConnection::new(connection, Some(key), Path::Direct, shared);
            let Ok(mut dispatch) = dispatch.lock() else {
                return;
            };
            if let Some(waiter) = dispatch.waiters.get(&key) {
                if let Err(mpsc::error::SendError(peer)) = waiter.send(peer) {
                    dispatch.waiters.remove(&key);
                    dispatch.unclaimed.push_back(peer);
                }
            } else {
                dispatch.unclaimed.push_back(peer);
            }
            while dispatch.unclaimed.len() > MAX_UNCLAIMED {
                if let Some(old) = dispatch.unclaimed.pop_front() {
                    old.close("sin reclamar");
                }
            }
            unclaimed.notify_waiters();
            unclaimed.notify_one();
        });
    }
}

/// Stream bidireccional de QUIC (envío y recepción).
pub type BiStream = (quinn::SendStream, quinn::RecvStream);

/// Cola de datagramas de una pista: gana el más reciente si se llena.
#[derive(Debug, Default)]
struct DatagramQueue {
    queue: Mutex<VecDeque<Bytes>>,
    notify: Notify,
    /// Para el lector que espera bloqueado ([`DatagramQueue::pop_blocking`]).
    cond: Condvar,
    closed: std::sync::atomic::AtomicBool,
    dropped: std::sync::atomic::AtomicU64,
}

impl DatagramQueue {
    fn push(&self, datagram: Bytes) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.push_back(datagram);
            if queue.len() > DATAGRAM_QUEUE {
                queue.pop_front();
                self.dropped
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        self.notify.notify_one();
        self.cond.notify_one();
    }

    fn close(&self) {
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        self.notify.notify_one();
        self.cond.notify_all();
    }

    /// Como [`DatagramQueue::pop`], bloqueando el hilo hasta `timeout`: `Ok(None)` si no llegó
    /// nada, `Err(())` si la conexión se cerró y no queda ninguno.
    fn pop_blocking(&self, timeout: Duration) -> Result<Option<Bytes>, ()> {
        let deadline = Instant::now() + timeout;
        let mut queue = self.queue.lock().map_err(|_| ())?;
        loop {
            if let Some(datagram) = queue.pop_front() {
                return Ok(Some(datagram));
            }
            if self.closed.load(std::sync::atomic::Ordering::Acquire) {
                return Err(());
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            queue = self
                .cond
                .wait_timeout(queue, deadline - now)
                .map_err(|_| ())?
                .0;
        }
    }

    /// Siguiente datagrama; `None` si la conexión se cerró y no queda ninguno. Pensada para un
    /// solo lector a la vez; se puede cancelar sin perder datagramas.
    async fn pop(&self) -> Option<Bytes> {
        loop {
            if let Some(datagram) = self.queue.lock().ok().and_then(|mut q| q.pop_front()) {
                return Some(datagram);
            }
            if self.closed.load(std::sync::atomic::Ordering::Acquire) {
                // Otro lector podría estar esperando también: que se entere.
                self.notify.notify_one();
                return None;
            }
            self.notify.notified().await;
        }
    }
}

struct PeerInner {
    conn: quinn::Connection,
    peer_key: Option<PublicKey>,
    path: Path,
    congestion: Arc<CongestionShared>,
    control: tokio::sync::Mutex<mpsc::Receiver<BiStream>>,
    input: tokio::sync::Mutex<mpsc::Receiver<BiStream>>,
    media_datagrams: Arc<DatagramQueue>,
    input_datagrams: Arc<DatagramQueue>,
    audio_datagrams: Arc<DatagramQueue>,
    control_datagrams: Arc<DatagramQueue>,
    /// Tamaño de la cola de datagramas por enviar (el hueco libre al crearse, con nada en cola).
    send_queue_capacity: usize,
    streams_task: JoinHandle<()>,
    datagrams_task: JoinHandle<()>,
}

impl Drop for PeerInner {
    fn drop(&mut self) {
        self.streams_task.abort();
        self.datagrams_task.abort();
    }
}

/// Reparte los datagramas recibidos por pista hasta que la conexión se cierra.
async fn route_datagrams(
    conn: quinn::Connection,
    media: Arc<DatagramQueue>,
    input: Arc<DatagramQueue>,
    audio: Arc<DatagramQueue>,
    control: Arc<DatagramQueue>,
) {
    while let Ok(datagram) = conn.read_datagram().await {
        if coroom_proto::input::is_input_datagram(&datagram) {
            input.push(datagram);
        } else if coroom_proto::audio::is_audio_datagram(&datagram) {
            audio.push(datagram);
        } else if coroom_proto::control::is_control_datagram(&datagram) {
            control.push(datagram);
        } else {
            media.push(datagram);
        }
    }
    media.close();
    input.close();
    audio.close();
    control.close();
}

/// Conexión QUIC con un par (directa) o con el relay.
#[derive(Clone)]
pub struct PeerConnection {
    inner: Arc<PeerInner>,
}

impl fmt::Debug for PeerConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PeerConnection")
            .field("remote", &self.remote_address())
            .field("path", &self.inner.path)
            .finish_non_exhaustive()
    }
}

impl PeerConnection {
    pub(crate) fn new(
        conn: quinn::Connection,
        peer_key: Option<PublicKey>,
        path: Path,
        congestion: Arc<CongestionShared>,
    ) -> Self {
        let (control_tx, control_rx) = mpsc::channel(8);
        let (input_tx, input_rx) = mpsc::channel(8);
        let streams_task = tokio::spawn(route_streams(conn.clone(), control_tx, input_tx));
        let media_datagrams = Arc::new(DatagramQueue::default());
        let input_datagrams = Arc::new(DatagramQueue::default());
        let audio_datagrams = Arc::new(DatagramQueue::default());
        let control_datagrams = Arc::new(DatagramQueue::default());
        let datagrams_task = tokio::spawn(route_datagrams(
            conn.clone(),
            Arc::clone(&media_datagrams),
            Arc::clone(&input_datagrams),
            Arc::clone(&audio_datagrams),
            Arc::clone(&control_datagrams),
        ));
        let send_queue_capacity = conn.datagram_send_buffer_space();
        Self {
            inner: Arc::new(PeerInner {
                send_queue_capacity,
                conn,
                peer_key,
                path,
                congestion,
                control: tokio::sync::Mutex::new(control_rx),
                input: tokio::sync::Mutex::new(input_rx),
                media_datagrams,
                input_datagrams,
                audio_datagrams,
                control_datagrams,
                streams_task,
                datagrams_task,
            }),
        }
    }

    /// `quinn::Connection` subyacente.
    pub fn quinn(&self) -> &quinn::Connection {
        &self.inner.conn
    }

    /// Identidad verificada del par (por TLS); `None` en el relay, cuyo certificado es el del
    /// servidor.
    pub fn peer_key(&self) -> Option<PublicKey> {
        self.inner.peer_key
    }

    /// Ruta de la conexión.
    pub fn path(&self) -> Path {
        self.inner.path
    }

    /// Dirección remota actual (normalizada a IPv4 si estaba mapeada).
    pub fn remote_address(&self) -> SocketAddr {
        canonical(self.inner.conn.remote_address())
    }

    /// RTT suavizado de QUIC.
    pub fn rtt(&self) -> Duration {
        self.inner.conn.rtt()
    }

    /// Estadísticas de QUIC.
    pub fn stats(&self) -> quinn::ConnectionStats {
        self.inner.conn.stats()
    }

    /// Tamaño máximo de un datagrama de medios: [`ObjectHeader::MAX_PACKET`] o menos si el camino
    /// negoció menos. `0` si el par no admite datagramas.
    pub fn max_packet(&self) -> usize {
        self.inner
            .conn
            .max_datagram_size()
            .map_or(0, |size| size.min(ObjectHeader::MAX_PACKET))
    }

    /// Bytes de datagramas que esperan para salir (la ventana de congestión no los deja pasar).
    pub fn datagram_queued_bytes(&self) -> usize {
        self.inner
            .send_queue_capacity
            .saturating_sub(self.inner.conn.datagram_send_buffer_space())
    }

    /// Envía un datagrama (no bloquea; si el búfer está lleno, `quinn` descarta los más antiguos).
    pub fn send_datagram(&self, datagram: Bytes) -> Result<(), TransportError> {
        Ok(self.inner.conn.send_datagram(datagram)?)
    }

    /// Siguiente datagrama recibido que no es de entrada (medios). Un solo lector a la vez.
    pub async fn read_datagram(&self) -> Result<Bytes, TransportError> {
        match self.inner.media_datagrams.pop().await {
            Some(datagram) => Ok(datagram),
            None => Err(self.closed_error()),
        }
    }

    /// Siguiente datagrama de entrada (pista `Input`). Un solo lector a la vez.
    pub async fn read_input_datagram(&self) -> Result<Bytes, TransportError> {
        match self.inner.input_datagrams.pop().await {
            Some(datagram) => Ok(datagram),
            None => Err(self.closed_error()),
        }
    }

    /// Siguiente datagrama de control (pista `Control`: copias de `Nack` y `KeyframeRequest`, ver
    /// [`coroom_proto::control::control_datagram`]). Un solo lector a la vez.
    pub async fn read_control_datagram(&self) -> Result<Bytes, TransportError> {
        match self.inner.control_datagrams.pop().await {
            Some(datagram) => Ok(datagram),
            None => Err(self.closed_error()),
        }
    }

    /// Siguiente datagrama de audio (pistas `Audio` y `Voice`). Un solo lector a la vez.
    pub async fn read_audio_datagram(&self) -> Result<Bytes, TransportError> {
        match self.inner.audio_datagrams.pop().await {
            Some(datagram) => Ok(datagram),
            None => Err(self.closed_error()),
        }
    }

    /// Como [`PeerConnection::read_audio_datagram`], bloqueando el hilo hasta `timeout` (para un
    /// hilo de audio propio, fuera del runtime): `Ok(None)` si no llegó nada en ese tiempo.
    pub fn read_audio_datagram_blocking(
        &self,
        timeout: Duration,
    ) -> Result<Option<Bytes>, TransportError> {
        self.inner
            .audio_datagrams
            .pop_blocking(timeout)
            .map_err(|()| self.closed_error())
    }

    /// Datagramas de audio descartados por llenarse su cola.
    pub fn audio_datagrams_dropped(&self) -> u64 {
        self.inner
            .audio_datagrams
            .dropped
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Datagramas descartados por llenarse sus colas: `(medios, entrada)`.
    pub fn datagrams_dropped(&self) -> (u64, u64) {
        let load = |queue: &DatagramQueue| queue.dropped.load(std::sync::atomic::Ordering::Relaxed);
        (
            load(&self.inner.media_datagrams),
            load(&self.inner.input_datagrams),
        )
    }

    fn closed_error(&self) -> TransportError {
        match self.inner.conn.close_reason() {
            Some(reason) => TransportError::Connection(reason),
            None => TransportError::Closed,
        }
    }

    /// Estado compartido con el controlador de congestión de la conexión.
    pub fn congestion(&self) -> &Arc<CongestionShared> {
        &self.inner.congestion
    }

    /// Fija el bitrate objetivo que dimensiona la ventana del controlador de medios.
    pub fn set_target_kbps(&self, kbps: u32) {
        self.inner.congestion.set_target_kbps(kbps);
    }

    /// Abre el stream de control (lo hace el invitado) y espera el `Hello` del otro extremo.
    pub async fn open_control(&self, hello: Hello) -> Result<ControlStream, TransportError> {
        let (mut send, recv) = self.inner.conn.open_bi().await?;
        send.write_all(&[STREAM_CONTROL]).await?;
        send.write_all(&codec::encode(&ControlMessage::Hello(hello))?)
            .await?;
        let mut receiver = ControlReceiver::new(recv);
        let peer = read_hello(&mut receiver).await?;
        Ok(ControlStream::new(send, receiver, peer))
    }

    /// Acepta el siguiente stream de control (el anfitrión; uno por invitado en el relay) y
    /// responde a su `Hello` con `hello`.
    pub async fn accept_control(&self, hello: Hello) -> Result<ControlStream, TransportError> {
        let (mut send, recv) = self
            .inner
            .control
            .lock()
            .await
            .recv()
            .await
            .ok_or(TransportError::Closed)?;
        let mut receiver = ControlReceiver::new(recv);
        let peer = read_hello(&mut receiver).await?;
        send.write_all(&codec::encode(&ControlMessage::Hello(hello))?)
            .await?;
        Ok(ControlStream::new(send, receiver, peer))
    }

    /// Abre un stream de entrada (lo hace el invitado). El formato que va dentro está en
    /// [`coroom_proto::input`].
    pub async fn open_input(&self) -> Result<BiStream, TransportError> {
        let (mut send, recv) = self.inner.conn.open_bi().await?;
        send.write_all(&[STREAM_INPUT]).await?;
        Ok((send, recv))
    }

    /// Acepta el siguiente stream de entrada (el anfitrión; uno por invitado, también por el relay).
    pub async fn accept_input(&self) -> Result<BiStream, TransportError> {
        self.inner
            .input
            .lock()
            .await
            .recv()
            .await
            .ok_or(TransportError::Closed)
    }

    /// Espera a que la conexión se cierre y devuelve el motivo.
    pub async fn closed(&self) -> TransportError {
        TransportError::Connection(self.inner.conn.closed().await)
    }

    /// Cierra la conexión.
    pub fn close(&self, reason: &str) {
        self.inner
            .conn
            .close(quinn::VarInt::from_u32(0), reason.as_bytes());
    }
}

/// Reparte los streams bidireccionales entrantes según su primer byte.
async fn route_streams(
    conn: quinn::Connection,
    control: mpsc::Sender<BiStream>,
    input: mpsc::Sender<BiStream>,
) {
    while let Ok((send, mut recv)) = conn.accept_bi().await {
        let control = control.clone();
        let input = input.clone();
        tokio::spawn(async move {
            let mut kind = [0u8; 1];
            let read = tokio::time::timeout(HELLO_TIMEOUT, recv.read_exact(&mut kind)).await;
            if !matches!(read, Ok(Ok(()))) {
                return;
            }
            let target = match kind[0] {
                STREAM_CONTROL => control,
                STREAM_INPUT => input,
                other => {
                    debug!(kind = other, "stream de tipo desconocido");
                    let _ = recv.stop(quinn::VarInt::from_u32(1));
                    return;
                }
            };
            let _ = target.send((send, recv)).await;
        });
    }
}

async fn read_hello(receiver: &mut ControlReceiver) -> Result<Hello, TransportError> {
    let message = tokio::time::timeout(HELLO_TIMEOUT, receiver.recv())
        .await
        .map_err(|_| TransportError::Timeout("saludo del stream de control"))??;
    match message {
        Some(ControlMessage::Hello(hello)) => {
            if hello.protocol_version != coroom_proto::PROTOCOL_VERSION {
                return Err(TransportError::Protocol(format!(
                    "el par habla la versión {} del protocolo y esta es la {}",
                    hello.protocol_version,
                    coroom_proto::PROTOCOL_VERSION
                )));
            }
            Ok(hello)
        }
        Some(_) => Err(TransportError::Protocol(
            "el stream de control no empieza por Hello".to_owned(),
        )),
        None => Err(TransportError::Closed),
    }
}

/// Stream de control ya saludado.
#[derive(Debug)]
pub struct ControlStream {
    /// Mitad de envío (clonable).
    pub sender: ControlSender,
    /// Mitad de recepción.
    pub receiver: ControlReceiver,
    peer: Hello,
}

impl ControlStream {
    fn new(send: quinn::SendStream, receiver: ControlReceiver, peer: Hello) -> Self {
        Self {
            sender: ControlSender::spawn(send),
            receiver,
            peer,
        }
    }

    /// `Hello` del otro extremo.
    pub fn peer_hello(&self) -> &Hello {
        &self.peer
    }

    /// Separa las dos mitades y el saludo del otro extremo.
    pub fn split(self) -> (ControlSender, ControlReceiver, Hello) {
        (self.sender, self.receiver, self.peer)
    }
}

/// Mitad de envío de un stream de control: una cola hacia una tarea que escribe en orden.
#[derive(Debug, Clone)]
pub struct ControlSender {
    tx: mpsc::Sender<Vec<u8>>,
}

/// Tarea que escribe en orden en `send` los mensajes ya enmarcados de la cola devuelta y termina
/// el stream cuando se sueltan todos los emisores.
pub(crate) fn spawn_writer(mut send: quinn::SendStream) -> mpsc::Sender<Vec<u8>> {
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(CONTROL_QUEUE);
    tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if send.write_all(&frame).await.is_err() {
                return;
            }
        }
        let _ = send.finish();
    });
    tx
}

impl ControlSender {
    pub(crate) fn spawn(send: quinn::SendStream) -> Self {
        Self {
            tx: spawn_writer(send),
        }
    }

    /// Encola un mensaje sin esperar. Falla si la conexión se cerró o la cola está llena.
    pub fn send(&self, message: &ControlMessage) -> Result<(), TransportError> {
        let frame = codec::encode(message)?;
        self.tx.try_send(frame).map_err(|err| match err {
            mpsc::error::TrySendError::Full(_) => {
                TransportError::Protocol("la cola de control está llena".to_owned())
            }
            mpsc::error::TrySendError::Closed(_) => TransportError::Closed,
        })
    }

    /// Encola un mensaje esperando a que haya sitio.
    pub async fn send_wait(&self, message: &ControlMessage) -> Result<(), TransportError> {
        let frame = codec::encode(message)?;
        self.tx
            .send(frame)
            .await
            .map_err(|_| TransportError::Closed)
    }
}

/// Mitad de recepción de un stream de control.
#[derive(Debug)]
pub struct ControlReceiver {
    recv: quinn::RecvStream,
    buffer: FrameBuffer,
}

impl ControlReceiver {
    pub(crate) fn new(recv: quinn::RecvStream) -> Self {
        Self {
            recv,
            buffer: FrameBuffer::new(),
        }
    }

    /// Siguiente mensaje; `None` si el otro extremo cerró el stream ordenadamente.
    ///
    /// Se puede cancelar (por ejemplo en `tokio::select!`) sin perder datos.
    pub async fn recv(&mut self) -> Result<Option<ControlMessage>, TransportError> {
        loop {
            if let Some(message) = self.buffer.next_message::<ControlMessage>()? {
                return Ok(Some(message));
            }
            match self
                .recv
                .read_chunk(codec::MAX_MESSAGE_LEN, true)
                .await
                .map_err(|err| TransportError::Protocol(err.to_string()))?
            {
                Some(chunk) => self.buffer.extend(&chunk.bytes),
                None if self.buffer.pending() == 0 => return Ok(None),
                None => return Err(TransportError::Closed),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_blocking_reader_waits_times_out_and_sees_the_close() {
        let queue = Arc::new(DatagramQueue::default());
        assert_eq!(queue.pop_blocking(Duration::from_millis(5)), Ok(None));
        let writer = Arc::clone(&queue);
        let sent = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            writer.push(Bytes::from_static(b"audio"));
        });
        // Se despierta al llegar, sin esperar al plazo.
        let started = Instant::now();
        let got = queue.pop_blocking(Duration::from_secs(5));
        assert_eq!(got, Ok(Some(Bytes::from_static(b"audio"))));
        assert!(started.elapsed() < Duration::from_secs(2));
        sent.join().unwrap();
        queue.push(Bytes::from_static(b"queda"));
        queue.close();
        // Lo que quedaba se entrega antes del cierre.
        assert_eq!(
            queue.pop_blocking(Duration::from_millis(5)),
            Ok(Some(Bytes::from_static(b"queda")))
        );
        assert_eq!(queue.pop_blocking(Duration::from_millis(5)), Err(()));
    }
}
