//! Medios sobre datagramas QUIC: objetos con FEC RaptorQ, pacing, NACK y fecha límite.
//!
//! **Emisor** ([`MediaSender`]): cada fotograma (ya cifrado con SFrame por la sesión) se prefija con
//! su longitud (4 bytes, para recortar el relleno cuando se reconstruye por FEC), se parte en
//! objetos de origen de como mucho `max_packet − 16` bytes y se le añaden símbolos de reparación
//! RaptorQ en la proporción `repair_ratio` (0 % sin pérdida, lineal hasta 30 % con pérdida ≥ 5 %,
//! actualizada con cada `Feedback`; 10 % hasta el primero, cuando la pérdida aún no se conoce).
//! Todo sale con **pacing** ([`pacing_duration`]): el fotograma se reparte en lo que tardaría al
//! ritmo medido del enlace ([`MediaSender::pacing_plan`]: 1,25 veces el bitrate objetivo o, si es
//! más, el 70 % de lo que midió la sonda del enlace o la estimación del control de tasa, con el
//! techo [`PROBE_MAX_PACE_KBPS`] y **sin** acotar al bitrate máximo del usuario: ese máximo es un
//! promedio, no la capacidad del enlace, y acotar el ritmo a él estiraba cada fotograma de un
//! enlace con holgura hasta el recorte del `share`, 5 ms a 60 fps y 10 ms a 30 fps con 10 Mbit/s,
//! dos tercios de la latencia de captura a presentación medida en LAN), como
//! mucho [`PacingConfig::share`] del intervalo (0,3). Un intra ocupa como mucho
//! [`PacingConfig::keyframe_intervals`] (2) y, solo si el enlace **midió** al menos
//! [`KEYFRAME_HEADROOM`] veces lo que ese ritmo necesita, [`PacingConfig::keyframe_intervals_measured`]
//! (1): la ventana de congestión de medios (`rate.rs`, ≥ 2 s × objetivo) no acota las ráfagas,
//! así que un intra a un intervalo sin esa holgura medida saldría al doble de ritmo que antes y un
//! cuello con poco búfer (DSL, 4G, un punto de acceso Wi-Fi) lo perdería en parte. Antes se metía
//! en 4 ms: a 120 fps un intra de cientos de KB salía a varios cientos de Mbit/s, llenaba la cola
//! del camino y el búfer del socket del receptor (64 KB en Windows) y se perdía entero, con lo que
//! se pedía otro intra igual de grande. Después se repartía en 0,8 intervalos al bitrate objetivo
//! aunque el enlace diese diez veces más: cada fotograma llegaba ~13 ms después de lo necesario a
//! 60 fps. Guarda los objetos de origen de los últimos 16 grupos para responder a los `Nack`; los
//! reenvíos grandes salen con el mismo ritmo.
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
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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

/// Datagramas de cada tren de la sonda del enlace ([`MediaSender::send_probe`]): 64 × 1200 B son
/// ~77 KB, bastantes para que la dispersión del tren la ponga el cuello del camino y no el jitter
/// de un par de paquetes (un tren entero dura al menos ~6 ms a [`PROBE_MAX_PACE_KBPS`], varias
/// veces la resolución de los temporizadores), y pocos para que en un enlace de 10 Mbit/s no hagan
/// más de ~60 ms de cola (antes de que haya vídeo): el tren tarda 62 ms en vaciarse por ese
/// enlace y sale en 6 ms a [`PROBE_MAX_PACE_KBPS`], así que deja ~55 ms de cola.
pub const PROBE_PACKETS: u16 = 64;

/// Trenes que se envían a cada receptor al conectar, separados [`PROBE_SPACING`], en escalera de
/// ritmos ([`startup_probe_paces`]); vale el mejor (el tráfico ajeno solo puede hacer que un tren
/// mida de menos).
pub const PROBE_TRAINS: u32 = 4;

/// Ritmos de la sonda al arrancar: una escalera hasta [`probe_pace_kbps`] (un octavo, un cuarto,
/// la mitad y el ritmo entero). Los de abajo pasan limpios por casi cualquier enlace y dicen que
/// hay al menos eso; el primero que pierde datagramas contra un limitador dice cuánto pasa, y el
/// techo de ráfaga empieza ahí ([`RateController::on_probe`]), antes de que haya vídeo que perder.
pub fn startup_probe_paces(max_kbps: u32) -> [u32; PROBE_TRAINS as usize] {
    let top = probe_pace_kbps(max_kbps);
    [top / 8, top / 4, top / 2, top].map(|pace| pace.max(1))
}

/// Cada cuánto se mira si algún receptor pide una sonda periódica ([`MediaSender::probe_due`]).
pub const PROBE_POLL: Duration = Duration::from_millis(250);

/// Separación entre dos trenes de sonda.
pub const PROBE_SPACING: Duration = Duration::from_millis(100);

/// Ritmo máximo al que sale un tren y techo del ritmo del pacing ([`pacing_pace`]): la medida de
/// un tren es «al menos esto» (la dispersión solo mide cuellos por debajo del ritmo de envío), así
/// que el pacing nunca puede apoyarse en más de lo que un tren pudo demostrar; además acota las
/// ráfagas que recibe el otro extremo (si el sistema no concede los 8 MB de búfer de socket que se
/// le piden y deja los 64 KB por defecto de Windows, a 100 Mbit/s se llenan si el lector se para
/// 5 ms).
pub const PROBE_MAX_PACE_KBPS: u32 = 100_000;

/// Veces el bitrate máximo del usuario a las que salen los trenes de la sonda
/// ([`probe_pace_kbps`]). El pacing toma [`PROBE_START_SHARE`] (0,7) de lo medido y la dispersión
/// solo mide hasta el ritmo de envío, así que con los trenes al máximo del usuario (lo que se hacía
/// antes) un fotograma del tamaño medio a ese máximo (máximo / fps) se repartía en 1 / 0,7 = 1,4
/// intervalos y el [`FRAME_PACING_SHARE`] (0,3) lo recortaba siempre: 5 ms a 60 fps y 10 ms a
/// 30 fps, fuese el enlace de 10 Mbit/s o de 1 Gbit/s. Con los trenes a `k` veces el máximo, ese
/// fotograma se reparte en `1 / (0,7 k)` del intervalo; para que quede en la mitad del `share`
/// (0,15 del intervalo: 2,5 ms a 60 fps, 5 ms a 30 fps) hace falta `k ≥ 1 / (0,7 × 0,15) = 9,5`
/// (lo comprueba un test). Con 4× quedaría en 0,36 del intervalo y el `share` seguiría recortando
/// igual que antes. No se envía siempre a [`PROBE_MAX_PACE_KBPS`] porque un cuello que **descarta**
/// en vez de encolar (un limitador de ISP con poco cubo, 4G) deja pasar parte del tren con la
/// dispersión del ritmo de salida y mide de más; acotar el tren a `k` veces el máximo acota ese
/// error, y con él las ráfagas que luego haría el pacing, a `0,7 k` veces el máximo. Con el máximo
/// de referencia de 1080p60 (10 Mbit/s) `k = 10` es justo el techo; por debajo (720p, 4-6 Mbit/s)
/// los trenes y las ráfagas quedan proporcionales. La cola que deja el tren en un enlace justo al
/// máximo apenas depende de `k` (el tren sale en `1 / k` de lo que tarda en vaciarse: 55 ms a
/// 10 Mbit/s con `k = 10`, 46 ms con `k = 4`), dentro de lo que admite [`PROBE_PACKETS`].
pub const PROBE_PACE_GAIN: u32 = 10;

/// Ritmo, en kbit/s, al que salen los trenes de la sonda para un bitrate máximo del usuario
/// `max_kbps`: [`PROBE_PACE_GAIN`] veces ese máximo, como mucho [`PROBE_MAX_PACE_KBPS`] (y nunca
/// menos que el propio máximo, que `PROBE_PACE_GAIN ≥ 1` garantiza). Así la dispersión mide los
/// cuellos por encima del máximo del usuario, que es lo que el pacing necesita saber
/// ([`pacing_pace`]).
pub fn probe_pace_kbps(max_kbps: u32) -> u32 {
    max_kbps
        .max(1)
        .saturating_mul(PROBE_PACE_GAIN)
        .min(PROBE_MAX_PACE_KBPS)
}

