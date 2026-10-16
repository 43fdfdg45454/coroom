//! Sesión remota: el anfitrión comparte su pantalla con los invitados por QUIC, en
//! directo o por el relay del servidor, con los medios cifrados de extremo a extremo.
//!
//! [`RemoteSession`] implementa [`Session`] para los dos papeles:
//!
//! - **Anfitrión** ([`RemoteSession::host`]): crea la sala por la señalización y el grupo MLS
//!   ([`RoomGroup`](coroom_transport::crypto::RoomGroup)). Por cada invitado que entra (`PeerJoined`)
//!   lo añade al grupo (commit a los que ya estaban, `Welcome` al nuevo, los dos por `Mls`), le
//!   publica su clave X-Wing firmada, intercambia candidatos y conecta (`Connector`; si la apertura
//!   de NAT falla o se fuerza, por el relay). Los invitados entran aprobados con permiso de mirar y
//!   hablar, o solo de mirar si la sala los quiere callados ([`Permissions::on_entry`]). El
//!   pipeline local se reutiliza: captura →
//!   codificación → **SFrame** (una vez por fotograma) → `MediaSender` (uno por invitado directo y,
//!   como mucho, uno hacia el relay, que abre en abanico). El bitrate del codificador es el
//!   **mínimo** de los objetivos de todos los invitados (cada `RateController` ya aplica su
//!   histéresis); un `KeyframeRequest` pide un intra.
//! - **Invitado** ([`RemoteSession::guest`]): entra con su `KeyPackage`, construye el grupo con el
//!   `Welcome`, instala las claves de medios, inicia el secreto pareja a pareja HPKE (lo guarda la
//!   sesión; lo usa la entrada), conecta y presenta: `MediaReceiver` → SFrame →
//!   decodificación → [`FrameSink`].
//! - **Expulsión y salida**: el anfitrión expulsa con [`Session::kick`] (o ve salir a un invitado):
//!   `remove_member` → época nueva, que instala al momento; el expulsado procesa el commit, sabe
//!   que está fuera, deja de presentar y ya no puede descifrar nada de la época nueva.
//!
//! Hilos y tareas: un runtime `tokio` multihilo pequeño (dos hilos `coroom-red`) para la
//! señalización, MLS, HPKE y el transporte; los hilos síncronos del pipeline local (`capture`, `encode`,
//! `send` en el anfitrión; `decode`, `present` en el invitado) se comunican con él por canales con
//! la política "gana el último". El hilo `send` cifra y entrega el fotograma a todos los emisores
//! con `Handle::block_on`.
//!
//! **Rotación de claves al entrar alguien**: el anfitrión instala la época nueva cuando el invitado
//! nuevo conecta (no al crear el commit). Para entonces los invitados que ya estaban han procesado
//! el commit, que salió antes que el `Welcome` por la misma señalización, así que no pierden
//! fotogramas; el nuevo solo puede descifrar la época nueva y no recibe nada antes de conectar. Al
//! **salir o expulsar** a alguien la época nueva se instala enseguida (lo primero es dejarle fuera);
//! los demás pueden perder algún fotograma hasta procesar el commit y piden un intra.
//!
//! **Códec y cursor**: el invitado anuncia lo que decodifica (`Capabilities`) al abrir
//! el control; el anfitrión no crea el codificador hasta conocer las capacidades del primer
//! invitado (o hasta [`CAPABILITIES_TIMEOUT`], y entonces H.264), elige con [`choose_codec`] el
//! mejor códec común, lo anuncia (`CodecSelected`) y baja al siguiente común si entra un invitado
//! que no decodifica el que está en uso (reinicia el codificador; todos reciben el cambio y un
//! intra). El invitado crea su decodificador al recibir `CodecSelected` y lo recrea si cambia. El
//! cursor de la captura viaja por el control (`Cursor` a [`CURSOR_PERIOD`] como mucho, solo si
//! cambia, y `CursorShape` una vez por forma) y el invitado lo dibuja con su forma real.
//!
//! **Latencia**: el anfitrión envía por el control las marcas de sus etapas (`FrameTiming`, en el
//! reloj de su `MediaSender`) y el invitado las sitúa en el suyo con el desfase de `Ping`/`Pong`;
//! su [`LatencyLedger`] tiene así las ocho etapas de cada fotograma.
//!
//! **Entrada ([`input`])**: el invitado abre el stream de entrada y envía su teclado,
//! ratón y mando cifrados con el secreto pareja a pareja ([`GuestInput`]); el anfitrión los abre,
//! aplica permisos ([`Session::set_permissions`], con `InputPermissions` al invitado), pausa
//! ([`Session::set_paused`]) y expulsión, y los inyecta en el [`InputSink`] de
//! [`RemoteSession::host_with_sink`] (el de la plataforma con [`RemoteSession::host`]). La latencia
//! de entrada (`recibido − enviado` con el reloj de la sesión) sale en el resumen de los dos
//! extremos (`InputReport` cada segundo al invitado).
//!
//! **Audio y voz ([`audio`])**: con [`RemoteSession::with_host_audio`] el anfitrión envía
//! el audio del juego (pista `Audio`) y su voz (pista `Voice`), oye la de los invitados con permiso
//! de voz ([`Permissions::voice`]) y la reenvía cifrada a los demás; con
//! [`RemoteSession::with_guest_audio`] el invitado oye el juego sincronizado con el vídeo y las
//! voces, y habla si tiene permiso. [`RemoteSession::audio`] da los mandos (silenciar el micrófono o
//! el juego, pulsar para hablar) y quién habla. Todos los `MediaSender` del anfitrión comparten el
//! origen de su reloj ([`Core::clock_origin`]): las marcas de captura del audio van en ese reloj.
//!
//! **Chat ([`chat`])**: texto plano por el stream de control, en estrella (el invitado escribe al
//! anfitrión y el anfitrión, que fija el orden y la hora, lo reenvía a todos), cifrado de extremo a
//! extremo con la clave de chat de la época MLS para que ni el servidor ni el relay lo lean.
//! [`RemoteSession::send_chat`] y [`RemoteSession::clear_chat`] (anfitrión) envían;
//! [`SessionEvent::ChatMessage`], [`SessionEvent::ChatHistory`], [`SessionEvent::ChatCleared`] y
//! [`SessionEvent::ChatRejected`] cuentan lo que llega. Funciona en cuanto hay control y claves,
//! antes del vídeo.
//!
//! No se debe llamar a [`Session::start`] ni a [`Session::close`] desde dentro de un runtime de
//! `tokio` (la sesión tiene el suyo).

pub mod audio;
pub mod chat;
mod guest;
mod host;
pub mod input;
pub mod negotiate;
pub mod wire;

pub use audio::{AudioControl, GuestAudio, HostAudio, TalkMode, VoiceInput, talker_label};
pub use chat::{CHAT_RATE_MESSAGES, CHAT_RATE_WINDOW, ChatAuthor, ChatLine};
pub use input::{GuestInput, eis_provider};
pub use negotiate::{GuestCapabilities, choose_codec};

use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use coroom_audio::apps::AppFilter;
use coroom_capture::{CaptureOptions, Capturer, GpuFrames};
use coroom_codec::{Codec, DecoderSet, EncoderSet};
use coroom_input::InputSink;
use coroom_input::inject::{EisProvider, SinkOptions, platform_sink};
use coroom_transport::crypto::ChatKey;
use coroom_transport::crypto::hpke::PairwiseSecret;
use coroom_transport::crypto::sframe::MediaKey;
use coroom_transport::crypto::{Identity, MediaKeyRing, SafetyNumber, sha256_fingerprint};
use coroom_transport::media::MediaReceiver;
use coroom_transport::nat::{Mapper, MappingReport, PortMapping};
use coroom_transport::quic::ControlSender;
pub use coroom_transport::{CandidateClass, CandidateOutcome, CandidateReport};
use coroom_transport::{Endpoint, Path, PeerConnection, ServerTrust};
use crossbeam_channel as channel;
use tokio::sync::mpsc;
use tracing::debug;

use crate::latency::{InputLatency, LatencyLedger, LatencySummary, LinkSummary};
use crate::loopback::{FrameSink, LoopbackConfig, Shared};
use crate::{GuestId, Permissions, Role, Session, SessionError, SessionState};

/// Espera máxima de cada respuesta de la señalización y del saludo del control.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// Intervalo de fotograma que supone el invitado para los plazos del reensamblado hasta que
/// `CodecSelected` dice los fps del anfitrión (entonces el vídeo pasa a su intervalo real; antes,
/// con menos fps los plazos solo quedan más justos, nunca más largos).
pub const GUEST_FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

/// Cada cuánto el anfitrión recalcula el bitrate del codificador y el estado de los invitados.
pub const RATE_PERIOD: Duration = Duration::from_millis(100);

/// Espera del anfitrión a las capacidades de un invitado antes de suponer H.264.
pub const CAPABILITIES_TIMEOUT: Duration = Duration::from_secs(3);

/// Periodo mínimo entre dos `Cursor` (125 Hz, `proto::control::CURSOR_MAX_HZ`).
pub const CURSOR_PERIOD: Duration = Duration::from_millis(8);

/// Lo que mide el invitado el RTT con el anfitrión para [`SessionEvent::QualityEstimate`]. La
/// medida va en paralelo con el vídeo (las capacidades se envían nada más conectar; antes se
/// esperaba este segundo entero antes de pedirlo).
pub const QUALITY_PROBE: Duration = Duration::from_secs(1);

/// Invitado: con el códec ya elegido, tiempo sin ningún datagrama del anfitrión ni respuesta a los
/// `Ping` (uno por segundo) tras el que la conexión se da por muerta y se reconecta, sin esperar al
/// `IDLE_TIMEOUT` de QUIC (8 s). Con la pantalla quieta no hay vídeo, pero los `Pong` siguen.
pub const MEDIA_WATCHDOG: Duration = Duration::from_secs(3);

/// Invitado: tras un `Cursor` llegado en datagrama, lo que se ignoran las copias del stream (que
/// podrían ser posiciones viejas retenidas por una pérdida). Varias veces el periodo de la copia
/// del stream ([`CURSOR_STREAM_PERIOD`]).
pub const CURSOR_DATAGRAM_FRESH: Duration = Duration::from_millis(400);

/// Anfitrión: periodo mínimo de la copia del `Cursor` por el stream de control (los datagramas
/// numerados llevan las posiciones a [`CURSOR_PERIOD`]; el stream es el respaldo si no pasan).
pub const CURSOR_STREAM_PERIOD: Duration = Duration::from_millis(100);

/// Cada cuánto se envía un `Ping` durante la medida.
pub const QUALITY_PING_PERIOD: Duration = Duration::from_millis(100);

/// Reintentos de reconexión del invitado antes de rendirse.
pub const MAX_RECONNECT_ATTEMPTS: u32 = 5;

/// Espera antes del primer reintento; se duplica en cada uno ([`reconnect_delay`]).
pub const RECONNECT_BASE_DELAY: Duration = Duration::from_millis(500);

/// Tope del bitrate «Auto sin tope» (kbit/s): el control de tasa sube mientras el camino no haga
/// cola, hasta llenar el enlace. Es un techo técnico, no uno que se espere alcanzar: un
/// codificador por hardware a 4K60 casi sin pérdidas se queda por debajo.
pub const UNLIMITED_KBPS: u32 = 500_000;

/// Bitrate con el que arranca el control de tasa para un máximo `max_kbps`: 8 Mbit/s (la
/// 1080p60 «buena» de `docs/ARQUITECTURA.md` §2.1), o el máximo si es menor. Es la única cifra que
/// no sale de una medida: con el primer `Feedback` el control de tasa sube un 5 % cada 100 ms hasta
/// lo que el enlace entrega. Arrancar en la mitad del máximo (40 Mbit/s sin tope) mandaba el
/// primer intra, el más grande, a un bitrate que nadie había comprobado.
pub fn start_kbps(max_kbps: u32) -> u32 {
    max_kbps.clamp(1, 8_000)
}

