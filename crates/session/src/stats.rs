//! Contadores del pipeline para el registro periódico ([`STATS_PERIOD`]): una línea compacta por
//! extremo con los fotogramas por segundo de cada etapa, los tiempos de codificación o
//! decodificación, el bitrate y los intra. Va al registro de la aplicación (nivel `info`, objetivo
//! `coroom::stats`) para poder diagnosticar una sesión real con el fichero de registro.
//!
//! Los contadores son acumulados y atómicos (los tocan los hilos de cada etapa); quien escribe la
//! línea guarda la instantánea anterior ([`StatsWindow`]) y resta.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::latency::{AudioCounters, AudioTrack, LatencyLedger, Stage};

/// Cada cuánto se escribe la línea de estadísticas.
pub const STATS_PERIOD: Duration = Duration::from_secs(5);

/// Muestras de tiempo que se guardan entre dos líneas como mucho (unos 8 s a 480 fps).
const MAX_SAMPLES: usize = 4096;

/// Contadores acumulados de un extremo.
#[derive(Debug, Default)]
pub(crate) struct Counters {
    pub(crate) captured: AtomicU64,
    pub(crate) encoded: AtomicU64,
    pub(crate) encoded_bytes: AtomicU64,
    pub(crate) sent: AtomicU64,
    pub(crate) received: AtomicU64,
    pub(crate) received_bytes: AtomicU64,
    pub(crate) decoded: AtomicU64,
    pub(crate) presented: AtomicU64,
    /// Intra que el codificador hizo porque alguien los pidió.
    pub(crate) forced_keyframes: AtomicU64,
    /// Peticiones de intra que el anfitrión ignoró o aplazó (ya venía uno de camino).
    pub(crate) coalesced_requests: AtomicU64,
    /// `KeyframeRequest` que envió el invitado (además de los del receptor de medios).
    pub(crate) keyframe_requests: AtomicU64,
    /// Cambios de bitrate aplicados al codificador.
    pub(crate) bitrate_changes: AtomicU64,
    /// Decodificadores que se dejaron por fallar.
    pub(crate) decoder_fallbacks: AtomicU64,
    /// Fotogramas que no se enviaron a algún invitado por tener su enlace atascado.
    pub(crate) stalled_sends: AtomicU64,
    encode_us: Mutex<Vec<u32>>,
    decode_us: Mutex<Vec<u32>>,
}

