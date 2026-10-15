//! Medios sobre datagramas QUIC: objetos con FEC RaptorQ, pacing, NACK y fecha límite.
//!
//! **Emisor** ([`MediaSender`]): cada fotograma (ya cifrado con SFrame por la sesión) se prefija con
//! su longitud (4 bytes, para recortar el relleno cuando se reconstruye por FEC), se parte en
//! objetos de origen de como mucho `max_packet − 16` bytes y se le añaden símbolos de reparación
//! RaptorQ en la proporción `repair_ratio` (0 % sin pérdida, lineal hasta 30 % con pérdida ≥ 5 %,
//! actualizada con cada `Feedback`; 10 % hasta el primero, cuando la pérdida aún no se conoce).
//! Todo sale con **pacing** ([`pacing_duration`]): el fotograma se reparte en lo que tardaría a
//! 1,25 veces el bitrate objetivo, como mucho 0,8 intervalos (2 intervalos un intra). Antes se
//! metía en 4 ms: a 120 fps un intra de cientos de KB salía a varios cientos de Mbit/s, llenaba la
//! cola del camino y el búfer del socket del receptor (64 KB en Windows) y se perdía entero, con lo
//! que se pedía otro intra igual de grande. Guarda los objetos de origen de los últimos 16 grupos
//! para responder a los `Nack`; los reenvíos grandes salen con el mismo ritmo.
//!
//! **Receptor** ([`MediaReceiver`]): reensambla por pista y grupo, con FEC; plazo por grupo de
//! 1,5 intervalos desde su primer objeto; a mitad de plazo, si faltan objetos que la FEC no puede
//! cubrir, envía `Nack`. Si el plazo vence sin recuperar el grupo, lo descarta, anota la pérdida y
//! pide un intra (`KeyframeRequest`; todo grupo de vídeo es referencia). Entrega los grupos de cada
//! pista **en orden**.
//!
//! Desviación del diseño inicial, documentada: tras enviar un `Nack` el plazo del grupo se amplía a
//! `momento del Nack + 1,5 × RTT + 5 ms` (como mucho 4 intervalos desde el primer objeto) y, si el
//! reenvío tampoco llega en `1,25 × RTT`, se manda un segundo `Nack`; con el plazo fijo de 1,5
//! intervalos el reenvío no llegaría nunca a tiempo cuando el RTT supera medio intervalo (con
//! 60 fps, 8 ms), y la puerta de "nunca más de 100 ms sin fotograma" se sigue cumpliendo
//! (4 intervalos a 60 fps son 67 ms).
//!
//! En vídeo, el plazo máximo tras un `Nack` crece con el RTT cuando 4 intervalos no bastan para el
//! reenvío (3 RTT + 1 intervalo, 400 ms como mucho): descartar un fotograma de vídeo cuesta pedir un
//! intra, que tarda un RTT y es el fotograma más grande, así que esperar el reenvío congela menos.
//!
//! El intervalo es el de la configuración (60 fps si no se sabe más) hasta que la sesión fija el de
//! una pista con [`MediaReceiver::set_track_interval`]: el invitado lo hace con los fps de
//! `CodecSelected`. Con un anfitrión a 30 fps y el intervalo de 60, el plazo máximo de 67 ms dejaba
//! sin reenvío a todo enlace con más de ~45 ms de RTT (lo destapó la matriz de red).
//!
//! **Reloj**: el receptor envía `Ping` cada segundo; con los `Pong` estima RTT y desfase (mediana de
//! los últimos 8) y expresa la llegada de cada grupo en el reloj del emisor; el emisor resta su hora
//! de envío y obtiene las muestras de OWD para [`crate::rate::RateController`].

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use tracing::debug;

use coroom_proto::control::{ControlMessage, Feedback};
use coroom_proto::media::{ObjectHeader, Track};

use crate::TransportError;
use crate::fec::{self, GroupDecoder};
use crate::quic::{ControlSender, PeerConnection};
use crate::rate::{PROBE_START_SHARE, RateConfig, RateController, RateInput};

/// Datagrama de un objeto de audio: cabecera de medios de un solo objeto (`group` =
/// `seq`) y la carga (el paquete SFrame del emisor). Sin FEC ni historia para `Nack`: la
/// redundancia del audio es la de Opus. Se envía con [`PeerConnection::send_datagram`].
pub fn audio_datagram(track: Track, seq: u32, epoch_hint: u16, payload: &[u8]) -> Option<Bytes> {
    let len = u16::try_from(payload.len())
        .ok()
        .filter(|len| usize::from(*len) <= ObjectHeader::MAX_PAYLOAD)?;
    let header = coroom_proto::audio::object_header(track, seq, len, epoch_hint);
    Some(datagram(header, payload))
}

/// Prefijo de longitud de cada fotograma dentro de su grupo.
pub const LENGTH_PREFIX: usize = 4;

/// Grupos que el emisor guarda para responder a los `Nack`.
pub const HISTORY_GROUPS: usize = 16;

/// Periodo de los `Feedback` del receptor.
pub const FEEDBACK_INTERVAL: Duration = Duration::from_millis(50);

/// Periodo de los `Ping` del receptor.
pub const PING_INTERVAL: Duration = Duration::from_secs(1);

/// Datagramas de cada tren de la sonda del enlace ([`MediaSender::send_probe`]): 48 × 1200 B son
/// ~58 KB, bastantes para que la dispersión del tren la ponga el cuello del camino y no el jitter
/// de un par de paquetes, y pocos para que en un enlace de 10 Mbit/s no hagan más de 50 ms de cola
/// (antes de que haya vídeo).
pub const PROBE_PACKETS: u16 = 48;

/// Trenes que se envían a cada receptor al conectar, separados [`PROBE_SPACING`]; vale el mejor
/// (el tráfico ajeno solo puede hacer que un tren mida de menos).
pub const PROBE_TRAINS: u32 = 2;

/// Separación entre dos trenes de sonda.
pub const PROBE_SPACING: Duration = Duration::from_millis(100);

/// Ritmo máximo al que sale un tren: con el máximo del usuario por encima, el tren sale a este
/// ritmo y la medida es «al menos esto» (la dispersión solo mide cuellos por debajo del ritmo de
/// envío).
pub const PROBE_MAX_PACE_KBPS: u32 = 200_000;

/// Plazo del receptor para dar un tren por terminado desde su primer datagrama (lo que no llegó,
/// se perdió).
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Trenes de sonda abiertos a la vez como mucho en el receptor.
const MAX_OPEN_PROBES: usize = 8;

/// Resultado de un tren de sonda ([`ControlMessage::ProbeResult`] ya interpretado).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeOutcome {
    /// Identificador del tren.
    pub train: u32,
    /// Datagramas enviados.
    pub sent: u16,
    /// Datagramas recibidos.
    pub received: u16,
    /// Bytes recibidos.
    pub bytes: u32,
    /// Entre el primero y el último recibidos.
    pub span: Duration,
}

impl ProbeOutcome {
    /// Lo que el camino entregó, en kbit/s: los bytes de todos menos el primero (que marca el
    /// instante inicial) sobre el tiempo entre el primero y el último. `None` con menos de dos
    /// datagramas o sin separación medible.
    pub fn kbps(&self) -> Option<u32> {
        if self.received < 2 || self.span.is_zero() {
            return None;
        }
        let bytes = f64::from(self.bytes) * f64::from(self.received - 1) / f64::from(self.received);
        Some((bytes * 8.0 / 1000.0 / self.span.as_secs_f64()).round() as u32)
    }

    /// Datagramas del tren que no llegaron.
    pub fn lost(&self) -> u16 {
        self.sent.saturating_sub(self.received)
    }
}

/// Ritmo del pacing sobre el bitrate objetivo: algo por encima, para que un fotograma algo mayor
/// que la media no se retrase más de lo que tarda en salir.
pub const PACING_GAIN: f64 = 1.25;

/// Parte del intervalo que puede ocupar el pacing de un fotograma normal: lo que sobra deja al hilo
/// `send` ponerse al día si un fotograma llegó tarde.
pub const FRAME_PACING_SHARE: f64 = 0.8;

/// Intervalos que puede ocupar el pacing de un intra (como mucho dos fotogramas esperan detrás, lo
/// que cabe en el canal del codificador).
pub const KEYFRAME_PACING_INTERVALS: u32 = 2;

/// Objetos de un `Nack` que se reenvían de golpe; los demás, con el ritmo del pacing.
pub const NACK_BURST: usize = 8;

/// Tiempo en que se reparten los `bytes` de un fotograma: lo que tardan a [`PACING_GAIN`] veces
/// `target_kbps`, como mucho [`FRAME_PACING_SHARE`] de `interval` (o [`KEYFRAME_PACING_INTERVALS`]
/// intervalos si es un intra).
pub fn pacing_duration(
    bytes: usize,
    target_kbps: u32,
    interval: Duration,
    keyframe: bool,
) -> Duration {
    let rate_bps = PACING_GAIN * f64::from(target_kbps.max(1)) * 1000.0;
    let spread = Duration::from_secs_f64(bytes as f64 * 8.0 / rate_bps);
    let cap = if keyframe {
        interval * KEYFRAME_PACING_INTERVALS
    } else {
        interval.mul_f64(FRAME_PACING_SHARE)
    };
    spread.min(cap)
}

/// Envía `packets` por `conn` repartidos en `pacing` (en ráfagas de 0,5 ms: los temporizadores no
/// dan más resolución). Devuelve los que salieron, o el primer error que no sea de tamaño.
async fn send_paced(
    conn: &PeerConnection,
    packets: Vec<Bytes>,
    pacing: Duration,
    group: u32,
) -> Result<usize, TransportError> {
    let n = packets.len() as u32;
    let start = tokio::time::Instant::now();
    let mut sent = 0;
    for (i, packet) in packets.into_iter().enumerate() {
        let due = start + pacing * i as u32 / n.max(1);
        if due > tokio::time::Instant::now() + Duration::from_micros(500) {
            tokio::time::sleep_until(due).await;
        }
        match conn.send_datagram(packet) {
            Ok(()) => sent += 1,
            // El MTU del camino bajó a mitad del fotograma (detección de agujero negro): ese
            // objeto se pierde y los siguientes fotogramas ya usan el tamaño nuevo.
            Err(TransportError::Datagram(quinn::SendDatagramError::TooLarge)) => {
                debug!(group, "datagrama mayor que el MTU actual; se descarta");
            }
            Err(err) => return Err(err),
        }
    }
    Ok(sent)
}

/// Espera en la cola de datagramas de QUIC (al bitrate objetivo) a partir de la que el enlace se
/// da por atascado (un corte de wifi, una VPN que se renegocia): lo que se pusiera detrás saldría
/// tarde, cuando el invitado ya no lo quiere, y retrasaría el intra que lo arregla.
pub const STALE_QUEUE: Duration = Duration::from_millis(150);

/// Proporción de reparación hasta el primer `Feedback` (la pérdida aún no se conoce).
pub const INITIAL_REPAIR_RATIO: f64 = 0.10;

/// Proporción máxima de reparación y pérdida a partir de la que se aplica.
pub const MAX_REPAIR_RATIO: f64 = 0.30;
const FULL_REPAIR_LOSS: f64 = 0.05;

/// Espera máxima de un grupo de vídeo con `Nack` cuando el RTT no cabe en 4 intervalos.
pub const MAX_VIDEO_NACK_WAIT: Duration = Duration::from_millis(400);

/// Plazo máximo de un grupo con `Nack` desde su primer objeto: 4 intervalos y, en vídeo, lo que
/// tarde el reenvío si es más (3 RTT + 1 intervalo, hasta [`MAX_VIDEO_NACK_WAIT`]: da para el
/// segundo `Nack`, que sale 1,25 RTT después del primero, y su reenvío con jitter). Todo fotograma
/// de vídeo es referencia: si se descarta, los siguientes no se decodifican hasta el intra que se
/// pide, que tarda también un RTT y además es el fotograma más grande. Esperar al reenvío siempre
/// congela menos. El audio no espera: el PLC oculta la trama.
fn nack_wait(track: Track, interval: Duration, rtt: Duration) -> Duration {
    let base = interval.mul_f64(MAX_DEADLINE_INTERVALS);
    if track == Track::Video {
        base.max((rtt * 3 + interval).min(MAX_VIDEO_NACK_WAIT))
    } else {
        base
    }
}

/// Intervalo de pista más corto que se acepta del emisor (240 fps).
pub const MIN_TRACK_INTERVAL: Duration = Duration::from_micros(4_167);

/// Intervalo de pista más largo que se acepta del emisor (15 fps): con 4 intervalos de plazo
/// máximo, ningún grupo espera más de 267 ms.
pub const MAX_TRACK_INTERVAL: Duration = Duration::from_micros(66_667);

/// Plazo de un grupo en intervalos de fotograma, y máximo tras un `Nack`.
const DEADLINE_INTERVALS: f64 = 1.5;
const MAX_DEADLINE_INTERVALS: f64 = 4.0;

/// `Nack` por grupo como mucho (el segundo, si el reenvío también se perdió).
const MAX_NACKS: u8 = 2;

/// Muestras del reloj para las medianas.
const CLOCK_SAMPLES: usize = 8;

/// Grupos por delante del esperado a partir de los que se reinicia la pista.
const RESET_DISTANCE: u32 = 1024;