/// Bitrate mínimo del control de tasa para un máximo `max_kbps`: una décima parte, entre 0,1 y
/// 0,5 Mbit/s. El bitrate no tiene tope (siempre [`UNLIMITED_KBPS`]), así que el mínimo no puede
/// salir del máximo: con 8 Mbit/s de suelo, un enlace de 3 Mbit/s se inundaba sin remedio.
pub fn min_kbps(max_kbps: u32) -> u32 {
    (max_kbps / 10).clamp(100, 500).min(max_kbps.max(1))
}

/// Espera antes del reintento `attempt` (desde 1): 0,5 s, 1 s, 2 s, 4 s y 8 s.
pub fn reconnect_delay(attempt: u32) -> Duration {
    RECONNECT_BASE_DELAY * 2u32.saturating_pow(attempt.saturating_sub(1).min(10))
}

/// Pipeline típico que se suma al RTT para estimar el sobrecoste (`docs/ARQUITECTURA.md` §2.1 y §2.2).
pub const TYPICAL_PIPELINE_MS: f64 = 25.0;

/// Nivel de experiencia esperado según el sobrecoste añadido (`docs/ARQUITECTURA.md` §2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QualityLevel {
    /// Sobrecoste ≤ 40 ms: shooters, lucha, ritmo.
    Excellent,
    /// ≤ 70 ms: acción, plataformas, cooperativo.
    Good,
    /// ≤ 110 ms: estrategia, turnos, casual.
    Acceptable,
    /// > 110 ms.
    Poor,
}

impl QualityLevel {
    /// Nivel para un sobrecoste total (RTT + pipeline) en ms.
    pub fn from_overhead_ms(overhead_ms: f64) -> Self {
        if overhead_ms <= 40.0 {
            QualityLevel::Excellent
        } else if overhead_ms <= 70.0 {
            QualityLevel::Good
        } else if overhead_ms <= 110.0 {
            QualityLevel::Acceptable
        } else {
            QualityLevel::Poor
        }
    }

    /// Nivel para un RTT medido, con el pipeline típico de [`TYPICAL_PIPELINE_MS`] sumado.
    pub fn from_rtt_ms(rtt_ms: f64) -> Self {
        Self::from_overhead_ms(rtt_ms.max(0.0) + TYPICAL_PIPELINE_MS)
    }

    /// Nombre del nivel.
    pub const fn label(self) -> &'static str {
        match self {
            QualityLevel::Excellent => "Excelente",
            QualityLevel::Good => "Buena",
            QualityLevel::Acceptable => "Aceptable",
            QualityLevel::Poor => "Mala",
        }
    }

    /// Para qué juegos va bien.
    pub const fn description(self) -> &'static str {
        match self {
            QualityLevel::Excellent => "ideal para shooters, lucha y juegos de ritmo",
            QualityLevel::Good => "ideal para acción, plataformas y cooperativo",
            QualityLevel::Acceptable => "bien para estrategia, juegos por turnos y casuales",
            QualityLevel::Poor => "se notará el retraso: mejor juegos por turnos",
        }
    }
}

impl fmt::Display for QualityLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Configuración de red común a anfitrión e invitado.
#[derive(Debug, Clone)]
pub struct RemoteConfig {
    /// URL del servidor de coroom (`https://coroom.example.com`).
    pub server_url: String,
    /// Token de la señalización: el de sesión de la cuenta (`coroom login`, vinculado a la
    /// identidad de la sesión) o, con un servidor en modo desarrollo, `COROOM_DEV_TOKEN`.
    pub token: String,
    /// Confianza en el certificado del servidor: huella del autofirmado de desarrollo o raíces.
    pub trust: ServerTrust,
    /// Salta la conexión directa y usa siempre el relay del servidor.
    pub force_relay: bool,
    /// Pedir al router de la red local que abra el puerto UDP del `Endpoint` (PCP, NAT-PMP o UPnP)
    /// y anunciar su dirección WAN como candidato ([`coroom_transport::nat`]).
    pub port_mapping: bool,
    /// Dirección del socket UDP local; `None` = `[::]:0` (o `0.0.0.0:0` sin IPv6).
    pub bind: Option<SocketAddr>,
    /// Tiempo de la apertura simultánea de NAT antes de recurrir al relay.
    pub punch_timeout: Duration,
    /// Candidatos que se anuncian en lugar de los del socket (redes con un reenvío de puerto fijo
    /// y pruebas con un proxy delante); `None` = los locales y el reflejado.
    pub advertise_candidates: Option<Vec<SocketAddr>>,
    /// Nombre visible de la sala (solo anfitrión).
    pub room_name: String,
    /// Anfitrión: los invitados entran callados (solo mirar) en vez de con la voz permitida.
    pub guests_muted: bool,
    /// Invitado: cuánto se mide el RTT con el anfitrión antes de pedir el vídeo
    /// ([`QUALITY_PROBE`]; cero para no esperar).
    pub quality_probe: Duration,
    /// Invitado: anunciar los códecs con los que acepta 4:4:4 (Ajustes).
    pub accept_yuv444: bool,
    /// Invitado: anunciar los códecs con los que acepta HDR10 (Ajustes).
    pub accept_hdr: bool,
    /// Solo en builds de desarrollo: medios sin SFrame para la prueba A/B de `docs/ARQUITECTURA.md` §2.3.
    /// Los dos extremos deben ponerlo igual. No existe en release.
    #[cfg(debug_assertions)]
    pub insecure_no_sframe: bool,
    /// Anfitrión: reparto en el tiempo de cada fotograma. Por defecto las constantes de
    /// `coroom_transport::media`; `coroom host` lee `PacingConfig::from_env()`.
    pub pacing: coroom_transport::media::PacingConfig,
}

impl RemoteConfig {
    /// Configuración por defecto para `server_url` con el token `token`.
    pub fn new(
        server_url: impl Into<String>,
        token: impl Into<String>,
        trust: ServerTrust,
    ) -> Self {
        Self {
            server_url: server_url.into(),
            token: token.into(),
            trust,
            force_relay: false,
            port_mapping: true,
            bind: None,
            punch_timeout: coroom_transport::connect::PUNCH_TIMEOUT,
            advertise_candidates: None,
            room_name: "Sala de coroom".to_owned(),
            guests_muted: false,
            quality_probe: QUALITY_PROBE,
            accept_yuv444: false,
            accept_hdr: false,
            #[cfg(debug_assertions)]
            insecure_no_sframe: false,
            pacing: coroom_transport::media::PacingConfig::default(),
        }
    }

    /// Si los medios van cifrados con SFrame (siempre en release).
    pub fn sframe(&self) -> bool {
        #[cfg(debug_assertions)]
        {
            !self.insecure_no_sframe
        }
        #[cfg(not(debug_assertions))]
        {
            true
        }
    }
}

/// Lo que pasa en la sesión, para la interfaz y la línea de órdenes.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// Anfitrión: sala creada.
    RoomCreated {
        /// Código que hay que dar a los invitados.
        code: String,
        /// Huella de la identidad del anfitrión.
        fingerprint: String,
    },
    /// Anfitrión: un invitado entró (aprobado con permisos de solo mirar).
    GuestJoined {
        /// Invitado.
        guest: GuestId,
        /// Su usuario en el servidor (`0` en modo desarrollo).
        user: u64,
        /// Su nombre visible.
        name: String,
        /// Huella de su identidad.
        fingerprint: String,
        /// Número de seguridad entre los dos.
        safety_number: SafetyNumber,
        /// Permisos con que entró.
        permissions: Permissions,
    },
    /// Anfitrión: el invitado ya recibe medios.
    GuestConnected {
        /// Invitado.
        guest: GuestId,
        /// Ruta.
        path: Path,
        /// Dirección del invitado (o del relay) vista desde aquí.
        remote: SocketAddr,
    },
    /// Anfitrión: el invitado salió o se le expulsó.
    GuestLeft {
        /// Invitado.
        guest: GuestId,
        /// Si se le expulsó.
        kicked: bool,
    },
    /// Invitado: dentro de la sala.
    Joined {
        /// Nombre visible del anfitrión.
        host_name: String,
        /// Huella de la identidad del anfitrión.
        host_fingerprint: String,
        /// Número de seguridad entre los dos.
        safety_number: SafetyNumber,
    },
    /// Invitado: conectado con el anfitrión.
    Connected {
        /// Ruta.
        path: Path,
        /// Dirección del anfitrión (o del relay) vista desde aquí.
        remote: SocketAddr,
    },
    /// Invitado: RTT medido con el anfitrión antes de pedir el vídeo (durante
    /// [`RemoteConfig::quality_probe`]) y nivel esperado (`docs/ARQUITECTURA.md` §2.1).
    QualityEstimate {
        /// RTT en ms.
        rtt_ms: f64,
        /// Nivel con el pipeline típico.
        level: QualityLevel,
    },
    /// Qué respondió el router al pedirle que abra el puerto UDP (al abrir la sala o al entrar).
    PortMapping {
        /// El informe.
        report: MappingReport,
    },
    /// Qué pasó con cada candidato de conexión directa y, si se usa el relay, por qué (el
    /// invitado al conectar; el anfitrión por cada invitado).
    Candidates {
        /// El invitado del que se habla (`None` desde el invitado: el anfitrión).
        guest: Option<GuestId>,
        /// Cada dirección probada con su resultado.
        candidates: Vec<CandidateReport>,
        /// Por qué se va por el relay, si se va.
        relay_reason: Option<String>,
    },
    /// Lo que el camino entrega según la sonda del enlace (trenes de datagramas antes del vídeo):
    /// el anfitrión arranca el bitrate en una parte de esto. El anfitrión lo emite por invitado;
    /// el invitado, lo que midió él.
    LinkProbe {
        /// El invitado medido (`None` desde el invitado).
        guest: Option<GuestId>,
        /// kbit/s entregados.
        kbps: u32,
        /// Datagramas del tren que no llegaron, de `sent`.
        lost: u16,
        /// Datagramas del tren.
        sent: u16,
    },
    /// Invitado: se perdió la conexión y se vuelve a entrar en la sala.
    Reconnecting {
        /// Intento (desde 1).
        attempt: u32,
        /// Intentos como mucho.
        of: u32,
        /// Espera antes del intento.
        retry_in: Duration,
        /// Qué falló.
        reason: String,
    },
    /// Invitado: otra vez conectado con el anfitrión tras [`SessionEvent::Reconnecting`].
    Reconnected {
        /// Ruta.
        path: Path,
    },
    /// Época MLS nueva (claves de medios rotadas).
    EpochChanged {
        /// Época.
        epoch: u64,
    },
    /// Códec del vídeo: el que eligió el anfitrión o el que recibe el invitado.
    VideoCodec {
        /// Códec.
        codec: Codec,
        /// Implementación (`codifica SVT-AV1`, `decodifica dav1d`).
        detail: String,
    },
    /// Secreto pareja a pareja (HPKE) listo con `peer`.
    InputKeyReady {
        /// Par (el invitado para el anfitrión, el anfitrión `0` para el invitado).
        peer: u32,
    },
    /// Un amigo te invita a su sala.
    Invited {
        /// Usuario que invita.
        from_user: u64,
        /// Su nombre visible.
        from_name: String,
        /// Código de la sala.
        code: String,
    },
    /// Un amigo pide entrar en tu sala (solo al anfitrión): se le deja entrar con
    /// [`Session::invite`] o se le dice que no con [`Session::decline_join`].
    JoinRequested {
        /// Usuario que lo pide.
        from_user: u64,
        /// Su nombre visible.
        from_name: String,
    },
    /// El anfitrión al que pediste entrar dijo que ahora no.
    JoinDeclined {
        /// Usuario que respondió.
        from_user: u64,
        /// Su nombre visible.
        from_name: String,
    },
    /// Qué hace un amigo ahora (abre una sala, juega en una, nada). Llega al conectarse y cada
    /// vez que cambia; al desconectarse solo llega `Presence`.
    Activity {
        /// Usuario.
        user: u64,
        /// Su actividad.
        activity: coroom_transport::proto::signal::Activity,
    },
    /// Un amigo se conectó o desconectó de la señalización (llega también con la sala abierta o
    /// dentro de una, para que la lista de amigos no se congele).
    Presence {
        /// Usuario.
        user: u64,
        /// Si está conectado.
        online: bool,
    },
    /// La sala no tiene relay (cuota agotada o servidor sin relay): solo conexión directa.
    RelayUnavailable(String),
    /// Algo no fue bien pero la sesión sigue.
    Warning(String),
    /// Un micrófono o una salida no funciona: la sesión sigue y espera a que se elija otro.
    Audio(audio::AudioIssue),
    /// Invitado: quién está en la sala (el anfitrión incluido), cada vez que cambia.
    Members(Vec<RoomMember>),
    /// Invitado: el anfitrión comparte algo (`true`) o la sala es solo de voz y chat (`false`),
    /// al conectar y cada vez que cambia.
    Sharing(bool),
    /// Invitado: el anfitrión le ofrece pasarle el anfitrión ([`RemoteSession::reply_handoff`]).
    HandoffOffered,
    /// Invitado: el anfitrión retiró la oferta de pasarle el anfitrión (si ya abrió su sala,
    /// sobra).
    HandoffCancelled,
    /// Anfitrión: la respuesta de `guest` a la oferta de pasarle el anfitrión: el código de su
    /// sala nueva, o `None` si no acepta.
    HandoffReply {
        /// Invitado.
        guest: GuestId,
        /// Código de la sala nueva (válido).
        code: Option<String>,
    },
    /// Invitado: el anfitrión pasó a otro: entrar en la sala `code` de `host_name`.
    MoveToRoom {
        /// Código de la sala nueva (válido).
        code: String,
        /// Nombre del anfitrión nuevo.
        host_name: String,
    },
    /// Un mensaje del chat, en el orden que fija el anfitrión (los propios llegan así también:
    /// el anfitrión los devuelve con `own`).
    ChatMessage(ChatLine),
    /// Invitado, al entrar (o volver a entrar) en la sala: los últimos mensajes del chat. Sustituye
    /// lo que hubiese.
    ChatHistory {
        /// Del más antiguo al más nuevo (como mucho `proto::control::CHAT_HISTORY_MAX`).
        messages: Vec<ChatLine>,
    },
    /// El anfitrión vació el chat para todos.
    ChatCleared,
    /// Anfitrión: se descartó un mensaje de un invitado (ritmo, texto no válido o claves de una
    /// época anterior). Lo que no deja enviar [`RemoteSession::send_chat`] vuelve como error.
    ChatRejected {
        /// Motivo, en español.
        reason: String,
    },
    /// La sesión terminó (`None` = cierre normal).
    Ended(Option<SessionError>),
}