/// Suma uno a un contador.
pub(crate) fn bump(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

fn push_sample(samples: &Mutex<Vec<u32>>, took: Duration) {
    let mut samples = samples
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if samples.len() < MAX_SAMPLES {
        samples.push(u32::try_from(took.as_micros()).unwrap_or(u32::MAX));
    }
}

/// Mediana y percentil 95 en ms de las muestras, que se vacían.
fn take_percentiles(samples: &Mutex<Vec<u32>>) -> (f64, f64) {
    let mut samples = std::mem::take(
        &mut *samples
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    samples.sort_unstable();
    let at = |p: f64| {
        if samples.is_empty() {
            0.0
        } else {
            let index = ((samples.len() - 1) as f64 * p).round() as usize;
            f64::from(samples[index]) / 1000.0
        }
    };
    (at(0.5), at(0.95))
}

/// Contadores acumulados del vídeo de una sesión ([`crate::remote::RemoteSession::video_counters`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoCounters {
    /// Fotogramas codificados (anfitrión).
    pub encoded: u64,
    /// Intra que el codificador hizo porque alguien los pidió (anfitrión).
    pub forced_keyframes: u64,
    /// Peticiones de intra agrupadas o ignoradas porque ya venía uno (anfitrión).
    pub coalesced_requests: u64,
    /// Fotogramas de vídeo recibidos (invitado).
    pub received: u64,
    /// Fotogramas decodificados (invitado).
    pub decoded: u64,
    /// `KeyframeRequest` que envió el invitado (sin contar los del receptor de medios).
    pub keyframe_requests: u64,
    /// Decodificadores que se dejaron por no sacar imagen (invitado).
    pub decoder_fallbacks: u64,
}

impl Counters {
    /// Instantánea pública de los contadores del vídeo.
    pub(crate) fn video(&self) -> VideoCounters {
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        VideoCounters {
            encoded: load(&self.encoded),
            forced_keyframes: load(&self.forced_keyframes),
            coalesced_requests: load(&self.coalesced_requests),
            received: load(&self.received),
            decoded: load(&self.decoded),
            keyframe_requests: load(&self.keyframe_requests),
            decoder_fallbacks: load(&self.decoder_fallbacks),
        }
    }

    /// Anota lo que tardó una llamada al codificador.
    pub(crate) fn record_encode(&self, took: Duration) {
        push_sample(&self.encode_us, took);
    }

    /// Anota lo que tardó una llamada al decodificador.
    pub(crate) fn record_decode(&self, took: Duration) {
        push_sample(&self.decode_us, took);
    }

    fn snapshot(&self) -> [u64; 14] {
        [
            &self.captured,
            &self.encoded,
            &self.encoded_bytes,
            &self.sent,
            &self.received,
            &self.received_bytes,
            &self.decoded,
            &self.presented,
            &self.forced_keyframes,
            &self.coalesced_requests,
            &self.keyframe_requests,
            &self.bitrate_changes,
            &self.decoder_fallbacks,
            &self.stalled_sends,
        ]
        .map(|counter| counter.load(Ordering::Relaxed))
    }
}

/// Lo que da el receptor de medios del invitado para su línea (acumulado).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LinkCounters {
    /// Grupos descartados por vencer su plazo.
    pub(crate) dropped: u64,
    /// Objetos que llegaron tarde.
    pub(crate) late: u64,
    /// Objetos de origen perdidos en la red.
    pub(crate) lost_objects: u64,
    /// `Nack` enviados.
    pub(crate) nacks: u64,
    /// `KeyframeRequest` del receptor.
    pub(crate) keyframe_requests: u64,
}

/// Instantánea anterior para calcular las tasas de la línea siguiente.
#[derive(Debug)]
pub(crate) struct StatsWindow {
    at: Instant,
    counters: [u64; 14],
    drops: [u64; Stage::ALL.len()],
    link: LinkCounters,
    audio: [AudioCounters; 2],
}

impl StatsWindow {
    pub(crate) fn new(counters: &Counters, ledger: &LatencyLedger) -> Self {
        Self {
            at: Instant::now(),
            counters: counters.snapshot(),
            drops: drops(ledger),
            link: LinkCounters::default(),
            audio: AudioTrack::ALL.map(|track| ledger.audio_counters(track)),
        }
    }

    /// Línea del audio recibido desde la anterior, una parte por pista con paquetes: recibidos,
    /// tramas ocultadas (y de ellas recuperadas con FEC y DRED), tardíos, veces que se vació la
    /// cola, tramas recortadas y estiradas, racha más larga sin paquete, objetivo y jitter p95, y
    /// las faltas y desbordes de la salida. `None` si no llegó audio.
    pub(crate) fn audio_line(&mut self, ledger: &LatencyLedger) -> Option<String> {
        let now = AudioTrack::ALL.map(|track| ledger.audio_counters(track));
        let before = std::mem::replace(&mut self.audio, now);
        let parts: Vec<String> = AudioTrack::ALL
            .into_iter()
            .zip(now.into_iter().zip(before))
            .filter_map(|(track, (now, mut before))| {
                if now.received < before.received {
                    // Buffer nuevo (reconexión): empieza de cero.
                    before = AudioCounters::default();
                }
                let d = |f: fn(&AudioCounters) -> u64| f(&now).saturating_sub(f(&before));
                let received = d(|c| c.received);
                (received > 0).then(|| {
                    format!(
                        "{}: recibe {received}, ocultadas {} (FEC {}, DRED {}), tardíos {}, \
                         vacíos {}, recortadas {}, estiradas {}, racha máx. {} tramas, objetivo \
                         {:.0} ms, jitter p95 {:.1} ms, salida sin datos {} y desbordada {}",
                        track.label(),
                        d(|c| c.concealed),
                        d(|c| c.recovered_fec),
                        d(|c| c.recovered_dred),
                        d(|c| c.late),
                        d(|c| c.underruns),
                        d(|c| c.trimmed),
                        d(|c| c.stretched),
                        now.max_gap_frames,
                        now.jitter_target_ms,
                        now.jitter_p95_ms,
                        d(|c| c.device_underruns),
                        d(|c| c.device_overruns),
                    )
                })
            })
            .collect();
        (!parts.is_empty()).then(|| format!("invitado, audio: {}", parts.join("; ")))
    }