/// Un tren que mide más de este múltiplo del ritmo al que salió no midió el camino: sus
/// datagramas esperaron en una cola del receptor y salieron de golpe (o el reloj del receptor no
/// vale). Entre el ritmo de envío y este múltiplo la medida se acota al ritmo de envío (la
/// dispersión no puede ser menor que la de salida; el exceso es jitter de los temporizadores).
pub const PROBE_PACE_TOLERANCE: f64 = 2.0;

/// Trenes de sonda cuyo ritmo de envío recuerda el emisor para comprobar los resultados.
const PROBE_PACES_REMEMBERED: usize = 16;

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

    /// [`ProbeOutcome::kbps`] comprobado contra el ritmo al que salió el tren, si se conoce: por
    /// encima de [`PROBE_PACE_TOLERANCE`] veces el ritmo de envío la medida no vale (`None`); entre
    /// el ritmo de envío y ese múltiplo, vale el ritmo de envío (el camino dio al menos eso).
    pub fn checked_kbps(&self, pace_kbps: Option<u32>) -> Option<u32> {
        let kbps = self.kbps()?;
        let Some(pace) = pace_kbps.filter(|pace| *pace > 0) else {
            return Some(kbps);
        };
        if f64::from(kbps) > f64::from(pace) * PROBE_PACE_TOLERANCE {
            return None;
        }
        Some(kbps.min(pace))
    }
}

/// Ritmo del pacing sobre el bitrate objetivo: algo por encima, para que un fotograma algo mayor
/// que la media no se retrase más de lo que tarda en salir.
pub const PACING_GAIN: f64 = 1.25;

/// Parte del intervalo que puede ocupar el pacing de un fotograma normal
/// ([`PacingConfig::share`] por defecto): lo que sobra deja al hilo `send` ponerse al día si un
/// fotograma llegó tarde, y el fotograma no llega más tarde de lo que el enlace obliga.
pub const FRAME_PACING_SHARE: f64 = 0.3;

/// Intervalos que puede ocupar el pacing de un intra ([`PacingConfig::keyframe_intervals`] por
/// defecto, lo mismo que tolera el receptor): como mucho dos fotogramas esperan detrás en el canal
/// del codificador.
pub const DEFAULT_KEYFRAME_PACING_INTERVALS: f64 = 2.0;

/// Intervalos de un intra cuando el enlace midió holgura ([`KEYFRAME_HEADROOM`]);
/// [`PacingConfig::keyframe_intervals_measured`] por defecto.
pub const DEFAULT_KEYFRAME_PACING_INTERVALS_MEASURED: f64 = 1.0;

/// Veces que el ritmo medido del enlace ([`MediaSender::pacing_plan`]) tiene que superar
/// `gain × objetivo` para repartir un intra en [`PacingConfig::keyframe_intervals_measured`] en vez
/// de [`PacingConfig::keyframe_intervals`].
pub const KEYFRAME_HEADROOM: f64 = 2.0;

/// Intervalos de pacing de un intra que el **receptor** tolera en su plazo (el emisor puede
/// configurar hasta esto; con menos, el plazo solo queda holgado).
pub const KEYFRAME_PACING_INTERVALS: u32 = 2;

/// Objetos de un `Nack` que se reenvían de golpe; los demás, con el ritmo del pacing.
pub const NACK_BURST: usize = 8;

/// Reparto en el tiempo de los fotogramas del emisor. [`PacingConfig::default`] son las
/// constantes del módulo (sin leer el entorno, para que una variable suelta no cambie los tests);
/// [`PacingConfig::from_env`] admite `COROOM_PACING_SHARE`, `COROOM_KEYFRAME_PACING_INTERVALS` y
/// `COROOM_KEYFRAME_PACING_INTERVALS_MEASURED` para comparar A/B con `coroom host` sin recompilar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PacingConfig {
    /// Ritmo del pacing sobre el bitrate objetivo ([`PACING_GAIN`]).
    pub gain: f64,
    /// Parte del intervalo de fotograma que puede ocupar un fotograma normal
    /// ([`FRAME_PACING_SHARE`]).
    pub share: f64,
    /// Intervalos que puede ocupar un intra ([`DEFAULT_KEYFRAME_PACING_INTERVALS`], como mucho
    /// [`KEYFRAME_PACING_INTERVALS`]).
    pub keyframe_intervals: f64,
    /// Intervalos de un intra cuando el enlace midió al menos [`KEYFRAME_HEADROOM`] veces el ritmo
    /// de pacing ([`DEFAULT_KEYFRAME_PACING_INTERVALS_MEASURED`]).
    pub keyframe_intervals_measured: f64,
}

impl Default for PacingConfig {
    fn default() -> Self {
        Self {
            gain: PACING_GAIN,
            share: FRAME_PACING_SHARE,
            keyframe_intervals: DEFAULT_KEYFRAME_PACING_INTERVALS,
            keyframe_intervals_measured: DEFAULT_KEYFRAME_PACING_INTERVALS_MEASURED,
        }
    }
}

impl PacingConfig {
    /// Los valores por defecto, cambiados por las variables de entorno `COROOM_PACING_SHARE`
    /// (0,05-1), `COROOM_KEYFRAME_PACING_INTERVALS` y `COROOM_KEYFRAME_PACING_INTERVALS_MEASURED`
    /// (0,25-[`KEYFRAME_PACING_INTERVALS`]) si están y valen; fuera de rango se ignoran. Solo lo
    /// usa la aplicación (`coroom host`), nunca los tests.
    pub fn from_env() -> Self {
        let env = |name: &str, lo: f64, hi: f64| {
            std::env::var(name)
                .ok()
                .and_then(|value| value.trim().parse::<f64>().ok())
                .filter(|value| value.is_finite() && (lo..=hi).contains(value))
        };
        let defaults = Self::default();
        let max = f64::from(KEYFRAME_PACING_INTERVALS);
        Self {
            share: env("COROOM_PACING_SHARE", 0.05, 1.0).unwrap_or(defaults.share),
            keyframe_intervals: env("COROOM_KEYFRAME_PACING_INTERVALS", 0.25, max)
                .unwrap_or(defaults.keyframe_intervals),
            keyframe_intervals_measured: env(
                "COROOM_KEYFRAME_PACING_INTERVALS_MEASURED",
                0.25,
                max,
            )
            .unwrap_or(defaults.keyframe_intervals_measured),
            ..defaults
        }
    }
}

/// Ritmo al que se reparte un fotograma, en kbit/s, y si el enlace midió holgura para los intras
/// (lo que calcula [`MediaSender::pacing_plan`] con los datos del receptor que limita):
/// `gain` veces el objetivo del control de tasa (`target_kbps`) o, si el enlace demostró más,
/// [`PROBE_START_SHARE`] de lo que midió la sonda (`probe_kbps`) o la estimación del control de
/// tasa (`estimate_kbps`), lo mayor de los dos; techo [`PROBE_MAX_PACE_KBPS`]. El bitrate máximo
/// del usuario **no** acota el ritmo: es un promedio para el codificador, y el control de tasa ya
/// lo respeta en `target_kbps`; la capacidad del enlace la dicen la sonda y la estimación. Sin
/// sonda ni estimación (`None` y 0), el ritmo es `gain × objetivo`, como siempre fue. La holgura
/// es que lo medido llegue a [`KEYFRAME_HEADROOM`] veces `gain × objetivo`.
pub fn pacing_pace(
    target_kbps: u32,
    probe_kbps: Option<u32>,
    estimate_kbps: f64,
    gain: f64,
) -> (u32, bool) {
    let target = f64::from(target_kbps) * gain;
    let probe = probe_kbps.map_or(0.0, |kbps| f64::from(kbps) * PROBE_START_SHARE);
    let measured = probe.max(estimate_kbps);
    let pace = target.max(measured).min(f64::from(PROBE_MAX_PACE_KBPS));
    let headroom = measured >= target * KEYFRAME_HEADROOM;
    (pace.round().max(1.0) as u32, headroom)
}