impl fmt::Display for SessionEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionEvent::RoomCreated { code, fingerprint } => write!(
                f,
                "Sala creada. Código: {code}\n  Huella del anfitrión: {fingerprint}"
            ),
            SessionEvent::GuestJoined {
                guest,
                name,
                fingerprint,
                safety_number,
                permissions,
                ..
            } => write!(
                f,
                "Invitado {} ({name}) en la sala ({}).\n  Huella: {fingerprint}\n  Número de \
                 seguridad: {safety_number}",
                guest.0,
                permissions_label(permissions)
            ),
            SessionEvent::GuestConnected {
                guest,
                path,
                remote,
            } => {
                write!(f, "Invitado {} conectado ({path}, {remote}).", guest.0)
            }
            SessionEvent::GuestLeft {
                guest,
                kicked: true,
            } => write!(
                f,
                "Invitado {} expulsado; claves de la sala rotadas.",
                guest.0
            ),
            SessionEvent::GuestLeft { guest, .. } => {
                write!(f, "El invitado {} ha salido de la sala.", guest.0)
            }
            SessionEvent::Joined {
                host_name,
                host_fingerprint,
                safety_number,
            } => write!(
                f,
                "Dentro de la sala de {host_name}.\n  Huella del anfitrión: {host_fingerprint}\n  Número de \
                 seguridad: {safety_number} (compáralo con el que ve el anfitrión)"
            ),
            SessionEvent::Connected { path, remote } => {
                write!(f, "Conectado con el anfitrión ({path}, {remote}).")
            }
            SessionEvent::Members(members) => {
                let names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
                write!(f, "En la sala: {}.", names.join(", "))
            }
            SessionEvent::HandoffOffered => f.write_str("El anfitrión te ofrece pasarte la sala."),
            SessionEvent::HandoffCancelled => {
                f.write_str("El anfitrión ya no te ofrece pasarte la sala.")
            }
            SessionEvent::HandoffReply {
                guest,
                code: Some(code),
            } => write!(f, "El invitado {} abrió la sala {code}.", guest.0),
            SessionEvent::HandoffReply { guest, code: None } => {
                write!(f, "El invitado {} no acepta la sala.", guest.0)
            }
            SessionEvent::MoveToRoom { code, host_name } => {
                write!(f, "La sala pasa a {host_name}: código {code}.")
            }
            SessionEvent::Sharing(true) => f.write_str("El anfitrión comparte su pantalla."),
            SessionEvent::Sharing(false) => {
                f.write_str("Nadie está compartiendo: la sala es solo de voz y chat.")
            }
            SessionEvent::QualityEstimate { rtt_ms, level } => write!(
                f,
                "RTT con el anfitrión: {rtt_ms:.1} ms. Calidad esperada: {level} ({}).",
                level.description()
            ),
            SessionEvent::PortMapping { report } => write!(f, "Puerto en el router: {report}."),
            SessionEvent::Candidates {
                guest,
                candidates,
                relay_reason,
            } => {
                let who = guest.map_or_else(
                    || "el anfitrión".to_owned(),
                    |guest| format!("el invitado {}", guest.0),
                );
                write!(f, "Conexión directa con {who}:")?;
                if candidates.is_empty() {
                    write!(f, " sin direcciones que probar.")?;
                }
                for candidate in candidates {
                    write!(f, "\n  {candidate}")?;
                }
                if let Some(reason) = relay_reason {
                    write!(f, "\n  Se usa el relay: {reason}.")?;
                }
                Ok(())
            }
            SessionEvent::LinkProbe {
                guest,
                kbps,
                lost,
                sent,
            } => {
                let who = guest.map_or_else(String::new, |guest| {
                    format!(" hacia el invitado {}", guest.0)
                });
                write!(
                    f,
                    "El enlace{who} entrega {:.1} Mbit/s ({lost} de {sent} datagramas de la sonda \
                     perdidos).",
                    f64::from(*kbps) / 1000.0
                )
            }
            SessionEvent::Reconnecting {
                attempt,
                of,
                retry_in,
                reason,
            } => write!(
                f,
                "Se perdió la conexión ({reason}). Reconectando en {:.1} s (intento {attempt} de \
                 {of})…",
                retry_in.as_secs_f64()
            ),
            SessionEvent::Reconnected { path } => {
                write!(f, "Otra vez conectado con el anfitrión ({path}).")
            }
            SessionEvent::EpochChanged { epoch } => {
                write!(f, "Claves de la sala en la época MLS {epoch}.")
            }
            SessionEvent::VideoCodec { codec, detail } => {
                write!(f, "Vídeo en {codec} ({detail}).")
            }
            SessionEvent::InputKeyReady { peer } => {
                write!(
                    f,
                    "Clave de entrada pareja a pareja lista con el par {peer}."
                )
            }
            SessionEvent::Invited {
                from_name, code, ..
            } => write!(f, "{from_name} te invita a su sala {code}."),
            SessionEvent::JoinRequested { from_name, .. } => {
                write!(f, "{from_name} pide entrar en la sala.")
            }
            SessionEvent::JoinDeclined { from_name, .. } => {
                write!(f, "{from_name} no te deja entrar ahora.")
            }
            SessionEvent::Activity { user, activity } => {
                write!(f, "El usuario {user} está {activity:?}.")
            }
            SessionEvent::Presence { user, online: true } => {
                write!(f, "El usuario {user} se ha conectado.")
            }
            SessionEvent::Presence {
                user,
                online: false,
            } => write!(f, "El usuario {user} se ha desconectado."),
            SessionEvent::RelayUnavailable(message) => {
                write!(f, "Sin relay: {message}.")
            }
            SessionEvent::Warning(message) => write!(f, "Aviso: {message}"),
            SessionEvent::Audio(issue) => write!(f, "Aviso: {issue}"),
            SessionEvent::ChatMessage(line) => write!(f, "{line}"),
            SessionEvent::ChatHistory { messages } if messages.is_empty() => {
                f.write_str("Historial del chat: vacío.")
            }
            SessionEvent::ChatHistory { messages } => {
                write!(f, "Historial del chat ({} mensajes):", messages.len())?;
                for line in messages {
                    write!(f, "\n  {line}")?;
                }
                Ok(())
            }
            SessionEvent::ChatCleared => f.write_str("El anfitrión ha vaciado el chat."),
            SessionEvent::ChatRejected { reason } => {
                write!(f, "Mensaje del chat descartado: {reason}.")
            }
            SessionEvent::Ended(None) => f.write_str("Sesión terminada."),
            SessionEvent::Ended(Some(err)) => write!(f, "Sesión terminada: {err}."),
        }
    }
}

/// Permisos en palabras: "solo mirar", "teclado y ratón", "mando en ranura 2"…
pub fn permissions_label(permissions: &Permissions) -> String {
    let mut parts = Vec::new();
    if permissions.keyboard_mouse {
        parts.push("teclado y ratón".to_owned());
    }
    if let Some(slot) = permissions.gamepad_slot {
        parts.push(format!("mando en ranura {slot}"));
    }
    if permissions.voice {
        parts.push("voz".to_owned());
    }
    if parts.is_empty() {
        "solo mirar".to_owned()
    } else {
        parts.join(", ")
    }
}

/// Estado de un invitado visto por el anfitrión.
#[derive(Debug, Clone, PartialEq)]
pub struct GuestStatus {
    /// Invitado.
    pub guest: GuestId,
    /// Su usuario en el servidor (`0` en modo desarrollo).
    pub user: u64,
    /// Su nombre visible.
    pub name: String,
    /// Clave pública de su identidad (la de su credencial MLS).
    pub public_key: [u8; 32],
    /// Huella de su identidad.
    pub fingerprint: String,
    /// Número de seguridad entre anfitrión e invitado.
    pub safety_number: SafetyNumber,
    /// Permisos.
    pub permissions: Permissions,
    /// Hoja MLS.
    pub leaf: u32,
    /// Ruta, cuando ya está conectado.
    pub path: Option<Path>,
    /// Dirección del otro extremo de la conexión (el invitado o, por el relay, el servidor).
    pub remote: Option<SocketAddr>,
    /// RTT de su conexión (en el relay, el del tramo anfitrión ↔ relay), en ms.
    pub rtt_ms: Option<f64>,
    /// Bitrate objetivo que pide su control de tasa, en kbit/s.
    pub target_kbps: Option<u32>,
    /// Lo que la sonda del enlace midió hacia él al conectar, en kbit/s.
    pub link_kbps: Option<u32>,
    /// Qué pasó con cada candidato de conexión directa hacia él.
    pub candidates: Vec<CandidateReport>,
    /// Si ya hay secreto pareja a pareja con él (sin él, su entrada se rechaza).
    pub input_key: bool,
    /// Latencia de su entrada y eventos aplicados y descartados.
    pub input: InputLatency,
    /// El mando físico que declaró para su ranura, si lo hay.
    pub gamepad: Option<GuestPad>,
}