/// Grupos vacíos que se crean como mucho para detectar grupos perdidos enteros.
const MAX_PLACEHOLDERS: u32 = 8;

/// Grupos abiertos (sin resolver) por pista como mucho. Con el plazo máximo (4 intervalos, o
/// [`MAX_VIDEO_NACK_WAIT`] en vídeo con mucho RTT) y los 8 grupos vacíos de relleno, un emisor
/// legítimo no pasa de ~56 a 120 fps (los 120 fps del protocolo 7); por encima, los
/// datagramas de grupos nuevos se descartan como no válidos (`docs/seguridad.md`, hallazgo M-1:
/// sin esta cota, un par o un relay malicioso abría 1024 grupos de 65 535 objetos con un
/// datagrama cada uno y reservaba ~1,5 GB).
pub const MAX_OPEN_GROUPS: usize = 64;

/// Objetos de origen pendientes como mucho entre todos los grupos abiertos del receptor (cada uno
/// reserva su hueco al conocerse el grupo). 131 072 objetos de 1 184 B son ~150 MB de datos en
/// vuelo, más de 20 fotogramas clave de 4K a 60 Mbit/s; el hueco vacío cuesta 24 B.
pub const MAX_PENDING_OBJECTS: usize = 1 << 17;

/// Proporción de reparación para una pérdida medida.
pub fn repair_ratio_for_loss(loss: f64) -> f64 {
    MAX_REPAIR_RATIO * (loss / FULL_REPAIR_LOSS).clamp(0.0, 1.0)
}

/// Reloj local en µs y estimación del reloj del otro extremo por `Ping`/`Pong`.
#[derive(Debug)]
pub struct Clock {
    origin: Instant,
    samples: VecDeque<(Duration, i64)>,
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock {
    /// Reloj con origen ahora.
    pub fn new() -> Self {
        Self::with_origin(Instant::now())
    }

    /// Reloj con origen en `origin` (varios emisores comparten así el mismo reloj).
    pub fn with_origin(origin: Instant) -> Self {
        Self {
            origin,
            samples: VecDeque::with_capacity(CLOCK_SAMPLES),
        }
    }

    /// Origen del reloj (su instante cero).
    pub fn origin(&self) -> Instant {
        self.origin
    }

    /// µs de `at` en este reloj.
    pub fn at_us(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.origin).as_micros() as u64
    }

    /// µs de ahora en este reloj.
    pub fn now_us(&self) -> u64 {
        self.at_us(Instant::now())
    }

    /// `Ping` con la hora actual.
    pub fn ping(&self) -> ControlMessage {
        ControlMessage::Ping { t: self.now_us() }
    }

    /// `Pong` para un `Ping` recibido ahora.
    pub fn pong(&self, t: u64) -> ControlMessage {
        ControlMessage::Pong {
            t,
            t_recv: self.now_us(),
        }
    }

    /// Procesa un `Pong` recibido en `now`.
    pub fn on_pong(&mut self, now: Instant, t: u64, t_recv: u64) {
        let now_us = self.at_us(now);
        if t > now_us {
            return;
        }
        let rtt = now_us - t;
        let offset = t_recv as i64 - (t + rtt / 2) as i64;
        if self.samples.len() == CLOCK_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back((Duration::from_micros(rtt), offset));
    }

    /// RTT medido por `Ping`/`Pong` (mediana de las últimas 8 muestras).
    pub fn rtt(&self) -> Option<Duration> {
        let mut rtts: Vec<Duration> = self.samples.iter().map(|(rtt, _)| *rtt).collect();
        rtts.sort_unstable();
        rtts.get(rtts.len() / 2).copied()
    }

    /// Desfase `reloj remoto − reloj local` en µs (mediana de las últimas 8 muestras).
    pub fn offset_us(&self) -> Option<i64> {
        let mut offsets: Vec<i64> = self.samples.iter().map(|(_, offset)| *offset).collect();
        offsets.sort_unstable();
        offsets.get(offsets.len() / 2).copied()
    }

    /// `at` expresado en el reloj remoto y truncado a 32 bits, si ya hay desfase.
    pub fn remote_us_i32(&self, at: Instant) -> Option<i32> {
        let offset = self.offset_us()?;
        Some((self.at_us(at) as i64).wrapping_add(offset) as u32 as i32)
    }
}

/// Configuración del emisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaSenderConfig {
    /// Intervalo entre fotogramas (1/fps).
    pub frame_interval: Duration,
    /// Límites del control de tasa.
    pub rate: RateConfig,
}

impl Default for MediaSenderConfig {
    fn default() -> Self {
        Self {
            frame_interval: Duration::from_micros(16_667),
            rate: RateConfig::default(),
        }
    }
}

/// Resumen del envío de un fotograma.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendReport {
    /// Objetos de origen.
    pub sources: usize,
    /// Objetos de reparación.
    pub repairs: usize,
    /// Bytes enviados (cabeceras incluidas).
    pub bytes: usize,
}

#[derive(Debug)]
struct SentGroup {
    track: Track,
    group: u32,
    sources: Vec<Bytes>,
    sent_us: u64,
    /// Último `Nack` atendido: cuándo y qué pedía. El mismo `Nack` llega dos veces (por el stream
    /// y en un datagrama de control) y la copia no se reenvía.
    last_nack: Option<(Instant, Vec<u16>)>,
}

/// Estado del control de tasa de un receptor que informa a este emisor (uno en directo; uno por
/// invitado cuando el emisor va al relay y el relay abre en abanico).
#[derive(Debug, Clone)]
struct FeedbackSource {
    id: u64,
    rate: RateController,
    repair_ratio: f64,
    last_lost: u32,
    last_sources: u64,
    /// Bytes enviados hasta el grupo que cubría el último informe.
    last_sent_bytes: u64,
    /// `received_bytes` del último informe.
    last_received: u64,
    last_ecn: u64,
    last_feedback_ecn: u32,
    /// Mejor tren de sonda que informó este receptor.
    probe: Option<ProbeOutcome>,
}

impl FeedbackSource {
    fn new(id: u64, config: RateConfig) -> Self {
        Self {
            id,
            rate: RateController::new(config),
            repair_ratio: INITIAL_REPAIR_RATIO,
            last_lost: 0,
            last_sources: 0,
            last_sent_bytes: 0,
            last_received: 0,
            last_ecn: 0,
            last_feedback_ecn: 0,
            probe: None,
        }
    }
}

#[derive(Debug)]
struct SenderState {
    history: VecDeque<SentGroup>,
    repair_ratio: f64,
    /// Receptores que informan, por identificador (ver [`MediaSender::on_feedback_from`]).
    sources: Vec<FeedbackSource>,
    epoch_hint: u16,
    clock: Clock,
    sources_sent: u64,
    bytes_sent: u64,
    /// Por grupo enviado: objetos de origen y bytes (cabeceras y reparación) acumulados hasta él.
    cumulative: VecDeque<(u32, u64, u64)>,
}

impl SenderState {
    /// El receptor con el objetivo más bajo, que es el que manda.
    fn limiting(&self) -> Option<&FeedbackSource> {
        self.sources
            .iter()
            .min_by_key(|source| source.rate.target_kbps())
    }
}

/// Identificador del receptor de [`MediaSender::on_feedback`] (el único en una conexión directa).
pub const DEFAULT_SOURCE: u64 = 0;

/// Emisor de medios hacia una conexión (un par o el relay).
///
/// Los métodos toman `&self`: se puede compartir con `Arc` entre la tarea que envía fotogramas y la
/// que lee el stream de control.
///
/// Hacia el relay, un solo emisor sirve a varios invitados: cada uno informa con su propio
/// `Feedback` ([`MediaSender::on_feedback_from`] con un identificador por invitado) y el emisor
/// lleva un control de tasa por receptor; [`MediaSender::target_kbps`] es el **mínimo** de todos
/// (un solo flujo para todos) y la FEC se dimensiona para el receptor con más pérdida.
#[derive(Debug)]
pub struct MediaSender {
    conn: PeerConnection,
    config: MediaSenderConfig,
    /// Máximo de [`MediaSender::set_max_kbps`] (0: el de `config`).
    max_kbps: AtomicU32,
    /// Identificador del siguiente tren de sonda.
    next_train: AtomicU32,
    state: Mutex<SenderState>,
}

impl MediaSender {
    /// Emisor sobre `conn`.
    pub fn new(conn: PeerConnection, config: MediaSenderConfig) -> Self {
        Self::with_clock_origin(conn, config, Instant::now())
    }

    /// Emisor sobre `conn` con el origen de reloj `origin`: el anfitrión da el mismo a todos sus
    /// emisores, así las marcas de su reloj (las del audio) valen para cualquier invitado.
    pub fn with_clock_origin(
        conn: PeerConnection,
        config: MediaSenderConfig,
        origin: Instant,
    ) -> Self {
        let rate = RateController::new(config.rate);
        conn.set_target_kbps(rate.target_kbps());
        Self {
            conn,
            config,
            max_kbps: AtomicU32::new(0),
            next_train: AtomicU32::new(1),
            state: Mutex::new(SenderState {
                history: VecDeque::with_capacity(HISTORY_GROUPS + 1),
                repair_ratio: INITIAL_REPAIR_RATIO,
                sources: Vec::new(),
                epoch_hint: 0,
                clock: Clock::with_origin(origin),
                sources_sent: 0,
                bytes_sent: 0,
                cumulative: VecDeque::new(),
            }),
        }
    }

    /// `at` en µs del reloj del emisor: el que responde a los `Ping` del receptor y el de las
    /// muestras de OWD. Sirve para que el receptor sitúe en su reloj las marcas del emisor.
    pub fn clock_us(&self, at: Instant) -> u64 {
        self.state.lock().map_or(0, |state| state.clock.at_us(at))
    }

    /// Conexión de este emisor.
    pub fn connection(&self) -> &PeerConnection {
        &self.conn
    }

    /// Cambia el bitrate máximo en marcha, para todos los receptores (los que informen después
    /// también lo usan).
    pub fn set_max_kbps(&self, max_kbps: u32) {
        self.max_kbps.store(max_kbps.max(1), Ordering::Release);
        if let Ok(mut state) = self.state.lock() {
            for source in &mut state.sources {
                source.rate.set_max_kbps(max_kbps);
            }
        }
    }

    /// Límites del control de tasa, con el máximo de [`MediaSender::set_max_kbps`] si lo hay.
    fn rate_config(&self) -> RateConfig {
        let mut rate = RateController::new(self.config.rate);
        let max = self.max_kbps.load(Ordering::Acquire);
        if max > 0 {
            rate.set_max_kbps(max);
        }
        rate.config()
    }

    /// Fija los 16 bits bajos de la época SFrame que llevan las cabeceras.
    pub fn set_epoch_hint(&self, epoch_hint: u16) {
        if let Ok(mut state) = self.state.lock() {
            state.epoch_hint = epoch_hint;
        }
    }

    /// Fija la proporción de reparación (normalmente la calcula el `Feedback`).
    pub fn set_repair_ratio(&self, ratio: f64) {
        if let Ok(mut state) = self.state.lock() {
            state.repair_ratio = ratio.clamp(0.0, 1.0);
        }
    }

    /// Proporción de reparación actual.
    pub fn repair_ratio(&self) -> f64 {
        self.state.lock().map_or(0.0, |state| state.repair_ratio)
    }

    /// Lo que espera en la cola de datagramas por enviar de QUIC, en tiempo al bitrate objetivo.
    pub fn send_queue_delay(&self) -> Duration {
        let queued = self.conn.datagram_queued_bytes() as f64;
        let rate = f64::from(self.target_kbps().max(1)) * 1000.0 / 8.0;
        Duration::from_secs_f64(queued / rate)
    }

    /// Si el enlace está atascado y lo que se envíe ahora llegaría tarde: la cola de QUIC lleva
    /// más de [`STALE_QUEUE`] (nada sale) o el último `Feedback` del receptor que limita midió más
    /// de [`STALE_QUEUE`] de cola en el camino (sale, pero se acumula por delante).
    pub fn stalled(&self) -> bool {
        self.send_queue_delay() > STALE_QUEUE
            || self.state.lock().is_ok_and(|state| {
                state
                    .limiting()
                    .is_some_and(|source| source.rate.queue_delay_now() > STALE_QUEUE)
            })
    }

    /// Bitrate para el codificador: el objetivo ([`MediaSender::target_kbps`]) menos la parte que
    /// se lleva la reparación, para que vídeo más FEC sumen el objetivo y no lo pasen (con un 30 %
    /// de FEC el camino recibía un 30 % más de lo que el control de tasa había decidido).
    pub fn encoder_target_kbps(&self) -> u32 {
        let target = f64::from(self.target_kbps());
        let ratio = self.repair_ratio();
        ((target / (1.0 + ratio)).round() as u32).max(1)
    }

