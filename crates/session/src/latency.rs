//! Marcas de tiempo por etapa y su resumen por percentiles.
//!
//! Cada fotograma lleva una marca [`Instant`] por [`Stage`]. El [`LatencyLedger`] guarda las de los
//! últimos [`LatencyLedger::WINDOW`] fotogramas y, con [`LatencyLedger::summary`], calcula para cada
//! tramo entre etapas consecutivas y para el total `Captured → Presented` los percentiles 50, 95 y 99
//! y el máximo (percentil por rango más cercano), además de descartes, fps y bitrate real. Lo usan
//! `coroom probe`, `coroom join`, el overlay y las puertas de latencia de los tests.
//!
//! Hay ocho etapas: el cifrado SFrame (`Encrypted`) y el descifrado (`Decrypted`)
//! tienen la suya. En el bucle local no se cifra y esas dos marcas coinciden con la
//! anterior (tramo de 0). En la sesión remota, el invitado recibe las marcas del anfitrión
//! (`Captured`, `Encoded`, `Encrypted`, `Sent`) por el control y las sitúa en su reloj con el
//! desfase de `Ping`/`Pong`; el tramo `Sent → Received` incluye por eso el error de esa estimación
//! (la mitad de la asimetría del camino). La sesión remota añade el resumen del enlace
//! ([`LinkSummary`]).
//!
//! El registro cuenta también las **copias de la imagen en memoria de CPU** por
//! etapa ([`LatencyLedger::record_cpu_copies`]): captura (memoria compartida o *staging*),
//! codificación (conversión de color y subida a la GPU), decodificación (bajada de la GPU y copia
//! al búfer de salida) y presentación (subida de los planos). En la ruta sin copias de hardware
//! deben ser 0; [`LatencySummary::cpu_copies`] da la media por fotograma de cada etapa. En la
//! sesión remota cada extremo cuenta las suyas (el anfitrión, captura y codificación; el invitado,
//! decodificación y presentación).
//!
//! Registra también el **audio** ([`AudioStage`], [`LatencyLedger::record_audio`]):
//! por pista (juego o voz) las marcas de cada trama en cada extremo. El emisor anota captura,
//! procesado (voz), codificado, cifrado y enviado; el receptor, la captura del emisor (su
//! `capture_us` convertido con el reloj de la sesión), recibido, decodificado (al salir del buffer de
//! jitter) y la hora estimada a la que suena. El total `captura → suena` del receptor es el
//! **boca-oído**; [`AudioSummary`] lo da con el desglose por tramo, |A−V| (sincronía del juego con
//! el vídeo), pérdidas, PLC, FEC y DRED, el nivel de jitter y quién habla.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::Serialize;

/// Número de etapas.
pub const STAGES: usize = 8;

/// Etapa del pipeline de vídeo, en su orden fijo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum Stage {
    /// El backend entregó el fotograma (instante del evento de captura).
    Captured,
    /// El codificador devolvió el fotograma comprimido.
    Encoded,
    /// SFrame cifró el fotograma.
    Encrypted,
    /// Salió el último datagrama del fotograma.
    Sent,
    /// El reensamblador completó el fotograma.
    Received,
    /// SFrame lo descifró y autenticó.
    Decrypted,
    /// El decodificador devolvió la imagen.
    Decoded,
    /// El presentador terminó `present`.
    Presented,
}

impl Stage {
    /// Todas las etapas en orden.
    pub const ALL: [Stage; STAGES] = [
        Stage::Captured,
        Stage::Encoded,
        Stage::Encrypted,
        Stage::Sent,
        Stage::Received,
        Stage::Decrypted,
        Stage::Decoded,
        Stage::Presented,
    ];

    /// Posición de la etapa en [`Stage::ALL`].
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Nombre de la etapa en español, para tablas y overlay.
    pub const fn label(self) -> &'static str {
        match self {
            Stage::Captured => "captura",
            Stage::Encoded => "codificado",
            Stage::Encrypted => "cifrado",
            Stage::Sent => "enviado",
            Stage::Received => "recibido",
            Stage::Decrypted => "descifrado",
            Stage::Decoded => "decodificado",
            Stage::Presented => "presentado",
        }
    }
}

/// Marcas de tiempo de un fotograma.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTimeline {
    /// Identificador del fotograma.
    pub frame_id: u64,
    /// Marca de cada etapa, indexada por [`Stage::index`].
    pub marks: [Option<Instant>; STAGES],
}

impl FrameTimeline {
    /// Marca de una etapa.
    pub fn at(&self, stage: Stage) -> Option<Instant> {
        self.marks[stage.index()]
    }

    /// Indica si el fotograma tiene las marcas de todas las etapas.
    pub fn is_complete(&self) -> bool {
        self.marks.iter().all(Option::is_some)
    }

    /// Tiempo entre dos etapas, si ambas están marcadas (0 si la segunda se marcó antes).
    pub fn span(&self, from: Stage, to: Stage) -> Option<Duration> {
        let (from, to) = (self.at(from)?, self.at(to)?);
        Some(to.saturating_duration_since(from))
    }
}

/// Percentiles de un tramo, en microsegundos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SpanStats {
    /// Etapa de partida.
    pub from: Stage,
    /// Etapa de llegada.
    pub to: Stage,
    /// Fotogramas con ambas marcas.
    pub samples: usize,
    /// Mediana.
    pub p50_us: u64,
    /// Percentil 95.
    pub p95_us: u64,
    /// Percentil 99.
    pub p99_us: u64,
    /// Máximo.
    pub max_us: u64,
}

/// Descartes en una etapa: fotogramas que no llegaron a ella.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StageDrops {
    /// Etapa a la que no llegaron.
    pub stage: Stage,
    /// Número de fotogramas.
    pub count: u64,
}

/// Copias de la imagen en memoria de CPU en una etapa.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct StageCopies {
    /// Etapa que copia.
    pub stage: Stage,
    /// Fotogramas contados.
    pub frames: u64,
    /// Copias en total.
    pub total: u64,
    /// Copias por fotograma (media).
    pub per_frame: f64,
}