    /// Si ya toca otra línea.
    pub(crate) fn due(&self) -> bool {
        self.at.elapsed() >= STATS_PERIOD
    }

    /// Diferencias desde la línea anterior: segundos, contadores y descartes por etapa.
    fn advance(&mut self, counters: &Counters, ledger: &LatencyLedger) -> (f64, [u64; 14], String) {
        let now = Instant::now();
        let seconds = now.duration_since(self.at).as_secs_f64().max(1e-3);
        let current = counters.snapshot();
        let mut delta = [0; 14];
        for (i, value) in current.iter().enumerate() {
            delta[i] = value.saturating_sub(self.counters[i]);
        }
        let drops_now = drops(ledger);
        let dropped: Vec<String> = Stage::ALL
            .iter()
            .zip(drops_now.iter().zip(self.drops))
            .filter_map(|(stage, (now, before))| {
                let count = now.saturating_sub(before);
                (count > 0).then(|| format!("{}={count}", stage_id(*stage)))
            })
            .collect();
        self.at = now;
        self.counters = current;
        self.drops = drops_now;
        let dropped = if dropped.is_empty() {
            "0".to_owned()
        } else {
            dropped.join(",")
        };
        (seconds, delta, dropped)
    }

    /// Línea del anfitrión: `captura/codifica/envía` en fps, tiempos de codificación, objetivo y
    /// bitrate real, intra forzados y peticiones agrupadas, cambios de bitrate y descartes.
    pub(crate) fn host_line(
        &mut self,
        counters: &Counters,
        ledger: &LatencyLedger,
        target_kbps: u32,
    ) -> String {
        let (seconds, d, dropped) = self.advance(counters, ledger);
        let (p50, p95) = take_percentiles(&counters.encode_us);
        let fps = |n: u64| n as f64 / seconds;
        format!(
            "anfitrión: captura {:.1} fps, codifica {:.1} fps ({p50:.1}/{p95:.1} ms p50/p95), \
             envía {:.1} fps, objetivo {target_kbps} kbit/s, real {:.0} kbit/s, intra forzados {} \
             (agrupados {}), cambios de bitrate {}, sin enviar por enlace atascado {}, descartes \
             {dropped}",
            fps(d[0]),
            fps(d[1]),
            fps(d[3]),
            d[2] as f64 * 8.0 / 1000.0 / seconds,
            d[8],
            d[9],
            d[11],
            d[13],
        )
    }

    /// Línea del invitado: `recibe/decodifica/presenta` en fps, tiempos de decodificación,
    /// bitrate recibido, grupos perdidos, objetos tardíos y perdidos, `Nack` y peticiones de intra.
    pub(crate) fn guest_line(
        &mut self,
        counters: &Counters,
        ledger: &LatencyLedger,
        link: LinkCounters,
    ) -> String {
        let (seconds, d, dropped) = self.advance(counters, ledger);
        let (p50, p95) = take_percentiles(&counters.decode_us);
        let fps = |n: u64| n as f64 / seconds;
        let mut before = std::mem::replace(&mut self.link, link);
        if link.dropped < before.dropped || link.nacks < before.nacks {
            // Receptor nuevo (reconexión): sus contadores empiezan de cero.
            before = LinkCounters::default();
        }
        format!(
            "invitado: recibe {:.1} fps ({:.0} kbit/s), decodifica {:.1} fps ({p50:.1}/{p95:.1} ms \
             p50/p95), presenta {:.1} fps, grupos perdidos {}, objetos perdidos {} y tardíos {}, \
             nack {}, intra pedidos {} (+{} del receptor), cambios de decodificador {}, descartes \
             {dropped}",
            fps(d[4]),
            d[5] as f64 * 8.0 / 1000.0 / seconds,
            fps(d[6]),
            fps(d[7]),
            link.dropped.saturating_sub(before.dropped),
            link.lost_objects.saturating_sub(before.lost_objects),
            link.late.saturating_sub(before.late),
            link.nacks.saturating_sub(before.nacks),
            d[10],
            link.keyframe_requests
                .saturating_sub(before.keyframe_requests),
            d[12],
        )
    }
}