    /// Envía un tren de sonda ([`PROBE_PACKETS`] datagramas de relleno de la pista
    /// [`Track::Probe`]) a `pace_kbps` (acotado a [`PROBE_MAX_PACE_KBPS`]) y devuelve su
    /// identificador. El receptor contesta con `ProbeResult`, que atiende
    /// [`MediaSender::handle_control`]. No cuenta como medios: ni en el historial de `Nack` ni en
    /// lo enviado para el control de tasa.
    pub async fn send_probe(&self, pace_kbps: u32) -> Result<u32, TransportError> {
        let train = self.next_train.fetch_add(1, Ordering::Relaxed);
        let max_packet = self.conn.max_packet();
        if max_packet < ObjectHeader::LEN + 64 {
            return Err(TransportError::Protocol(format!(
                "el camino solo admite datagramas de {max_packet} bytes"
            )));
        }
        let payload = vec![0u8; max_packet - ObjectHeader::LEN];
        let packets: Vec<Bytes> = (0..PROBE_PACKETS)
            .map(|index| {
                let flags = if index + 1 == PROBE_PACKETS {
                    ObjectHeader::LAST_OBJECT
                } else {
                    0
                };
                datagram(
                    ObjectHeader {
                        track: Track::Probe,
                        flags,
                        group: train,
                        object: index,
                        object_count: PROBE_PACKETS,
                        payload_len: payload.len() as u16,
                        epoch_hint: 0,
                    },
                    &payload,
                )
            })
            .collect();
        let bytes: usize = packets.iter().map(Bytes::len).sum();
        let pace = f64::from(pace_kbps.clamp(1, PROBE_MAX_PACE_KBPS)) * 1000.0;
        let pacing = Duration::from_secs_f64(bytes as f64 * 8.0 / pace);
        send_paced(&self.conn, packets, pacing, train).await?;
        Ok(train)
    }

    /// Anota el resultado de un tren de sonda del receptor `source` y, si es el mejor que ha
    /// informado, arranca su control de tasa en [`PROBE_START_SHARE`] de lo medido (solo mientras
    /// no haya llegado ningún `Feedback`: después manda la red en marcha). Devuelve lo medido en
    /// kbit/s.
    pub fn on_probe_result(&self, source: u64, outcome: ProbeOutcome) -> Option<u32> {
        let kbps = outcome.kbps()?;
        let mut state = self.state.lock().ok()?;
        let index = match state.sources.iter().position(|s| s.id == source) {
            Some(index) => index,
            None => {
                state
                    .sources
                    .push(FeedbackSource::new(source, self.rate_config()));
                state.sources.len() - 1
            }
        };
        let entry = &mut state.sources[index];
        let best = entry.probe.and_then(|probe| probe.kbps()).unwrap_or(0);
        if kbps > best {
            entry.probe = Some(outcome);
            if entry.last_sources == 0 {
                let start = (f64::from(kbps) * PROBE_START_SHARE).round() as u32;
                entry.rate.restart(start);
            }
        }
        let target = state
            .limiting()
            .map_or(self.rate_config().start_kbps, |s| s.rate.target_kbps());
        self.conn.set_target_kbps(target);
        Some(kbps)
    }

    /// Mejor tren de sonda que informó el receptor `source`.
    pub fn probe(&self, source: u64) -> Option<ProbeOutcome> {
        let state = self.state.lock().ok()?;
        state.sources.iter().find(|s| s.id == source)?.probe
    }

    /// Bitrate objetivo para el codificador: el mínimo de los receptores que informan (el inicial
    /// mientras no informa ninguno).
    pub fn target_kbps(&self) -> u32 {
        let start = || RateController::new(self.rate_config()).target_kbps();
        self.state.lock().map_or_else(
            |_| start(),
            |state| {
                state
                    .limiting()
                    .map_or_else(start, |source| source.rate.target_kbps())
            },
        )
    }

    /// Copia del control de tasa del receptor que limita (estimación, cola y pérdida).
    pub fn rate(&self) -> Option<RateController> {
        let state = self.state.lock().ok()?;
        Some(state.limiting().map_or_else(
            || RateController::new(self.rate_config()),
            |s| s.rate.clone(),
        ))
    }

    /// Objetivo del receptor `source`, si ya informó.
    pub fn source_target_kbps(&self, source: u64) -> Option<u32> {
        let state = self.state.lock().ok()?;
        state
            .sources
            .iter()
            .find(|s| s.id == source)
            .map(|s| s.rate.target_kbps())
    }

    /// Olvida el control de tasa del receptor `source` (el invitado salió).
    pub fn forget_source(&self, source: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.sources.retain(|s| s.id != source);
            let target = state
                .limiting()
                .map_or(self.rate_config().start_kbps, |s| s.rate.target_kbps());
            state.repair_ratio = if state.sources.is_empty() {
                INITIAL_REPAIR_RATIO
            } else {
                state
                    .sources
                    .iter()
                    .map(|s| s.repair_ratio)
                    .fold(0.0, f64::max)
            };
            self.conn.set_target_kbps(target);
        }
    }

    /// Parte `payload` en objetos con FEC y los envía con pacing.
    pub async fn send_frame(
        &self,
        track: Track,
        group: u32,
        keyframe: bool,
        payload: &[u8],
    ) -> Result<SendReport, TransportError> {
        let max_packet = self.conn.max_packet();
        if max_packet < ObjectHeader::LEN + 64 {
            return Err(TransportError::Protocol(format!(
                "el camino solo admite datagramas de {max_packet} bytes"
            )));
        }
        let symbol_size = max_packet - ObjectHeader::LEN;
        let total = LENGTH_PREFIX + payload.len();
        let count = total.div_ceil(symbol_size);
        let object_count =
            u16::try_from(count).map_err(|_| TransportError::FrameTooLarge(payload.len()))?;
        let length = u32::try_from(payload.len())
            .map_err(|_| TransportError::FrameTooLarge(payload.len()))?;
        let mut data = Vec::with_capacity(count * symbol_size);
        data.extend_from_slice(&length.to_be_bytes());
        data.extend_from_slice(payload);

        let (ratio, epoch_hint) = self
            .state
            .lock()
            .map(|state| (state.repair_ratio, state.epoch_hint))
            .unwrap_or((0.0, 0));
        let flags = if keyframe { ObjectHeader::KEYFRAME } else { 0 };

        let mut sources = Vec::with_capacity(count);
        for index in 0..object_count {
            let start = usize::from(index) * symbol_size;
            let chunk = &data[start..(start + symbol_size).min(total)];
            let mut header_flags = flags;
            if index + 1 == object_count {
                header_flags |= ObjectHeader::LAST_OBJECT;
            }
            sources.push(datagram(
                ObjectHeader {
                    track,
                    flags: header_flags,
                    group,
                    object: index,
                    object_count,
                    // Cabe: `chunk.len() <= symbol_size <= MAX_PAYLOAD`.
                    payload_len: chunk.len() as u16,
                    epoch_hint,
                },
                chunk,
            ));
        }

        let repair_count = if ratio > 0.0 {
            ((count as f64 * ratio).ceil() as usize)
                .min(count)
                .min(usize::from(u16::MAX))
        } else {
            0
        };
        let mut repairs = Vec::with_capacity(repair_count);
        if repair_count > 0 {
            data.resize(count * symbol_size, 0);
            if let Some(symbols) = fec::encode_repairs(&data, symbol_size, repair_count as u32) {
                for (index, symbol) in symbols.iter().enumerate() {
                    repairs.push(datagram(
                        ObjectHeader {
                            track,
                            flags: flags | ObjectHeader::REPAIR,
                            group,
                            object: index as u16,
                            object_count,
                            payload_len: symbol.len() as u16,
                            epoch_hint,
                        },
                        symbol,
                    ));
                }
            }
        }

        let bytes: usize = sources.iter().chain(&repairs).map(Bytes::len).sum();
        if let Ok(mut state) = self.state.lock() {
            let sent_us = state.clock.now_us();
            state.sources_sent += count as u64;
            state.bytes_sent += bytes as u64;
            let (cumulative, cumulative_bytes) = (state.sources_sent, state.bytes_sent);
            state
                .cumulative
                .push_back((group, cumulative, cumulative_bytes));
            while state.cumulative.len() > 512 {
                state.cumulative.pop_front();
            }
            state.history.push_back(SentGroup {
                track,
                group,
                sources: sources.clone(),
                sent_us,
                last_nack: None,
            });
            while state.history.len() > HISTORY_GROUPS {
                state.history.pop_front();
            }
        }

        let report = SendReport {
            sources: sources.len(),
            repairs: repairs.len(),
            bytes,
        };
        let pacing = pacing_duration(
            report.bytes,
            self.target_kbps(),
            self.config.frame_interval,
            keyframe,
        );
        let all: Vec<Bytes> = sources.into_iter().chain(repairs).collect();
        send_paced(&self.conn, all, pacing, group).await?;
        Ok(report)
    }

    /// Reenvía los objetos de origen que pide un `Nack` (todos si `missing` está vacío). Devuelve
    /// cuántos salen (los de un reenvío grande, en una tarea con pacing).
    ///
    /// El mismo `Nack` repetido antes de medio RTT es la copia del datagrama de control (o la del
    /// stream) y no reenvía nada: el receptor no pide otra vez hasta pasado 1,25 RTT.
    pub fn on_nack(&self, track: Track, group: u32, missing: &[u16]) -> usize {
        let now = Instant::now();
        let window = self.conn.rtt() / 2;
        let packets: Vec<Bytes> = match self.state.lock() {
            Ok(mut state) => match state
                .history
                .iter_mut()
                .find(|sent| sent.track == track && sent.group == group)
            {
                Some(sent)
                    if sent.last_nack.as_ref().is_some_and(|(at, previous)| {
                        previous == missing && now.saturating_duration_since(*at) < window
                    }) =>
                {
                    Vec::new()
                }
                Some(sent) => {
                    sent.last_nack = Some((now, missing.to_vec()));
                    if missing.is_empty() {
                        sent.sources.clone()
                    } else {
                        missing
                            .iter()
                            .filter_map(|index| sent.sources.get(usize::from(*index)).cloned())
                            .collect()
                    }
                }
                None => Vec::new(),
            },
            Err(_) => Vec::new(),
        };
        // Un reenvío grande (un intra entero) de golpe es la misma ráfaga que se evita al enviar:
        // pasado [`NACK_BURST`], sale con el ritmo del pacing en una tarea aparte.
        let runtime = tokio::runtime::Handle::try_current();
        if packets.len() > NACK_BURST
            && let Ok(runtime) = runtime
        {
            let bytes = packets.iter().map(Bytes::len).sum();
            let pacing =
                pacing_duration(bytes, self.target_kbps(), self.config.frame_interval, true);
            let conn = self.conn.clone();
            let count = packets.len();
            runtime.spawn(async move {
                if let Err(err) = send_paced(&conn, packets, pacing, group).await {
                    debug!(error = %err, group, "no se pudo reenviar lo que pidió un Nack");
                }
            });
            return count;
        }
        let mut sent = 0;
        for packet in packets {
            if self.conn.send_datagram(packet).is_ok() {
                sent += 1;
            }
        }
        sent
    }

    /// Procesa un `Feedback` del único receptor ([`DEFAULT_SOURCE`]): actualiza el control de
    /// tasa, la ventana de QUIC y la FEC. Devuelve el bitrate objetivo.
    pub fn on_feedback(&self, feedback: &Feedback) -> u32 {
        self.on_feedback_from(DEFAULT_SOURCE, feedback)
    }

    /// Procesa un `Feedback` del receptor `source` (cada invitado detrás del relay tiene el suyo).
    /// Devuelve el bitrate objetivo del emisor: el mínimo de todos los receptores.
    pub fn on_feedback_from(&self, source: u64, feedback: &Feedback) -> u32 {
        let now = Instant::now();
        let ecn_events = self.conn.congestion().ecn_events();
        let Ok(mut state) = self.state.lock() else {
            return self.rate_config().start_kbps;
        };
        let state = &mut *state;
        let owd: Vec<i64> = feedback
            .owd_samples
            .iter()
            .filter_map(|(group, arrival)| {
                let sent = state.history.iter().find(|sent| sent.group == *group)?;
                Some(i64::from(
                    (*arrival as u32).wrapping_sub(sent.sent_us as u32) as i32,
                ))
            })
            .collect();
        let covered = state
            .cumulative
            .iter()
            .rev()
            .find(|(group, _, _)| *group <= feedback.highest_group)
            .map(|(_, objects, bytes)| (*objects, *bytes));

        let index = match state.sources.iter().position(|s| s.id == source) {
            Some(index) => index,
            None => {
                state
                    .sources
                    .push(FeedbackSource::new(source, self.rate_config()));
                state.sources.len() - 1
            }
        };
        let entry = &mut state.sources[index];
        let (sources, sent_bytes_total) =
            covered.unwrap_or((entry.last_sources, entry.last_sent_bytes));
        let lost = feedback.lost_objects.saturating_sub(entry.last_lost);
        let sent = sources.saturating_sub(entry.last_sources);
        let sent_bytes = sent_bytes_total.saturating_sub(entry.last_sent_bytes);
        let received_bytes = feedback.received_bytes.saturating_sub(entry.last_received);
        let loss_fraction = if sent > 0 {
            (f64::from(lost) / sent as f64).min(1.0)
        } else {
            0.0
        };
        if sent > 0 {
            entry.last_sources = sources;
            entry.last_lost = feedback.lost_objects;
            entry.last_sent_bytes = sent_bytes_total;
        }
        entry.last_received = feedback.received_bytes;
        let ecn_ce = (ecn_events.saturating_sub(entry.last_ecn)
            + u64::from(feedback.ecn_ce.saturating_sub(entry.last_feedback_ecn)))
        .min(u64::from(u32::MAX)) as u32;
        entry.last_ecn = ecn_events;
        entry.last_feedback_ecn = feedback.ecn_ce;

        entry.rate.update(
            now,
            RateInput {
                owd_us: &owd,
                loss_fraction,
                sent_objects: sent,
                ecn_ce,
                received_bytes,
                sent_bytes,
            },
        );
        entry.repair_ratio = repair_ratio_for_loss(entry.rate.loss().max(loss_fraction * 0.5));
        state.repair_ratio = state
            .sources
            .iter()
            .map(|s| s.repair_ratio)
            .fold(0.0, f64::max);
        let target = state
            .limiting()
            .map_or(self.rate_config().start_kbps, |s| s.rate.target_kbps());
        self.conn.set_target_kbps(target);
        target
    }

    /// Atiende los mensajes de control que le tocan al emisor (`Feedback`, `Nack`, `Ping`) y
    /// devuelve los demás (`KeyframeRequest`, `Bye`…) al llamante.
    pub fn handle_control(
        &self,
        message: ControlMessage,
        reply: &ControlSender,
    ) -> Option<ControlMessage> {
        self.handle_control_from(DEFAULT_SOURCE, message, reply)
    }

    /// Como [`MediaSender::handle_control`], con los `Feedback` atribuidos al receptor `source`.
    pub fn handle_control_from(
        &self,
        source: u64,
        message: ControlMessage,
        reply: &ControlSender,
    ) -> Option<ControlMessage> {
        match message {
            ControlMessage::Feedback(feedback) => {
                self.on_feedback_from(source, &feedback);
                None
            }
            ControlMessage::Nack {
                track,
                group,
                missing,
            } => {
                self.on_nack(track, group, &missing);
                None
            }
            ControlMessage::Ping { t } => {
                let pong = self
                    .state
                    .lock()
                    .map(|state| state.clock.pong(t))
                    .unwrap_or(ControlMessage::Pong { t, t_recv: 0 });
                if let Err(err) = reply.send(&pong) {
                    debug!(error = %err, "no se pudo responder al Ping");
                }
                None
            }
            ControlMessage::Pong { .. } => None,
            ControlMessage::ProbeResult {
                train,
                sent,
                received,
                bytes,
                span_us,
            } => {
                self.on_probe_result(
                    source,
                    ProbeOutcome {
                        train,
                        sent,
                        received,
                        bytes,
                        span: Duration::from_micros(u64::from(span_us)),
                    },
                );
                // Se devuelve para que la sesión lo anote (diagnóstico).
                Some(ControlMessage::ProbeResult {
                    train,
                    sent,
                    received,
                    bytes,
                    span_us,
                })
            }
            other => Some(other),
        }
    }
}