/// Resumen de la ventana de medida.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LatencySummary {
    /// Fotogramas en la ventana (como mucho [`LatencyLedger::WINDOW`]).
    pub frames_in_window: usize,
    /// Fotogramas capturados desde el principio.
    pub frames_captured: u64,
    /// Fotogramas presentados desde el principio.
    pub frames_presented: u64,
    /// Tramos entre etapas consecutivas, en orden.
    pub spans: Vec<SpanStats>,
    /// Tramo total `Captured → Presented`.
    pub total: SpanStats,
    /// Descartes por etapa desde el principio, en el orden de [`Stage::ALL`].
    pub drops: Vec<StageDrops>,
    /// Suma de los descartes.
    pub drops_total: u64,
    /// Fotogramas que el decodificador rechazó desde el principio (corruptos o sin referencias).
    pub decode_errors: u64,
    /// Fotogramas presentados por segundo en la ventana.
    pub fps: f64,
    /// Bitrate codificado real en la ventana, en kbit/s.
    pub bitrate_kbps: f64,
    /// Códec y backends en uso (`AV1 · SVT-AV1 → dav1d`), si se conocen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video: Option<String>,
    /// Copias a CPU de las etapas que copian (captura, codificado, decodificado, presentado) que
    /// han contado algún fotograma, en el orden de [`Stage::ALL`].
    pub cpu_copies: Vec<StageCopies>,
    /// Suma de las copias por fotograma de todas las etapas (0 en la ruta sin copias).
    pub cpu_copies_per_frame: f64,
    /// Enlace con el anfitrión (solo en la sesión remota del invitado).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkSummary>,
    /// Latencia de entrada: `recibido − enviado` en el anfitrión con el reloj
    /// sincronizado; en el invitado, la que le informa el anfitrión.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<InputLatency>,
    /// Audio del juego y voz, si hay.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioSummary>,
}

/// Número de etapas de audio.
pub const AUDIO_STAGES: usize = 8;

/// Etapa de una trama de audio, en su orden fijo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum AudioStage {
    /// Captura de la primera muestra (en el receptor, la del emisor convertida a su reloj).
    Captured,
    /// Procesado de la voz (NS, AGC, AEC, VAD).
    Processed,
    /// Opus devolvió el paquete.
    Encoded,
    /// SFrame lo cifró.
    Encrypted,
    /// Salió el datagrama.
    Sent,
    /// Llegó y se descifró.
    Received,
    /// Salió del buffer de jitter y se decodificó.
    Decoded,
    /// Hora estimada a la que suena (cola y latencia del dispositivo incluidas).
    Played,
}

impl AudioStage {
    /// Todas las etapas en orden.
    pub const ALL: [AudioStage; AUDIO_STAGES] = [
        AudioStage::Captured,
        AudioStage::Processed,
        AudioStage::Encoded,
        AudioStage::Encrypted,
        AudioStage::Sent,
        AudioStage::Received,
        AudioStage::Decoded,
        AudioStage::Played,
    ];

    /// Posición en [`AudioStage::ALL`].
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Nombre en español.
    pub const fn label(self) -> &'static str {
        match self {
            AudioStage::Captured => "captura",
            AudioStage::Processed => "procesado",
            AudioStage::Encoded => "codificado",
            AudioStage::Encrypted => "cifrado",
            AudioStage::Sent => "enviado",
            AudioStage::Received => "recibido",
            AudioStage::Decoded => "decodificado",
            AudioStage::Played => "suena",
        }
    }
}

/// Pista de audio del registro.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum AudioTrack {
    /// Audio del juego (del anfitrión).
    Game,
    /// Voz de la sala.
    Voice,
}

impl AudioTrack {
    /// Las dos pistas.
    pub const ALL: [AudioTrack; 2] = [AudioTrack::Game, AudioTrack::Voice];

    /// Nombre en español.
    pub const fn label(self) -> &'static str {
        match self {
            AudioTrack::Game => "audio del juego",
            AudioTrack::Voice => "voz",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// Marcas de una trama de audio en un extremo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AudioTimeline {
    /// Marca de cada etapa, indexada por [`AudioStage::index`].
    pub marks: [Option<Instant>; AUDIO_STAGES],
}

impl AudioTimeline {
    /// Marca `stage` en `at`.
    pub fn mark(&mut self, stage: AudioStage, at: Instant) {
        self.marks[stage.index()] = Some(at);
    }

    /// Marca de una etapa.
    pub fn at(&self, stage: AudioStage) -> Option<Instant> {
        self.marks[stage.index()]
    }
}

/// Percentiles de un tramo de audio, en µs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AudioSpan {
    /// Etapa de partida.
    pub from: AudioStage,
    /// Etapa de llegada.
    pub to: AudioStage,
    /// Tramas con ambas marcas.
    pub samples: usize,
    /// Mediana.
    pub p50_us: u64,
    /// Percentil 95.
    pub p95_us: u64,
    /// Percentil 99.
    pub p99_us: u64,
    /// Máximo.
    pub max_us: u64,
}

/// Contadores de recepción de una pista de audio (los del buffer de jitter, sumando emisores).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct AudioCounters {
    /// Paquetes recibidos.
    pub received: u64,
    /// Tramas reproducidas desde su paquete.
    pub played: u64,
    /// Tramas perdidas ocultadas (PLC, FEC o DRED).
    pub concealed: u64,
    /// Recuperadas con FEC en banda.
    pub recovered_fec: u64,
    /// Recuperadas con DRED.
    pub recovered_dred: u64,
    /// Llegadas tarde.
    pub late: u64,
    /// Veces que la cola se vació en mitad de una frase.
    pub underruns: u64,
    /// Tramas recortadas para bajar el retardo.
    pub trimmed: u64,
    /// Tramas de PLC insertadas para subirlo.
    pub stretched: u64,
    /// Mayor racha de tramas seguidas sin paquete.
    pub max_gap_frames: u32,
    /// Objetivo del buffer de jitter en ms (el mayor de los emisores).
    pub jitter_target_ms: f64,
    /// p95 del jitter medido en ms (el mayor de los emisores).
    pub jitter_p95_ms: f64,
    /// Paquetes enviados (emisor).
    pub sent: u64,
    /// Veces que la salida de audio (el dispositivo o su cola) se quedó sin nada y sonó silencio;
    /// la salida es común a las pistas, así que es el mismo valor en las dos.
    pub device_underruns: u64,
    /// Veces que no cupo en la cola de la salida lo que se escribió y se perdió audio.
    pub device_overruns: u64,
}