/// Cómo puede llegar el mando físico de un invitado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PadTransport {
    /// HID: tal cual, con su descriptor.
    Hid,
    /// XInput (Xbox por cable): con su identidad.
    XInput,
    /// Solo convertido, por lo que dijo el invitado.
    Converted,
}

/// El mando físico de un invitado y cómo va.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestPad {
    /// Nombre que da el mando.
    pub name: String,
    /// Fabricante.
    pub vendor: u16,
    /// Producto.
    pub product: u16,
    /// Cómo puede llegar.
    pub transport: PadTransport,
    /// Si va tal cual ahora (si no, convertido a Xbox 360).
    pub raw: bool,
    /// Por qué no va tal cual, si se pidió y no se pudo.
    pub note: Option<String>,
}

/// Miembro de la sala visto desde un invitado ([`SessionEvent::Members`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomMember {
    /// Id del invitado (0 = el anfitrión), el de los avisos de quién habla.
    pub guest: u32,
    /// Hoja MLS: con ella se silencia su voz ([`AudioControl::set_voice_muted`]).
    pub leaf: u32,
    /// Nombre visible.
    pub name: String,
    /// Si es este mismo equipo.
    pub you: bool,
}

/// Cambio de vídeo con la sala abierta ([`RemoteSession::set_video`]). Lo que no sea `None` se
/// aplica sin cerrar la sala ni cortar a los invitados.
#[derive(Debug, Clone, Default)]
pub struct VideoChange {
    /// Codificadores entre los que elegir ahora (`EncoderSet::probe().only_codec(…)…`): se
    /// renegocia con los invitados y, si sale otro códec o backend, se abre (un intra).
    pub encoders: Option<EncoderSet>,
    /// Bitrate máximo nuevo en kbit/s: el control de tasa de cada invitado lo usa enseguida.
    pub max_kbps: Option<u32>,
    /// Resolución máxima nueva (ancho × alto, pares y no nulos): la captura que elige su tamaño
    /// (la sintética) lo reduce ya, el codificador se reabre con un intra y los invitados reciben
    /// el tamaño nuevo en el flujo (`CodecSelected` y `HostFrameSize` les anuncian el cambio).
    /// La captura de pantalla sigue entregando el tamaño de la fuente.
    pub max_size: Option<(u32, u32)>,
    /// Fotogramas por segundo nuevos (no nulos): la cadencia de la captura en marcha, el ritmo
    /// del emisor a cada invitado, el codificador (reabierto, un intra) y los plazos del invitado.
    pub fps: Option<u32>,
}

impl VideoChange {
    /// El cambio con la resolución máxima redondeada a lados pares (hacia abajo), como hace
    /// la sala al abrirse: el códec necesita croma 4:2:0 entero y una ventana de 1199×808 no es
    /// motivo para rechazar nada.
    pub fn normalized(mut self) -> Self {
        if let Some(size) = self.max_size.as_mut() {
            *size = even(*size);
        }
        self
    }

    /// Por qué el cambio no vale, si no vale: tamaño nulo (menor que 2×2) o fps nulos.
    pub fn validate(&self) -> Result<(), String> {
        if let Some((width, height)) = self.max_size
            && (width < 2 || height < 2)
        {
            return Err(format!(
                "la resolución máxima necesita al menos 2×2 píxeles, no {width}×{height}"
            ));
        }
        if self.fps == Some(0) {
            return Err("los fps no pueden ser 0".into());
        }
        Ok(())
    }
}

/// `size` con los dos lados redondeados hacia abajo a par (lo que exige el croma 4:2:0).
pub(crate) fn even((width, height): (u32, u32)) -> (u32, u32) {
    (width & !1, height & !1)
}

/// Órdenes de la interfaz a la tarea de red.
#[derive(Debug)]
pub(crate) enum Command {
    Kick(GuestId),
    /// Invitar a un amigo a la sala (`Invite` por la señalización).
    Invite(u64),
    /// Anfitrión: decir a un amigo que pidió entrar que ahora no (`JoinDecline`).
    DeclineJoin(u64),
    /// Avisar al invitado de sus permisos nuevos (`InputPermissions`).
    PermissionsChanged(GuestId),
    /// Avisar al invitado de si su mando va tal cual (`GamepadMode`).
    GamepadMode {
        guest: GuestId,
        slot: u8,
        raw: bool,
    },
    /// Informe de salida para el mando tal cual del invitado (`GamepadOutput`).
    GamepadOutput {
        guest: GuestId,
        slot: u8,
        data: Vec<u8>,
    },
    /// Anfitrión: otros codificadores (otro códec o backend preferido), bitrate máximo,
    /// resolución máxima o fps.
    Video(VideoChange),
    /// Invitado: volver a entrar en la sala ya.
    Reconnect,
    /// Anfitrión: enviar a la sala un mensaje del chat ya validado y contado en el límite.
    Chat(String),
    /// Anfitrión: vaciar el chat para todos.
    ChatClear,
    /// Anfitrión: ofrecer a un invitado pasarle el anfitrión (`HandoffOffer`).
    OfferHandoff(GuestId),
    /// Anfitrión: retirar esa oferta (`HandoffCancel`).
    CancelHandoff(GuestId),
    /// Anfitrión: mandar a todos los invitados (también a los que entren después) a la sala
    /// `code` (`MoveToRoom`); `done` avisa cuando ya no queda nadie en esta.
    MoveToRoom {
        code: String,
        host_name: String,
        done: channel::Sender<()>,
    },
    Close,
}

/// Estado del enlace del invitado con el anfitrión.
#[derive(Default)]
pub(crate) struct GuestLink {
    pub(crate) path: Option<Path>,
    pub(crate) connection: Option<PeerConnection>,
    pub(crate) receiver: Option<Arc<MediaReceiver>>,
    /// Control hacia el anfitrión (para `VoiceState`).
    pub(crate) control: Option<ControlSender>,
    pub(crate) target_kbps: u32,
    pub(crate) safety_number: Option<SafetyNumber>,
    pub(crate) host_fingerprint: String,
    pub(crate) host_name: String,
}

#[derive(Default)]
struct End {
    done: bool,
    reason: Option<SessionError>,
}

/// Estado compartido entre la sesión, sus hilos y sus tareas.
pub(crate) struct Core {
    pub(crate) role: Role,
    state: Mutex<SessionState>,
    events: channel::Sender<SessionEvent>,
    events_rx: channel::Receiver<SessionEvent>,
    end: Mutex<End>,
    pub(crate) shared: Arc<Shared>,
    pub(crate) ledger: Arc<LatencyLedger>,
    pub(crate) ring: MediaKeyRing,
    pub(crate) sframe: bool,
    pub(crate) identity: Arc<Identity>,
    pub(crate) room_code: Mutex<Option<String>>,
    pub(crate) epoch: AtomicU64,
    pub(crate) own_leaf: AtomicU32,
    pub(crate) host_leaf: AtomicU32,
    pub(crate) removed: AtomicBool,
    pub(crate) guests: Mutex<BTreeMap<GuestId, GuestStatus>>,
    pub(crate) link: Mutex<GuestLink>,
    /// El puerto abierto en el router (se renueva solo y se borra al terminar), o su informe.
    pub(crate) port_mapping: Mutex<Option<PortMapping>>,
    pub(crate) input_keys: Mutex<BTreeMap<u32, PairwiseSecret>>,
    /// Anfitrión: la entrada de los invitados (desde que arranca la sesión).
    pub(crate) host_input: Mutex<Option<Arc<input::HostInput>>>,
    /// Invitado: su entrada.
    pub(crate) guest_input: GuestInput,
    /// Invitado: control del hilo `decode` (para decodificar por software en marcha).
    pub(crate) decode: Mutex<Option<Arc<crate::loopback::DecodeControl>>>,
    /// Invitado: decodificar solo por software desde el principio.
    pub(crate) software_decoder: AtomicBool,
    /// Origen del reloj del anfitrión (el de todos sus `MediaSender`).
    pub(crate) clock_origin: Instant,
    /// Mandos y estado del audio.
    pub(crate) audio: AudioControl,
    /// Invitado: entró alguna vez en la sala (solo entonces se reconecta).
    pub(crate) joined_once: AtomicBool,
    /// Invitado: reintentos de reconexión seguidos sin volver a conectar.
    pub(crate) reconnect_attempts: AtomicU32,
    /// Invitado: su id de par en la sala (`Joined`), la etiqueta de sus datagramas de control.
    pub(crate) own_peer_id: AtomicU32,
    /// Invitado: número de orden del último `Cursor` aplicado desde un datagrama (ver
    /// [`guest::control_datagrams`]) y cuándo llegó, en µs de `clock_origin` (0: ninguno aún).
    pub(crate) cursor_seq: AtomicU32,
    pub(crate) cursor_datagram_us: AtomicU64,
    /// Paquetes de audio hacia `audio-play`, si hay reproducción.
    pub(crate) audio_tx: Mutex<Option<channel::Sender<audio::Packet>>>,
    /// Órdenes a la tarea de red (para el hilo `inject`: modo y salida de los mandos tal cual).
    pub(crate) commands: Mutex<Option<mpsc::UnboundedSender<Command>>>,
    /// Anfitrión: cómo se arranca una captura (imágenes de GPU y opciones), para las que
    /// sustituyan a la primera.
    pub(crate) capture_setup: Mutex<Option<(GpuFrames, CaptureOptions)>>,
    /// Anfitrión: aplicaciones cuyo audio se envía, pendiente de que lo recoja `audio-cap`.
    pub(crate) game_filter: Mutex<Option<AppFilter>>,
    /// Anfitrión: hay hilo `audio-cap` (la sala se abrió con audio del juego).
    pub(crate) game_audio: AtomicBool,
    /// Chat: claves por época, límite de ritmo e historial (anfitrión).
    pub(crate) chat: Mutex<chat::ChatState>,
}