fn datagram(header: ObjectHeader, payload: &[u8]) -> Bytes {
    let mut out = Vec::with_capacity(ObjectHeader::LEN + payload.len());
    out.extend_from_slice(&header.to_bytes());
    out.extend_from_slice(payload);
    Bytes::from(out)
}

/// Fotograma reensamblado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteFrame {
    /// Pista.
    pub track: Track,
    /// Grupo.
    pub group: u32,
    /// Si el grupo es intra.
    pub keyframe: bool,
    /// Carga útil tal como la pasó el emisor a `send_frame`.
    pub payload: Vec<u8>,
    /// Momento en que se completó.
    pub received_at: Instant,
}

/// Configuración del receptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaReceiverConfig {
    /// Intervalo entre fotogramas (1/fps) que fija los plazos.
    pub frame_interval: Duration,
}

impl Default for MediaReceiverConfig {
    fn default() -> Self {
        Self {
            frame_interval: Duration::from_micros(16_667),
        }
    }
}

/// Contadores del receptor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReceiverStats {
    /// Grupos entregados.
    pub delivered: u64,
    /// Grupos descartados por vencer su plazo.
    pub dropped: u64,
    /// Grupos completados gracias a la FEC.
    pub recovered_fec: u64,
    /// Grupos completados gracias a un `Nack`.
    pub recovered_nack: u64,
    /// `Nack` enviados.
    pub nacks: u64,
    /// `KeyframeRequest` enviados.
    pub keyframe_requests: u64,
    /// Bytes de datagramas válidos recibidos.
    pub received_bytes: u64,
    /// Datagramas válidos recibidos (objetos de origen y de reparación).
    pub received_objects: u64,
    /// Objetos de origen que no llegaron por la red.
    pub lost_objects: u64,
    /// Datagramas no válidos o incoherentes.
    pub invalid: u64,
    /// Objetos llegados tarde (grupo ya entregado o descartado).
    pub late: u64,
}

#[derive(Debug)]
struct GroupState {
    keyframe: bool,
    count: Option<u16>,
    symbol_size: Option<usize>,
    sources: Vec<Option<Vec<u8>>>,
    have: usize,
    direct: usize,
    repairs: Vec<(u16, Vec<u8>)>,
    decoder: Option<GroupDecoder>,
    first: Instant,
    deadline: Instant,
    nack_at: Instant,
    nacked: bool,
    /// Hasta cuándo lo que llega tras el primer `Nack` no puede ser su reenvío (medio RTT): un
    /// objeto que llega antes es el original retrasado por el jitter y cuenta como llegado por la
    /// red, no como recuperado (con 60 ± 20 ms de jitter, la pérdida medida triplicaba la
    /// real y el control de tasa se hundía).
    nack_grace: Option<Instant>,
    nacks: u8,
    /// Llegada del último objeto (de origen o reparación) y espaciado medio entre llegadas: el
    /// pacing reparte el grupo y el silencio que lo da por terminado depende de ese ritmo.
    last_arrival: Option<Instant>,
    spacing: Option<Duration>,
    /// Hasta cuándo lo que falta puede seguir en camino (último objeto más un silencio).
    quiet_at: Option<Instant>,
    /// Mayor índice de origen recibido y desde cuándo falta alguno por debajo: ese hueco es
    /// pérdida en cuanto pasa la ventana de reordenación (como RACK en TCP).
    highest: Option<u16>,
    hole_at: Option<Instant>,
    done: Option<(Vec<u8>, Instant)>,
    expired: bool,
}

impl GroupState {
    fn new(now: Instant, interval: Duration) -> Self {
        Self {
            keyframe: false,
            count: None,
            symbol_size: None,
            sources: Vec::new(),
            have: 0,
            direct: 0,
            repairs: Vec::new(),
            decoder: None,
            first: now,
            deadline: now + interval.mul_f64(DEADLINE_INTERVALS),
            nack_at: now + interval.mul_f64(DEADLINE_INTERVALS / 2.0),
            nacked: false,
            nack_grace: None,
            nacks: 0,
            last_arrival: None,
            spacing: None,
            quiet_at: None,
            highest: None,
            hole_at: None,
            done: None,
            expired: false,
        }
    }

    /// Si un objeto que llega en `now` cuenta como llegado por la red (no por un `Nack`).
    fn arrives_directly(&self, now: Instant) -> bool {
        !self.nacked || self.nack_grace.is_some_and(|grace| now < grace)
    }

    fn missing(&self) -> Vec<u16> {
        self.sources
            .iter()
            .enumerate()
            .filter(|(_, source)| source.is_none())
            .map(|(index, _)| index as u16)
            .collect()
    }

    /// Lo que falta por debajo del mayor índice recibido (lo de más arriba puede seguir en camino).
    fn missing_below_highest(&self) -> Vec<u16> {
        let below = self.highest.map_or(0, usize::from);
        self.sources[..below.min(self.sources.len())]
            .iter()
            .enumerate()
            .filter(|(_, source)| source.is_none())
            .map(|(index, _)| index as u16)
            .collect()
    }

    /// Que el grupo no venza antes de su primer `Nack` (un grupo que llega repartido hasta casi
    /// el final de su plazo no tendría tiempo de pedir lo que le falta), sin pasar del máximo.
    fn keep_until_nack(&mut self, interval: Duration) {
        if !self.nacked {
            let cap = self.first + interval.mul_f64(MAX_DEADLINE_INTERVALS);
            self.deadline = self.deadline.max((self.nack_at + interval / 4).min(cap));
        }
    }

    /// Anota una llegada en `now` y recalcula cuándo pedir lo que falta: tras un silencio de
    /// unos tres espaciados (la cola del grupo) o, si hay un hueco por debajo de lo ya recibido,
    /// tras la ventana de reordenación (dos espaciados).
    fn note_arrival(&mut self, now: Instant, interval: Duration) {
        if let Some(last) = self.last_arrival {
            let gap = now.saturating_duration_since(last);
            self.spacing = Some(match self.spacing {
                Some(spacing) => (spacing * 3 + gap) / 4,
                None => gap,
            });
        }
        self.last_arrival = Some(now);
        let holes = self.highest.is_some_and(|highest| {
            self.sources[..usize::from(highest)]
                .iter()
                .any(Option::is_none)
        });
        if holes {
            self.hole_at.get_or_insert(now);
        } else {
            self.hole_at = None;
        }
        if self.nacked {
            return;
        }
        let spacing = self.spacing.unwrap_or(interval / 8);
        let quiet = (spacing * 3).clamp(Duration::from_millis(3), interval / 2);
        self.quiet_at = Some(now + quiet);
        self.nack_at = self.nack_at.max(now + quiet);
        if let Some(hole) = self.hole_at {
            let reorder = (spacing * 2).clamp(Duration::from_millis(2), interval / 4);
            self.nack_at = self.nack_at.min(hole + reorder);
        }
    }
}

/// Grupo ya resuelto cuyos objetos de origen aún pueden llegar tarde (por reordenación): la
/// pérdida se cuenta al olvidarlo, no al resolverlo, para no contar como perdido lo que solo se
/// adelantó la FEC.
#[derive(Debug)]
struct Recent {
    track: Track,
    group: u32,
    received: Vec<bool>,
    direct: usize,
    nacked: bool,
    nack_grace: Option<Instant>,
}

/// Grupos resueltos que se recuerdan para contar las llegadas tardías (8 fotogramas: más que
/// cualquier reordenación con un jitter de decenas de milisegundos).
const RECENT_GROUPS: usize = 8;

/// Lógica del receptor sin IO (reensamblado, FEC, plazos, NACK e informes).
#[derive(Debug)]
pub struct ReceiverCore {
    interval: Duration,
    /// Intervalo propio de una pista cuando el emisor lo anuncia (el vídeo, con `CodecSelected`).
    track_intervals: HashMap<Track, Duration>,
    groups: BTreeMap<(Track, u32), GroupState>,
    recent: VecDeque<Recent>,
    next: HashMap<Track, u32>,
    ready: VecDeque<CompleteFrame>,
    outbox: Vec<ControlMessage>,
    stats: ReceiverStats,
    highest_group: u32,
    owd_pending: Vec<(u32, i32)>,
    /// Último `KeyframeRequest` de vídeo, mientras no llegue un intra (ver [`keyframe_hold`]).
    last_keyframe_request: Option<Instant>,
    /// Trenes de sonda en curso, por identificador.
    probes: BTreeMap<u32, ProbeState>,
    /// Último tren de sonda informado.
    last_probe: Option<ProbeOutcome>,
}

/// Tren de sonda del que ya llegó algo.
#[derive(Debug)]
struct ProbeState {
    count: u16,
    received: u16,
    bytes: u32,
    first: Instant,
    last: Instant,
}

/// Tiempo tras un `KeyframeRequest` en que no se pide otro por perder un fotograma normal: lo que
/// tarda en llegar el intra pedido (ida y vuelta, su pacing de 2 intervalos y margen). Cuando se
/// perdían varios grupos seguidos (una ráfaga que no cabe en el búfer del socket), cada uno pedía su
/// intra y el anfitrión enviaba uno detrás de otro, cada uno con su propia ráfaga. Si lo que se
/// pierde es un intra, se pide otro siempre.
fn keyframe_hold(interval: Duration, rtt: Duration) -> Duration {
    (rtt * 2 + interval * 4).max(Duration::from_millis(50))
}

impl ReceiverCore {
    /// Fija el intervalo de fotograma de `track` (los plazos de sus grupos nuevos y el máximo tras
    /// un `Nack` son múltiplos de él). Se acota a [`MIN_TRACK_INTERVAL`, `MAX_TRACK_INTERVAL`]:
    /// el valor viene del otro extremo.
    pub fn set_track_interval(&mut self, track: Track, interval: Duration) {
        let interval = interval.clamp(MIN_TRACK_INTERVAL, MAX_TRACK_INTERVAL);
        self.track_intervals.insert(track, interval);
    }

    /// Intervalo de fotograma de `track`.
    pub fn track_interval(&self, track: Track) -> Duration {
        self.track_intervals
            .get(&track)
            .copied()
            .unwrap_or(self.interval)
    }

    /// Receptor con plazos para `frame_interval`.
    pub fn new(frame_interval: Duration) -> Self {
        Self {
            interval: frame_interval,
            track_intervals: HashMap::new(),
            groups: BTreeMap::new(),
            recent: VecDeque::new(),
            next: HashMap::new(),
            ready: VecDeque::new(),
            outbox: Vec::new(),
            stats: ReceiverStats::default(),
            highest_group: 0,
            owd_pending: Vec::new(),
            last_keyframe_request: None,
            probes: BTreeMap::new(),
            last_probe: None,
        }
    }

    /// Contadores.
    pub fn stats(&self) -> ReceiverStats {
        self.stats
    }

    /// Último tren de sonda que se informó al emisor.
    pub fn last_probe(&self) -> Option<ProbeOutcome> {
        self.last_probe
    }