impl AudioCounters {
    /// Tramas perdidas sobre las esperadas, en %.
    pub fn loss_percent(&self) -> f64 {
        let expected = self.played + self.concealed;
        if expected == 0 {
            0.0
        } else {
            self.concealed as f64 * 100.0 / expected as f64
        }
    }
}

/// Resumen de una pista de audio.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AudioTrackSummary {
    /// Pista.
    pub track: AudioTrack,
    /// Tramos entre etapas consecutivas con muestras (los del emisor en el emisor, los del
    /// receptor en el receptor).
    pub spans: Vec<AudioSpan>,
    /// `captura → suena` (en el receptor: boca-oído).
    pub total: AudioSpan,
    /// Contadores.
    pub counters: AudioCounters,
}

/// Sincronía del audio del juego con el vídeo (invitado).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct AvSummary {
    /// Muestras.
    pub samples: usize,
    /// Mediana de |A−V| en ms.
    pub p50_ms: f64,
    /// Percentil 95 de |A−V| en ms.
    pub p95_ms: f64,
    /// Desfase medio con signo (positivo: el audio va tarde), en ms.
    pub offset_ms: f64,
    /// Ajuste aplicado al buffer del audio, en ms.
    pub adjust_ms: i32,
}

/// Audio de la sesión.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AudioSummary {
    /// Pistas con algo medido.
    pub tracks: Vec<AudioTrackSummary>,
    /// |A−V| (solo el invitado con audio del juego).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub av: Option<AvSummary>,
    /// Quién habla ahora («anfitrión», «invitado 2»…).
    pub speaking: Vec<String>,
    /// Estado local: micrófono, modo, juego silenciado…
    pub status: String,
}

impl AudioSummary {
    /// Resumen de `track`, si se midió.
    pub fn track(&self, track: AudioTrack) -> Option<&AudioTrackSummary> {
        self.tracks.iter().find(|summary| summary.track == track)
    }
}

impl fmt::Display for AudioSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = |us: u64| us as f64 / 1000.0;
        let mut first = true;
        for track in &self.tracks {
            if !first {
                writeln!(f)?;
            }
            first = false;
            let name = match track.track {
                AudioTrack::Game => "Audio del juego (captura → suena)",
                AudioTrack::Voice => "Voz (boca-oído)",
            };
            let c = &track.counters;
            write!(
                f,
                "{name}: p50 {:.1} ms · p95 {:.1} ms ({} tramas) · pérdida {:.1} % · PLC {} · \
                 FEC {} · DRED {} · tarde {} · recortes {} · estiramientos {} · jitter objetivo \
                 {:.0} ms (p95 {:.1} ms)",
                ms(track.total.p50_us),
                ms(track.total.p95_us),
                track.total.samples,
                c.loss_percent(),
                c.concealed
                    .saturating_sub(c.recovered_fec)
                    .saturating_sub(c.recovered_dred),
                c.recovered_fec,
                c.recovered_dred,
                c.late,
                c.trimmed,
                c.stretched,
                c.jitter_target_ms,
                c.jitter_p95_ms
            )?;
            if c.sent > 0 {
                write!(f, " · {} enviados", c.sent)?;
            }
            if c.device_underruns + c.device_overruns > 0 {
                write!(
                    f,
                    " · salida sin audio {} · desbordes {}",
                    c.device_underruns, c.device_overruns
                )?;
            }
            for span in &track.spans {
                write!(
                    f,
                    "\n  {:<26} p50 {:>6.2} ms · p95 {:>6.2} ms",
                    format!("{} → {}", span.from.label(), span.to.label()),
                    ms(span.p50_us),
                    ms(span.p95_us)
                )?;
            }
        }
        if let Some(av) = &self.av {
            if !first {
                writeln!(f)?;
            }
            first = false;
            write!(
                f,
                "|A−V|: p50 {:.1} ms · p95 {:.1} ms ({} muestras) · desfase {:+.1} ms · ajuste \
                 del audio {:+} ms",
                av.p50_ms, av.p95_ms, av.samples, av.offset_ms, av.adjust_ms
            )?;
        }
        if !self.status.is_empty() || !self.speaking.is_empty() {
            if !first {
                writeln!(f)?;
            }
            let speaking = if self.speaking.is_empty() {
                "nadie".to_owned()
            } else {
                self.speaking.join(", ")
            };
            write!(f, "Voz: {} · habla: {speaking}", self.status)?;
        }
        Ok(())
    }
}

/// Tramas de audio que se guardan por pista y extremo (20 s de tramas de 10 ms).
const AUDIO_WINDOW: usize = 2048;

/// Lado de una línea de tiempo de audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSide {
    /// La trama la envió este extremo.
    Sender,
    /// La trama la recibió este extremo.
    Receiver,
}

#[derive(Debug, Default)]
struct AudioInner {
    /// Por pista y lado: las últimas marcas.
    timelines: [[VecDeque<AudioTimeline>; 2]; 2],
    counters: [AudioCounters; 2],
    av: Option<AvSummary>,
    speaking: Vec<String>,
    status: String,
}

/// Latencia de la entrada del invitado al anfitrión y eventos aplicados y descartados.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct InputLatency {
    /// Muestras (paquetes con hora de envío).
    pub samples: usize,
    /// Mediana, en µs.
    pub p50_us: u64,
    /// Percentil 95, en µs.
    pub p95_us: u64,
    /// Percentil 99, en µs.
    pub p99_us: u64,
    /// Máximo, en µs.
    pub max_us: u64,
    /// Eventos aplicados.
    pub applied: u64,
    /// Eventos descartados (permisos, pausa, antigüedad o el sistema no los aceptó).
    pub dropped: u64,
}

impl InputLatency {
    /// Estadísticas de unas muestras en µs (se ordenan aquí).
    pub fn from_samples(mut samples: Vec<u64>, applied: u64, dropped: u64) -> InputLatency {
        samples.sort_unstable();
        InputLatency {
            samples: samples.len(),
            p50_us: nearest_rank(&samples, 50.0),
            p95_us: nearest_rank(&samples, 95.0),
            p99_us: nearest_rank(&samples, 99.0),
            max_us: samples.last().copied().unwrap_or(0),
            applied,
            dropped,
        }
    }
}