/// Tiempo en que se reparten los `bytes` de un fotograma: lo que tardan a `pace_kbps` (el ritmo
/// medido del enlace, ver [`MediaSender::pacing_plan`]), como mucho [`PacingConfig::share`] de
/// `interval`; un intra, [`PacingConfig::keyframe_intervals`] intervalos o, con `headroom` (el
/// enlace midió [`KEYFRAME_HEADROOM`] veces ese ritmo), [`PacingConfig::keyframe_intervals_measured`].
///
/// Con `capped` (el ritmo es el techo de ráfaga del control de tasa: el enlace perdió al pasarlo),
/// el reparto puede ocupar hasta [`CAPPED_INTERVALS`] intervalos (un intra,
/// [`CAPPED_KEYFRAME_INTERVALS`]): comprimirlo en el 30 % del intervalo lo haría salir a unas tres
/// veces el techo y el limitador del enlace volvería a tirarlo.
pub fn pacing_duration(
    bytes: usize,
    pace_kbps: u32,
    interval: Duration,
    keyframe: bool,
    headroom: bool,
    capped: bool,
    pacing: &PacingConfig,
) -> Duration {
    let rate_bps = f64::from(pace_kbps.max(1)) * 1000.0;
    let spread = Duration::from_secs_f64(bytes as f64 * 8.0 / rate_bps);
    let cap = if capped && keyframe {
        interval.mul_f64(CAPPED_KEYFRAME_INTERVALS)
    } else if capped {
        interval.mul_f64(CAPPED_INTERVALS)
    } else if keyframe && headroom {
        interval.mul_f64(pacing.keyframe_intervals_measured)
    } else if keyframe {
        interval.mul_f64(pacing.keyframe_intervals)
    } else {
        interval.mul_f64(pacing.share)
    };
    spread.min(cap)
}

/// Intervalos que puede ocupar un fotograma repartido al techo de ráfaga ([`pacing_duration`]):
/// nunca el intervalo entero, para que termine antes de que llegue el siguiente aunque una espera
/// se retrase (si no, el retraso se acumula fotograma a fotograma).
pub const CAPPED_INTERVALS: f64 = 0.85;

/// Intervalos de un intra repartido al techo de ráfaga: un intra pesa decenas de fotogramas y,
/// pasado el techo, el limitador lo tira entero (y se vuelve a pedir otro); mejor que tarde, pero
/// nunca más de lo que espera el receptor: un intervalo menos que su plazo máximo (4 intervalos;
/// con 8 intervalos se daba por vencido a los 4, se perdía entero y
/// se pedía otro, y el vídeo quedaba congelado).
pub const CAPPED_KEYFRAME_INTERVALS: f64 = MAX_DEADLINE_INTERVALS - 1.0;

/// Lo que una espera del pacing puede adelantarse sin dormir aunque el retraso medido sea menor:
/// los temporizadores de `tokio` van en pasos de 1 ms, así que una espera más corta que esto se
/// despertaría igual de tarde que no esperar.
const MIN_PACING_WAIT: Duration = Duration::from_micros(500);

/// Techo del retraso que se recuerda: una pausa del sistema de medio segundo no debe anular el
/// pacing durante los fotogramas siguientes.
const MAX_TIMER_LAG: Duration = Duration::from_millis(50);

/// Retraso **medido** con que se despiertan las esperas del pacing ([`send_paced`]): hora real de
/// despertar menos la pedida, media móvil (1/4 de peso a cada medida nueva). Sin carga es el
/// redondeo de los temporizadores (0-1 ms); con la CPU saturada (un juego que ocupa todos los
/// núcleos, un runner pequeño) sube a varios milisegundos, y entonces dormir para repartir los
/// últimos paquetes de un fotograma solo lo retrasa: los paquetes cuyo turno llega antes que ese
/// retraso salen sin esperar. Cuando un reparto entero sale sin dormir no hay medida nueva, así
/// que el retraso recordado se rebaja un octavo por reparto y la siguiente espera larga lo vuelve
/// a medir. Nada de esto depende del sistema ni del hardware: solo de lo que tardó la última espera.
#[derive(Debug, Default)]
pub struct TimerLag {
    /// Microsegundos.
    lag_us: AtomicU32,
}

impl TimerLag {
    /// Sin medidas: solo cuenta [`MIN_PACING_WAIT`].
    pub const fn new() -> Self {
        Self {
            lag_us: AtomicU32::new(0),
        }
    }

    /// Retraso medido.
    pub fn get(&self) -> Duration {
        Duration::from_micros(u64::from(self.lag_us.load(Ordering::Relaxed)))
    }

    /// Una espera pedida para `due` despertó en `woke`.
    pub fn observe(&self, due: tokio::time::Instant, woke: tokio::time::Instant) {
        let late = woke.saturating_duration_since(due).min(MAX_TIMER_LAG);
        let late = u32::try_from(late.as_micros()).unwrap_or(u32::MAX);
        let _ = self
            .lag_us
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |lag| {
                Some(lag - lag / 4 + late / 4)
            });
    }

    /// Un reparto entero salió sin dormir: no hubo medida, así que el retraso recordado se rebaja
    /// para volver a medirlo pronto.
    pub fn decay(&self) {
        let _ = self
            .lag_us
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |lag| {
                Some(lag - lag / 8)
            });
    }

    /// Si merece la pena dormir hasta `due` estando en `now`: solo cuando falta más que el retraso
    /// medido (y que [`MIN_PACING_WAIT`]); si no, el paquete sale ya.
    pub fn worth_waiting(&self, now: tokio::time::Instant, due: tokio::time::Instant) -> bool {
        due.saturating_duration_since(now) > self.get().max(MIN_PACING_WAIT)
    }
}