    /// Datagrama de un tren de sonda: cuenta y, con el tren completo, lo informa.
    fn probe_datagram(&mut self, header: &ObjectHeader, len: usize, now: Instant) {
        if header.object_count == 0 || header.object >= header.object_count {
            self.stats.invalid += 1;
            return;
        }
        let count = header.object_count;
        if !self.probes.contains_key(&header.group) && self.probes.len() >= MAX_OPEN_PROBES {
            self.stats.invalid += 1;
            return;
        }
        let state = self.probes.entry(header.group).or_insert(ProbeState {
            count,
            received: 0,
            bytes: 0,
            first: now,
            last: now,
        });
        if state.count != count {
            self.stats.invalid += 1;
            return;
        }
        state.received = state.received.saturating_add(1);
        state.bytes = state.bytes.saturating_add(len as u32);
        state.last = now;
        if state.received >= count {
            self.report_probe(header.group);
        }
    }

    /// Informa el tren `train` al emisor y lo olvida.
    fn report_probe(&mut self, train: u32) {
        let Some(state) = self.probes.remove(&train) else {
            return;
        };
        let outcome = ProbeOutcome {
            train,
            sent: state.count,
            received: state.received,
            bytes: state.bytes,
            span: state.last.saturating_duration_since(state.first),
        };
        self.outbox.push(ControlMessage::ProbeResult {
            train,
            sent: outcome.sent,
            received: outcome.received,
            bytes: outcome.bytes,
            span_us: u32::try_from(outcome.span.as_micros()).unwrap_or(u32::MAX),
        });
        self.last_probe = Some(outcome);
    }

    /// Siguiente fotograma listo, en orden por pista.
    pub fn pop_frame(&mut self) -> Option<CompleteFrame> {
        self.ready.pop_front()
    }

    /// Mensajes de control pendientes (`Nack`, `KeyframeRequest`).
    pub fn take_messages(&mut self) -> Vec<ControlMessage> {
        std::mem::take(&mut self.outbox)
    }

    /// Próximo instante en que vence un `Nack`, un plazo o un tren de sonda.
    pub fn next_timer(&self) -> Option<Instant> {
        self.groups
            .values()
            .filter(|group| group.done.is_none() && !group.expired)
            .map(|group| {
                if group.nacks >= MAX_NACKS {
                    group.deadline
                } else {
                    group.nack_at.min(group.deadline)
                }
            })
            .chain(
                self.probes
                    .values()
                    .map(|probe| probe.first + PROBE_TIMEOUT),
            )
            .min()
    }

    /// Informe para el emisor; vacía las muestras de llegada.
    pub fn feedback(&mut self) -> Feedback {
        Feedback {
            highest_group: self.highest_group,
            received_bytes: self.stats.received_bytes,
            lost_objects: self.stats.lost_objects.min(u64::from(u32::MAX)) as u32,
            owd_samples: std::mem::take(&mut self.owd_pending),
            // `quinn` 0.11 no expone al receptor las marcas CE (ver `crate::rate`).
            ecn_ce: 0,
        }
    }

    /// Procesa un datagrama recibido en `now`; `arrival_remote` es `now` en el reloj del emisor.
    pub fn push(&mut self, datagram: &[u8], now: Instant, arrival_remote: Option<i32>) {
        let Ok((header, payload)) = ObjectHeader::parse_datagram(datagram) else {
            self.stats.invalid += 1;
            return;
        };
        if header.track == Track::Probe {
            // Relleno de la sonda: no es un medio ni cuenta en lo recibido.
            self.probe_datagram(&header, datagram.len(), now);
            return;
        }
        self.stats.received_bytes += datagram.len() as u64;
        self.stats.received_objects += 1;
        let track = header.track;
        let group = header.group;
        let next = *self.next.entry(track).or_insert(group);
        if group < next {
            self.note_late(&header, now);
            return;
        }
        if group - next > RESET_DISTANCE {
            self.groups.retain(|(t, _), _| *t != track);
            self.next.insert(track, group);
        }
        let next = self.next.get(&track).copied().unwrap_or(group);
        // Cotas de memoria frente a un emisor malicioso: grupos abiertos por pista y huecos de
        // objetos reservados en total.
        if !self.groups.contains_key(&(track, group)) && self.open_groups(track) >= MAX_OPEN_GROUPS
        {
            // Lleno: antes de rechazar, que avance lo que se pueda (un hueco viejo atascaba la
            // pista hasta el reinicio de RESET_DISTANCE grupos, 17 s a 60 fps).
            self.deliver(track, now);
        }
        let known = self.groups.get(&(track, group));
        if known.is_none() && self.open_groups(track) >= MAX_OPEN_GROUPS {
            self.stats.invalid += 1;
            return;
        }
        if known.is_none_or(|state| state.count.is_none())
            && self.pending_objects() + usize::from(header.object_count) > MAX_PENDING_OBJECTS
        {
            self.stats.invalid += 1;
            return;
        }
        self.highest_group = self.highest_group.max(group);

        // Grupos anteriores sin ningún objeto: se crean vacíos para pedirlos enteros.
        let interval = self.track_interval(track);
        let first_missing = group.saturating_sub(MAX_PLACEHOLDERS).max(next);
        for missing in first_missing..group {
            if self.open_groups(track) + 1 >= MAX_OPEN_GROUPS {
                break;
            }
            self.groups
                .entry((track, missing))
                .or_insert_with(|| GroupState::new(now, interval));
        }

        let state = self
            .groups
            .entry((track, group))
            .or_insert_with(|| GroupState::new(now, interval));
        if state.done.is_some() || state.expired {
            self.stats.late += 1;
            let index = usize::from(header.object);
            if !header.is_repair()
                && state.count == Some(header.object_count)
                && state.sources.get(index).is_some_and(Option::is_none)
            {
                state.sources[index] = Some(Vec::new());
                if state.arrives_directly(now) {
                    state.direct += 1;
                }
            }
            return;
        }
        if state.count.is_none() {
            // Primer objeto real del grupo (el plazo cuenta desde que se supo de él).
            state.count = Some(header.object_count);
            state.sources = vec![None; usize::from(header.object_count)];
            state.keyframe = header.is_keyframe();
            if state.keyframe {
                // Un intra sale repartido en hasta KEYFRAME_PACING_INTERVALS intervalos: su plazo
                // los incluye.
                let pacing = interval * KEYFRAME_PACING_INTERVALS;
                state.deadline = state.deadline.max(state.first + pacing + interval / 2);
            }
            // Los reenvíos tras un Nack no son muestras de OWD válidas.
            if let Some(arrival) = arrival_remote.filter(|_| !state.nacked)
                && self.owd_pending.len() < 64
            {
                self.owd_pending.push((group, arrival));
            }
        }
        if state.count != Some(header.object_count) || state.keyframe != header.is_keyframe() {
            self.stats.invalid += 1;
            return;
        }
        let count = header.object_count;
        let len = payload.len();
        let is_last_source = !header.is_repair() && header.object + 1 == count;
        if header.is_repair() || !is_last_source {
            match state.symbol_size {
                Some(size) if size != len => {
                    self.stats.invalid += 1;
                    return;
                }
                Some(_) => {}
                None => state.symbol_size = Some(len),
            }
        }

        // Mientras el grupo sigue llegando, lo que falta no está perdido: el `Nack` espera a un
        // silencio y el plazo da tiempo a la cola del grupo (sin pasar del máximo).
        if !state.nacked {
            let cap = state.first + interval.mul_f64(MAX_DEADLINE_INTERVALS);
            state.deadline = state.deadline.max((now + interval / 2).min(cap));
        }

        let is_source = !header.is_repair();
        if is_source {
            let slot = &mut state.sources[usize::from(header.object)];
            if slot.is_some() {
                return;
            }
            *slot = Some(payload.to_vec());
            state.have += 1;
            if state.arrives_directly(now) {
                state.direct += 1;
            }
            state.highest = state.highest.max(Some(header.object));
            state.note_arrival(now, interval);
            state.keep_until_nack(interval);
        } else {
            // Nunca hacen falta más símbolos de reparación que objetos de origen: el resto solo
            // gastaría memoria y CPU del decodificador.
            if state.repairs.len() >= usize::from(count)
                || state
                    .repairs
                    .iter()
                    .any(|(index, _)| *index == header.object)
            {
                return;
            }
            state.repairs.push((header.object, payload.to_vec()));
            state.note_arrival(now, interval);
            state.keep_until_nack(interval);
        }

        if state.have == usize::from(count) {
            let data: Vec<u8> = state.sources.iter().flatten().flatten().copied().collect();
            finish(state, &data, now, &mut self.stats, Recovery::Direct);
        } else if let Some(size) = state.symbol_size
            && state.have + state.repairs.len() >= usize::from(count)
        {
            let pad = |index: u16, mut data: Vec<u8>| {
                data.resize(size, 0);
                (index, data)
            };
            let decoded = match &mut state.decoder {
                Some(decoder) if is_source => {
                    decoder.push([pad(header.object, payload.to_vec())], [])
                }
                Some(decoder) => decoder.push([], [(header.object, payload.to_vec())]),
                None => match GroupDecoder::new(usize::from(count), size) {
                    Some(mut decoder) => {
                        let sources: Vec<(u16, Vec<u8>)> = state
                            .sources
                            .iter()
                            .enumerate()
                            .filter_map(|(index, source)| {
                                source.clone().map(|data| pad(index as u16, data))
                            })
                            .collect();
                        let result = decoder.push(sources, state.repairs.clone());
                        state.decoder = Some(decoder);
                        result
                    }
                    None => None,
                },
            };
            if let Some(block) = decoded {
                finish(state, &block, now, &mut self.stats, Recovery::Fec);
            }
        }
        self.deliver(track, now);
    }

    /// Grupos de `track` que aún no se han entregado ni olvidado.
    fn open_groups(&self, track: Track) -> usize {
        self.groups.range((track, 0)..=(track, u32::MAX)).count()
    }

    /// Huecos de objetos de origen reservados entre todos los grupos abiertos.
    pub fn pending_objects(&self) -> usize {
        self.groups.values().map(|state| state.sources.len()).sum()
    }

    /// Grupos abiertos entre todas las pistas.
    pub fn open_group_count(&self) -> usize {
        self.groups.len()
    }

    /// Vence `Nack` y plazos a `now`; `rtt` amplía el plazo de los grupos con `Nack`.
    pub fn poll(&mut self, now: Instant, rtt: Duration) {
        let expired: Vec<u32> = self
            .probes
            .iter()
            .filter(|(_, probe)| now.saturating_duration_since(probe.first) >= PROBE_TIMEOUT)
            .map(|(&train, _)| train)
            .collect();
        for train in expired {
            self.report_probe(train);
        }
        let mut tracks = Vec::new();
        for (&(track, group), state) in self.groups.iter_mut() {
            if state.done.is_some() || state.expired {
                continue;
            }
            let interval = self
                .track_intervals
                .get(&track)
                .copied()
                .unwrap_or(self.interval);
            if state.nacks < MAX_NACKS && now >= state.nack_at && now < state.deadline {
                // A estas alturas ya llegó todo lo que no se perdió: lo que falta y la FEC no
                // cubre (o no consiguió decodificar) se pide de nuevo.
                if !state.nacked {
                    state.nack_grace = Some(now + rtt / 2);
                }
                state.nacked = true;
                state.nacks += 1;
                // Mientras el grupo sigue llegando, solo se pide el hueco de abajo.
                let arriving = state.quiet_at.is_some_and(|quiet| now < quiet);
                let missing = match state.count {
                    Some(_) if arriving => state.missing_below_highest(),
                    Some(_) => state.missing(),
                    None => Vec::new(),
                };
                self.outbox.push(ControlMessage::Nack {
                    track,
                    group,
                    missing,
                });
                self.stats.nacks += 1;
                let cap = state.first + nack_wait(track, interval, rtt);
                let extended = now + rtt.mul_f64(1.5) + Duration::from_millis(5);
                state.deadline = state.deadline.max(extended.min(cap));
                // Si el reenvío también se pierde, otro Nack cuando ya debería haber llegado.
                state.nack_at = now + rtt.mul_f64(1.25) + Duration::from_millis(2);
            }
            if now >= state.deadline {
                state.expired = true;
                self.stats.dropped += 1;
                if state.count.is_none() {
                    self.stats.lost_objects += 1;
                }
                let held = self.last_keyframe_request.is_some_and(|at| {
                    now.saturating_duration_since(at) < keyframe_hold(interval, rtt)
                });
                if track == Track::Video && (state.keyframe || !held) {
                    self.outbox.push(ControlMessage::KeyframeRequest { group });
                    self.stats.keyframe_requests += 1;
                    self.last_keyframe_request = Some(now);
                }
                tracks.push(track);
            }
        }
        // Todas las pistas, no solo las que vencieron algo: así un hueco viejo se salta aunque no
        // llegue nada más.
        tracks.extend(self.next.keys().copied());
        tracks.sort_unstable_by_key(|track| *track as u8);
        tracks.dedup();
        for track in tracks {
            self.deliver(track, now);
        }
    }