impl fmt::Display for InputLatency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Entrada: p50 {:.2} ms · p95 {:.2} ms · p99 {:.2} ms · máx {:.2} ms ({} muestras) · {} \
             eventos aplicados, {} descartados",
            ms(self.p50_us),
            ms(self.p95_us),
            ms(self.p99_us),
            ms(self.max_us),
            self.samples,
            self.applied,
            self.dropped
        )
    }
}

/// Estado del enlace remoto que acompaña al resumen de latencia del invitado.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LinkSummary {
    /// Ruta: `"directo"` o `"relay"`.
    pub path: String,
    /// RTT suavizado de QUIC, en ms.
    pub rtt_ms: f64,
    /// Bitrate objetivo que aplica el anfitrión (el mínimo de los invitados), en kbit/s.
    pub target_kbps: u32,
    /// Objetos perdidos en la red (antes de FEC y NACK), en %.
    pub loss_percent: f64,
    /// Grupos descartados por vencer su plazo.
    pub groups_dropped: u64,
    /// Grupos recuperados por FEC y por NACK.
    pub groups_recovered: u64,
    /// Época MLS actual de la sala.
    pub epoch: u64,
    /// Número de seguridad con el anfitrión (60 dígitos en 12 grupos).
    pub safety_number: String,
    /// Huella de la identidad del anfitrión.
    pub peer_fingerprint: String,
    /// Nombre visible del anfitrión en el servidor.
    pub peer_name: String,
    /// Si los medios van cifrados con SFrame (solo se desactiva en desarrollo).
    pub sframe: bool,
}

impl fmt::Display for LinkSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "Enlace con {}: {} · RTT {:.1} ms · objetivo {} kbit/s · pérdida {:.1} % · {} \
             grupos descartados, {} recuperados",
            self.peer_name,
            self.path,
            self.rtt_ms,
            self.target_kbps,
            self.loss_percent,
            self.groups_dropped,
            self.groups_recovered
        )?;
        write!(
            f,
            "Cifrado: {} · época MLS {} · número de seguridad {}",
            if self.sframe {
                "SFrame"
            } else {
                "DESACTIVADO (desarrollo)"
            },
            self.epoch,
            self.safety_number
        )
    }
}

impl LatencySummary {
    /// Proporción de fotogramas capturados que se descartaron (0 a 1).
    pub fn drop_ratio(&self) -> f64 {
        if self.frames_captured == 0 {
            0.0
        } else {
            self.drops_total as f64 / self.frames_captured as f64
        }
    }

    /// Estadísticas del tramo que empieza en `from`, si existe.
    pub fn span_from(&self, from: Stage) -> Option<&SpanStats> {
        self.spans.iter().find(|span| span.from == from)
    }
}

fn ms(us: u64) -> f64 {
    us as f64 / 1000.0
}