fn drops(ledger: &LatencyLedger) -> [u64; Stage::ALL.len()] {
    let mut out = [0; Stage::ALL.len()];
    for drop in ledger.summary().drops {
        out[drop.stage.index()] = drop.count;
    }
    out
}

/// Nombre corto de una etapa para la línea.
fn stage_id(stage: Stage) -> &'static str {
    match stage {
        Stage::Captured => "captura",
        Stage::Encoded => "codif",
        Stage::Encrypted => "cifr",
        Stage::Sent => "envío",
        Stage::Received => "recep",
        Stage::Decrypted => "descifr",
        Stage::Decoded => "decod",
        Stage::Presented => "pres",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_report_rates_since_the_previous_one() {
        let counters = Counters::default();
        let ledger = LatencyLedger::new();
        let mut window = StatsWindow::new(&counters, &ledger);
        for _ in 0..30 {
            bump(&counters.encoded);
        }
        counters.record_encode(Duration::from_millis(2));
        counters.record_encode(Duration::from_millis(4));
        ledger.record_drop(Stage::Sent);
        bump(&counters.forced_keyframes);
        let line = window.host_line(&counters, &ledger, 8_000);
        assert!(line.contains("intra forzados 1"), "{line}");
        assert!(line.contains("descartes envío=1"), "{line}");
        assert!(line.contains("objetivo 8000 kbit/s"), "{line}");
        // La siguiente solo cuenta lo nuevo.
        let line = window.host_line(&counters, &ledger, 8_000);
        assert!(line.contains("intra forzados 0"), "{line}");
        assert!(line.contains("descartes 0"), "{line}");
        let line = window.guest_line(
            &counters,
            &ledger,
            LinkCounters {
                dropped: 2,
                ..LinkCounters::default()
            },
        );
        assert!(line.contains("grupos perdidos 2"), "{line}");
    }

    #[test]
    fn the_audio_line_counts_what_is_new_per_track() {
        let counters = Counters::default();
        let ledger = LatencyLedger::new();
        let mut window = StatsWindow::new(&counters, &ledger);
        assert_eq!(window.audio_line(&ledger), None, "sin audio no hay línea");
        let game = |received, concealed, device_underruns| AudioCounters {
            received,
            concealed,
            device_underruns,
            max_gap_frames: 3,
            jitter_target_ms: 20.0,
            ..AudioCounters::default()
        };
        ledger.set_audio_counters(AudioTrack::Game, game(250, 4, 2));
        let line = window.audio_line(&ledger).unwrap();
        assert!(
            line.contains("audio del juego: recibe 250, ocultadas 4"),
            "{line}"
        );
        assert!(line.contains("salida sin datos 2"), "{line}");
        assert!(!line.contains("voz"), "{line}");
        ledger.set_audio_counters(AudioTrack::Game, game(500, 5, 2));
        let line = window.audio_line(&ledger).unwrap();
        assert!(line.contains("recibe 250, ocultadas 1"), "{line}");
        assert!(line.contains("salida sin datos 0"), "{line}");
        // Reconexión: los contadores vuelven a empezar.
        ledger.set_audio_counters(AudioTrack::Game, game(10, 0, 0));
        let line = window.audio_line(&ledger).unwrap();
        assert!(line.contains("recibe 10,"), "{line}");
    }
}