    /// Si ya llegó entero un intra de `track` por delante de lo que falta por entregar, lo de
    /// antes ya no sirve (el decodificador vuelve a empezar en el intra): se descarta sin esperar a
    /// sus plazos ni a sus `Nack` y se entrega el intra enseguida.
    fn jump_to_keyframe(&mut self, track: Track) {
        let Some(&next) = self.next.get(&track) else {
            return;
        };
        let keyframe = self
            .groups
            .range((track, next.wrapping_add(1))..=(track, u32::MAX))
            .filter(|(_, state)| state.keyframe && state.done.is_some() && !state.expired)
            .map(|(&(_, group), _)| group)
            .next_back();
        let Some(keyframe) = keyframe else {
            return;
        };
        let stale: Vec<u32> = self
            .groups
            .range((track, next)..(track, keyframe))
            .map(|(&(_, group), _)| group)
            .collect();
        for group in stale {
            if let Some(state) = self.groups.remove(&(track, group))
                && state.done.is_none()
                && !state.expired
            {
                self.stats.dropped += 1;
            }
        }
        self.next.insert(track, keyframe);
    }

    /// Da por perdido entero el grupo `group` de `track` (un hueco que ya nadie va a llenar) y, si
    /// es vídeo, pide un intra (uno por ráfaga, como al vencer un plazo).
    fn skip_lost(&mut self, track: Track, group: u32, now: Instant) {
        self.next.insert(track, group.wrapping_add(1));
        self.stats.dropped += 1;
        self.stats.lost_objects += 1;
        let interval = self.track_interval(track);
        let held = self.last_keyframe_request.is_some_and(|at| {
            now.saturating_duration_since(at) < keyframe_hold(interval, Duration::ZERO)
        });
        if track == Track::Video && !held {
            self.outbox.push(ControlMessage::KeyframeRequest { group });
            self.stats.keyframe_requests += 1;
            self.last_keyframe_request = Some(now);
        }
    }

    /// Recuerda un grupo resuelto y cuenta la pérdida de los que se olvidan.
    fn remember(&mut self, recent: Recent) {
        self.recent.push_back(recent);
        while self.recent.len() > RECENT_GROUPS {
            if let Some(old) = self.recent.pop_front() {
                self.stats.lost_objects += (old.received.len() - old.direct) as u64;
            }
        }
    }

    /// Objeto de un grupo ya entregado o descartado.
    fn note_late(&mut self, header: &ObjectHeader, now: Instant) {
        self.stats.late += 1;
        if header.is_repair() {
            return;
        }
        if let Some(recent) = self
            .recent
            .iter_mut()
            .find(|recent| recent.track == header.track && recent.group == header.group)
            && let Some(slot) = recent.received.get_mut(usize::from(header.object))
            && !*slot
        {
            *slot = true;
            if !recent.nacked || recent.nack_grace.is_some_and(|grace| now < grace) {
                recent.direct += 1;
            }
        }
    }

