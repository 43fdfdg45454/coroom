//! Sincronía del audio del juego con el vídeo.
//!
//! El invitado conoce, en su reloj, cuándo se capturó cada fotograma (la marca `Captured` del
//! anfitrión convertida con el desfase de `Ping`/`Pong`) y cuándo lo presentó, y lo mismo para
//! cada trama de audio (su `capture_us` convertido y la hora estimada a la que suena,
//! [`crate::Played`]). Para cada trama de audio, [`AvSync`] busca el fotograma cuya captura es la
//! más cercana y calcula
//!
//! `A − V = (suena − capturado)_audio − (presentado − capturado)_vídeo`
//!
//! (positivo: el audio va tarde). Con la media móvil de esas muestras recomienda cuánto retardo
//! extra dar al buffer de jitter del audio ([`AvSync::recommend`]): si el audio va adelantado, más
//! retardo (se estira); si va retrasado, menos (se recorta, hasta el mínimo del buffer). El vídeo no
//! se retrasa nunca. Cada recomendación mueve el ajuste como mucho una trama (10 ms) y solo si el
//! desfase supera media trama, para no oscilar.

use std::time::{Duration, Instant};

use crate::FRAME_DURATION_MS;

/// Fotogramas de vídeo recordados (unos 4 s a 60 fps).
pub const VIDEO_WINDOW: usize = 256;

/// Muestras de |A−V| para los percentiles.
pub const OFFSET_WINDOW: usize = 512;

/// Distancia máxima entre capturas de audio y vídeo para emparejarlas.
pub const MAX_PAIR_DISTANCE: Duration = Duration::from_millis(100);

/// Ajuste máximo que recomienda, en ms (en los dos sentidos).
pub const MAX_ADJUST_MS: i32 = 200;

/// Peso de cada muestra nueva en la media móvil.
const SMOOTHING: f64 = 0.05;

/// Desfase a partir del que se corrige, en µs (media trama).
const DEADBAND_US: f64 = (FRAME_DURATION_MS as f64) * 1000.0 / 2.0;

/// Percentiles de |A−V|.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AvStats {
    /// Muestras en la ventana.
    pub samples: usize,
    /// Mediana de |A−V| en ms.
    pub p50_abs_ms: f64,
    /// Percentil 95 de |A−V| en ms.
    pub p95_abs_ms: f64,
    /// Desfase medio actual (con signo; positivo = audio tarde) en ms.
    pub offset_ms: f64,
    /// Retardo extra recomendado al buffer de audio, en ms.
    pub adjust_ms: i32,
}

/// Estimador de A−V.
#[derive(Debug, Clone)]
pub struct AvSync {
    video: [(Instant, Instant); VIDEO_WINDOW],
    video_len: usize,
    video_pos: usize,
    offsets: [i64; OFFSET_WINDOW],
    offsets_len: usize,
    offsets_pos: usize,
    smoothed: Option<f64>,
    adjust_ms: i32,
    limits: (i32, i32),
}

impl Default for AvSync {
    fn default() -> Self {
        Self::new()
    }
}