impl fmt::Display for LatencySummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{:<30} {:>8} {:>9} {:>9} {:>9} {:>9}",
            "Tramo", "muestras", "p50 (ms)", "p95 (ms)", "p99 (ms)", "máx (ms)"
        )?;
        let row = |f: &mut fmt::Formatter<'_>, name: &str, s: &SpanStats| {
            writeln!(
                f,
                "{:<30} {:>8} {:>9.2} {:>9.2} {:>9.2} {:>9.2}",
                name,
                s.samples,
                ms(s.p50_us),
                ms(s.p95_us),
                ms(s.p99_us),
                ms(s.max_us)
            )
        };
        for span in &self.spans {
            let name = format!("{} → {}", span.from.label(), span.to.label());
            row(f, &name, span)?;
        }
        row(f, "Total (captura → presentado)", &self.total)?;
        let drops = self
            .drops
            .iter()
            .filter(|drop| drop.stage != Stage::Captured)
            .map(|drop| format!("{} {}", drop.stage.label(), drop.count))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            f,
            "Descartes: {} (en total {} de {} capturados, {:.1} %)",
            drops,
            self.drops_total,
            self.frames_captured,
            self.drop_ratio() * 100.0
        )?;
        write!(
            f,
            "Presentados: {} · {:.1} fps · {:.0} kbit/s codificados",
            self.frames_presented, self.fps, self.bitrate_kbps
        )?;
        if let Some(video) = &self.video {
            write!(f, "\nVídeo: {video}")?;
        }
        if !self.cpu_copies.is_empty() {
            let stages = self
                .cpu_copies
                .iter()
                .map(|copies| format!("{} {:.1}", copies.stage.label(), copies.per_frame))
                .collect::<Vec<_>>()
                .join(", ");
            write!(
                f,
                "\nCopias a CPU por fotograma: {:.1} ({stages})",
                self.cpu_copies_per_frame
            )?;
        }
        if let Some(link) = &self.link {
            write!(f, "\n{link}")?;
        }
        if let Some(input) = &self.input {
            write!(f, "\n{input}")?;
        }
        if let Some(audio) = &self.audio {
            write!(f, "\n{audio}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
struct Inner {
    frames: BTreeMap<u64, [Option<Instant>; STAGES]>,
    marks_total: [u64; STAGES],
    drops: [u64; STAGES],
    /// Bytes codificados con su instante, en la misma ventana que los fotogramas.
    encoded: VecDeque<(Instant, u64)>,
    /// Códec y backends en uso.
    video: Option<String>,
    /// Copias a CPU por etapa: fotogramas contados y copias.
    copies: [(u64, u64); STAGES],
    /// Latencias de entrada en µs (las últimas [`LatencyLedger::WINDOW`]).
    input: VecDeque<u64>,
    /// Eventos de entrada aplicados y descartados.
    input_counts: (u64, u64),
    /// Latencia de entrada que informa el anfitrión (en el invitado).
    input_report: Option<InputLatency>,
    /// Fotogramas que el decodificador rechazó por corruptos o sin referencias.
    decode_errors: u64,
}

/// Registro compartido (en `Arc`) de marcas de tiempo por etapa.
#[derive(Debug, Default)]
pub struct LatencyLedger {
    inner: Mutex<Inner>,
    audio: Mutex<AudioInner>,
}

impl LatencyLedger {
    /// Fotogramas que se guardan; los más antiguos salen de la ventana.
    pub const WINDOW: usize = 4096;

    /// Crea un registro vacío.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // Un pánico en otro hilo no invalida las marcas ya guardadas: se siguen usando.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Anota el códec y los backends en uso (`AV1 · SVT-AV1 → dav1d`), que salen en el resumen.
    pub fn set_video(&self, video: impl Into<String>) {
        self.lock().video = Some(video.into());
    }

    /// Anota que `frame_id` llegó a `stage` en el instante `at`.
    pub fn mark(&self, frame_id: u64, stage: Stage, at: Instant) {
        let mut inner = self.lock();
        inner.marks_total[stage.index()] += 1;
        if inner.frames.len() >= Self::WINDOW
            && inner
                .frames
                .first_key_value()
                .is_some_and(|(&oldest, _)| frame_id < oldest)
        {
            return;
        }
        inner.frames.entry(frame_id).or_default()[stage.index()] = Some(at);
        while inner.frames.len() > Self::WINDOW {
            inner.frames.pop_first();
        }
    }

    /// Marcas de un fotograma que sigue en la ventana.
    pub fn timeline(&self, frame_id: u64) -> Option<FrameTimeline> {
        let inner = self.lock();
        inner.frames.get(&frame_id).map(|marks| FrameTimeline {
            frame_id,
            marks: *marks,
        })
    }

    /// Pasa a `f` la captura y la presentación de los fotogramas desde el id `next` que ya tienen
    /// las dos marcas, en orden, y avanza `next`. Se para en el primero al que le falta alguna (aún
    /// sin presentar, o sin la captura, que llega por el control); se salta cuando ya hay 30 más
    /// nuevos (un descartado nunca se presenta). Lo usa la sincronía A/V del invitado.
    pub fn video_pairs_since(&self, next: &mut u64, mut f: impl FnMut(Instant, Instant)) {
        let inner = self.lock();
        let newest = inner.frames.last_key_value().map_or(0, |(id, _)| *id);
        for (&id, marks) in inner.frames.range(*next..) {
            match (
                marks[Stage::Captured.index()],
                marks[Stage::Presented.index()],
            ) {
                (Some(captured), Some(presented)) => {
                    f(captured, presented);
                    *next = id + 1;
                }
                // Sin una de las dos marcas: se espera, salvo que ya haya 30 más nuevos (un
                // fotograma descartado nunca se presenta y no debe parar la sincronía).
                _ if id + 30 < newest => *next = id + 1,
                _ => break,
            }
        }
    }

    /// Marcas de todos los fotogramas de la ventana, del más antiguo al más reciente.
    pub fn timelines(&self) -> Vec<FrameTimeline> {
        let inner = self.lock();
        inner
            .frames
            .iter()
            .map(|(&frame_id, marks)| FrameTimeline {
                frame_id,
                marks: *marks,
            })
            .collect()
    }

    /// Anota un fotograma descartado antes de llegar a `stage`.
    pub fn record_drop(&self, stage: Stage) {
        self.lock().drops[stage.index()] += 1;
    }

    /// Anota un fotograma que el decodificador rechazó (corrupto o sin referencias).
    pub fn record_decode_error(&self) {
        self.lock().decode_errors += 1;
    }

    /// Anota que un fotograma necesitó `copies` copias de la imagen en memoria de CPU en `stage`
    /// (0 si la imagen no pasó por la CPU).
    pub fn record_cpu_copies(&self, stage: Stage, copies: u32) {
        let mut inner = self.lock();
        let entry = &mut inner.copies[stage.index()];
        entry.0 += 1;
        entry.1 += u64::from(copies);
    }

    /// Anota `bytes` codificados ahora.
    pub fn record_encoded_bytes(&self, bytes: u64) {
        self.record_encoded_bytes_at(bytes, Instant::now());
    }

    /// Anota `bytes` codificados en el instante `at`.
    pub fn record_encoded_bytes_at(&self, bytes: u64, at: Instant) {
        let mut inner = self.lock();
        inner.encoded.push_back((at, bytes));
        while inner.encoded.len() > Self::WINDOW {
            inner.encoded.pop_front();
        }
    }

    /// Anota una latencia de entrada (`recibido − enviado`) en µs.
    pub fn record_input_latency(&self, us: u64) {
        let mut inner = self.lock();
        inner.input.push_back(us);
        while inner.input.len() > Self::WINDOW {
            inner.input.pop_front();
        }
    }

    /// Eventos de entrada aplicados y descartados hasta ahora.
    pub fn set_input_counts(&self, applied: u64, dropped: u64) {
        self.lock().input_counts = (applied, dropped);
    }

    /// Latencia de entrada que informa el anfitrión (en el invitado).
    pub fn set_input_report(&self, report: InputLatency) {
        self.lock().input_report = Some(report);
    }

    fn audio(&self) -> MutexGuard<'_, AudioInner> {
        self.audio
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Anota las marcas de una trama de audio de `track` vista desde `side`.
    pub fn record_audio(&self, track: AudioTrack, side: AudioSide, timeline: AudioTimeline) {
        let mut audio = self.audio();
        let ring = &mut audio.timelines[track.index()][side as usize];
        if ring.len() >= AUDIO_WINDOW {
            ring.pop_front();
        }
        ring.push_back(timeline);
    }

    /// Contadores de recepción de `track` (la reproducción los publica cada medio segundo); el
    /// emisor solo pone `sent` con [`LatencyLedger::record_audio_sent`].
    pub fn set_audio_counters(&self, track: AudioTrack, counters: AudioCounters) {
        let mut audio = self.audio();
        let sent = audio.counters[track.index()].sent;
        audio.counters[track.index()] = AudioCounters { sent, ..counters };
    }

    /// Suma `packets` enviados en `track`.
    pub fn record_audio_sent(&self, track: AudioTrack, packets: u64) {
        self.audio().counters[track.index()].sent += packets;
    }

    /// Sincronía A/V medida.
    pub fn set_av(&self, av: AvSummary) {
        self.audio().av = Some(av);
    }

    /// Quién habla y el estado local de la voz.
    pub fn set_voice_status(&self, speaking: Vec<String>, status: String) {
        let mut audio = self.audio();
        audio.speaking = speaking;
        audio.status = status;
    }

    /// Resumen del audio, si hay algo medido.
    pub fn audio_summary(&self) -> Option<AudioSummary> {
        let audio = self.audio();
        let span = |timelines: &[&VecDeque<AudioTimeline>], from: AudioStage, to: AudioStage| {
            let mut samples: Vec<u64> = timelines
                .iter()
                .flat_map(|ring| ring.iter())
                .filter_map(|timeline| {
                    let (a, b) = (timeline.at(from)?, timeline.at(to)?);
                    Some(duration_us(b.saturating_duration_since(a)))
                })
                .collect();
            samples.sort_unstable();
            AudioSpan {
                from,
                to,
                samples: samples.len(),
                p50_us: nearest_rank(&samples, 50.0),
                p95_us: nearest_rank(&samples, 95.0),
                p99_us: nearest_rank(&samples, 99.0),
                max_us: samples.last().copied().unwrap_or(0),
            }
        };
        let mut tracks = Vec::new();
        for track in AudioTrack::ALL {
            let [sender, receiver] = &audio.timelines[track.index()];
            let counters = audio.counters[track.index()];
            if sender.is_empty() && receiver.is_empty() && counters == AudioCounters::default() {
                continue;
            }
            // Tramos de cada lado entre etapas marcadas consecutivas (las que ese lado conoce).
            let mut spans = Vec::new();
            for ring in [sender, receiver] {
                let Some(sample) = ring.back() else {
                    continue;
                };
                let marked: Vec<AudioStage> = AudioStage::ALL
                    .into_iter()
                    .filter(|stage| sample.at(*stage).is_some())
                    .collect();
                for pair in marked.windows(2) {
                    let stats = span(&[ring], pair[0], pair[1]);
                    if stats.samples > 0 && !spans.iter().any(|s: &AudioSpan| s.from == pair[0]) {
                        spans.push(stats);
                    }
                }
            }
            spans.sort_by_key(|s| s.from);
            // El total es el del receptor (boca-oído); si este extremo solo envía, el suyo.
            let total = if receiver.is_empty() {
                span(&[sender], AudioStage::Captured, AudioStage::Sent)
            } else {
                span(&[receiver], AudioStage::Captured, AudioStage::Played)
            };
            tracks.push(AudioTrackSummary {
                track,
                spans,
                total,
                counters,
            });
        }
        if tracks.is_empty() && audio.av.is_none() && audio.status.is_empty() {
            return None;
        }
        Some(AudioSummary {
            tracks,
            av: audio.av,
            speaking: audio.speaking.clone(),
            status: audio.status.clone(),
        })
    }

    /// Resumen de la ventana actual.
    pub fn summary(&self) -> LatencySummary {
        let inner = self.lock();
        let stats = |from: Stage, to: Stage| {
            let mut samples: Vec<u64> = inner
                .frames
                .values()
                .filter_map(|marks| {
                    let (a, b) = (marks[from.index()]?, marks[to.index()]?);
                    // Dos hilos marcan etapas contiguas: el receptor puede completar el fotograma
                    // antes de que el emisor vuelva del último `send` y marque `Sent`. Ese tramo
                    // vale 0, no se descarta.
                    Some(duration_us(b.saturating_duration_since(a)))
                })
                .collect();
            samples.sort_unstable();
            SpanStats {
                from,
                to,
                samples: samples.len(),
                p50_us: nearest_rank(&samples, 50.0),
                p95_us: nearest_rank(&samples, 95.0),
                p99_us: nearest_rank(&samples, 99.0),
                max_us: samples.last().copied().unwrap_or(0),
            }
        };
        let spans = Stage::ALL
            .windows(2)
            .map(|pair| stats(pair[0], pair[1]))
            .collect();
        let total = stats(Stage::Captured, Stage::Presented);

        let mut presented: Vec<Instant> = inner
            .frames
            .values()
            .filter_map(|marks| marks[Stage::Presented.index()])
            .collect();
        presented.sort_unstable();
        // n presentados cubren n - 1 intervalos.
        let fps = rate(presented.len().saturating_sub(1) as u64, &presented);

        let bitrate_kbps = match (inner.encoded.front(), inner.encoded.back()) {
            (Some(&(first, _)), Some(&(last, _))) if last > first => {
                // Los bytes del primer registro se produjeron antes del intervalo medido.
                let bytes: u64 = inner.encoded.iter().skip(1).map(|&(_, n)| n).sum();
                bytes as f64 * 8.0 / (last - first).as_secs_f64() / 1000.0
            }
            _ => 0.0,
        };

        let drops: Vec<StageDrops> = Stage::ALL
            .iter()
            .map(|&stage| StageDrops {
                stage,
                count: inner.drops[stage.index()],
            })
            .collect();
        let cpu_copies: Vec<StageCopies> = Stage::ALL
            .iter()
            .filter_map(|&stage| {
                let (frames, total) = inner.copies[stage.index()];
                (frames > 0).then(|| StageCopies {
                    stage,
                    frames,
                    total,
                    per_frame: total as f64 / frames as f64,
                })
            })
            .collect();
        let cpu_copies_per_frame = cpu_copies.iter().map(|copies| copies.per_frame).sum();
        LatencySummary {
            frames_in_window: inner.frames.len(),
            frames_captured: inner.marks_total[Stage::Captured.index()],
            frames_presented: inner.marks_total[Stage::Presented.index()],
            spans,
            total,
            drops_total: drops.iter().map(|drop| drop.count).sum(),
            decode_errors: inner.decode_errors,
            drops,
            fps,
            bitrate_kbps,
            video: inner.video.clone(),
            cpu_copies,
            cpu_copies_per_frame,
            link: None,
            input: inner.input_report.or_else(|| {
                let (applied, dropped) = inner.input_counts;
                (!inner.input.is_empty() || applied + dropped > 0).then(|| {
                    InputLatency::from_samples(
                        inner.input.iter().copied().collect(),
                        applied,
                        dropped,
                    )
                })
            }),
            audio: {
                drop(inner);
                self.audio_summary()
            },
        }
    }
}

/// Eventos por segundo entre el primer y el último instante de `sorted`.
fn rate(events: u64, sorted: &[Instant]) -> f64 {
    match (sorted.first(), sorted.last()) {
        (Some(&first), Some(&last)) if last > first => events as f64 / (last - first).as_secs_f64(),
        _ => 0.0,
    }
}

fn duration_us(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

/// Percentil por rango más cercano sobre muestras ordenadas: el valor en la posición
/// `ceil(p / 100 × n)` (contando desde 1). Cero si no hay muestras.
pub fn nearest_rank(sorted: &[u64], percentile: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (percentile / 100.0 * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn us(n: u64) -> Duration {
        Duration::from_micros(n)
    }

    /// Fotograma con todas las etapas separadas por `step` microsegundos.
    fn mark_full(ledger: &LatencyLedger, id: u64, start: Instant, step: u64) {
        for (i, stage) in Stage::ALL.into_iter().enumerate() {
            ledger.mark(id, stage, start + us(step * i as u64));
        }
    }

    #[test]
    fn nearest_rank_matches_the_definition() {
        let samples: Vec<u64> = (1..=100).collect();
        assert_eq!(nearest_rank(&samples, 50.0), 50);
        assert_eq!(nearest_rank(&samples, 95.0), 95);
        assert_eq!(nearest_rank(&samples, 99.0), 99);
        assert_eq!(nearest_rank(&samples, 100.0), 100);
        assert_eq!(nearest_rank(&[7], 50.0), 7);
        assert_eq!(nearest_rank(&[1, 2, 3, 4], 50.0), 2);
        assert_eq!(nearest_rank(&[], 50.0), 0);
    }

    #[test]
    fn percentiles_of_known_spans() {
        let ledger = LatencyLedger::new();
        let base = Instant::now();
        // El fotograma i tarda i µs entre cada par de etapas: total 7·i µs.
        for id in 1..=100u64 {
            mark_full(&ledger, id, base + Duration::from_millis(id * 16), id);
        }
        let summary = ledger.summary();
        assert_eq!(summary.spans.len(), STAGES - 1);
        for (span, pair) in summary.spans.iter().zip(Stage::ALL.windows(2)) {
            assert_eq!((span.from, span.to), (pair[0], pair[1]));
            assert_eq!(span.samples, 100);
            assert_eq!(
                (span.p50_us, span.p95_us, span.p99_us, span.max_us),
                (50, 95, 99, 100)
            );
        }
        assert_eq!(
            (
                summary.total.p50_us,
                summary.total.p95_us,
                summary.total.p99_us,
                summary.total.max_us
            ),
            (350, 665, 693, 700)
        );
        assert_eq!(summary.frames_captured, 100);
        assert_eq!(summary.frames_presented, 100);
        assert_eq!(summary.drops_total, 0);
        // Presentados cada 16 ms (más 7 µs por fotograma): unos 62,5 fps.
        assert!((summary.fps - 62.47).abs() < 0.1, "{}", summary.fps);
        assert!(ledger.timeline(50).unwrap().is_complete());
    }

    #[test]
    fn the_window_forgets_old_frames() {
        let ledger = LatencyLedger::new();
        let base = Instant::now();
        let extra = 10u64;
        for id in 0..LatencyLedger::WINDOW as u64 + extra {
            // Los primeros fotogramas son lentos (1 ms por tramo); los demás, rápidos.
            let step = if id < extra { 1_000 } else { 1 };
            mark_full(&ledger, id, base, step);
        }
        let summary = ledger.summary();
        assert_eq!(summary.frames_in_window, LatencyLedger::WINDOW);
        assert_eq!(summary.total.max_us, 7, "los lentos salieron de la ventana");
        assert!(ledger.timeline(0).is_none());
        assert!(ledger.timeline(extra).is_some());
        // Una marca tardía de un fotograma ya olvidado no vuelve a entrar.
        ledger.mark(0, Stage::Presented, base);
        assert!(ledger.timeline(0).is_none());
        assert_eq!(
            summary.frames_captured,
            LatencyLedger::WINDOW as u64 + extra
        );
    }

    #[test]
    fn marks_out_of_order_count_as_zero() {
        let ledger = LatencyLedger::new();
        let base = Instant::now();
        ledger.mark(1, Stage::Sent, base + us(10));
        ledger.mark(1, Stage::Received, base + us(5));
        let summary = ledger.summary();
        let span = summary.span_from(Stage::Sent).unwrap();
        assert_eq!((span.samples, span.max_us), (1, 0));
        assert_eq!(
            ledger
                .timeline(1)
                .unwrap()
                .span(Stage::Sent, Stage::Received),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn drops_are_counted_per_stage() {
        let ledger = LatencyLedger::new();
        let base = Instant::now();
        for id in 0..20 {
            ledger.mark(id, Stage::Captured, base);
        }
        ledger.record_drop(Stage::Encoded);
        ledger.record_drop(Stage::Encoded);
        ledger.record_drop(Stage::Received);
        let summary = ledger.summary();
        assert_eq!(summary.drops[Stage::Encoded.index()].count, 2);
        assert_eq!(summary.drops[Stage::Received.index()].count, 1);
        assert_eq!(summary.drops_total, 3);
        assert!((summary.drop_ratio() - 0.15).abs() < 1e-9);
        // Sin presentados no hay muestras del total.
        assert_eq!(summary.total.samples, 0);
        assert_eq!(summary.fps, 0.0);
    }

    #[test]
    fn cpu_copies_are_averaged_per_stage() {
        let ledger = LatencyLedger::new();
        assert!(ledger.summary().cpu_copies.is_empty());
        for _ in 0..4 {
            ledger.record_cpu_copies(Stage::Captured, 0);
            ledger.record_cpu_copies(Stage::Encoded, 1);
            ledger.record_cpu_copies(Stage::Decoded, 2);
        }
        ledger.record_cpu_copies(Stage::Presented, 0);
        let summary = ledger.summary();
        let stages: Vec<Stage> = summary.cpu_copies.iter().map(|c| c.stage).collect();
        assert_eq!(
            stages,
            vec![
                Stage::Captured,
                Stage::Encoded,
                Stage::Decoded,
                Stage::Presented
            ]
        );
        assert_eq!(summary.cpu_copies[2].total, 8);
        assert!((summary.cpu_copies[2].per_frame - 2.0).abs() < 1e-9);
        assert!((summary.cpu_copies_per_frame - 3.0).abs() < 1e-9);
        let table = summary.to_string();
        assert!(
            table.contains("Copias a CPU por fotograma: 3.0 (captura 0.0, codificado 1.0"),
            "{table}"
        );
        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["cpu_copies"][1]["stage"], "Encoded");
        assert_eq!(json["cpu_copies_per_frame"], 3.0);
    }

    #[test]
    fn bitrate_uses_the_encoded_bytes() {
        let ledger = LatencyLedger::new();
        let base = Instant::now();
        for i in 0..=10u64 {
            // 1250 bytes cada 10 ms = 1 Mbit/s.
            ledger.record_encoded_bytes_at(1_250, base + Duration::from_millis(i * 10));
        }
        let summary = ledger.summary();
        assert!((summary.bitrate_kbps - 1_000.0).abs() < 1e-6);
    }

    #[test]
    fn video_pairs_come_in_order_once() {
        let ledger = LatencyLedger::new();
        let base = Instant::now();
        for id in 0..5 {
            ledger.mark(id, Stage::Captured, base + us(id * 1_000));
            if id != 3 {
                ledger.mark(id, Stage::Presented, base + us(id * 1_000 + 50));
            }
        }
        let mut next = 0;
        let mut seen = Vec::new();
        ledger.video_pairs_since(&mut next, |captured, presented| {
            seen.push(presented - captured);
        });
        assert_eq!(seen.len(), 3, "se para en el 3, aún sin presentar");
        assert_eq!(next, 3);
        ledger.mark(3, Stage::Presented, base + us(3_050));
        ledger.video_pairs_since(&mut next, |_, _| seen.push(Duration::ZERO));
        assert_eq!((seen.len(), next), (5, 5));
        // Uno descartado (capturado, nunca presentado) no para la cuenta cuando hay 30 más nuevos.
        ledger.mark(5, Stage::Captured, base);
        for id in 6..40 {
            ledger.mark(id, Stage::Captured, base);
            ledger.mark(id, Stage::Presented, base);
        }
        ledger.video_pairs_since(&mut next, |_, _| seen.push(Duration::ZERO));
        assert_eq!(next, 40);
        assert_eq!(seen.len(), 5 + 34);
    }

    #[test]
    fn audio_timelines_summarise_per_track() {
        let ledger = LatencyLedger::new();
        assert!(ledger.audio_summary().is_none());
        let base = Instant::now();
        for i in 0..100u64 {
            let start = base + Duration::from_millis(i * 10);
            let mut sent = AudioTimeline::default();
            sent.mark(AudioStage::Captured, start);
            sent.mark(AudioStage::Encoded, start + us(500));
            sent.mark(AudioStage::Sent, start + us(700));
            ledger.record_audio(AudioTrack::Voice, AudioSide::Sender, sent);
            let mut heard = AudioTimeline::default();
            heard.mark(AudioStage::Captured, start);
            heard.mark(AudioStage::Received, start + us(2_000));
            heard.mark(AudioStage::Decoded, start + us(22_000));
            heard.mark(AudioStage::Played, start + us(32_000 + i));
            ledger.record_audio(AudioTrack::Voice, AudioSide::Receiver, heard);
        }
        ledger.record_audio_sent(AudioTrack::Voice, 100);
        ledger.set_audio_counters(
            AudioTrack::Voice,
            AudioCounters {
                played: 98,
                concealed: 2,
                recovered_fec: 1,
                ..AudioCounters::default()
            },
        );
        ledger.set_voice_status(vec!["anfitrión".into()], "micrófono activo".into());
        let audio = ledger.summary().audio.expect("audio");
        let voice = audio.track(AudioTrack::Voice).expect("voz");
        assert_eq!(voice.total.samples, 100);
        assert!((32_000..=32_100).contains(&voice.total.p95_us), "{voice:?}");
        assert_eq!(voice.counters.sent, 100);
        assert!((voice.counters.loss_percent() - 2.0).abs() < 1e-9);
        let froms: Vec<AudioStage> = voice.spans.iter().map(|s| s.from).collect();
        assert!(froms.contains(&AudioStage::Captured) && froms.contains(&AudioStage::Decoded));
        assert!(audio.track(AudioTrack::Game).is_none());
        let text = audio.to_string();
        assert!(text.contains("Voz (boca-oído): p50 32.0 ms"), "{text}");
        assert!(text.contains("habla: anfitrión"), "{text}");
        assert!(text.contains("recibido → decodificado"), "{text}");
        let json = serde_json::to_value(&audio).expect("json");
        assert_eq!(json["tracks"][0]["track"], "Voice");
    }

    #[test]
    fn summary_serializes_and_prints_in_spanish() {
        let ledger = LatencyLedger::new();
        mark_full(&ledger, 1, Instant::now(), 1_500);
        let summary = ledger.summary();
        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["spans"][0]["from"], "Captured");
        assert_eq!(json["total"]["p50_us"], 10_500);
        assert!(
            json.get("link").is_none(),
            "sin enlace remoto no se serializa"
        );
        let table = summary.to_string();
        assert!(table.contains("captura → codificado"), "{table}");
        assert!(table.contains("Total (captura → presentado)"), "{table}");
        assert!(table.contains("10.50"), "{table}");
        assert!(table.contains("codificado → cifrado"), "{table}");

        let mut remote = summary.clone();
        remote.link = Some(LinkSummary {
            path: "relay".to_owned(),
            rtt_ms: 12.5,
            target_kbps: 8_000,
            loss_percent: 2.0,
            groups_dropped: 1,
            groups_recovered: 9,
            epoch: 3,
            safety_number: "12345 67890".to_owned(),
            peer_fingerprint: "ab".repeat(32),
            peer_name: "Ana".to_owned(),
            sframe: true,
        });
        let table = remote.to_string();
        assert!(
            table.contains("Enlace con Ana: relay · RTT 12.5 ms"),
            "{table}"
        );
        assert!(table.contains("época MLS 3"), "{table}");
        let json = serde_json::to_value(&remote).unwrap();
        assert_eq!(json["link"]["target_kbps"], 8_000);
        assert!(table.contains("Descartes"), "{table}");
    }
}