    /// Entrega en orden los grupos resueltos de `track`.
    fn deliver(&mut self, track: Track, now: Instant) {
        self.jump_to_keyframe(track);
        loop {
            let Some(&next) = self.next.get(&track) else {
                return;
            };
            let Some(state) = self.groups.get(&(track, next)) else {
                // Hueco: un grupo del que no llegó nada. Si está entre los MAX_PLACEHOLDERS
                // anteriores al más nuevo, el siguiente datagrama crea su grupo vacío (y se pide);
                // si es más viejo, ya nadie lo va a crear: se da por perdido entero y se sigue.
                let newest = self
                    .groups
                    .range((track, next)..=(track, u32::MAX))
                    .next_back()
                    .map(|(&(_, group), _)| group);
                match newest {
                    Some(newest) if newest - next > MAX_PLACEHOLDERS => {
                        self.skip_lost(track, next, now);
                        continue;
                    }
                    _ => return,
                }
            };
            if state.done.is_none() && !state.expired {
                return;
            }
            let Some(state) = self.groups.remove(&(track, next)) else {
                return;
            };
            self.next.insert(track, next.wrapping_add(1));
            if state.count.is_some() {
                self.remember(Recent {
                    track,
                    group: next,
                    received: state.sources.iter().map(Option::is_some).collect(),
                    direct: state.direct,
                    nacked: state.nacked,
                    nack_grace: state.nack_grace,
                });
            }
            if let Some((payload, received_at)) = state.done {
                if state.expired {
                    continue;
                }
                self.stats.delivered += 1;
                if state.keyframe && track == Track::Video {
                    // Llegó un intra: la próxima pérdida vuelve a pedir otro al momento.
                    self.last_keyframe_request = None;
                }
                self.ready.push_back(CompleteFrame {
                    track,
                    group: next,
                    keyframe: state.keyframe,
                    payload,
                    received_at,
                });
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recovery {
    Direct,
    Fec,
}

/// Recorta el prefijo de longitud y marca el grupo como completo (o como no válido).
fn finish(
    state: &mut GroupState,
    data: &[u8],
    now: Instant,
    stats: &mut ReceiverStats,
    how: Recovery,
) {
    let payload = data
        .first_chunk::<LENGTH_PREFIX>()
        .map(|prefix| u32::from_be_bytes(*prefix) as usize)
        .and_then(|len| data.get(LENGTH_PREFIX..LENGTH_PREFIX + len));
    match payload {
        Some(payload) => {
            if how == Recovery::Fec {
                stats.recovered_fec += 1;
            } else if state.nacked {
                stats.recovered_nack += 1;
            }
            state.done = Some((payload.to_vec(), now));
        }
        None => {
            stats.invalid += 1;
            state.expired = true;
            stats.dropped += 1;
        }
    }
    state.decoder = None;
}

struct ReceiverInner {
    core: ReceiverCore,
    clock: Clock,
    next_feedback: Instant,
    next_ping: Instant,
}

/// Receptor de medios de una conexión.
///
/// [`MediaReceiver::recv_frame`] lee datagramas y vence los plazos; envía por sí mismo `Nack`,
/// `KeyframeRequest`, `Feedback` (cada 50 ms) y `Ping` (cada segundo) por `control`. Los `Pong`
/// que lleguen por el stream de control hay que pasárselos con [`MediaReceiver::handle_control`].
/// Los métodos toman `&self`, pero solo una tarea debe llamar a `recv_frame` a la vez.
pub struct MediaReceiver {
    conn: PeerConnection,
    control: ControlSender,
    inner: Mutex<ReceiverInner>,
}

impl std::fmt::Debug for MediaReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaReceiver")
            .field("conn", &self.conn)
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

impl MediaReceiver {
    /// Receptor sobre `conn` que responde por `control`.
    pub fn new(conn: PeerConnection, control: ControlSender, config: MediaReceiverConfig) -> Self {
        let now = Instant::now();
        Self {
            conn,
            control,
            inner: Mutex::new(ReceiverInner {
                core: ReceiverCore::new(config.frame_interval),
                clock: Clock::new(),
                next_feedback: now + FEEDBACK_INTERVAL,
                next_ping: now,
            }),
        }
    }

    /// Fija el intervalo de fotograma de `track` con lo que anuncia el emisor (ver
    /// [`ReceiverCore::set_track_interval`]).
    pub fn set_track_interval(&self, track: Track, interval: Duration) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.core.set_track_interval(track, interval);
        }
    }

    /// Contadores.
    pub fn stats(&self) -> ReceiverStats {
        self.inner
            .lock()
            .map(|inner| inner.core.stats())
            .unwrap_or_default()
    }

    /// Grupos descartados por vencer su plazo.
    pub fn dropped(&self) -> u64 {
        self.stats().dropped
    }

    /// RTT medido por `Ping`/`Pong`.
    pub fn clock_rtt(&self) -> Option<Duration> {
        self.inner.lock().ok().and_then(|inner| inner.clock.rtt())
    }

    /// Desfase estimado `reloj del emisor − reloj local`, en µs.
    pub fn clock_offset_us(&self) -> Option<i64> {
        self.inner
            .lock()
            .ok()
            .and_then(|inner| inner.clock.offset_us())
    }

    /// Instante local que corresponde a `remote_us` del reloj del emisor
    /// ([`MediaSender::clock_us`]), con el desfase estimado por `Ping`/`Pong`. `None` mientras no
    /// hay desfase (o si el instante no es representable).
    pub fn remote_instant(&self, remote_us: u64) -> Option<Instant> {
        let inner = self.inner.lock().ok()?;
        let offset = inner.clock.offset_us()?;
        let local_us = i64::try_from(remote_us).ok()?.checked_sub(offset)?;
        let origin = inner.clock.origin();
        if local_us >= 0 {
            origin.checked_add(Duration::from_micros(local_us.unsigned_abs()))
        } else {
            origin.checked_sub(Duration::from_micros(local_us.unsigned_abs()))
        }
    }

    /// `at` en µs del reloj del emisor ([`MediaSender::clock_us`]), con el desfase estimado por
    /// `Ping`/`Pong`; `None` mientras no hay desfase. Sirve para que la entrada del invitado lleve
    /// la hora de envío en el reloj del anfitrión.
    pub fn remote_us(&self, at: Instant) -> Option<u64> {
        let inner = self.inner.lock().ok()?;
        let offset = inner.clock.offset_us()?;
        u64::try_from((inner.clock.at_us(at) as i64).checked_add(offset)?).ok()
    }

    /// Atiende `Ping` y `Pong`; devuelve los demás mensajes.
    pub fn handle_control(&self, message: ControlMessage) -> Option<ControlMessage> {
        match message {
            ControlMessage::Pong { t, t_recv } => {
                if let Ok(mut inner) = self.inner.lock() {
                    inner.clock.on_pong(Instant::now(), t, t_recv);
                }
                None
            }
            ControlMessage::Ping { t } => {
                let pong = self
                    .inner
                    .lock()
                    .map(|inner| inner.clock.pong(t))
                    .unwrap_or(ControlMessage::Pong { t, t_recv: 0 });
                self.send(&pong);
                None
            }
            other => Some(other),
        }
    }

    /// Último tren de la sonda del enlace que se informó al emisor (lo que el camino entrega).
    pub fn last_probe(&self) -> Option<ProbeOutcome> {
        self.inner
            .lock()
            .ok()
            .and_then(|inner| inner.core.last_probe())
    }

    /// Siguiente fotograma completo, en orden por pista.
    pub async fn recv_frame(&self) -> Result<CompleteFrame, TransportError> {
        loop {
            let wake = {
                let mut inner = self.inner.lock().map_err(|_| {
                    TransportError::Protocol("estado del receptor envenenado".into())
                })?;
                if let Some(frame) = inner.core.pop_frame() {
                    return Ok(frame);
                }
                let mut wake = inner.next_feedback.min(inner.next_ping);
                if let Some(timer) = inner.core.next_timer() {
                    wake = wake.min(timer);
                }
                wake
            };
            tokio::select! {
                datagram = self.conn.read_datagram() => {
                    let datagram = datagram?;
                    let now = Instant::now();
                    let messages = match self.inner.lock() {
                        Ok(mut inner) => {
                            let arrival = inner.clock.remote_us_i32(now);
                            inner.core.push(&datagram, now, arrival);
                            inner.core.take_messages()
                        }
                        Err(_) => Vec::new(),
                    };
                    self.send_all(messages);
                }
                () = tokio::time::sleep_until(wake.into()) => {
                    let now = Instant::now();
                    let mut messages = Vec::new();
                    if let Ok(mut inner) = self.inner.lock() {
                        // El mayor de los dos RTT: el de QUIC tarda en converger al principio.
                        let rtt = self.conn.rtt().max(inner.clock.rtt().unwrap_or_default());
                        inner.core.poll(now, rtt);
                        messages = inner.core.take_messages();
                        if now >= inner.next_feedback {
                            inner.next_feedback = now + FEEDBACK_INTERVAL;
                            let feedback = inner.core.feedback();
                            messages.push(ControlMessage::Feedback(feedback));
                        }
                        if now >= inner.next_ping {
                            inner.next_ping = now + PING_INTERVAL;
                            messages.push(inner.clock.ping());
                        }
                    }
                    self.send_all(messages);
                }
            }
        }
    }

    /// Envía un `Ping` ahora, fuera del ritmo de [`PING_INTERVAL`] (para medir el RTT antes de
    /// que lleguen los medios). Su `Pong` cuenta como los demás en
    /// [`MediaReceiver::clock_rtt`].
    pub fn send_ping(&self) {
        let ping = self.inner.lock().ok().map(|inner| inner.clock.ping());
        if let Some(ping) = ping {
            self.send(&ping);
        }
    }

    fn send_all(&self, messages: Vec<ControlMessage>) {
        for message in &messages {
            self.send(message);
        }
    }

    fn send(&self, message: &ControlMessage) {
        self.send_control(message);
    }

    /// Envía un mensaje por el stream de control y, si corre prisa (`Nack`, `KeyframeRequest`),
    /// una copia en un datagrama de control: si el paquete del stream se pierde, QUIC no lo
    /// reenvía hasta su PTO (60-70 ms con un RTT de 20 ms) y lo que viene detrás espera con él.
    pub fn send_control(&self, message: &ControlMessage) {
        if let Err(err) = self.control.send(message) {
            debug!(error = %err, "no se pudo enviar un mensaje de control");
        }
        if let Some(datagram) = coroom_proto::control::control_datagram(message)
            && let Err(err) = self.conn.send_datagram(Bytes::from(datagram))
        {
            debug!(error = %err, "no se pudo enviar un datagrama de control");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe_datagram(train: u32, index: u16, count: u16) -> Bytes {
        datagram(
            ObjectHeader {
                track: Track::Probe,
                flags: if index + 1 == count {
                    ObjectHeader::LAST_OBJECT
                } else {
                    0
                },
                group: train,
                object: index,
                object_count: count,
                payload_len: 1000,
                epoch_hint: 0,
            },
            &[0u8; 1000],
        )
    }

    /// Un tren completo se informa al llegar su último datagrama, con la dispersión medida, y no
    /// cuenta como medios (ni bytes recibidos ni grupos abiertos).
    #[test]
    fn a_complete_probe_train_is_reported_and_is_not_media() {
        let mut core = ReceiverCore::new(Duration::from_millis(16));
        let start = Instant::now();
        for index in 0..4u16 {
            let at = start + Duration::from_millis(u64::from(index));
            core.push(&probe_datagram(9, index, 4), at, None);
        }
        assert_eq!(
            core.take_messages(),
            vec![ControlMessage::ProbeResult {
                train: 9,
                sent: 4,
                received: 4,
                bytes: 4 * 1016,
                span_us: 3_000,
            }]
        );
        let outcome = core.last_probe().expect("tren informado");
        // Tres datagramas de 1016 B (el primero solo marca el inicio) en 3 ms.
        assert_eq!(outcome.kbps(), Some(8_128));
        assert_eq!(outcome.lost(), 0);
        assert_eq!(core.stats().received_bytes, 0);
        assert_eq!(core.open_group_count(), 0);
        assert_eq!(core.next_timer(), None, "el tren ya no está abierto");
    }

    /// Un tren al que le faltan datagramas se informa al vencer [`PROBE_TIMEOUT`], con lo que
    /// llegó (la pérdida de la ráfaga).
    #[test]
    fn an_incomplete_probe_train_is_reported_on_timeout() {
        let mut core = ReceiverCore::new(Duration::from_millis(16));
        let start = Instant::now();
        core.push(&probe_datagram(3, 0, 4), start, None);
        core.push(
            &probe_datagram(3, 2, 4),
            start + Duration::from_millis(2),
            None,
        );
        assert_eq!(core.next_timer(), Some(start + PROBE_TIMEOUT));
        core.poll(
            start + PROBE_TIMEOUT - Duration::from_millis(1),
            Duration::ZERO,
        );
        assert!(core.take_messages().is_empty(), "aún no venció");
        core.poll(start + PROBE_TIMEOUT, Duration::ZERO);
        let messages = core.take_messages();
        assert_eq!(messages.len(), 1);
        let outcome = core.last_probe().expect("tren informado");
        assert_eq!((outcome.received, outcome.sent, outcome.lost()), (2, 4, 2));
        assert_eq!(outcome.kbps(), Some(4_064));
    }

    /// Con un solo datagrama no hay dispersión que medir.
    #[test]
    fn a_probe_with_one_datagram_measures_nothing() {
        let outcome = ProbeOutcome {
            train: 1,
            sent: 48,
            received: 1,
            bytes: 1_200,
            span: Duration::ZERO,
        };
        assert_eq!(outcome.kbps(), None);
        assert_eq!(outcome.lost(), 47);
    }

    const INTERVAL: Duration = Duration::from_micros(16_667);

    /// Datagramas de un fotograma como los produce `MediaSender::send_frame`.
    fn packetize(
        group: u32,
        payload: &[u8],
        symbol_size: usize,
        repairs: usize,
    ) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
        let mut data = (payload.len() as u32).to_be_bytes().to_vec();
        data.extend_from_slice(payload);
        let total = data.len();
        let count = total.div_ceil(symbol_size);
        let sources = (0..count)
            .map(|i| {
                let chunk = &data[i * symbol_size..((i + 1) * symbol_size).min(total)];
                let header = ObjectHeader {
                    track: Track::Video,
                    flags: if i + 1 == count {
                        ObjectHeader::LAST_OBJECT
                    } else {
                        0
                    },
                    group,
                    object: i as u16,
                    object_count: count as u16,
                    payload_len: chunk.len() as u16,
                    epoch_hint: 0,
                };
                datagram(header, chunk).to_vec()
            })
            .collect();
        data.resize(count * symbol_size, 0);
        let repairs = if repairs > 0 {
            fec::encode_repairs(&data, symbol_size, repairs as u32)
                .expect("reparación")
                .into_iter()
                .enumerate()
                .map(|(i, symbol)| {
                    let header = ObjectHeader {
                        track: Track::Video,
                        flags: ObjectHeader::REPAIR,
                        group,
                        object: i as u16,
                        object_count: count as u16,
                        payload_len: symbol.len() as u16,
                        epoch_hint: 0,
                    };
                    datagram(header, &symbol).to_vec()
                })
                .collect()
        } else {
            Vec::new()
        };
        (sources, repairs)
    }

    fn payload(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(29) ^ seed)
            .collect()
    }

    #[test]
    fn complete_groups_are_delivered_in_order() {
        let mut core = ReceiverCore::new(INTERVAL);
        let now = Instant::now();
        let (start, _) = packetize(9, &payload(10, 0), 1184, 0);
        let (first, _) = packetize(10, &payload(3000, 1), 1184, 0);
        let (second, _) = packetize(11, &payload(500, 2), 1184, 0);
        core.push(&start[0], now, None);
        assert_eq!(core.pop_frame().map(|frame| frame.group), Some(9));
        for packet in &second {
            core.push(packet, now, None);
        }
        assert!(core.pop_frame().is_none(), "el 11 espera al 10");
        // El grupo 10 se creó vacío: se completa al llegar.
        for packet in first.iter().rev() {
            core.push(packet, now, None);
        }
        let a = core.pop_frame().expect("grupo 10");
        let b = core.pop_frame().expect("grupo 11");
        assert_eq!((a.group, b.group), (10, 11));
        assert_eq!(a.payload, payload(3000, 1));
        assert_eq!(b.payload, payload(500, 2));
        assert_eq!(core.stats().delivered, 3);
    }

    #[test]
    fn fec_recovers_lost_sources() {
        let mut core = ReceiverCore::new(INTERVAL);
        let now = Instant::now();
        let data = payload(20_000, 3);
        let (sources, repairs) = packetize(0, &data, 1184, 4);
        for (i, packet) in sources.iter().enumerate() {
            if ![0, 5, sources.len() - 1].contains(&i) {
                core.push(packet, now, None);
            }
        }
        assert!(core.pop_frame().is_none());
        for packet in &repairs {
            core.push(packet, now, None);
        }
        let frame = core.pop_frame().expect("recuperado por FEC");
        assert_eq!(frame.payload, data);
        assert_eq!(core.stats().recovered_fec, 1);
        // Un objeto "perdido" que llega tarde no cuenta como pérdida.
        let late = core.stats().late;
        core.push(&sources[5], now, None);
        assert_eq!(core.stats().late, late + 1);
        assert_eq!(core.stats().lost_objects, 0, "aún puede llegar tarde");
        for group in 1..=RECENT_GROUPS as u32 {
            let (more, _) = packetize(group, &payload(10, 0), 1184, 0);
            core.push(&more[0], now, None);
        }
        assert_eq!(core.stats().lost_objects, 2);
    }

    /// Con 30 fps anunciados y un RTT de 120 ms (el perfil `mala`), un fotograma de vídeo con un
    /// objeto perdido espera al reenvío aunque llegue después de 4 intervalos (133 ms), en vez de
    /// descartarse y pedir un intra; el audio no espera más de 4 intervalos.
    #[test]
    fn video_waits_for_the_retransmission_when_the_rtt_is_long() {
        let interval = Duration::from_micros(33_333);
        let rtt = Duration::from_millis(120);
        let mut core = ReceiverCore::new(INTERVAL);
        core.set_track_interval(Track::Video, interval);
        assert_eq!(core.track_interval(Track::Video), interval);
        assert_eq!(core.track_interval(Track::Audio), INTERVAL);
        let start = Instant::now();
        let (sources, _) = packetize(1, &payload(5000, 6), 1184, 0);
        for packet in sources.iter().skip(1) {
            core.push(packet, start, None);
        }
        core.poll(start + Duration::from_millis(26), rtt);
        assert_eq!(core.take_messages().len(), 1, "Nack a mitad de plazo");
        // Sin la espera por RTT, el grupo vencía a los 133 ms.
        core.poll(start + Duration::from_millis(150), rtt);
        assert!(
            !core
                .take_messages()
                .contains(&ControlMessage::KeyframeRequest { group: 1 }),
            "no se descarta antes de que llegue el reenvío"
        );
        core.push(&sources[0], start + Duration::from_millis(155), None);
        assert!(core.pop_frame().is_some());
        assert_eq!(core.stats().dropped, 0);
        assert_eq!(core.stats().recovered_nack, 1);

        assert_eq!(
            nack_wait(Track::Audio, Duration::from_millis(20), rtt),
            Duration::from_millis(80)
        );
        assert_eq!(
            nack_wait(Track::Video, interval, rtt),
            rtt * 3 + interval,
            "3 RTT + 1 intervalo"
        );
        assert_eq!(
            nack_wait(Track::Video, interval, Duration::from_secs(2)),
            MAX_VIDEO_NACK_WAIT
        );
        assert_eq!(
            nack_wait(Track::Video, interval, Duration::from_millis(10)),
            interval * 4,
            "con poco RTT, los 4 intervalos de siempre"
        );
    }

    /// El intervalo anunciado viene del otro extremo: se acota.
    #[test]
    fn announced_track_intervals_are_clamped() {
        let mut core = ReceiverCore::new(INTERVAL);
        core.set_track_interval(Track::Video, Duration::from_secs(10));
        assert_eq!(core.track_interval(Track::Video), MAX_TRACK_INTERVAL);
        core.set_track_interval(Track::Video, Duration::ZERO);
        assert_eq!(core.track_interval(Track::Video), MIN_TRACK_INTERVAL);
    }

    #[test]
    fn missing_objects_are_nacked_at_half_deadline_then_dropped() {
        let mut core = ReceiverCore::new(INTERVAL);
        let start = Instant::now();
        let (sources, _) = packetize(5, &payload(5000, 4), 1184, 0);
        // Llegan del 1 al 4 a la vez: falta el 0, por debajo de lo recibido (un hueco).
        for packet in sources.iter().skip(1) {
            core.push(packet, start, None);
        }
        let rtt = Duration::from_millis(10);
        core.poll(start + Duration::from_millis(1), rtt);
        assert!(
            core.take_messages().is_empty(),
            "dentro de la ventana de reordenación"
        );
        core.poll(start + Duration::from_millis(3), rtt);
        assert_eq!(
            core.take_messages(),
            vec![ControlMessage::Nack {
                track: Track::Video,
                group: 5,
                missing: vec![0]
            }]
        );
        // El plazo se amplió por el Nack (3 + 15 + 5 = 23 ms, y el de 25 ms sigue); el reenvío
        // no llegó en 1,25 × RTT (17,5 ms): segundo Nack, y el plazo pasa a 18 + 15 + 5 = 38 ms.
        core.poll(start + Duration::from_millis(10), rtt);
        assert!(core.take_messages().is_empty());
        core.poll(start + Duration::from_millis(18), rtt);
        assert_eq!(core.take_messages().len(), 1);
        core.poll(start + Duration::from_millis(30), rtt);
        assert!(core.take_messages().is_empty(), "no hay tercer Nack");
        core.poll(start + Duration::from_millis(38), rtt);
        assert_eq!(
            core.take_messages(),
            vec![ControlMessage::KeyframeRequest { group: 5 }]
        );
        assert_eq!(core.stats().dropped, 1);
        assert_eq!(core.stats().nacks, 2);
        assert!(core.next_timer().is_none());
        // Lo que llegue después del grupo descartado se ignora.
        core.push(&sources[0], start + Duration::from_millis(39), None);
        assert!(core.pop_frame().is_none());
    }

    /// La cola de un grupo (los últimos objetos) no se pide mientras siguen llegando objetos:
    /// solo tras un silencio de unos tres espaciados (como mucho medio intervalo).
    #[test]
    fn a_lost_tail_is_nacked_after_a_quiet_spell() {
        let mut core = ReceiverCore::new(INTERVAL);
        let start = Instant::now();
        let (sources, _) = packetize(2, &payload(6000, 2), 1184, 0);
        let rtt = Duration::from_millis(10);
        let last = sources.len() - 1;
        // Llegan cada 4 ms (el pacing a poco bitrate) y se pierde el último.
        for (i, packet) in sources.iter().take(last).enumerate() {
            let now = start + Duration::from_millis(4 * i as u64);
            core.poll(now, rtt);
            assert!(core.take_messages().is_empty(), "objeto {i}: aún llegando");
            core.push(packet, now, None);
        }
        let arrived = start + Duration::from_millis(4 * (last as u64 - 1));
        // Tres espaciados (12 ms), como mucho medio intervalo (8,3 ms).
        core.poll(arrived + Duration::from_millis(8), rtt);
        assert!(core.take_messages().is_empty(), "aún dentro del silencio");
        core.poll(arrived + Duration::from_millis(9), rtt);
        assert_eq!(
            core.take_messages(),
            vec![ControlMessage::Nack {
                track: Track::Video,
                group: 2,
                missing: vec![last as u16]
            }]
        );
    }

    #[test]
    fn a_burst_longer_than_the_placeholders_does_not_stall_the_track() {
        let mut core = ReceiverCore::new(INTERVAL);
        let start = Instant::now();
        let rtt = Duration::from_millis(5);
        let at = |group: u32| start + INTERVAL * group;
        for group in 0..3 {
            let (sources, _) = packetize(group, &payload(3000, group as u8), 1184, 0);
            for packet in &sources {
                core.push(packet, at(group), None);
            }
        }
        while core.pop_frame().is_some() {}
        // Se pierden del 3 al 29 enteros (450 ms); a partir del 30 todo llega.
        let mut presented = Vec::new();
        for group in 30..90 {
            let (sources, _) = packetize(group, &payload(3000, group as u8), 1184, 0);
            for packet in &sources {
                core.push(packet, at(group), None);
            }
            core.poll(at(group) + INTERVAL / 2, rtt);
            while let Some(frame) = core.pop_frame() {
                presented.push((frame.group, group));
            }
        }
        let first = presented.first().copied().expect("vídeo después del corte");
        assert!(
            first.1 - first.0 <= 4,
            "el primer grupo tras el corte sale como mucho 4 fotogramas tarde: {first:?}"
        );
        assert!(presented.iter().any(|(group, _)| *group == 89));
        assert!(
            core.take_messages()
                .iter()
                .any(|m| matches!(m, ControlMessage::KeyframeRequest { .. })),
            "se pide un intra"
        );
        assert_eq!(core.stats().invalid, 0, "nada se rechaza por grupos llenos");
    }

    /// Un fotograma repartido por el pacing (80 % del intervalo) y un intra repartido en dos
    /// intervalos llegan enteros sin pedir nada: el `Nack` espera a que el grupo deje de llegar.
    #[test]
    fn paced_groups_are_not_nacked_while_they_keep_arriving() {
        let mut core = ReceiverCore::new(INTERVAL);
        let start = Instant::now();
        let rtt = Duration::from_millis(3);
        for (group, spread, keyframe) in [
            (0u32, INTERVAL.mul_f64(0.8), false),
            (1, INTERVAL * 2, true),
        ] {
            let (mut sources, _) = packetize(group, &payload(120_000, group as u8), 1184, 0);
            if keyframe {
                for packet in &mut sources {
                    let (mut header, body) =
                        ObjectHeader::parse_datagram(packet).expect("cabecera");
                    header.flags |= ObjectHeader::KEYFRAME;
                    *packet = datagram(header, body).to_vec();
                }
            }
            let begin = start + INTERVAL * 3 * group;
            let n = sources.len() as u32;
            for (i, packet) in sources.iter().enumerate() {
                let now = begin + spread * i as u32 / n;
                core.poll(now, rtt);
                core.push(packet, now, None);
            }
            core.poll(begin + spread, rtt);
            let frame = core.pop_frame().expect("grupo entero");
            assert_eq!(frame.group, group);
            assert_eq!(frame.keyframe, keyframe);
        }
        assert_eq!(core.stats().nacks, 0, "{:?}", core.take_messages());
        assert_eq!(core.stats().dropped, 0);
    }

    #[test]
    fn a_nacked_object_that_arrives_completes_the_group() {
        let mut core = ReceiverCore::new(INTERVAL);
        let start = Instant::now();
        let data = payload(5000, 5);
        let (sources, _) = packetize(1, &data, 1184, 0);
        for packet in sources.iter().skip(1) {
            core.push(packet, start, None);
        }
        core.poll(start + Duration::from_millis(13), Duration::from_millis(10));
        assert_eq!(core.take_messages().len(), 1);
        core.push(&sources[0], start + Duration::from_millis(25), None);
        assert_eq!(core.pop_frame().expect("recuperado").payload, data);
        assert_eq!(core.stats().recovered_nack, 1);
    }

    /// El original que llega tras el `Nack` pero antes de medio RTT (antes de que pueda
    /// llegar ningún reenvío) es jitter, no pérdida; después, sí cuenta como perdido y recuperado.
    #[test]
    fn jitter_late_originals_after_a_nack_are_not_counted_as_lost() {
        let rtt = Duration::from_millis(120);
        for (arrival_ms, lost) in [(40u64, 0u64), (90, 1)] {
            let mut core = ReceiverCore::new(INTERVAL);
            let start = Instant::now();
            let (sources, _) = packetize(1, &payload(5000, 5), 1184, 0);
            for packet in sources.iter().skip(1) {
                core.push(packet, start, None);
            }
            core.poll(start + Duration::from_millis(13), rtt);
            assert_eq!(core.take_messages().len(), 1, "Nack");
            core.push(&sources[0], start + Duration::from_millis(arrival_ms), None);
            assert!(core.pop_frame().is_some());
            // La pérdida se cuenta al olvidar el grupo.
            for group in 2..=(RECENT_GROUPS as u32 + 2) {
                let (more, _) = packetize(group, &payload(10, 0), 1184, 0);
                core.push(&more[0], start + Duration::from_millis(arrival_ms), None);
            }
            assert_eq!(
                core.stats().lost_objects,
                lost,
                "llegada a los {arrival_ms} ms"
            );
        }
    }

    #[test]
    fn whole_lost_groups_are_requested_entirely() {
        let mut core = ReceiverCore::new(INTERVAL);
        let start = Instant::now();
        let (g0, _) = packetize(0, &payload(100, 0), 1184, 0);
        let (g2, _) = packetize(2, &payload(100, 2), 1184, 0);
        core.push(&g0[0], start, None);
        assert_eq!(core.pop_frame().map(|f| f.group), Some(0));
        core.push(&g2[0], start, None);
        core.poll(start + Duration::from_millis(13), Duration::from_millis(1));
        assert_eq!(
            core.take_messages(),
            vec![ControlMessage::Nack {
                track: Track::Video,
                group: 1,
                missing: vec![]
            }]
        );
        core.poll(start + Duration::from_millis(80), Duration::from_millis(1));
        assert_eq!(core.pop_frame().map(|f| f.group), Some(2));
    }

    #[test]
    fn feedback_reports_counters_and_arrivals() {
        let mut core = ReceiverCore::new(INTERVAL);
        let now = Instant::now();
        let (sources, _) = packetize(7, &payload(2000, 7), 1184, 0);
        for packet in &sources {
            core.push(packet, now, Some(1234));
        }
        let feedback = core.feedback();
        assert_eq!(feedback.highest_group, 7);
        assert_eq!(feedback.owd_samples, vec![(7, 1234)]);
        assert_eq!(
            feedback.received_bytes,
            sources.iter().map(Vec::len).sum::<usize>() as u64
        );
        assert!(core.feedback().owd_samples.is_empty());
    }

    #[test]
    fn garbage_is_counted_and_ignored() {
        let mut core = ReceiverCore::new(INTERVAL);
        let now = Instant::now();
        core.push(&[1, 2, 3], now, None);
        let (mut sources, _) = packetize(0, &payload(3000, 9), 1184, 0);
        sources[0].truncate(100);
        core.push(&sources[0], now, None);
        assert_eq!(core.stats().invalid, 2);
        assert!(core.pop_frame().is_none());
    }

    /// Hallazgo M-1 de `docs/seguridad.md`: un datagrama por grupo con 65 535 objetos anunciados
    /// no puede reservar memoria sin cota.
    #[test]
    fn hostile_groups_cannot_reserve_unbounded_memory() {
        let mut core = ReceiverCore::new(INTERVAL);
        let now = Instant::now();
        for track in Track::ALL {
            for group in 0..1024u32 {
                let header = ObjectHeader {
                    track,
                    flags: 0,
                    group: group * 9,
                    object: 0,
                    object_count: u16::MAX,
                    payload_len: 1,
                    epoch_hint: 0,
                };
                core.push(&datagram(header, &[0]), now, None);
                // Reparaciones de sobra para el mismo grupo: no se acumulan sin fin.
                let repair = ObjectHeader {
                    flags: ObjectHeader::REPAIR,
                    object: (group % 512) as u16,
                    ..header
                };
                core.push(&datagram(repair, &[0]), now, None);
            }
        }
        assert!(core.pending_objects() <= MAX_PENDING_OBJECTS);
        assert!(core.open_group_count() <= MAX_OPEN_GROUPS * Track::ALL.len());
        assert!(core.stats().invalid > 0);

        // Un grupo con más reparaciones (que no decodifican: símbolos vacíos) que objetos guarda
        // como mucho tantas como objetos.
        let mut core = ReceiverCore::new(INTERVAL);
        for object in 0..40u16 {
            let repair = ObjectHeader {
                track: Track::Video,
                flags: ObjectHeader::REPAIR,
                group: 0,
                object,
                object_count: 5,
                payload_len: 0,
                epoch_hint: 0,
            };
            core.push(&datagram(repair, &[]), now, None);
        }
        let state = core.groups.get(&(Track::Video, 0)).expect("grupo");
        assert_eq!(state.repairs.len(), 5);

        // Pasados los plazos, todo se libera y la pista sigue funcionando.
        core.poll(now + Duration::from_secs(1), Duration::from_millis(10));
        let (sources, _) = packetize(1, &payload(3000, 4), 1184, 0);
        for source in &sources {
            core.push(source, now + Duration::from_secs(1), None);
        }
        assert!(core.pop_frame().is_some());
    }

    #[test]
    fn repair_ratio_is_linear_up_to_five_percent_loss() {
        assert_eq!(repair_ratio_for_loss(0.0), 0.0);
        assert!((repair_ratio_for_loss(0.02) - 0.12).abs() < 1e-9);
        assert_eq!(repair_ratio_for_loss(0.05), MAX_REPAIR_RATIO);
        assert_eq!(repair_ratio_for_loss(0.5), MAX_REPAIR_RATIO);
    }

    #[test]
    fn clock_estimates_rtt_and_offset() {
        let mut clock = Clock::new();
        let base = clock.origin;
        // Ping en t = 1000 µs, el remoto va 50 ms adelantado, RTT 20 ms.
        let t = 1_000;
        let t_recv = t + 10_000 + 50_000;
        clock.on_pong(base + Duration::from_micros(t + 20_000), t, t_recv);
        assert_eq!(clock.rtt(), Some(Duration::from_millis(20)));
        assert_eq!(clock.offset_us(), Some(50_000));
        let remote = clock.remote_us_i32(base + Duration::from_micros(100_000));
        assert_eq!(remote, Some(150_000));
    }

    /// El pacing reparte cada fotograma al ritmo de 1,25 veces el objetivo, sin pasar de 0,8
    /// intervalos (2 en un intra): a 120 fps y 20 Mbit/s, un intra de 400 KB ya no sale en 4 ms.
    #[test]
    fn pacing_follows_the_target_and_is_capped_per_frame() {
        let interval = Duration::from_micros(8_333);
        // 20 KB a 20 Mbit/s × 1,25 = 25 Mbit/s: 6,4 ms.
        let normal = pacing_duration(20_000, 20_000, interval, false);
        assert!((normal.as_secs_f64() - 0.0064).abs() < 1e-6, "{normal:?}");
        // 100 KB serían 32 ms: se recorta a 0,8 intervalos.
        assert_eq!(
            pacing_duration(100_000, 20_000, interval, false),
            interval.mul_f64(FRAME_PACING_SHARE)
        );
        // Un intra de 400 KB: 2 intervalos (16,7 ms) en vez de los 4 ms de antes.
        assert_eq!(
            pacing_duration(400_000, 20_000, interval, true),
            interval * 2
        );
        // Lo pequeño apenas espera; un objetivo de 0 no divide por cero.
        assert!(pacing_duration(1_200, 80_000, interval, false) < Duration::from_micros(100));
        assert_eq!(
            pacing_duration(1_200, 0, interval, false),
            interval.mul_f64(FRAME_PACING_SHARE)
        );
    }

    /// Varios grupos de vídeo perdidos seguidos piden un solo intra mientras ese viene de camino;
    /// si lo que se pierde es el intra, se pide otro aunque no haya pasado la espera.
    #[test]
    fn a_burst_of_lost_groups_requests_a_single_keyframe() {
        let mut core = ReceiverCore::new(INTERVAL);
        let start = Instant::now();
        let rtt = Duration::from_millis(2);
        let (g0, _) = packetize(0, &payload(100, 0), 1184, 0);
        core.push(&g0[0], start, None);
        assert!(core.pop_frame().is_some());
        // Los grupos 1 a 5 llegan a medias (falta el objeto 0 de cada uno).
        for group in 1..=5 {
            let (sources, _) = packetize(group, &payload(3000, group as u8), 1184, 0);
            for packet in sources.iter().skip(1) {
                core.push(packet, start, None);
            }
        }
        core.poll(start + Duration::from_millis(500), rtt);
        let requests: Vec<_> = core
            .take_messages()
            .into_iter()
            .filter(|m| matches!(m, ControlMessage::KeyframeRequest { .. }))
            .collect();
        assert_eq!(requests, vec![ControlMessage::KeyframeRequest { group: 1 }]);
        assert_eq!(core.stats().dropped, 5);

        // El intra que llega (grupo 6) se pierde también a medias: se pide otro enseguida.
        let (sources, _) = packetize(6, &payload(3000, 6), 1184, 0);
        for packet in sources.iter().skip(1) {
            let mut keyframe = packet.clone();
            keyframe[2] |= ObjectHeader::KEYFRAME;
            core.push(&keyframe, start + Duration::from_millis(510), None);
        }
        // 60 ms después del primero: dentro de la espera (70 ms), pero es un intra.
        core.poll(start + Duration::from_millis(560), rtt);
        assert!(
            core.take_messages()
                .contains(&ControlMessage::KeyframeRequest { group: 6 })
        );
    }
}
