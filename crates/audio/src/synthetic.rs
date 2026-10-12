//! Fuente y sumidero sintéticos, para los tests y `--headless` (sin dispositivos de audio).
//!
//! - [`ToneSource`]: una senoidal continua (fase sin saltos entre tramas) que entrega una trama
//!   cada 10 ms **en tiempo real** (como un dispositivo: la trama está lista 10 ms después de su
//!   primera muestra) con la marca de captura exacta de esa primera muestra.
//! - [`CaptureSink`]: un dispositivo de reproducción virtual que consume a ritmo real con una
//!   latencia fija: `write` bloquea mientras haya más de [`CaptureSink::BUFFER`] en cola, devuelve
//!   cuándo sonará la trama y guarda todo lo reproducido (con esas horas) en una [`Recording`]
//!   compartida para que el test la analice. Es para tests: la grabación crece.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::{
    AudioConfig, AudioError, AudioFormat, AudioSink, AudioSource, Captured, FRAME_DURATION, Played,
    SAMPLES_PER_CHANNEL,
};

/// Senoidal con marcas de tiempo exactas.
#[derive(Debug, Clone)]
pub struct ToneSource {
    freq: f64,
    amplitude: f64,
    channels: u16,
    paced: bool,
    origin: Option<Instant>,
    frame: u64,
}

impl ToneSource {
    /// Tono de `freq` Hz a −10 dBFS (amplitud 0,3) en `channels` canales, a ritmo real.
    pub fn new(freq: f64, channels: u16) -> Self {
        Self {
            freq,
            amplitude: 0.3,
            channels: channels.clamp(1, 2),
            paced: true,
            origin: None,
            frame: 0,
        }
    }

    /// Amplitud de pico (0-1).
    pub fn with_amplitude(mut self, amplitude: f64) -> Self {
        self.amplitude = amplitude.clamp(0.0, 1.0);
        self
    }

    /// Sin esperar: cada `read` devuelve la trama siguiente al momento (las marcas siguen siendo
    /// las de una captura a ritmo real desde `start`).
    pub fn unpaced(mut self) -> Self {
        self.paced = false;
        self
    }

    /// Tramas entregadas.
    pub fn frames_read(&self) -> u64 {
        self.frame
    }

    /// Frecuencia del tono.
    pub fn freq(&self) -> f64 {
        self.freq
    }
}

impl AudioSource for ToneSource {
    fn format(&self) -> AudioFormat {
        AudioFormat {
            sample_rate_hz: crate::SAMPLE_RATE_HZ,
            channels: self.channels,
        }
    }

    fn start(&mut self, config: &AudioConfig) -> Result<(), AudioError> {
        self.channels = config.format.channels.clamp(1, 2);
        self.origin = Some(Instant::now());
        self.frame = 0;
        Ok(())
    }

    fn read(&mut self, out: &mut Vec<i16>) -> Result<Captured, AudioError> {
        let origin = self.origin.ok_or(AudioError::Stopped)?;
        let captured_at = origin + FRAME_DURATION * self.frame as u32;
        let ready = captured_at + FRAME_DURATION;
        if self.paced {
            let now = Instant::now();
            if ready > now {
                std::thread::sleep(ready - now);
            }
        }
        let channels = usize::from(self.channels);
        out.clear();
        out.resize(SAMPLES_PER_CHANNEL * channels, 0);
        let first = self.frame * SAMPLES_PER_CHANNEL as u64;
        let step = 2.0 * std::f64::consts::PI * self.freq / f64::from(crate::SAMPLE_RATE_HZ);
        for (i, frame) in out.chunks_exact_mut(channels).enumerate() {
            let value = ((first + i as u64) as f64 * step).sin() * self.amplitude * 32_767.0;
            frame.fill(value as i16);
        }
        self.frame += 1;
        Ok(Captured {
            samples: out.len(),
            captured_at,
            channels: self.channels,
        })
    }

    fn stop(&mut self) {
        self.origin = None;
    }

    fn describe(&self) -> String {
        format!("tono sintético de {} Hz", self.freq)
    }
}