impl AvSync {
    /// Estimador vacío.
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            video: [(now, now); VIDEO_WINDOW],
            video_len: 0,
            video_pos: 0,
            offsets: [0; OFFSET_WINDOW],
            offsets_len: 0,
            offsets_pos: 0,
            smoothed: None,
            adjust_ms: 0,
            limits: (-MAX_ADJUST_MS, MAX_ADJUST_MS),
        }
    }

    /// Lo que el buffer de audio puede ajustar de verdad, en ms (el recorte acaba en su mínimo):
    /// la recomendación no pasa de ahí.
    pub fn set_limits(&mut self, min_ms: i32, max_ms: i32) {
        let min = min_ms.clamp(-MAX_ADJUST_MS, 0);
        let max = max_ms.clamp(0, MAX_ADJUST_MS);
        self.limits = (min, max);
        self.adjust_ms = self.adjust_ms.clamp(min, max);
    }

    /// Un fotograma capturado en `captured` (reloj local) se presentó en `presented`.
    pub fn on_video(&mut self, captured: Instant, presented: Instant) {
        self.video[self.video_pos] = (captured, presented);
        self.video_pos = (self.video_pos + 1) % VIDEO_WINDOW;
        self.video_len = (self.video_len + 1).min(VIDEO_WINDOW);
    }

    /// Latencia del fotograma cuya captura es la más cercana a `captured`, si hay alguno a menos
    /// de [`MAX_PAIR_DISTANCE`].
    fn video_latency_near(&self, captured: Instant) -> Option<Duration> {
        let distance = |at: Instant| {
            if at > captured {
                at - captured
            } else {
                captured - at
            }
        };
        self.video[..self.video_len]
            .iter()
            .min_by_key(|(at, _)| distance(*at))
            .filter(|(at, _)| distance(*at) <= MAX_PAIR_DISTANCE)
            .map(|(at, presented)| presented.saturating_duration_since(*at))
    }

    /// Una trama de audio capturada en `captured` (reloj local) suena en `played`. Devuelve su
    /// A−V en µs, si hay un fotograma con el que compararla.
    pub fn on_audio(&mut self, captured: Instant, played: Instant) -> Option<i64> {
        let video = self.video_latency_near(captured)?;
        let audio = played.saturating_duration_since(captured);
        let offset = audio.as_micros() as i64 - video.as_micros() as i64;
        self.offsets[self.offsets_pos] = offset;
        self.offsets_pos = (self.offsets_pos + 1) % OFFSET_WINDOW;
        self.offsets_len = (self.offsets_len + 1).min(OFFSET_WINDOW);
        self.smoothed = Some(match self.smoothed {
            Some(previous) => previous * (1.0 - SMOOTHING) + offset as f64 * SMOOTHING,
            None => offset as f64,
        });
        Some(offset)
    }

    /// Desfase medio (positivo: el audio va tarde), en ms.
    pub fn offset_ms(&self) -> Option<f64> {
        self.smoothed.map(|us| us / 1000.0)
    }

    /// Retardo extra recomendado para el buffer de audio, en ms. Llamar cada 100 ms o así:
    /// cada llamada lo mueve como mucho una trama. El llamante aplica el ajuste y, como el
    /// audio cambia de latencia en esa cantidad, la media se corrige con ella.
    pub fn recommend(&mut self) -> i32 {
        let Some(offset) = self.smoothed else {
            return self.adjust_ms;
        };
        let step = FRAME_DURATION_MS as i32;
        let (min, max) = self.limits;
        let wanted = if offset < -DEADBAND_US {
            (self.adjust_ms + step).min(max)
        } else if offset > DEADBAND_US {
            (self.adjust_ms - step).max(min)
        } else {
            self.adjust_ms
        };
        let moved = wanted - self.adjust_ms;
        if moved != 0 {
            self.adjust_ms = wanted;
            // La media ya cuenta con la corrección (la latencia del audio cambia una trama).
            self.smoothed = Some(offset + f64::from(moved) * 1000.0);
        }
        self.adjust_ms
    }

    /// Ajuste recomendado ahora (sin moverlo).
    pub fn adjust_ms(&self) -> i32 {
        self.adjust_ms
    }

    /// Percentiles de |A−V| de la ventana.
    pub fn stats(&self) -> AvStats {
        let mut samples: Vec<u64> = self.offsets[..self.offsets_len]
            .iter()
            .map(|us| us.unsigned_abs())
            .collect();
        samples.sort_unstable();
        let rank = |p: f64| -> f64 {
            if samples.is_empty() {
                return 0.0;
            }
            let index =
                ((p / 100.0 * samples.len() as f64).ceil() as usize).clamp(1, samples.len());
            samples[index - 1] as f64 / 1000.0
        };
        AvStats {
            samples: samples.len(),
            p50_abs_ms: rank(50.0),
            p95_abs_ms: rank(95.0),
            offset_ms: self.offset_ms().unwrap_or(0.0),
            adjust_ms: self.adjust_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vídeo a 60 fps con 60 ms de latencia y audio cada 10 ms con `audio_ms` + el ajuste; el
    /// ajuste se aplica cada 100 ms. Devuelve las estadísticas al final.
    fn converge(audio_ms: i64) -> (AvStats, Vec<f64>) {
        let mut sync = AvSync::new();
        let base = Instant::now();
        let mut adjust = 0i64;
        let mut trace = Vec::new();
        let mut next_video = Duration::ZERO;
        for tick in 0..600u32 {
            let at = Duration::from_millis(u64::from(tick) * 10);
            while next_video <= at {
                let captured = base + next_video;
                sync.on_video(captured, captured + Duration::from_millis(60));
                next_video += Duration::from_micros(16_667);
            }
            let captured = base + at;
            let latency = Duration::from_millis((audio_ms + adjust).max(0) as u64);
            sync.on_audio(captured, captured + latency);
            if tick % 10 == 9 {
                adjust = i64::from(sync.recommend());
                trace.push(sync.offset_ms().unwrap_or(0.0));
            }
        }
        (sync.stats(), trace)
    }

    #[test]
    fn converges_with_an_injected_30_ms_offset() {
        // El audio llega 30 ms antes que el vídeo: hay que retrasarlo 30 ms.
        let (early, trace) = converge(30);
        println!("audio adelantado: {early:?}\n{trace:?}");
        assert_eq!(early.adjust_ms, 30, "{early:?}");
        assert!(early.offset_ms.abs() < 10.0, "{early:?}");
        // Y si va 30 ms tarde, se recorta.
        let (late, _) = converge(90);
        println!("audio retrasado: {late:?}");
        assert_eq!(late.adjust_ms, -30, "{late:?}");
        assert!(late.offset_ms.abs() < 10.0, "{late:?}");
    }

    #[test]
    fn the_adjustment_stops_at_what_the_buffer_can_do() {
        let mut sync = AvSync::new();
        sync.set_limits(-10, 200);
        let base = Instant::now();
        sync.on_video(base, base + Duration::from_millis(10));
        for _ in 0..20 {
            sync.on_audio(base, base + Duration::from_millis(60));
            sync.recommend();
        }
        assert_eq!(sync.adjust_ms(), -10, "no recorta más de lo que puede");
        assert!(
            sync.offset_ms().is_some_and(|ms| ms > 40.0),
            "la media sigue siendo real"
        );
    }

    #[test]
    fn already_in_sync_stays_put() {
        let (stats, _) = converge(60);
        assert_eq!(stats.adjust_ms, 0);
        assert!(stats.p95_abs_ms < 1.0, "{stats:?}");
        assert_eq!(stats.samples, OFFSET_WINDOW);
    }

    #[test]
    fn audio_without_nearby_video_is_ignored() {
        let mut sync = AvSync::new();
        let base = Instant::now();
        assert_eq!(sync.on_audio(base, base), None);
        sync.on_video(base, base + Duration::from_millis(50));
        let far = base + Duration::from_secs(1);
        assert_eq!(sync.on_audio(far, far), None);
        assert_eq!(
            sync.on_audio(
                base + Duration::from_millis(5),
                base + Duration::from_millis(51)
            ),
            Some(-4_000)
        );
        assert_eq!(
            sync.recommend(),
            0,
            "−4 ms cae en la zona muerta (media trama)"
        );
        assert_eq!(sync.stats().samples, 1);
    }
}