/// Envía `packets` por `conn` repartidos en `pacing`, en ráfagas de al menos [`MIN_PACING_WAIT`]
/// (los temporizadores no dan más resolución) y, con la CPU saturada, del retraso medido de las
/// esperas (`lag`, ver [`TimerLag`]). Devuelve los que salieron, o el primer error que no sea de
/// tamaño.
async fn send_paced(
    conn: &PeerConnection,
    packets: Vec<Bytes>,
    pacing: Duration,
    group: u32,
    lag: &TimerLag,
) -> Result<usize, TransportError> {
    let n = packets.len() as u32;
    let start = tokio::time::Instant::now();
    let mut sent = 0;
    let mut slept = false;
    for (i, packet) in packets.into_iter().enumerate() {
        let due = start + pacing * i as u32 / n.max(1);
        if lag.worth_waiting(tokio::time::Instant::now(), due) {
            tokio::time::sleep_until(due).await;
            lag.observe(due, tokio::time::Instant::now());
            slept = true;
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
    if !slept && n > 1 {
        lag.decay();
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

/// RTT con el invitado, en intervalos de fotograma, a partir del que todo grupo de vídeo de
/// [`FEC_FLOOR_MIN_OBJECTS`] o más objetos lleva al menos un símbolo de reparación aunque la
/// ventana de pérdida esté limpia: con un RTT largo, una pérdida aislada sin FEC cuesta un `Nack`
/// y su ida y vuelta, y como los grupos se entregan en orden, retiene a todos los que vienen
/// detrás (con 55 ms de RTT a 60 fps, cuatro fotogramas). El símbolo se descuenta del presupuesto
/// del codificador ([`MediaSender::encoder_target_kbps`]). El RTT es el medido con el invitado
/// ([`MediaSender::guest_rtt`]), no el de la conexión QUIC (por el relay sería anfitrión↔relay).
pub const FEC_FLOOR_RTT_INTERVALS: f64 = 2.0;

/// Objetos que tiene que tener un grupo para que el piso de FEC le añada un símbolo: con menos,
/// el símbolo costaría más de [`MAX_REPAIR_RATIO`] del grupo (uno solo, el 100 %).
pub const FEC_FLOOR_MIN_OBJECTS: usize = 4;

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

    /// `at` expresado en el reloj remoto y truncado a 32 bits, si el desfase ya es estable: con
    /// las [`CLOCK_SAMPLES`] muestras de `Pong`. Son las marcas de llegada del `Feedback`, de las
    /// que el emisor saca el retardo de ida y su cola: con el desfase de una o dos muestras
    /// (±jitter), la mediana se movía decenas de ms al llegar las siguientes y el control de tasa
    /// tomaba el salto por una cola que no existía (con 120 ms de RTT y 20 ms de jitter, el bitrate
    /// caía al mínimo y se quedaba ahí). Hasta entonces el `Feedback` va sin muestras y el control
    /// de tasa se guía por la pérdida y el caudal. El vídeo arranca sin esperar a esto.
    pub fn remote_us_i32(&self, at: Instant) -> Option<i32> {
        if self.samples.len() < CLOCK_SAMPLES {
            return None;
        }
        let offset = self.offset_us()?;
        Some((self.at_us(at) as i64).wrapping_add(offset) as u32 as i32)
    }
}

/// Configuración del emisor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MediaSenderConfig {
    /// Intervalo entre fotogramas (1/fps).
    pub frame_interval: Duration,
    /// Límites del control de tasa.
    pub rate: RateConfig,
    /// Reparto en el tiempo de cada fotograma.
    pub pacing: PacingConfig,
}

impl Default for MediaSenderConfig {
    fn default() -> Self {
        Self {
            frame_interval: Duration::from_micros(16_667),
            rate: RateConfig::default(),
            pacing: PacingConfig::default(),
        }
    }
}

/// `interval` en microsegundos, al menos 1 (un intervalo nulo anularía el pacing).
fn interval_us(interval: Duration) -> u64 {
    u64::try_from(interval.as_micros())
        .unwrap_or(u64::MAX)
        .max(1)
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
    /// Último `Feedback` procesado: el mismo llega dos veces (por el stream y en un datagrama de
    /// control) y la copia no se vuelve a aplicar.
    last_feedback: Option<Feedback>,
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
            last_feedback: None,
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
    /// Ritmo (kbit/s) al que salió cada tren de sonda, por identificador, para comprobar lo que
    /// informe el receptor ([`ProbeOutcome::checked_kbps`]).
    probe_paces: VecDeque<(u32, u32)>,
    /// Objetos de origen por fotograma de vídeo, media móvil (para descontar del presupuesto del
    /// codificador el símbolo de reparación mínimo).
    video_objects: f64,
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
    /// Intervalo entre fotogramas en microsegundos ([`MediaSender::set_frame_interval`]; al
    /// crear, el de `config`).
    frame_interval_us: AtomicU64,
    /// Identificador del siguiente tren de sonda.
    next_train: AtomicU32,
    /// Retraso medido de las esperas del pacing ([`TimerLag`]); compartido con los reenvíos.
    timer_lag: Arc<TimerLag>,
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
            frame_interval_us: AtomicU64::new(interval_us(config.frame_interval)),
            config,
            max_kbps: AtomicU32::new(0),
            next_train: AtomicU32::new(1),
            timer_lag: Arc::new(TimerLag::new()),
            state: Mutex::new(SenderState {
                history: VecDeque::with_capacity(HISTORY_GROUPS + 1),
                repair_ratio: INITIAL_REPAIR_RATIO,
                sources: Vec::new(),
                epoch_hint: 0,
                clock: Clock::with_origin(origin),
                sources_sent: 0,
                bytes_sent: 0,
                cumulative: VecDeque::new(),
                probe_paces: VecDeque::with_capacity(PROBE_PACES_REMEMBERED + 1),
                video_objects: 0.0,
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

    /// Cambia el intervalo entre fotogramas (1/fps) en marcha: el pacing de cada fotograma y el
    /// suelo de la FEC lo usan desde el siguiente.
    pub fn set_frame_interval(&self, interval: Duration) {
        self.frame_interval_us
            .store(interval_us(interval), Ordering::Release);
    }

    /// Intervalo entre fotogramas en uso (el de la configuración o el último de
    /// [`MediaSender::set_frame_interval`]).
    pub fn frame_interval(&self) -> Duration {
        Duration::from_micros(self.frame_interval_us.load(Ordering::Acquire))
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
    /// de FEC el camino recibía un 30 % más de lo que el control de tasa había decidido). El
    /// símbolo mínimo por grupo con RTT largo ([`MediaSender::fec_floor`]) también se descuenta,
    /// con la media de objetos por fotograma.
    pub fn encoder_target_kbps(&self) -> u32 {
        let target = f64::from(self.target_kbps());
        let mut ratio = self.repair_ratio();
        if self.fec_floor() {
            let objects = self.state.lock().map_or(0.0, |state| state.video_objects);
            if objects >= FEC_FLOOR_MIN_OBJECTS as f64 {
                ratio = ratio.max(1.0 / objects);
            }
        }
        ((target / (1.0 + ratio)).round() as u32).max(1)
    }

    /// Si los grupos de vídeo de [`FEC_FLOOR_MIN_OBJECTS`] o más objetos llevan al menos un
    /// símbolo de reparación: cuando el RTT medido con el invitado ([`MediaSender::guest_rtt`])
    /// pasa de [`FEC_FLOOR_RTT_INTERVALS`] intervalos de fotograma (con menos, el `Nack` llega a
    /// tiempo y la FEC solo gastaría bitrate). Sin medida, no.
    pub fn fec_floor(&self) -> bool {
        self.guest_rtt()
            .is_some_and(|rtt| rtt > self.frame_interval().mul_f64(FEC_FLOOR_RTT_INTERVALS))
    }

    /// RTT con el invitado que limita: en una conexión directa, el de QUIC; por el relay, el doble
    /// del retardo de ida base que ve su control de tasa (las marcas de llegada del `Feedback`,
    /// en el reloj del emisor estimado por `Ping`/`Pong`, dan ~RTT/2 sin cola), o `None` hasta
    /// que haya muestras.
    pub fn guest_rtt(&self) -> Option<Duration> {
        if self.conn.path() == crate::Path::Direct {
            return Some(self.conn.rtt());
        }
        let state = self.state.lock().ok()?;
        let base_us = state.limiting()?.rate.base_owd_us()?;
        Some(Duration::from_micros(
            u64::try_from(base_us.max(0)).unwrap_or(0) * 2,
        ))
    }

    /// Ritmo al que se reparte un fotograma, en kbit/s, y si el enlace midió holgura para los
    /// intras ([`pacing_pace`] con el objetivo actual y lo que midió la sonda del receptor que
    /// limita o la estimación de su control de tasa: repartir al objetivo cuando el enlace da
    /// varias veces más solo retrasa el fotograma). Sin ella, los intras se reparten en
    /// [`PacingConfig::keyframe_intervals`]. El objetivo ya lleva el máximo de
    /// [`MediaSender::set_max_kbps`]: bajarlo en marcha baja el ritmo mínimo del pacing al momento.
    pub fn pacing_plan(&self) -> (u32, bool) {
        let (pace, headroom, _) = self.pacing_plan_capped();
        (pace, headroom)
    }

    /// [`MediaSender::pacing_plan`] y si el ritmo es el techo de ráfaga del control de tasa
    /// ([`RateController::burst_cap_kbps`]): entonces nunca se pasa de él ni hay holgura para los
    /// intras. Informa al control de tasa del ritmo natural ([`RateController::note_pace`]).
    fn pacing_plan_capped(&self) -> (u32, bool, bool) {
        let target = self.target_kbps();
        let Ok(mut state) = self.state.lock() else {
            let (pace, headroom) = pacing_pace(target, None, 0.0, self.config.pacing.gain);
            return (pace, headroom, false);
        };
        let limiting = state
            .sources
            .iter()
            .min_by_key(|source| source.rate.target_kbps())
            .map(|source| {
                (
                    source.probe.and_then(|probe| probe.kbps()),
                    source.rate.estimate_kbps(),
                )
            });
        let Some((probe, estimate)) = limiting else {
            let (pace, headroom) = pacing_pace(target, None, 0.0, self.config.pacing.gain);
            return (pace, headroom, false);
        };
        let (pace, headroom) = pacing_pace(target, probe, estimate, self.config.pacing.gain);
        // Sin techo, un fotograma medio sale en `share` del intervalo: a `target / share` si eso
        // pasa del ritmo. Lo saben todos los receptores (en el relay, cada uno tiene su techo), y
        // el techo que manda es el más bajo.
        let natural = (f64::from(target) / self.config.pacing.share).max(f64::from(pace));
        let mut cap: Option<u32> = None;
        for source in &mut state.sources {
            source.rate.note_pace(natural.round() as u32);
            if let Some(source_cap) = source.rate.burst_cap_kbps() {
                cap = Some(cap.map_or(source_cap, |cap| cap.min(source_cap)));
            }
        }
        match cap {
            Some(cap) => (cap, false, true),
            _ => (pace, headroom, false),
        }
    }

    /// El ritmo de [`MediaSender::pacing_plan`].
    pub fn pacing_kbps(&self) -> u32 {
        self.pacing_plan().0
    }

    /// Si toca una sonda periódica para algún receptor ([`RateController::probe_due`]): el ritmo
    /// del tren. Sin techo se mide el enlace a [`probe_pace_kbps`] del máximo configurado.
    pub fn probe_due(&self) -> Option<u32> {
        let refresh = probe_pace_kbps(self.rate_config().max_kbps);
        let mut state = self.state.lock().ok()?;
        let now = Instant::now();
        state
            .sources
            .iter_mut()
            .filter_map(|source| source.rate.probe_due(now, refresh))
            .max()
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
        let pace_kbps = pace_kbps.clamp(1, PROBE_MAX_PACE_KBPS);
        if let Ok(mut state) = self.state.lock() {
            state.probe_paces.push_back((train, pace_kbps));
            while state.probe_paces.len() > PROBE_PACES_REMEMBERED {
                state.probe_paces.pop_front();
            }
        }
        let pacing = Duration::from_secs_f64(bytes as f64 * 8.0 / (f64::from(pace_kbps) * 1000.0));
        send_paced(&self.conn, packets, pacing, train, &self.timer_lag).await?;
        Ok(train)
    }

    /// Anota el resultado de un tren de sonda del receptor `source` y, si es el mejor que ha
    /// informado, arranca su control de tasa en [`PROBE_START_SHARE`] de lo medido (solo mientras
    /// no haya llegado ningún `Feedback`: después manda la red en marcha). Devuelve lo medido en
    /// kbit/s, acotado al ritmo al que salió el tren ([`ProbeOutcome::checked_kbps`]); `None` si
    /// el resultado no vale (más de [`PROBE_PACE_TOLERANCE`] veces ese ritmo: el receptor leyó el
    /// tren de golpe, no midió el camino).
    pub fn on_probe_result(&self, source: u64, outcome: ProbeOutcome) -> Option<u32> {
        let mut state = self.state.lock().ok()?;
        let pace = state
            .probe_paces
            .iter()
            .find(|(train, _)| *train == outcome.train)
            .map(|(_, pace)| *pace);
        let Some(kbps) = outcome.checked_kbps(pace) else {
            debug!(
                train = outcome.train,
                raw_kbps = outcome.kbps(),
                pace_kbps = pace,
                "tren de sonda descartado: midió muy por encima de su ritmo de salida"
            );
            return None;
        };
        let outcome = ProbeOutcome {
            // Lo que se guarda ya es coherente con el ritmo de salida.
            span: span_for(outcome, kbps),
            ..outcome
        };
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
        // El control de tasa decide con el tren: techo al arrancar si perdió, subida del techo o
        // medida del enlace para dejar crecer el objetivo.
        entry.rate.on_probe(
            Instant::now(),
            pace.unwrap_or(kbps),
            Some(kbps),
            outcome.lost(),
            outcome.sent,
        );
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

        let mut repair_count = if ratio > 0.0 {
            ((count as f64 * ratio).ceil() as usize)
                .min(count)
                .min(usize::from(u16::MAX))
        } else {
            0
        };
        if track == Track::Video {
            repair_count = floor_repairs(count, repair_count, self.fec_floor());
        }
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
            if track == Track::Video {
                state.video_objects = if state.video_objects < 1.0 {
                    count as f64
                } else {
                    state.video_objects * 0.9 + count as f64 * 0.1
                };
            }
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
        let (pace_kbps, headroom, capped) = self.pacing_plan_capped();
        let pacing = pacing_duration(
            report.bytes,
            pace_kbps,
            self.frame_interval(),
            keyframe,
            headroom,
            capped,
            &self.config.pacing,
        );
        let all: Vec<Bytes> = sources.into_iter().chain(repairs).collect();
        send_paced(&self.conn, all, pacing, group, &self.timer_lag).await?;
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
            let (pace_kbps, headroom, capped) = self.pacing_plan_capped();
            let pacing = pacing_duration(
                bytes,
                pace_kbps,
                self.frame_interval(),
                true,
                headroom,
                capped,
                &self.config.pacing,
            );
            let conn = self.conn.clone();
            let lag = Arc::clone(&self.timer_lag);
            let count = packets.len();
            runtime.spawn(async move {
                if let Err(err) = send_paced(&conn, packets, pacing, group, &lag).await {
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
        // El mismo informe llega dos veces (datagrama y stream) y, si el datagrama de uno se
        // perdió, su copia del stream llega después de informes más nuevos: solo se aplica lo que
        // avanza (los contadores son acumulados). Aplicar uno viejo hacía retroceder `last_received`
        // y el siguiente informe parecía entregar el doble.
        if let Some(last) = &entry.last_feedback
            && (last == feedback
                || feedback.received_bytes < last.received_bytes
                || feedback.highest_group < last.highest_group)
        {
            return state
                .limiting()
                .map_or(self.rate_config().start_kbps, |s| s.rate.target_kbps());
        }
        entry.last_feedback = Some(feedback.clone());
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

/// Símbolos de reparación de un grupo de vídeo de `count` objetos: los `repairs` del ratio de
/// pérdida y, con el piso de FEC activo ([`FEC_FLOOR_RTT_INTERVALS`]), al menos uno si el grupo
/// tiene [`FEC_FLOOR_MIN_OBJECTS`] o más objetos (con menos, un símbolo pasaría de
/// [`MAX_REPAIR_RATIO`] del grupo).
fn floor_repairs(count: usize, repairs: usize, floor: bool) -> usize {
    if floor && count >= FEC_FLOOR_MIN_OBJECTS {
        repairs.max(1).min(count)
    } else {
        repairs
    }
}

/// Dispersión que corresponde a `kbps` para los bytes de `outcome` (la inversa de
/// [`ProbeOutcome::kbps`]); la suya si ya coincide.
fn span_for(outcome: ProbeOutcome, kbps: u32) -> Duration {
    if outcome.kbps() == Some(kbps) || outcome.received < 2 || kbps == 0 {
        return outcome.span;
    }
    let bytes =
        f64::from(outcome.bytes) * f64::from(outcome.received - 1) / f64::from(outcome.received);
    Duration::from_secs_f64(bytes * 8.0 / 1000.0 / f64::from(kbps))
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
    /// Llegada del primer objeto del grupo (el reensamblado dura hasta `received_at`).
    pub first_arrival: Instant,
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
        // Los mínimos (3 y 2 ms) ceden ante los máximos cuando el intervalo es corto: a 240 fps
        // (4,17 ms) medio intervalo son 2,08 ms y un cuarto, 1,04 ms, y `clamp` con el mínimo por
        // encima del máximo entra en pánico (desde 167 fps el silencio y desde 126 fps la
        // reordenación, en cuanto faltaba un paquete).
        let quiet_max = interval / 2;
        let quiet = (spacing * 3).clamp(Duration::from_millis(3).min(quiet_max), quiet_max);
        self.quiet_at = Some(now + quiet);
        self.nack_at = self.nack_at.max(now + quiet);
        if let Some(hole) = self.hole_at {
            let reorder_max = interval / 4;
            let reorder =
                (spacing * 2).clamp(Duration::from_millis(2).min(reorder_max), reorder_max);
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
                    first_arrival: state.first,
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
    /// Origen del reloj local (copia de `Clock::origin`, que no cambia).
    origin: Instant,
    /// Último desfase `reloj del emisor − reloj local` en µs ([`NO_OFFSET`] mientras no hay),
    /// para leerlo sin esperar al cerrojo del receptor (lo lee el hilo de entrada por cada paquete).
    offset_us: AtomicI64,
    /// Último `Pong` del emisor, en µs desde `origin` ([`MediaReceiver::last_pong`]).
    last_pong_us: AtomicI64,
    /// Etiqueta de los datagramas de control que envía ([`MediaReceiver::set_control_tag`]).
    control_tag: AtomicU32,
}

/// Valor de `MediaReceiver::offset_us` mientras no hay desfase.
const NO_OFFSET: i64 = i64::MIN;

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
        let clock = Clock::new();
        Self {
            conn,
            control,
            origin: clock.origin(),
            offset_us: AtomicI64::new(NO_OFFSET),
            last_pong_us: AtomicI64::new(-1),
            control_tag: AtomicU32::new(0),
            inner: Mutex::new(ReceiverInner {
                core: ReceiverCore::new(config.frame_interval),
                clock,
                next_feedback: now + FEEDBACK_INTERVAL,
                next_ping: now,
            }),
        }
    }

    /// Desfase publicado por el último `Pong`, sin cerrojo.
    fn atomic_offset(&self) -> Option<i64> {
        let offset = self.offset_us.load(Ordering::Acquire);
        (offset != NO_OFFSET).then_some(offset)
    }

    /// Cuándo llegó el último `Pong` del emisor (ninguno aún: `None`). Con los `Ping` de cada
    /// segundo, varios segundos sin `Pong` ni datagramas son un camino muerto.
    pub fn last_pong(&self) -> Option<Instant> {
        let us = self.last_pong_us.load(Ordering::Acquire);
        u64::try_from(us)
            .ok()
            .map(|us| self.origin + Duration::from_micros(us))
    }

    /// Etiqueta que llevan los datagramas de control de este receptor (el `group` de su
    /// cabecera): detrás del relay, el anfitrión recibe los de todos los invitados por una misma
    /// conexión y con ella atribuye cada `Feedback` a su invitado (su id de par).
    pub fn set_control_tag(&self, tag: u32) {
        self.control_tag.store(tag, Ordering::Release);
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
        self.atomic_offset()
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
        let offset = self.atomic_offset()?;
        let local_us = at.saturating_duration_since(self.origin).as_micros() as i64;
        u64::try_from(local_us.checked_add(offset)?).ok()
    }

    /// Atiende `Ping` y `Pong`; devuelve los demás mensajes.
    pub fn handle_control(&self, message: ControlMessage) -> Option<ControlMessage> {
        match message {
            ControlMessage::Pong { t, t_recv } => {
                let now = Instant::now();
                self.last_pong_us.store(
                    i64::try_from(now.saturating_duration_since(self.origin).as_micros())
                        .unwrap_or(i64::MAX),
                    Ordering::Release,
                );
                if let Ok(mut inner) = self.inner.lock() {
                    inner.clock.on_pong(now, t, t_recv);
                    self.offset_us.store(
                        inner.clock.offset_us().unwrap_or(NO_OFFSET),
                        Ordering::Release,
                    );
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
                datagram = self.conn.read_datagram_timed() => {
                    // El instante es el de la llegada a `quinn`, no el de ahora: si esta tarea
                    // tardó en leer, las sondas y las muestras de OWD siguen midiendo el camino.
                    let (now, datagram) = datagram?;
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

    /// Envía un mensaje por el stream de control y, si corre prisa (`Nack`, `KeyframeRequest`,
    /// `Feedback`), una copia en un datagrama de control: si el paquete del stream se pierde, QUIC
    /// no lo reenvía hasta su PTO (60-70 ms con un RTT de 20 ms) y lo que viene detrás espera con
    /// él. El datagrama lleva la etiqueta de [`MediaReceiver::set_control_tag`].
    pub fn send_control(&self, message: &ControlMessage) {
        if let Err(err) = self.control.send(message) {
            debug!(error = %err, "no se pudo enviar un mensaje de control");
        }
        let tag = self.control_tag.load(Ordering::Acquire);
        if let Some(datagram) = coroom_proto::control::control_datagram_tagged(message, tag)
            && let Err(err) = self.conn.send_datagram(Bytes::from(datagram))
        {
            debug!(error = %err, "no se pudo enviar un datagrama de control");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El intervalo entre fotogramas arranca con el de la configuración y cambia en marcha;
    /// nunca queda en cero.
    #[test]
    fn the_frame_interval_changes_while_running() {
        assert_eq!(interval_us(Duration::from_micros(16_667)), 16_667);
        assert_eq!(interval_us(Duration::ZERO), 1, "nunca cero");
        assert_eq!(interval_us(Duration::from_secs(1) / 15), 66_666);
    }

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

    /// Un tren que el receptor saca de su cola de golpe (antes de que se leyera nada, los
    /// 64 datagramas se apilaban y se marcaban al leerlos) mide Gbit/s: con el ritmo al que salió
    /// se descarta, y uno que mide algo por encima de ese ritmo (jitter de los temporizadores)
    /// se acota a él. Un tren que mide por debajo del ritmo de salida es el camino y vale tal cual.
    #[test]
    fn a_train_drained_in_a_burst_is_rejected_and_a_fast_one_is_clamped() {
        let mut core = ReceiverCore::new(Duration::from_millis(16));
        let start = Instant::now();
        // Los 64 datagramas leídos uno tras otro, a 1 µs.
        for index in 0..PROBE_PACKETS {
            let at = start + Duration::from_micros(u64::from(index));
            core.push(&probe_datagram(1, index, PROBE_PACKETS), at, None);
        }
        let drained = core.last_probe().expect("tren informado");
        let raw = drained.kbps().expect("dos o más datagramas");
        // 63 × 1016 B en 63 µs: más de 8 Gbit/s.
        assert!(raw > 1_000_000, "{raw} kbit/s");
        // El emisor sabe que el tren salió a 100 Mbit/s: la medida no vale.
        assert_eq!(drained.checked_kbps(Some(PROBE_MAX_PACE_KBPS)), None);
        // Sin ritmo conocido no hay con qué comparar.
        assert_eq!(drained.checked_kbps(None), Some(raw));

        // Dispersión algo menor que la de salida: vale el ritmo de salida.
        let fast = ProbeOutcome {
            train: 2,
            sent: PROBE_PACKETS,
            received: PROBE_PACKETS,
            bytes: 64 * 1_200,
            span: Duration::from_micros(4_000),
        };
        let fast_raw = fast.kbps().expect("medible");
        assert!(fast_raw > 100_000 && fast_raw < 200_000, "{fast_raw}");
        assert_eq!(fast.checked_kbps(Some(100_000)), Some(100_000));
        assert_eq!(span_for(fast, 100_000), Duration::from_micros(6_048));

        // Un enlace de 20 Mbit/s dispersa el tren a 20 Mbit/s: se acepta tal cual.
        let slow = ProbeOutcome {
            train: 3,
            sent: PROBE_PACKETS,
            received: PROBE_PACKETS,
            bytes: 64 * 1_200,
            span: Duration::from_micros(30_240),
        };
        assert_eq!(slow.checked_kbps(Some(100_000)), Some(20_000));
        assert_eq!(span_for(slow, 20_000), slow.span);
    }

    /// Un tren entero a [`PROBE_MAX_PACE_KBPS`] dura varias veces la resolución de los
    /// temporizadores (0,5 ms), así la dispersión medida es la del camino y no la del reloj.
    #[test]
    fn a_full_probe_train_spans_several_milliseconds() {
        let bytes = usize::from(PROBE_PACKETS) * ObjectHeader::MAX_PACKET;
        let span = bytes as f64 * 8.0 / (f64::from(PROBE_MAX_PACE_KBPS) * 1000.0);
        assert!(span >= 0.005, "{span:.4} s");
    }

    /// El pacing reparte el fotograma al ritmo medido, acotado por la parte del intervalo
    /// configurada; con el enlace rápido el fotograma sale enseguida y no se estira al bitrate
    /// objetivo.
    #[test]
    fn pacing_follows_the_measured_rate_and_the_share_of_the_interval() {
        let pacing = PacingConfig::default();
        // 40 KB a 70 Mbit/s: 4,6 ms, por debajo del 30 % de 16,7 ms.
        let spread = pacing_duration(40_000, 70_000, INTERVAL, false, false, false, &pacing);
        assert!((spread.as_micros() as i64 - 4_571).abs() < 20, "{spread:?}");
        // 40 KB a 10 Mbit/s: 32 ms, acotado al 30 % del intervalo.
        assert_eq!(
            pacing_duration(40_000, 10_000, INTERVAL, false, false, false, &pacing),
            INTERVAL.mul_f64(FRAME_PACING_SHARE)
        );
        // Un intra de 300 KB a 70 Mbit/s: 34 ms. Sin holgura medida, dos intervalos (lo que
        // tolera el receptor); con ella, uno.
        assert_eq!(
            pacing_duration(300_000, 70_000, INTERVAL, true, false, false, &pacing),
            INTERVAL * 2
        );
        assert_eq!(
            pacing_duration(300_000, 70_000, INTERVAL, true, true, false, &pacing),
            INTERVAL
        );
        // La parte es configurable (A/B con `coroom host`).
        let wide = PacingConfig {
            share: 0.8,
            keyframe_intervals: 1.5,
            ..pacing
        };
        assert_eq!(
            pacing_duration(40_000, 10_000, INTERVAL, false, false, false, &wide),
            INTERVAL.mul_f64(0.8)
        );
        assert_eq!(
            pacing_duration(300_000, 70_000, INTERVAL, true, false, false, &wide),
            INTERVAL.mul_f64(1.5)
        );
        // El receptor tolera hasta KEYFRAME_PACING_INTERVALS: el emisor nunca configura más.
        assert!(pacing.keyframe_intervals <= f64::from(KEYFRAME_PACING_INTERVALS));
        assert!(pacing.keyframe_intervals_measured <= pacing.keyframe_intervals);
    }

    /// El ritmo del pacing sigue a lo que midió la sonda aunque supere el bitrate máximo del
    /// usuario (10 Mbit/s en todos los casos): con la sonda a 100 Mbit/s, 70 Mbit/s; sin sonda ni
    /// estimación, `gain × objetivo` como siempre; con una medida por encima del techo, el techo.
    /// Con esos ritmos, un fotograma del tamaño medio al máximo deja de recortarse al `share`.
    #[test]
    fn the_pacing_pace_follows_the_probe_above_the_bitrate_cap() {
        let gain = PACING_GAIN;
        // Lo medido en la PC de referencia: 1080p60 y 720p30 con 10 Mbit/s de máximo, sonda y
        // objetivo acotados al máximo → 12,5 Mbit/s, y cada fotograma recortado al `share`.
        assert_eq!(pacing_pace(10_000, None, 0.0, gain), (12_500, false));
        assert_eq!(pacing_pace(10_000, None, 10_000.0, gain), (12_500, false));
        assert_eq!(
            pacing_pace(10_000, Some(10_000), 10_000.0, gain),
            (12_500, false)
        );
        // Con la sonda a 100 Mbit/s (bucle local, LAN), el 70 %: 70 Mbit/s, y holgura para los
        // intras (70 ≥ 2 × 12,5).
        assert_eq!(
            pacing_pace(10_000, Some(100_000), 0.0, gain),
            (70_000, true)
        );
        // Una medida por encima del techo no pasa de él (la sonda nunca sale más rápido, así que
        // solo puede venir de la estimación o de un tren que midió de más).
        assert_eq!(
            pacing_pace(10_000, Some(200_000), 0.0, gain).0,
            PROBE_MAX_PACE_KBPS
        );
        assert_eq!(
            pacing_pace(10_000, None, 150_000.0, gain).0,
            PROBE_MAX_PACE_KBPS
        );
        // La estimación del control de tasa cuenta igual que la sonda.
        assert_eq!(pacing_pace(10_000, None, 30_000.0, gain), (30_000, true));
        assert_eq!(pacing_pace(10_000, None, 20_000.0, gain), (20_000, false));
        // Máximo cambiado en caliente: el objetivo baja a 4 Mbit/s. Sin medida, el ritmo lo
        // sigue (5 Mbit/s); con la sonda, manda la sonda (el enlace no cambió).
        assert_eq!(pacing_pace(4_000, None, 0.0, gain), (5_000, false));
        assert_eq!(pacing_pace(4_000, Some(100_000), 0.0, gain), (70_000, true));

        let pacing = PacingConfig::default();
        // 1080p60 a 10 Mbit/s: ~21 KB por fotograma. A 12,5 Mbit/s tardaría 13 ms y se recorta
        // al 30 % del intervalo (5 ms, lo medido); a 70 Mbit/s, 2,4 ms, por debajo del recorte.
        let interval_60 = INTERVAL;
        assert_eq!(
            pacing_duration(21_000, 12_500, interval_60, false, false, false, &pacing),
            interval_60.mul_f64(FRAME_PACING_SHARE)
        );
        let spread = pacing_duration(21_000, 70_000, interval_60, false, false, false, &pacing);
        assert!((spread.as_micros() as i64 - 2_400).abs() < 20, "{spread:?}");
        assert!(spread < interval_60.mul_f64(FRAME_PACING_SHARE));
        // 720p30 a 10 Mbit/s: ~42 KB por fotograma en un intervalo de 33 ms. A 12,5 Mbit/s,
        // 27 ms recortados al 30 % (10 ms, lo medido con el paquete 1.6.15); a 70 Mbit/s, 4,8 ms.
        let interval_30 = Duration::from_micros(33_333);
        let clipped = pacing_duration(42_000, 12_500, interval_30, false, false, false, &pacing);
        assert_eq!(clipped, interval_30.mul_f64(FRAME_PACING_SHARE));
        assert!(
            (clipped.as_micros() as i64 - 10_000).abs() < 20,
            "{clipped:?}"
        );
        let spread = pacing_duration(42_000, 70_000, interval_30, false, false, false, &pacing);
        assert!((spread.as_micros() as i64 - 4_800).abs() < 20, "{spread:?}");
        // Un intra con holgura: si cabe en un intervalo al ritmo medido sale a ese ritmo (nunca
        // por encima del techo); si no cabe, en un intervalo, como antes.
        let spread = pacing_duration(120_000, 70_000, interval_60, true, true, false, &pacing);
        assert!(
            (spread.as_micros() as i64 - 13_714).abs() < 20,
            "{spread:?}"
        );
        assert_eq!(
            pacing_duration(
                300_000,
                PROBE_MAX_PACE_KBPS,
                interval_60,
                true,
                true,
                false,
                &pacing
            ),
            interval_60
        );
    }

    /// El retraso de las esperas se mide y manda: sin medidas solo cuenta el medio milisegundo de
    /// los temporizadores; con esperas que despiertan 3 ms tarde, los paquetes a menos de 3 ms
    /// salen sin dormir; sin dormir no hay medida y el retraso recordado baja hasta volver a medir.
    #[test]
    fn the_measured_timer_lag_decides_when_waiting_is_worth_it() {
        let lag = TimerLag::new();
        let now = tokio::time::Instant::now();
        assert_eq!(lag.get(), Duration::ZERO);
        assert!(!lag.worth_waiting(now, now + Duration::from_micros(400)));
        assert!(lag.worth_waiting(now, now + Duration::from_micros(600)));
        // Cuatro esperas de 3 ms tarde: la media móvil se acerca (1 − (3/4)^4 ≈ 68 %).
        for _ in 0..4 {
            lag.observe(now, now + Duration::from_millis(3));
        }
        let measured = lag.get();
        assert!(
            measured > Duration::from_micros(2_000) && measured < Duration::from_millis(3),
            "{measured:?}"
        );
        assert!(!lag.worth_waiting(now, now + Duration::from_millis(2)));
        assert!(lag.worth_waiting(now, now + Duration::from_millis(4)));
        // Despertar a tiempo (o antes) no suma.
        lag.observe(now + Duration::from_millis(1), now);
        assert!(lag.get() < measured);
        // Un reparto sin esperas rebaja un octavo; tras unos cuantos vuelve al medio milisegundo.
        let before = lag.get();
        lag.decay();
        assert!(lag.get() < before && lag.get() > before.mul_f64(0.8));
        for _ in 0..40 {
            lag.decay();
        }
        assert!(lag.worth_waiting(now, now + Duration::from_micros(600)));
        // Una pausa enorme no se recuerda entera.
        for _ in 0..50 {
            lag.observe(now, now + Duration::from_secs(1));
        }
        assert!(lag.get() <= MAX_TIMER_LAG);
    }

    /// Los trenes de la sonda salen a [`PROBE_PACE_GAIN`] veces el máximo del usuario, hasta el
    /// techo, y el factor es el que hace falta para que un fotograma al máximo quede en la mitad
    /// del `share`; la comprobación del resultado contra el ritmo de salida sigue acotando.
    #[test]
    fn probe_trains_leave_at_a_multiple_of_the_cap_up_to_the_ceiling() {
        assert_eq!(probe_pace_kbps(10_000), 100_000);
        assert_eq!(probe_pace_kbps(5_000), 50_000);
        assert_eq!(probe_pace_kbps(50_000), PROBE_MAX_PACE_KBPS);
        assert_eq!(probe_pace_kbps(200_000), PROBE_MAX_PACE_KBPS);
        assert_eq!(probe_pace_kbps(0), PROBE_PACE_GAIN);
        assert_eq!(probe_pace_kbps(u32::MAX), PROBE_MAX_PACE_KBPS);
        // El porqué del factor: 1 / (PROBE_START_SHARE × FRAME_PACING_SHARE / 2) = 9,5.
        let needed = 1.0 / (PROBE_START_SHARE * FRAME_PACING_SHARE / 2.0);
        assert!(f64::from(PROBE_PACE_GAIN) >= needed, "{needed}");
        assert!(f64::from(PROBE_PACE_GAIN) < needed + 1.0, "{needed}");
        // El tren a 100 Mbit/s: medir 110 Mbit/s (jitter de los temporizadores) vale 100; medir
        // más del doble no vale.
        let outcome = |kbps: u32| ProbeOutcome {
            train: 1,
            sent: PROBE_PACKETS,
            received: PROBE_PACKETS,
            bytes: 76_800,
            span: Duration::from_secs_f64(75_600.0 * 8.0 / 1000.0 / f64::from(kbps)),
        };
        let pace = Some(probe_pace_kbps(10_000));
        assert_eq!(outcome(110_000).checked_kbps(pace), Some(100_000));
        assert_eq!(outcome(60_000).checked_kbps(pace), Some(60_000));
        assert_eq!(outcome(250_000).checked_kbps(pace), None);
    }

    /// El piso de FEC añade un símbolo solo a los grupos con objetos de sobra para que no pase de
    /// MAX_REPAIR_RATIO; con uno o dos objetos (poco bitrate) no dobla el gasto.
    #[test]
    fn the_fec_floor_only_adds_a_symbol_to_groups_of_four_or_more() {
        assert_eq!(floor_repairs(1, 0, true), 0);
        assert_eq!(floor_repairs(3, 0, true), 0);
        assert_eq!(floor_repairs(4, 0, true), 1);
        assert_eq!(floor_repairs(40, 0, true), 1);
        assert_eq!(floor_repairs(40, 12, true), 12);
        assert_eq!(floor_repairs(40, 0, false), 0);
        assert!(1.0 / FEC_FLOOR_MIN_OBJECTS as f64 <= MAX_REPAIR_RATIO);
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

    /// A 240 fps (el intervalo mínimo de una pista) medio intervalo y un cuarto quedan por debajo
    /// de los mínimos de silencio y de reordenación (3 y 2 ms): antes, `clamp` entraba en pánico
    /// en el hilo de red del invitado en cuanto llegaba un paquete con un hueco por debajo.
    #[test]
    fn a_hole_at_240_fps_is_nacked_without_panicking() {
        let mut core = ReceiverCore::new(MIN_TRACK_INTERVAL);
        let start = Instant::now();
        let (sources, _) = packetize(5, &payload(5000, 4), 1184, 0);
        for (index, packet) in sources.iter().enumerate().skip(1) {
            core.push(
                packet,
                start + Duration::from_micros(100 * index as u64),
                None,
            );
        }
        core.poll(start + Duration::from_millis(3), Duration::from_millis(1));
        assert_eq!(
            core.take_messages(),
            vec![ControlMessage::Nack {
                track: Track::Video,
                group: 5,
                missing: vec![0]
            }],
            "el hueco se pide antes del intervalo de 4,17 ms"
        );
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
        // Con una sola muestra el desfase sirve para mostrar, no para las marcas del Feedback.
        assert_eq!(
            clock.remote_us_i32(base + Duration::from_micros(100_000)),
            None
        );
        for i in 1..CLOCK_SAMPLES as u64 {
            let t = 1_000 + i * 100_000;
            clock.on_pong(
                base + Duration::from_micros(t + 20_000),
                t,
                t + 10_000 + 50_000,
            );
        }
        let remote = clock.remote_us_i32(base + Duration::from_micros(100_000));
        assert_eq!(remote, Some(150_000));
    }

    /// El pacing reparte cada fotograma al ritmo que se le da (el medido), sin pasar de la parte
    /// del intervalo configurada (un intervalo en un intra): a 120 fps y 20 Mbit/s, un intra de
    /// 400 KB ya no sale en 4 ms.
    #[test]
    fn pacing_follows_the_rate_and_is_capped_per_frame() {
        let interval = Duration::from_micros(8_333);
        let pacing = PacingConfig::default();
        // 2 KB a 20 Mbit/s: 0,8 ms.
        let normal = pacing_duration(2_000, 20_000, interval, false, false, false, &pacing);
        assert!((normal.as_secs_f64() - 0.0008).abs() < 1e-6, "{normal:?}");
        // 100 KB serían 40 ms: se recorta a la parte del intervalo.
        assert_eq!(
            pacing_duration(100_000, 20_000, interval, false, false, false, &pacing),
            interval.mul_f64(FRAME_PACING_SHARE)
        );
        // Un intra de 400 KB: dos intervalos (16,7 ms) en vez de los 4 ms de antes.
        assert_eq!(
            pacing_duration(400_000, 20_000, interval, true, false, false, &pacing),
            interval * 2
        );
        // Lo pequeño apenas espera; un ritmo de 0 no divide por cero.
        assert!(
            pacing_duration(1_200, 80_000, interval, false, false, false, &pacing)
                < Duration::from_micros(150)
        );
        assert_eq!(
            pacing_duration(1_200, 0, interval, false, false, false, &pacing),
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