/// Cerrojo que sobrevive a un pánico de otro hilo (el estado se sustituye entero).
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Core {
    fn new(role: Role, identity: Identity, ledger: Arc<LatencyLedger>, sframe: bool) -> Self {
        let (events, events_rx) = channel::unbounded();
        Self {
            role,
            state: Mutex::new(SessionState::Idle),
            events,
            events_rx,
            end: Mutex::new(End::default()),
            shared: Arc::new(Shared::default()),
            ledger,
            ring: MediaKeyRing::new(),
            sframe,
            identity: Arc::new(identity),
            room_code: Mutex::new(None),
            epoch: AtomicU64::new(0),
            own_leaf: AtomicU32::new(0),
            host_leaf: AtomicU32::new(0),
            removed: AtomicBool::new(false),
            guests: Mutex::new(BTreeMap::new()),
            link: Mutex::new(GuestLink::default()),
            input_keys: Mutex::new(BTreeMap::new()),
            host_input: Mutex::new(None),
            guest_input: GuestInput::default(),
            decode: Mutex::new(None),
            software_decoder: AtomicBool::new(false),
            clock_origin: Instant::now(),
            audio: AudioControl::default(),
            audio_tx: Mutex::new(None),
            commands: Mutex::new(None),
            capture_setup: Mutex::new(None),
            game_filter: Mutex::new(None),
            game_audio: AtomicBool::new(false),
            chat: Mutex::new(chat::ChatState::default()),
            joined_once: AtomicBool::new(false),
            reconnect_attempts: AtomicU32::new(0),
            own_peer_id: AtomicU32::new(0),
            cursor_seq: AtomicU32::new(0),
            cursor_datagram_us: AtomicU64::new(0),
            port_mapping: Mutex::new(None),
        }
    }

    /// `at` en µs del reloj del anfitrión (solo tiene sentido en el anfitrión).
    pub(crate) fn clock_us(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.clock_origin).as_micros() as u64
    }

    pub(crate) fn emit(&self, event: SessionEvent) {
        debug!(%event, "evento de sesión");
        let _ = self.events.send(event);
    }

    pub(crate) fn warn(&self, message: impl Into<String>) {
        self.emit(SessionEvent::Warning(message.into()));
    }

    pub(crate) fn state(&self) -> SessionState {
        *lock(&self.state)
    }

    /// Cambia de estado si la transición es válida (si no, la ignora).
    pub(crate) fn set_state(&self, to: SessionState) {
        let mut state = lock(&self.state);
        if state.can_transition_to(to) {
            *state = to;
        }
    }

    fn transition(&self, to: SessionState) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        if !state.can_transition_to(to) {
            return Err(SessionError::InvalidTransition { from: *state, to });
        }
        *state = to;
        Ok(())
    }

    /// Termina la sesión (la primera razón gana) y detiene los hilos del pipeline.
    pub(crate) fn end(&self, reason: Option<SessionError>) {
        {
            let mut end = lock(&self.end);
            if end.done {
                return;
            }
            end.done = true;
            end.reason = reason.clone();
        }
        *lock(&self.state) = SessionState::Closed;
        self.emit(SessionEvent::Ended(reason));
        self.shared.finish(None);
        self.guest_input.stop();
        lock(&self.audio_tx).take();
        lock(&self.port_mapping).take();
    }

    pub(crate) fn ended(&self) -> bool {
        lock(&self.end).done
    }

    fn end_reason(&self) -> Option<SessionError> {
        lock(&self.end).reason.clone()
    }

    /// Instala las claves de medios de `epoch` si es más nueva que la última instalada.
    pub(crate) fn install_epoch(&self, epoch: u64, keys: &[(u32, MediaKey)]) -> bool {
        if self
            .ring
            .epochs()
            .last()
            .is_some_and(|newest| *newest >= epoch)
        {
            return false;
        }
        match self.ring.install_epoch(epoch, keys) {
            Ok(()) => {
                self.epoch.store(epoch, Ordering::Release);
                self.emit(SessionEvent::EpochChanged { epoch });
                true
            }
            Err(err) => {
                self.warn(format!(
                    "no se pudieron instalar las claves de la época {epoch}: {err}"
                ));
                false
            }
        }
    }

    pub(crate) fn link_summary(&self) -> Option<LinkSummary> {
        let link = lock(&self.link);
        let safety = link.safety_number?;
        let (rtt_ms, stats) = match (&link.connection, &link.receiver) {
            (Some(conn), Some(receiver)) => {
                (conn.rtt().as_secs_f64() * 1000.0, Some(receiver.stats()))
            }
            _ => (0.0, None),
        };
        let stats = stats.unwrap_or_default();
        let total = stats.lost_objects + stats.received_objects;
        Some(LinkSummary {
            path: link
                .path
                .map_or_else(|| "sin conectar".to_owned(), |path| path.to_string()),
            remote: link
                .connection
                .as_ref()
                .map(|conn| conn.remote_address().to_string()),
            rtt_ms,
            target_kbps: link.target_kbps,
            loss_percent: if total == 0 {
                0.0
            } else {
                stats.lost_objects as f64 * 100.0 / total as f64
            },
            groups_dropped: stats.dropped,
            groups_recovered: stats.recovered_fec + stats.recovered_nack,
            epoch: self.epoch.load(Ordering::Acquire),
            safety_number: safety.to_string(),
            peer_fingerprint: link.host_fingerprint.clone(),
            peer_name: link.host_name.clone(),
            sframe: self.sframe,
        })
    }
}

/// Comprobación pública de un secreto pareja a pareja: los 16 primeros caracteres de
/// `SHA-256("coroom input key check" || secreto)`. Sirve para ver que los dos extremos derivaron lo
/// mismo sin enseñar el secreto.
pub fn input_key_check(secret: &PairwiseSecret) -> String {
    let mut data = b"coroom input key check".to_vec();
    data.extend_from_slice(secret.as_slice());
    let mut check = sha256_fingerprint(&data);
    check.truncate(16);
    check
}

/// Candidatos que se anuncian: los configurados o los locales del socket más el reflejado y, si
/// el router respondió, su dirección WAN ([`wan_candidate`]).
pub(crate) fn advertised_candidates(
    config: &RemoteConfig,
    endpoint: &Endpoint,
    observed: SocketAddr,
    mapping: Option<&MappingReport>,
) -> Vec<SocketAddr> {
    if let Some(list) = &config.advertise_candidates {
        return list.clone();
    }
    let mut out = coroom_transport::connect::local_candidates(endpoint).unwrap_or_default();
    out.push(observed);
    out.extend(mapping.and_then(|report| wan_candidate(config, report)));
    out.sort();
    out.dedup();
    out
}

/// La dirección WAN del router con el puerto que abrió; si dijo su dirección pero no abrió nada
/// y el socket está en un puerto fijo (`bind`), la WAN con ese puerto: un reenvío hecho a mano en
/// el router.
fn wan_candidate(config: &RemoteConfig, report: &MappingReport) -> Option<SocketAddr> {
    if let Some(mapping) = report.mapping() {
        return Some(SocketAddr::V4(mapping.external));
    }
    let external = report.external_ip()?;
    let port = config
        .bind
        .map(|bind| bind.port())
        .filter(|port| *port != 0)?;
    Some(SocketAddr::new(std::net::IpAddr::V4(external), port))
}

/// Pide al router que abra el puerto del `Endpoint`, en segundo plano, si la configuración lo
/// quiere y no se fuerza el relay.
pub(crate) fn start_port_mapping(
    config: &RemoteConfig,
    endpoint: &Endpoint,
) -> Option<tokio::task::JoinHandle<PortMapping>> {
    if !config.port_mapping || config.force_relay {
        return None;
    }
    let port = endpoint.local_addr().ok()?.port();
    let mapper = Mapper::new();
    Some(tokio::spawn(async move { mapper.map(port).await }))
}

/// Espera en segundo plano a la petición al router, la guarda en `core` (se renueva sola y se
/// borra con la sesión), la cuenta y entrega su informe por el canal que devuelve. Antes se
/// esperaba al router (hasta 2,2 s si no contesta nada) **antes** de anunciar ningún candidato,
/// y esos segundos se sumaban al tiempo hasta el primer fotograma; ahora los candidatos locales y
/// el reflejado salen enseguida y la dirección WAN que abra el router se anuncia después, con un
/// segundo `Candidates`, mientras la apertura simultánea sigue en marcha.
pub(crate) fn watch_port_mapping(
    core: &Arc<Core>,
    task: Option<tokio::task::JoinHandle<PortMapping>>,
) -> Option<tokio::sync::oneshot::Receiver<MappingReport>> {
    let task = task?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let core = Arc::clone(core);
    tokio::spawn(async move {
        if let Ok(mapping) = task.await {
            let _ = tx.send(store_port_mapping(&core, mapping));
        }
    });
    Some(rx)
}

/// El informe del router cuando llegue; espera para siempre si no se pidió nada (así la rama
/// del `select!` que lo usa no se dispara).
pub(crate) async fn mapping_report(
    rx: &mut Option<tokio::sync::oneshot::Receiver<MappingReport>>,
) -> Option<MappingReport> {
    match rx {
        Some(rx) => rx.await.ok(),
        None => std::future::pending().await,
    }
}

fn store_port_mapping(core: &Arc<Core>, mapping: PortMapping) -> MappingReport {
    let report = mapping.report().clone();
    core.emit(SessionEvent::PortMapping {
        report: report.clone(),
    });
    *lock(&core.port_mapping) = Some(mapping);
    report
}

enum Parts {
    Host {
        capturer: Box<dyn Capturer>,
        encoders: EncoderSet,
        video: LoopbackConfig,
        sink: Box<dyn InputSink>,
        audio: Option<HostAudio>,
    },
    Guest {
        decoders: DecoderSet,
        sink: Box<dyn FrameSink>,
        code: String,
        frames: Option<u64>,
        audio: Option<GuestAudio>,
    },
}

/// Lo que arranca cada papel.
pub(crate) struct Started {
    pub(crate) main: tokio::task::JoinHandle<()>,
    pub(crate) threads: Vec<(&'static str, JoinHandle<()>)>,
}

/// Espera de [`RemoteSession::move_to_room`]: hasta que todos los invitados salieron de la sala
/// (cada uno sale al recibir `MoveToRoom`, así que entonces ya lo tienen todos).
#[derive(Debug)]
pub struct MoveWaiter(channel::Receiver<()>);

impl MoveWaiter {
    /// Espera como mucho `limit` (para quien no conteste: un enlace muerto). Devuelve `true` si
    /// ya no queda nadie o la sala terminó, y `false` si venció el plazo con alguien dentro.
    pub fn wait(&self, limit: Duration) -> bool {
        match self.0.recv_timeout(limit) {
            Ok(()) | Err(channel::RecvTimeoutError::Disconnected) => true,
            Err(channel::RecvTimeoutError::Timeout) => false,
        }
    }
}

/// Mando del anfitrión para cambiar lo que se transmite con la sala abierta
/// ([`RemoteSession::host_control`]); se puede clonar y usar desde cualquier hilo.
#[derive(Clone)]
pub struct HostControl {
    core: Arc<Core>,
    commands: mpsc::UnboundedSender<Command>,
}

impl fmt::Debug for HostControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostControl").finish_non_exhaustive()
    }
}

impl HostControl {
    /// Anfitrión: cambia lo que se comparte con la sala abierta. Arranca `capturer` (en Linux
    /// puede abrir el diálogo del portal, así que bloquea: llámala fuera del hilo de la ventana)
    /// y el hilo `capture` la adopta en el siguiente fotograma; los invitados ven la última imagen
    /// hasta el intra de la nueva. `eis` es la conexión EIS de su sesión del portal (Linux, la de
    /// [`coroom_capture::share_capturer`]): el teclado y el ratón pasan a ella. Si no arranca, la
    /// captura de antes sigue y se devuelve el error.
    pub fn switch_capture(
        &self,
        mut capturer: Box<dyn Capturer>,
        eis: Option<EisProvider>,
    ) -> Result<(), SessionError> {
        let setup = lock(&self.core.capture_setup).clone();
        let Some((gpu, options)) = setup else {
            return Err(SessionError::Network("la sesión no está en marcha".into()));
        };
        // Si la sala ya pasó la captura a memoria de CPU (el codificador no leía sus imágenes de
        // GPU), la nueva arranca así.
        let gpu = self.core.shared.latest_capture_gpu_frames().unwrap_or(gpu);
        if self.core.ended() {
            return Err(SessionError::Network("la sesión terminó".into()));
        }
        let capture =
            |err: coroom_capture::CaptureError| SessionError::Media(format!("captura: {err}"));
        let target = capturer
            .targets()
            .map_err(capture)?
            .into_iter()
            .next()
            .ok_or_else(|| SessionError::Media("captura: no hay nada que capturar".into()))?;
        capturer.set_gpu_frames(gpu);
        capturer.set_options(options);
        capturer.start(&target).map_err(capture)?;
        if self.core.ended() {
            capturer.stop();
            return Err(SessionError::Network("la sesión terminó".into()));
        }
        self.core.shared.next_capturer.put(capturer);
        if let (Some(eis), Some(input)) = (eis, lock(&self.core.host_input).as_ref()) {
            input.job(input::Job::Eis(eis));
        }
        Ok(())
    }