/// Lo que ha reproducido un [`CaptureSink`].
#[derive(Debug, Clone, Default)]
pub struct Recording {
    /// Canales.
    pub channels: u16,
    /// Muestras intercaladas, en orden.
    pub samples: Vec<i16>,
    /// Por cada escritura: cuándo suena y en qué muestra empieza.
    pub writes: Vec<(Instant, usize)>,
    /// Veces que el dispositivo virtual se quedó sin nada que reproducir.
    pub underruns: u64,
}

impl Recording {
    /// Nivel RMS (dBFS) de cada trama de 10 ms reproducida.
    pub fn frame_levels(&self) -> Vec<f32> {
        let frame = SAMPLES_PER_CHANNEL * usize::from(self.channels.max(1));
        self.samples
            .chunks_exact(frame)
            .map(crate::rms_dbfs)
            .collect()
    }

    /// Mayor racha de tramas por debajo de `threshold_dbfs` después de la primera que lo supera
    /// (sin contar el silencio final si `ignore_tail`).
    pub fn longest_silence(&self, threshold_dbfs: f32, ignore_tail: bool) -> usize {
        let levels = self.frame_levels();
        let Some(start) = levels.iter().position(|level| *level > threshold_dbfs) else {
            return 0;
        };
        let end = if ignore_tail {
            levels
                .iter()
                .rposition(|level| *level > threshold_dbfs)
                .map_or(levels.len(), |i| i + 1)
        } else {
            levels.len()
        };
        let mut longest = 0;
        let mut run = 0;
        for level in &levels[start..end] {
            if *level > threshold_dbfs {
                run = 0;
            } else {
                run += 1;
                longest = longest.max(run);
            }
        }
        longest
    }
}

fn lock(recording: &Mutex<Recording>) -> MutexGuard<'_, Recording> {
    recording.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Dispositivo de reproducción virtual que graba.
#[derive(Debug)]
pub struct CaptureSink {
    channels: u16,
    latency: Duration,
    paced: bool,
    queued_until: Option<Instant>,
    recording: Arc<Mutex<Recording>>,
    started: bool,
}

impl CaptureSink {
    /// Cola máxima antes de bloquear (dos tramas).
    pub const BUFFER: Duration = Duration::from_millis(20);

    /// Sumidero de `channels` canales con 10 ms de latencia de dispositivo, a ritmo real.
    pub fn new(channels: u16) -> Self {
        Self {
            channels: channels.clamp(1, 2),
            latency: crate::DEVICE_LATENCY,
            paced: true,
            queued_until: None,
            recording: Arc::new(Mutex::new(Recording {
                channels: channels.clamp(1, 2),
                ..Recording::default()
            })),
            started: false,
        }
    }

    /// Latencia del dispositivo virtual (lo que tarda en sonar lo que sale de la cola).
    pub fn with_latency(mut self, latency: Duration) -> Self {
        self.latency = latency;
        self
    }

    /// Sin bloquear (consume al instante; las horas siguen el ritmo real desde la primera).
    pub fn unpaced(mut self) -> Self {
        self.paced = false;
        self
    }

    /// Grabación compartida (se puede leer mientras reproduce).
    pub fn recording(&self) -> Arc<Mutex<Recording>> {
        Arc::clone(&self.recording)
    }
}

impl AudioSink for CaptureSink {
    fn format(&self) -> AudioFormat {
        AudioFormat {
            sample_rate_hz: crate::SAMPLE_RATE_HZ,
            channels: self.channels,
        }
    }

    fn start(&mut self, config: &AudioConfig) -> Result<(), AudioError> {
        self.channels = config.format.channels.clamp(1, 2);
        lock(&self.recording).channels = self.channels;
        self.started = true;
        Ok(())
    }

    fn wait_ready(&mut self) -> Result<(), AudioError> {
        if !self.started {
            return Err(AudioError::Stopped);
        }
        // Hay sitio cuando en cola queda como mucho la cola máxima menos una trama.
        if let (true, Some(until)) = (self.paced, self.queued_until) {
            let now = Instant::now();
            let ready_at = until.checked_sub(Self::BUFFER - FRAME_DURATION);
            if let Some(ready_at) = ready_at
                && ready_at > now
            {
                std::thread::sleep(ready_at - now);
            }
        }
        Ok(())
    }