    /// Anfitrión: cambia qué aplicaciones suenan en la sala, sin cortar el audio. Hace falta
    /// que la sala se abriera con audio por aplicación (`GameCapture::Apps`).
    pub fn set_app_audio(&self, filter: AppFilter) -> Result<(), SessionError> {
        if !self.core.game_audio.load(Ordering::Acquire) {
            return Err(SessionError::Media(
                "la sala se abrió sin audio del juego".into(),
            ));
        }
        *lock(&self.core.game_filter) = Some(filter);
        Ok(())
    }

    /// Anfitrión: cambia el códec o el backend preferidos, el bitrate máximo, la resolución
    /// máxima o los fps con la sala abierta ([`VideoChange`]).
    pub fn set_video(&self, change: VideoChange) -> Result<(), SessionError> {
        if change.encoders.as_ref().is_some_and(EncoderSet::is_empty) {
            return Err(SessionError::Media(
                "no hay ningún codificador disponible con las opciones dadas".into(),
            ));
        }
        // Se valida lo pedido (así el error dice «1×1», no el «0×0» ya redondeado) y se manda
        // redondeado a par.
        change.validate().map_err(SessionError::Media)?;
        self.commands
            .send(Command::Video(change.normalized()))
            .map_err(|_| SessionError::Network("la sesión terminó".into()))
    }
}

/// Sesión remota de anfitrión o de invitado.
pub struct RemoteSession {
    core: Arc<Core>,
    config: Option<RemoteConfig>,
    parts: Option<Parts>,
    runtime: Option<tokio::runtime::Runtime>,
    main: Option<tokio::task::JoinHandle<()>>,
    threads: Vec<(&'static str, JoinHandle<()>)>,
    commands: Option<mpsc::UnboundedSender<Command>>,
    summary: Option<LatencySummary>,
}

impl fmt::Debug for RemoteSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteSession")
            .field("role", &self.core.role)
            .field("state", &self.core.state())
            .field("epoch", &self.epoch())
            .finish_non_exhaustive()
    }
}

impl RemoteSession {
    /// Anfitrión: comparte lo que entregue `capturer`, codificado con el mejor de `encoders` que
    /// decodifiquen los invitados, según `video` (`video.bitrate_kbps` es el máximo;
    /// `video.frames`, si se da, termina la sesión tras enviar ese número de fotogramas). Para
    /// forzar un códec, `encoders.only_codec(…)`. No arranca nada hasta [`Session::start`].
    ///
    /// La entrada de los invitados va al sumidero de la plataforma (`platform_sink`, perezoso:
    /// no abre el portal ni crea mandos hasta que algún invitado tiene permiso).
    pub fn host(
        identity: Identity,
        config: RemoteConfig,
        capturer: Box<dyn Capturer>,
        encoders: EncoderSet,
        video: LoopbackConfig,
        ledger: Arc<LatencyLedger>,
    ) -> Self {
        Self::host_with_sink(
            identity,
            config,
            capturer,
            encoders,
            video,
            ledger,
            platform_sink(SinkOptions::default()),
        )
    }

    /// Como [`RemoteSession::host`], con la entrada de los invitados inyectada en `sink` (el de la
    /// plataforma con la sesión del portal compartida, o un `RecordingSink` en los tests).
    pub fn host_with_sink(
        identity: Identity,
        config: RemoteConfig,
        capturer: Box<dyn Capturer>,
        encoders: EncoderSet,
        video: LoopbackConfig,
        ledger: Arc<LatencyLedger>,
        sink: Box<dyn InputSink>,
    ) -> Self {
        // Windows: el ratón absoluto del invitado (relativo al fotograma) va a la zona que captura
        // WGC (monitor o ventana, donde esté ahora).
        #[cfg(windows)]
        coroom_input::inject::windows::set_pointer_area_source(|| {
            coroom_capture::wgc::captured_area().map(|area| {
                coroom_input::inject::windows::PointerArea {
                    left: area.left,
                    top: area.top,
                    width: area.width,
                    height: area.height,
                }
            })
        });
        let sframe = config.sframe();
        Self::new(
            Core::new(Role::Host, identity, ledger, sframe),
            config,
            Parts::Host {
                capturer,
                encoders,
                video,
                sink,
                audio: None,
            },
        )
    }

    /// Invitado: entra en la sala `code`, anuncia que decodifica lo de `decoders` y presenta en
    /// `sink` (`frames`, si se da, termina la sesión tras presentar ese número de fotogramas).
    pub fn guest(
        identity: Identity,
        config: RemoteConfig,
        code: impl Into<String>,
        decoders: DecoderSet,
        sink: Box<dyn FrameSink>,
        frames: Option<u64>,
        ledger: Arc<LatencyLedger>,
    ) -> Self {
        let sframe = config.sframe();
        Self::new(
            Core::new(Role::Guest, identity, ledger, sframe),
            config,
            Parts::Guest {
                decoders,
                sink,
                code: code.into().trim().to_ascii_uppercase(),
                frames,
                audio: None,
            },
        )
    }

    /// Anfitrión: audio del juego, voz propia y reproducción de la voz de los invitados. Antes de
    /// [`Session::start`]; en un invitado no hace nada.
    pub fn with_host_audio(mut self, host_audio: HostAudio) -> Self {
        if let Some(Parts::Host { audio, .. }) = &mut self.parts {
            *audio = Some(host_audio);
        }
        self
    }

    /// Invitado: audio del juego y voces en `speaker`, y voz propia si se le permite. Antes de
    /// [`Session::start`]; en el anfitrión no hace nada.
    pub fn with_guest_audio(mut self, guest_audio: GuestAudio) -> Self {
        if let Some(Parts::Guest { audio, .. }) = &mut self.parts {
            *audio = Some(guest_audio);
        }
        self
    }

    /// Invitado: decodificar solo por software (o volver a todos los decodificadores), antes de
    /// arrancar o en marcha: el decodificador se vuelve a abrir enseguida, sin reconectar.
    pub fn set_software_decoder(&self, on: bool) {
        self.core.software_decoder.store(on, Ordering::Release);
        if let Some(decode) = lock(&self.core.decode).as_ref() {
            decode.set_software_only(on);
        }
    }

    /// Mandos y estado del audio (silenciar, pulsar para hablar, quién habla).
    pub fn audio(&self) -> AudioControl {
        self.core.audio.clone()
    }

    fn new(core: Core, config: RemoteConfig, parts: Parts) -> Self {
        Self {
            core: Arc::new(core),
            config: Some(config),
            parts: Some(parts),
            runtime: None,
            main: None,
            threads: Vec::new(),
            commands: None,
            summary: None,
        }
    }

    /// Eventos de la sesión (un solo consumidor recibe cada uno).
    pub fn events(&self) -> channel::Receiver<SessionEvent> {
        self.core.events_rx.clone()
    }

    /// Registro de marcas de tiempo.
    pub fn ledger(&self) -> &Arc<LatencyLedger> {
        &self.core.ledger
    }

    /// Contadores acumulados del vídeo (para diagnóstico y tests).
    pub fn video_counters(&self) -> crate::stats::VideoCounters {
        self.core.shared.counters.video()
    }

    /// Huella de la identidad local.
    pub fn fingerprint(&self) -> String {
        self.core.identity.fingerprint()
    }

    /// Código de la sala (el anfitrión, en cuanto el servidor la crea; el invitado, el que usó).
    pub fn room_code(&self) -> Option<String> {
        lock(&self.core.room_code).clone()
    }

    /// Época MLS de las claves de medios en uso.
    pub fn epoch(&self) -> u64 {
        self.core.epoch.load(Ordering::Acquire)
    }

    /// Hoja MLS propia.
    pub fn own_leaf(&self) -> u32 {
        self.core.own_leaf.load(Ordering::Acquire)
    }

    /// Anillo de claves de medios (compartido: lo que se haga con él afecta a la sesión).
    pub fn key_ring(&self) -> MediaKeyRing {
        self.core.ring.clone()
    }

    /// Si los medios van cifrados con SFrame.
    pub fn sframe(&self) -> bool {
        self.core.sframe
    }

    /// Anfitrión: invitados en la sala.
    pub fn guests(&self) -> Vec<GuestStatus> {
        lock(&self.core.guests).values().cloned().collect()
    }

    /// Invitado: estado del enlace con el anfitrión.
    pub fn link(&self) -> Option<LinkSummary> {
        self.core.link_summary()
    }

    /// Invitado: su entrada (para la ventana o el guion). En el anfitrión no hace nada.
    pub fn input(&self) -> GuestInput {
        self.core.guest_input.clone()
    }

    /// Comprobación ([`input_key_check`]) del secreto pareja a pareja con `peer` (el invitado para
    /// el anfitrión; `0`, el anfitrión, para el invitado).
    pub fn input_key_check(&self, peer: u32) -> Option<String> {
        lock(&self.core.input_keys).get(&peer).map(input_key_check)
    }

    /// Resumen de latencia ahora (con el enlace, en el invitado); tras [`Session::close`], el final.
    pub fn summary(&self) -> LatencySummary {
        if let Some(summary) = &self.summary {
            return summary.clone();
        }
        let mut summary = self.core.ledger.summary();
        summary.link = self.core.link_summary();
        summary
    }

    /// Indica si la sesión terminó (por `frames`, por error, por expulsión o por cierre).
    pub fn is_finished(&self) -> bool {
        self.core.shared.is_finished()
    }

    /// Espera hasta que la sesión termine o pase `timeout`. Devuelve si terminó.
    pub fn wait(&self, timeout: Duration) -> bool {
        self.core.shared.wait(timeout)
    }

    /// Por qué terminó la sesión, si fue por un error, una expulsión o el cierre de la sala.
    pub fn end_reason(&self) -> Option<SessionError> {
        self.core.end_reason().or_else(|| {
            self.core
                .shared
                .failure()
                .map(|err| SessionError::Media(err.to_string()))
        })
    }

    /// Anfitrión: invita al amigo `user` (su id en el servidor) a la sala; el servidor comprueba
    /// que sois amigos y que está conectado (si no, llega un aviso).
    pub fn invite(&self, user: u64) -> Result<(), SessionError> {
        self.require_host()?;
        self.commands
            .as_ref()
            .and_then(|commands| commands.send(Command::Invite(user)).ok())
            .ok_or(SessionError::Network("la sesión no está en marcha".into()))
    }

    /// Anfitrión: a un amigo `user` que pidió entrar ([`SessionEvent::JoinRequested`]) le dice
    /// que ahora no; para dejarle entrar basta con [`Session::invite`].
    pub fn decline_join(&self, user: u64) -> Result<(), SessionError> {
        self.require_host()?;
        self.commands
            .as_ref()
            .and_then(|commands| commands.send(Command::DeclineJoin(user)).ok())
            .ok_or(SessionError::Network("la sesión no está en marcha".into()))
    }

    /// Anfitrión con la sala en marcha: mando para cambiar lo que se transmite desde otro hilo
    /// sin tener la sesión ([`HostControl`]).
    pub fn host_control(&self) -> Result<HostControl, SessionError> {
        self.require_host()?;
        let commands = self
            .commands
            .clone()
            .ok_or(SessionError::Network("la sesión no está en marcha".into()))?;
        Ok(HostControl {
            core: Arc::clone(&self.core),
            commands,
        })
    }

    /// [`HostControl::switch_capture`].
    pub fn switch_capture(
        &self,
        capturer: Box<dyn Capturer>,
        eis: Option<EisProvider>,
    ) -> Result<(), SessionError> {
        self.host_control()?.switch_capture(capturer, eis)
    }

    /// [`HostControl::set_app_audio`].
    pub fn set_app_audio(&self, filter: AppFilter) -> Result<(), SessionError> {
        self.host_control()?.set_app_audio(filter)
    }

    /// [`HostControl::set_video`].
    pub fn set_video(&self, change: VideoChange) -> Result<(), SessionError> {
        self.host_control()?.set_video(change)
    }

    /// Invitado: deja la conexión actual y vuelve a entrar en la sala enseguida (el mismo camino
    /// que la reconexión automática: [`SessionEvent::Reconnecting`] y, al conectar,
    /// [`SessionEvent::Reconnected`]). Los hilos de decodificación y presentación siguen.
    pub fn reconnect(&self) -> Result<(), SessionError> {
        if self.core.role != Role::Guest {
            return Err(SessionError::GuestOnly);
        }
        self.commands
            .as_ref()
            .and_then(|commands| commands.send(Command::Reconnect).ok())
            .ok_or(SessionError::Network("la sesión no está en marcha".into()))
    }

    /// Envía `text` al chat de la sala (anfitrión e invitado). Antes de enviarlo comprueba el
    /// texto ([`chat::validate_text`]: ni vacío ni más de 2 000 caracteres, sin caracteres de
    /// control), el límite de ritmo ([`CHAT_RATE_MESSAGES`] cada [`CHAT_RATE_WINDOW`]) y que haya
    /// control y claves; si no, [`SessionError::Chat`] con el motivo en español. El mensaje
    /// aparece como [`SessionEvent::ChatMessage`] con `own` cuando el anfitrión lo reparte (en el
    /// anfitrión, enseguida).
    pub fn send_chat(&self, text: &str) -> Result<(), SessionError> {
        let text = chat::validate_text(text).map_err(SessionError::Chat)?;
        if self.core.ended() {
            return Err(SessionError::Chat("La sesión ha terminado.".to_owned()));
        }
        let not_ready = || {
            SessionError::Chat("El chat aún no está disponible: la sala se está abriendo.".into())
        };
        match self.core.role {
            Role::Host => {
                let commands = self.commands.as_ref().ok_or_else(not_ready)?;
                {
                    let mut chat = lock(&self.core.chat);
                    if chat.epoch().is_none() {
                        return Err(not_ready());
                    }
                    chat.own_rate(Instant::now(), true)
                        .map_err(SessionError::Chat)?;
                }
                commands
                    .send(Command::Chat(text))
                    .map_err(|_| SessionError::Chat("La sesión ha terminado.".to_owned()))
            }
            Role::Guest => {
                let control = lock(&self.core.link).control.clone().ok_or_else(|| {
                    SessionError::Chat(
                        "El chat aún no está disponible: esperando la conexión con el anfitrión."
                            .to_owned(),
                    )
                })?;
                let own_leaf = self.core.own_leaf.load(Ordering::Acquire);
                let message = lock(&self.core.chat)
                    .seal_own(own_leaf, &text, Instant::now())
                    .map_err(SessionError::Chat)?;
                control.send(&message).map_err(|err| {
                    SessionError::Chat(format!("No se pudo enviar el mensaje: {err}."))
                })
            }
        }
    }

    /// Anfitrión: ofrece a `guest` pasarle el anfitrión; su respuesta llega como
    /// [`SessionEvent::HandoffReply`].
    pub fn offer_handoff(&self, guest: GuestId) -> Result<(), SessionError> {
        self.require_host()?;
        self.commands
            .as_ref()
            .and_then(|commands| commands.send(Command::OfferHandoff(guest)).ok())
            .ok_or(SessionError::Network("la sesión no está en marcha".into()))
    }

    /// Anfitrión: retira la oferta de [`RemoteSession::offer_handoff`] (`HandoffCancel`): si
    /// `guest` ya abrió su sala, la cierra y sigue aquí.
    pub fn cancel_handoff(&self, guest: GuestId) -> Result<(), SessionError> {
        self.require_host()?;
        self.commands
            .as_ref()
            .and_then(|commands| commands.send(Command::CancelHandoff(guest)).ok())
            .ok_or(SessionError::Network("la sesión no está en marcha".into()))
    }

    /// Anfitrión: manda a todos los invitados a la sala `code` de `host_name` (al anfitrión
    /// nuevo también: es su confirmación) y a los que entren desde ahora en cuanto conecten. La
    /// sala se cierra después (lo hace quien llama) cuando [`MoveWaiter::wait`] diga que ya no
    /// queda nadie: cada invitado sale al recibir el aviso, y así ninguno se queda sin él.
    pub fn move_to_room(&self, code: &str, host_name: &str) -> Result<MoveWaiter, SessionError> {
        self.require_host()?;
        if !coroom_transport::proto::signal::is_valid_room_code(code) {
            return Err(SessionError::Network(format!(
                "código de sala no válido: {code}"
            )));
        }
        let (done, waiter) = channel::bounded(1);
        self.commands
            .as_ref()
            .and_then(|commands| {
                commands
                    .send(Command::MoveToRoom {
                        code: code.to_owned(),
                        host_name: host::truncate_name(host_name),
                        done,
                    })
                    .ok()
            })
            .ok_or(SessionError::Network("la sesión no está en marcha".into()))?;
        Ok(MoveWaiter(waiter))
    }

    /// Invitado: contesta a [`SessionEvent::HandoffOffered`] con el código de la sala que ya
    /// abrió (o `None` si no acepta). La respuesta se encola esperando sitio, sin perderse si la
    /// cola de control va llena; con un código, quien llama no sale de esta sala hasta que el
    /// anfitrión conteste (`MoveToRoom` con ese código, o el cierre de la sala), para no cortar
    /// la conexión antes de que salga.
    pub fn reply_handoff(&self, code: Option<String>) -> Result<(), SessionError> {
        if self.core.role != Role::Guest {
            return Err(SessionError::Network("solo un invitado contesta".into()));
        }
        let control = lock(&self.core.link)
            .control
            .clone()
            .ok_or_else(|| SessionError::Network("sin conexión con el anfitrión".into()))?;
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| SessionError::Network("la sesión no está en marcha".into()))?;
        let core = Arc::clone(&self.core);
        runtime.spawn(async move {
            let message = coroom_transport::proto::control::ControlMessage::HandoffReply { code };
            if let Err(err) = control.send_wait(&message).await {
                core.warn(format!("no se pudo contestar al anfitrión: {err}"));
            }
        });
        Ok(())
    }

    /// Anfitrión: vacía el chat para todos (historial incluido; [`SessionEvent::ChatCleared`]
    /// aquí y en cada invitado).
    pub fn clear_chat(&self) -> Result<(), SessionError> {
        self.require_host()?;
        self.commands
            .as_ref()
            .and_then(|commands| commands.send(Command::ChatClear).ok())
            .ok_or(SessionError::Network("la sesión no está en marcha".into()))
    }

    /// Época MLS de la clave del chat más nueva (`None` antes de tener claves).
    pub fn chat_epoch(&self) -> Option<u64> {
        lock(&self.core.chat).epoch()
    }

    /// Clave del chat de la época más nueva (para diagnóstico y tests: quien selle con ella debe
    /// usar un nonce que la sesión no use).
    pub fn chat_key(&self) -> Option<ChatKey> {
        lock(&self.core.chat).newest_key()
    }

    /// Anfitrión: vista previa de lo que se captura ([`crate::preview::PreviewTap`]); `None` la
    /// quita. Se puede poner antes o después de [`Session::start`].
    pub fn set_preview(&self, tap: Option<crate::preview::PreviewTap>) {
        *self
            .core
            .shared
            .preview
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = tap.map(Arc::new);
    }

    /// Anfitrión: la interfaz dice si la vista previa está a la vista; oculta, la toma no reduce
    /// nada ([`crate::preview::PreviewTap::set_enabled`]).
    pub fn set_preview_enabled(&self, enabled: bool) {
        if let Some(tap) = self
            .core
            .shared
            .preview
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            tap.set_enabled(enabled);
        }
    }

    fn require_host(&self) -> Result<(), SessionError> {
        if self.core.role == Role::Host {
            Ok(())
        } else {
            Err(SessionError::HostOnly)
        }
    }

    fn join_threads(&mut self) {
        self.core.shared.finish(None);
        for (name, handle) in self.threads.drain(..) {
            if handle.join().is_err() {
                self.core
                    .warn(format!("el hilo `{name}` terminó con un pánico"));
            }
        }
    }
}

impl Session for RemoteSession {
    fn role(&self) -> Role {
        self.core.role
    }

    fn state(&self) -> SessionState {
        self.core.state()
    }

    fn start(&mut self) -> Result<(), SessionError> {
        self.core.transition(SessionState::Connecting)?;
        let (Some(parts), Some(config)) = (self.parts.take(), self.config.take()) else {
            self.core.end(None);
            return Err(SessionError::Media("la sesión ya se usó".into()));
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("coroom-red")
            .enable_all()
            .build()
            .map_err(|err| {
                SessionError::Network(format!("no se pudo arrancar la tarea de red: {err}"))
            });
        let runtime = match runtime {
            Ok(runtime) => runtime,
            Err(err) => {
                self.core.end(Some(err.clone()));
                return Err(err);
            }
        };
        let (commands, command_rx) = mpsc::unbounded_channel();
        let started = match parts {
            Parts::Host {
                capturer,
                encoders,
                video,
                sink,
                audio,
            } => host::start(
                &self.core,
                config,
                capturer,
                encoders,
                video,
                sink,
                audio,
                runtime.handle(),
                command_rx,
            ),
            Parts::Guest {
                decoders,
                sink,
                code,
                frames,
                audio,
            } => {
                *lock(&self.core.room_code) = Some(code.clone());
                guest::start(
                    &self.core,
                    config,
                    code,
                    decoders,
                    sink,
                    frames,
                    audio,
                    runtime.handle(),
                    command_rx,
                )
            }
        };
        match started {
            Ok(started) => {
                self.main = Some(started.main);
                self.threads = started.threads;
                *lock(&self.core.commands) = Some(commands.clone());
                self.commands = Some(commands);
                self.runtime = Some(runtime);
                Ok(())
            }
            Err(err) => {
                self.core.end(Some(err.clone()));
                runtime.shutdown_timeout(Duration::from_millis(500));
                Err(err)
            }
        }
    }

    /// Pausa (o reanuda) la entrada de todos: en pausa se descarta todo y se suelta lo pulsado.
    fn set_paused(&mut self, paused: bool) -> Result<(), SessionError> {
        self.require_host()?;
        self.core.transition(if paused {
            SessionState::Paused
        } else {
            SessionState::Active
        })?;
        if let Some(input) = lock(&self.core.host_input).as_ref() {
            input.job(input::Job::Paused(paused));
        }
        Ok(())
    }

    /// Cambia los permisos de un invitado: se aplican antes de inyectar su entrada siguiente y se
    /// le avisan (`InputPermissions`). La ranura debe estar en `1..=COROOM_MAX_GAMEPADS` y libre.
    fn set_permissions(
        &mut self,
        guest: GuestId,
        permissions: Permissions,
    ) -> Result<(), SessionError> {
        self.require_host()?;
        let max = coroom_input::max_gamepads();
        let input_key = {
            let mut guests = lock(&self.core.guests);
            if !guests.contains_key(&guest) {
                return Err(SessionError::UnknownGuest(guest));
            }
            if let Some(slot) = permissions.gamepad_slot {
                if slot == 0 || slot > max {
                    return Err(SessionError::InvalidPermissions(format!(
                        "la ranura de mando {slot} no existe (hay de 1 a {max}; \
                         COROOM_MAX_GAMEPADS)"
                    )));
                }
                if let Some(other) = guests.values().find(|status| {
                    status.guest != guest && status.permissions.gamepad_slot == Some(slot)
                }) {
                    return Err(SessionError::InvalidPermissions(format!(
                        "la ranura de mando {slot} ya es del invitado {}",
                        other.guest.0
                    )));
                }
            }
            let Some(status) = guests.get_mut(&guest) else {
                return Err(SessionError::UnknownGuest(guest));
            };
            status.permissions = permissions;
            status.input_key
        };
        if !input_key && (permissions.keyboard_mouse || permissions.gamepad_slot.is_some()) {
            self.core.warn(format!(
                "el invitado {} aún no tiene clave de entrada pareja a pareja: su entrada se \
                 rechazará hasta que la establezca",
                guest.0
            ));
        }
        if let Some(input) = lock(&self.core.host_input).as_ref() {
            input.job(input::Job::Permissions { guest, permissions });
        }
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::PermissionsChanged(guest));
        }
        Ok(())
    }

    fn kick(&mut self, guest: GuestId) -> Result<(), SessionError> {
        self.require_host()?;
        if !lock(&self.core.guests).contains_key(&guest) {
            return Err(SessionError::UnknownGuest(guest));
        }
        self.commands
            .as_ref()
            .and_then(|commands| commands.send(Command::Kick(guest)).ok())
            .ok_or(SessionError::Network("la sesión no está en marcha".into()))
    }

    fn close(&mut self) {
        if let Some(commands) = self.commands.take() {
            let _ = commands.send(Command::Close);
        }
        if let (Some(runtime), Some(main)) = (&self.runtime, self.main.take()) {
            let _ = runtime
                .block_on(async { tokio::time::timeout(Duration::from_secs(3), main).await });
        }
        self.join_threads();
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(Duration::from_secs(1));
        }
        self.parts = None;
        if self.summary.is_none() {
            let mut summary = self.core.ledger.summary();
            summary.link = self.core.link_summary();
            self.summary = Some(summary);
        }
        self.core.end(None);
    }
}