    fn write(&mut self, samples: &[i16]) -> Result<Played, AudioError> {
        if !self.started {
            return Err(AudioError::Stopped);
        }
        let format = self.format();
        let duration = format.duration_of(samples.len());
        let mut now = Instant::now();
        let mut queued_until = match self.queued_until {
            Some(until) if until >= now || !self.paced => until,
            Some(_) => {
                lock(&self.recording).underruns += 1;
                now
            }
            None => now,
        };
        if self.paced && queued_until > now + Self::BUFFER {
            std::thread::sleep(queued_until - (now + Self::BUFFER));
            now = Instant::now();
            if queued_until < now {
                queued_until = now;
            }
        }
        let played_at_estimate = queued_until + self.latency;
        self.queued_until = Some(queued_until + duration);
        let mut recording = lock(&self.recording);
        let offset = recording.samples.len();
        recording.samples.extend_from_slice(samples);
        recording.writes.push((played_at_estimate, offset));
        Ok(Played { played_at_estimate })
    }

    fn stop(&mut self) {
        self.started = false;
        self.queued_until = None;
    }

    fn describe(&self) -> String {
        "sumidero sintético".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tone_is_continuous_and_timestamped() {
        let mut source = ToneSource::new(1_000.0, 2).with_amplitude(0.5).unpaced();
        assert_eq!(source.read(&mut Vec::new()), Err(AudioError::Stopped));
        source.start(&AudioConfig::game()).expect("arranque");
        let mut frame = Vec::new();
        let first = source.read(&mut frame).expect("trama");
        let second = source.read(&mut frame).expect("trama");
        assert_eq!(first.samples, 960);
        assert_eq!(second.captured_at - first.captured_at, FRAME_DURATION);
        // 1 kHz a 48 kHz: 48 muestras por periodo; los dos canales iguales.
        assert_eq!(frame[0], frame[1]);
        assert!(frame.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0) > 16_000);
        assert_eq!(source.frames_read(), 2);
    }

    #[test]
    fn a_paced_tone_takes_real_time() {
        let mut source = ToneSource::new(440.0, 1);
        source.start(&AudioConfig::voice()).expect("arranque");
        let start = Instant::now();
        let mut frame = Vec::new();
        for _ in 0..5 {
            source.read(&mut frame).expect("trama");
        }
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(49), "{elapsed:?}");
    }

    #[test]
    fn the_sink_paces_records_and_estimates_play_times() {
        let mut sink = CaptureSink::new(1);
        assert!(sink.write(&[0; 480]).is_err(), "sin arrancar");
        sink.start(&AudioConfig::voice()).expect("arranque");
        let recording = sink.recording();
        let start = Instant::now();
        let mut played = Vec::new();
        for i in 0..10 {
            let value = if i == 4 { 0 } else { 8_000 };
            played.push(
                sink.write(&[value; 480])
                    .expect("escritura")
                    .played_at_estimate,
            );
        }
        // 100 ms de audio con 20 ms de cola: la última escritura espera a que queden 20 ms en
        // cola de los 90 encolados antes, unos 70 ms.
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(65), "{elapsed:?}");
        assert!(played.windows(2).all(|w| w[1] - w[0] == FRAME_DURATION));
        assert!(played[0] >= start + Duration::from_millis(10));
        let recording = lock(&recording).clone();
        assert_eq!(recording.samples.len(), 4_800);
        assert_eq!(recording.longest_silence(-40.0, true), 1);
        assert_eq!(recording.frame_levels().len(), 10);
    }

    #[test]
    fn waiting_for_room_keeps_one_frame_queued() {
        let mut sink = CaptureSink::new(1);
        sink.start(&AudioConfig::voice()).expect("arranque");
        let mut lead = Vec::new();
        for _ in 0..10 {
            sink.wait_ready().expect("sitio");
            let now = Instant::now();
            let played = sink.write(&[1; 480]).expect("escritura").played_at_estimate;
            lead.push(played - now);
        }
        // En régimen: una trama en cola (10 ms) más la latencia del dispositivo (10 ms).
        let last = lead[9];
        assert!(last <= Duration::from_millis(21), "{lead:?}");
        assert!(last >= Duration::from_millis(15), "{lead:?}");
    }
}