impl Drop for RemoteSession {
    fn drop(&mut self) {
        if self.runtime.is_some() || !self.threads.is_empty() {
            self.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loopback::NullSink;
    use coroom_capture::synthetic::SyntheticCapturer;

    /// Un cambio de vídeo con tamaño impar o nulo, o fps nulos, no vale; el vacío sí.
    #[test]
    fn a_video_change_rounds_odd_sizes_down_and_rejects_empty_ones() {
        assert_eq!(VideoChange::default().validate(), Ok(()));
        let ok = VideoChange {
            max_size: Some((1280, 720)),
            fps: Some(120),
            ..VideoChange::default()
        };
        assert_eq!(ok.validate(), Ok(()));
        for (odd, rounded) in [((1281, 720), (1280, 720)), ((1199, 808), (1198, 808))] {
            let change = VideoChange {
                max_size: Some(odd),
                ..VideoChange::default()
            }
            .normalized();
            assert_eq!(change.max_size, Some(rounded));
            assert_eq!(change.validate(), Ok(()));
        }
        for size in [(0, 720), (1280, 0), (1, 1)] {
            let change = VideoChange {
                max_size: Some(size),
                ..VideoChange::default()
            };
            let err = change.validate().expect_err("tamaño inválido");
            assert!(err.contains("2×2"), "{err}");
            assert!(change.normalized().validate().is_err());
        }
        // El error nombra lo pedido, no lo redondeado.
        let err = VideoChange {
            max_size: Some((1, 1)),
            ..VideoChange::default()
        }
        .validate()
        .expect_err("1×1");
        assert!(err.contains("1×1"), "{err}");
        let change = VideoChange {
            fps: Some(0),
            ..VideoChange::default()
        };
        assert!(change.validate().is_err());
    }

    #[test]
    fn rate_limits_keep_the_usual_maxima_and_tame_unlimited() {
        // Arranque en 8 Mbit/s (o el máximo si es menor) y mínimo en la décima parte, hasta
        // 0,5 Mbit/s.
        assert_eq!((start_kbps(10_000), min_kbps(10_000)), (8_000, 500));
        assert_eq!((start_kbps(80_000), min_kbps(80_000)), (8_000, 500));
        assert_eq!((start_kbps(500), min_kbps(500)), (500, 100));
        // Sin tope arranca igual y puede bajar a lo que dé un enlace lento.
        assert_eq!(start_kbps(UNLIMITED_KBPS), 8_000);
        assert_eq!(min_kbps(UNLIMITED_KBPS), 500);
    }

    fn config() -> RemoteConfig {
        let mut config = RemoteConfig::new(
            "https://coroom.example.com",
            "change-me",
            ServerTrust::Fingerprint("00".repeat(32)),
        );
        config.port_mapping = false;
        config
    }

    #[test]
    fn the_wan_candidate_is_the_open_port_or_a_manual_forward() {
        use coroom_transport::nat::{Mapping, MappingOutcome, MappingProtocol};
        use std::net::{Ipv4Addr, SocketAddrV4};
        let report = |outcome| MappingReport {
            gateway: Some(Ipv4Addr::new(192, 168, 1, 1)),
            internal: Some(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 10), 47_000)),
            outcome,
        };
        let mut config = config();
        let mapped = report(MappingOutcome::Mapped(Mapping {
            external: SocketAddrV4::new(Ipv4Addr::new(100, 64, 3, 4), 47_001),
            protocol: MappingProtocol::Pcp,
            lifetime: Some(Duration::from_secs(7200)),
        }));
        assert_eq!(
            wan_candidate(&config, &mapped),
            Some("100.64.3.4:47001".parse().unwrap())
        );
        let external_only = report(MappingOutcome::ExternalOnly {
            external: Ipv4Addr::new(203, 0, 113, 9),
            protocol: MappingProtocol::Upnp,
            error: "no permite abrir puertos".to_owned(),
        });
        assert_eq!(
            wan_candidate(&config, &external_only),
            None,
            "sin puerto fijo no vale"
        );
        config.bind = Some("[::]:47000".parse().unwrap());
        assert_eq!(
            wan_candidate(&config, &external_only),
            Some("203.0.113.9:47000".parse().unwrap())
        );
        assert_eq!(
            wan_candidate(&config, &report(MappingOutcome::NoResponse)),
            None
        );
    }

    fn host(video: LoopbackConfig) -> RemoteSession {
        RemoteSession::host(
            Identity::generate().unwrap(),
            config(),
            Box::new(SyntheticCapturer::new(160, 90, 30)),
            EncoderSet::openh264(),
            video,
            Arc::new(LatencyLedger::new()),
        )
    }

    #[test]
    fn remote_sessions_are_sessions() {
        fn assert_send<T: Send>() {}
        assert_send::<RemoteSession>();
        let mut guest = RemoteSession::guest(
            Identity::generate().unwrap(),
            config(),
            " abc234 ",
            DecoderSet::openh264(),
            Box::new(NullSink),
            Some(10),
            Arc::new(LatencyLedger::new()),
        );
        assert_eq!(guest.role(), Role::Guest);
        assert_eq!(guest.state(), SessionState::Idle);
        assert_eq!(guest.set_paused(true), Err(SessionError::HostOnly));
        assert_eq!(guest.kick(GuestId(1)), Err(SessionError::HostOnly));
        assert_eq!(
            guest.set_permissions(GuestId(1), Permissions::VIEW_ONLY),
            Err(SessionError::HostOnly)
        );
        assert!(guest.sframe());
        assert_eq!(guest.link(), None, "sin entrar no hay enlace");

        let mut host = host(LoopbackConfig {
            width: 160,
            height: 90,
            fps: 30,
            bitrate_kbps: 300,
            frames: None,
            yuv444: false,
            hdr: false,
        });
        assert_eq!(host.role(), Role::Host);
        assert_eq!(
            host.kick(GuestId(7)),
            Err(SessionError::UnknownGuest(GuestId(7)))
        );
        assert!(host.guests().is_empty());
    }

    #[test]
    fn start_reports_media_errors_and_closes() {
        let mut session = host(LoopbackConfig {
            width: 0,
            ..LoopbackConfig::default()
        });
        assert!(matches!(session.start(), Err(SessionError::Media(_))));
        assert_eq!(session.state(), SessionState::Closed);
        assert!(session.start().is_err(), "una sesión cerrada no se reanuda");
        let events = session.events();
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, SessionEvent::Ended(Some(SessionError::Media(_))))),
            "se avisa del final"
        );
    }

    #[test]
    fn rtt_maps_to_the_quality_levels() {
        // docs/ARQUITECTURA.md §2.1 con el pipeline típico de 25 ms: ≤ 15, ≤ 45 y ≤ 85 ms de RTT.
        assert_eq!(QualityLevel::from_rtt_ms(0.0), QualityLevel::Excellent);
        assert_eq!(QualityLevel::from_rtt_ms(15.0), QualityLevel::Excellent);
        assert_eq!(QualityLevel::from_rtt_ms(15.1), QualityLevel::Good);
        assert_eq!(QualityLevel::from_rtt_ms(45.0), QualityLevel::Good);
        assert_eq!(QualityLevel::from_rtt_ms(45.1), QualityLevel::Acceptable);
        assert_eq!(QualityLevel::from_rtt_ms(85.0), QualityLevel::Acceptable);
        assert_eq!(QualityLevel::from_rtt_ms(85.1), QualityLevel::Poor);
        assert_eq!(QualityLevel::from_rtt_ms(-3.0), QualityLevel::Excellent);
        assert_eq!(
            QualityLevel::from_overhead_ms(110.0),
            QualityLevel::Acceptable
        );
        assert_eq!(QualityLevel::Good.label(), "Buena");
        assert!(QualityLevel::Good.description().contains("acción"));
    }

    #[test]
    fn reconnect_waits_grow() {
        let waits: Vec<u128> = (1..=MAX_RECONNECT_ATTEMPTS)
            .map(|attempt| reconnect_delay(attempt).as_millis())
            .collect();
        assert_eq!(waits, [500, 1000, 2000, 4000, 8000]);
        assert_eq!(reconnect_delay(0), RECONNECT_BASE_DELAY);
        assert!(reconnect_delay(u32::MAX) >= reconnect_delay(5));
    }

    #[test]
    fn hosts_cannot_reconnect() {
        let session = host(LoopbackConfig::default());
        assert_eq!(session.reconnect(), Err(SessionError::GuestOnly));
    }

    #[test]
    fn events_read_in_spanish() {
        let event = SessionEvent::GuestLeft {
            guest: GuestId(2),
            kicked: true,
        };
        assert_eq!(
            event.to_string(),
            "Invitado 2 expulsado; claves de la sala rotadas."
        );
        assert_eq!(
            SessionEvent::Ended(Some(SessionError::Removed)).to_string(),
            "Sesión terminada: el anfitrión te ha expulsado de la sala."
        );
        assert_eq!(
            SessionEvent::VideoCodec {
                codec: Codec::Av1,
                detail: "decodifica dav1d".into()
            }
            .to_string(),
            "Vídeo en AV1 (decodifica dav1d)."
        );
        let event = SessionEvent::Reconnecting {
            attempt: 2,
            of: 5,
            retry_in: Duration::from_secs(1),
            reason: "error de red: sin respuesta".into(),
        };
        assert!(event.to_string().contains("intento 2 de 5"), "{event}");
        let event = SessionEvent::QualityEstimate {
            rtt_ms: 12.0,
            level: QualityLevel::Excellent,
        };
        assert!(event.to_string().contains("Excelente"), "{event}");
        let event = SessionEvent::Presence {
            user: 7,
            online: true,
        };
        assert_eq!(event.to_string(), "El usuario 7 se ha conectado.");
        let event = SessionEvent::Presence {
            user: 7,
            online: false,
        };
        assert_eq!(event.to_string(), "El usuario 7 se ha desconectado.");
    }
}
