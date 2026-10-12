//! Procesado de la voz capturada: supresión de ruido, control de ganancia,
//! cancelación de eco y detección de voz, en tramas mono de 10 ms.
//!
//! - **Linux** (feature `webrtc-apm`): el módulo de procesado de WebRTC 2.1 por la crate
//!   `webrtc-audio-processing` 2.1.0 (tonarino, compilado con meson y ninja; ver `README.md`):
//!   filtro paso alto, NS nivel alto, AGC2 digital adaptativo y **AEC3** con la señal que se
//!   reproduce como referencia ([`VoiceProcessor::feed_render`] o, desde el hilo de reproducción,
//!   [`RenderFeed`]). La crate pide un vector de un puntero por trama (8 bytes): es la única
//!   asignación del camino.
//! - **Windows** (y sin la feature): un respaldo propio sencillo: puerta de ruido por RMS sobre el
//!   suelo de ruido estimado y control de ganancia lento hacia −20 dBFS. **Sin AEC**: en Windows se
//!   asumen auriculares (pendiente de una crate de AEC que compile con MSVC).
//! - **VAD**: el procesado de WebRTC 2.x ya no tiene detector de voz (se quitó del módulo), así que
//!   [`Vad`] es propio en las dos plataformas: nivel de la trama procesada contra el suelo de ruido
//!   (el mínimo de los últimos 1,5 s), con histéresis: entra con dos tramas por encima de
//!   `suelo + 10 dB` y sale tras 300 ms por debajo de `suelo + 6 dB`.
//!
//! DeepFilterNet queda descartado: su crate `deep_filter` no publica desde julio
//! de 2022 (0.2.5), y el principio del proyecto es no depender de nada sin mantener.

use crate::{AudioError, SAMPLES_PER_CHANNEL, f32_to_i16, i16_to_f32, rms_dbfs};

/// Tramas por debajo del umbral antes de dejar de considerar que se habla (300 ms).
pub const VAD_HANGOVER_FRAMES: u32 = 30;

/// Tramas por encima del umbral para empezar a considerar que se habla (20 ms).
pub const VAD_ATTACK_FRAMES: u32 = 2;

/// Nivel por debajo del cual nunca se habla, en dBFS (la voz llega al micrófono entre −40 y
/// −10 dBFS y el AGC la lleva hacia −20).
pub const VAD_MIN_DBFS: f32 = -45.0;

/// Nivel a partir del que una trama es silencio digital y no cuenta para el suelo de ruido.
const DIGITAL_SILENCE_DBFS: f32 = -100.0;

/// Qué procesado aplicar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessingOptions {
    /// Supresión de ruido.
    pub noise_suppression: bool,
    /// Control automático de ganancia.
    pub gain_control: bool,
    /// Cancelación de eco (solo con el módulo de WebRTC).
    pub echo_cancellation: bool,
}

impl Default for ProcessingOptions {
    fn default() -> Self {
        Self {
            noise_suppression: true,
            gain_control: true,
            echo_cancellation: true,
        }
    }
}

impl ProcessingOptions {
    /// Sin procesado (solo VAD): para las fuentes sintéticas de los tests.
    pub const NONE: ProcessingOptions = ProcessingOptions {
        noise_suppression: false,
        gain_control: false,
        echo_cancellation: false,
    };
}

/// Implementación del procesado de este binario.
pub fn backend_label() -> &'static str {
    if cfg!(all(target_os = "linux", feature = "webrtc-apm")) {
        "módulo de procesado de WebRTC 2.1 (NS, AGC2, AEC3) y VAD propio"
    } else {
        "respaldo propio (puerta de ruido y ganancia, sin AEC) y VAD propio"
    }
}

/// Tramas con las que se estima el suelo de ruido (mínimo de los últimos 1,5 s).
pub const VAD_FLOOR_WINDOW: usize = 150;

/// Suelo de ruido máximo, en dBFS: un tono o una voz continua más larga que la ventana no se
/// toman por ruido.
pub const VAD_MAX_FLOOR_DBFS: f32 = -40.0;

/// Detector de voz con histéresis sobre el nivel de cada trama.
///
/// El suelo de ruido es el mínimo del nivel de las últimas [`VAD_FLOOR_WINDOW`] tramas (las pausas
/// entre sílabas lo marcan), acotado a [`VAD_MAX_FLOOR_DBFS`].
#[derive(Debug, Clone)]
pub struct Vad {
    levels: [f32; VAD_FLOOR_WINDOW],
    len: usize,
    pos: usize,
    floor_db: f32,
    speaking: bool,
    above: u32,
    hang: u32,
}

impl Default for Vad {
    fn default() -> Self {
        Self::new()
    }
}

impl Vad {
    /// Detector en silencio, sin historia.
    pub fn new() -> Self {
        Self {
            levels: [0.0; VAD_FLOOR_WINDOW],
            len: 0,
            pos: 0,
            floor_db: -90.0,
            speaking: false,
            above: 0,
            hang: 0,
        }
    }

    /// Si ahora se habla.
    pub fn speaking(&self) -> bool {
        self.speaking
    }

    /// Suelo de ruido estimado en dBFS.
    pub fn floor_dbfs(&self) -> f32 {
        self.floor_db
    }

    /// Procesa el nivel de una trama; devuelve si se habla.
    pub fn update(&mut self, level_dbfs: f32) -> bool {
        let level = level_dbfs.max(-120.0);
        // El silencio digital (un dispositivo que aún no entrega, el retardo del procesado) no
        // dice nada del ruido de la sala.
        if level > DIGITAL_SILENCE_DBFS {
            self.levels[self.pos] = level;
            self.pos = (self.pos + 1) % VAD_FLOOR_WINDOW;
            self.len = (self.len + 1).min(VAD_FLOOR_WINDOW);
        }
        if self.len > 0 {
            self.floor_db = self.levels[..self.len]
                .iter()
                .copied()
                .fold(f32::INFINITY, f32::min)
                .clamp(-90.0, VAD_MAX_FLOOR_DBFS);
        }
        let on = (self.floor_db + 10.0).max(VAD_MIN_DBFS);
        let off = (self.floor_db + 6.0).max(VAD_MIN_DBFS - 5.0);
        if level > on {
            self.above += 1;
            if self.above >= VAD_ATTACK_FRAMES {
                self.speaking = true;
            }
            if self.speaking {
                self.hang = VAD_HANGOVER_FRAMES;
            }
        } else {
            self.above = 0;
            if self.speaking {
                if level > off {
                    self.hang = VAD_HANGOVER_FRAMES;
                } else {
                    self.hang = self.hang.saturating_sub(1);
                    if self.hang == 0 {
                        self.speaking = false;
                    }
                }
            }
        }
        self.speaking
    }
}

/// Resultado de procesar una trama.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VoiceFrame {
    /// Si se habla (VAD con histéresis).
    pub speaking: bool,
    /// Nivel de la trama procesada en dBFS.
    pub level_dbfs: f32,
}

/// Respaldo propio: puerta de ruido y ganancia lenta.
#[derive(Debug, Clone)]
struct Fallback {
    gate_gain: f32,
    agc_gain_db: f32,
}

impl Fallback {
    const TARGET_DBFS: f32 = -20.0;
    const MAX_GAIN_DB: f32 = 18.0;
    const MIN_GAIN_DB: f32 = -6.0;
    /// 3 dB/s en tramas de 10 ms.
    const STEP_DB: f32 = 0.03;

    fn new() -> Self {
        Self {
            gate_gain: 1.0,
            agc_gain_db: 0.0,
        }
    }

    fn process(&mut self, frame: &mut [f32], options: ProcessingOptions, vad: &Vad, level: f32) {
        if options.noise_suppression {
            let open = level > vad.floor_dbfs() + 6.0;
            let wanted = if open { 1.0 } else { 0.125 };
            // Abre en ~10 ms y cierra en ~50 ms.
            let rate = if open { 0.5 } else { 0.2 };
            self.gate_gain += (wanted - self.gate_gain) * rate;
        } else {
            self.gate_gain = 1.0;
        }
        if options.gain_control && vad.speaking() {
            let error = Self::TARGET_DBFS - (level + self.agc_gain_db);
            self.agc_gain_db += error.clamp(-Self::STEP_DB, Self::STEP_DB);
            self.agc_gain_db = self.agc_gain_db.clamp(Self::MIN_GAIN_DB, Self::MAX_GAIN_DB);
        } else if !options.gain_control {
            self.agc_gain_db = 0.0;
        }
        let gain = self.gate_gain * 10f32.powf(self.agc_gain_db / 20.0);
        if (gain - 1.0).abs() > 1e-4 {
            for sample in frame.iter_mut() {
                *sample = crate::mix::soft_limit(*sample * gain);
            }
        }
    }
}

#[cfg(all(target_os = "linux", feature = "webrtc-apm"))]
type Apm = std::sync::Arc<webrtc_audio_processing::Processor>;

#[cfg(all(target_os = "linux", feature = "webrtc-apm"))]
fn create_apm(options: ProcessingOptions) -> Result<Option<Apm>, AudioError> {
    use webrtc_audio_processing::config::{
        AdaptiveDigital, EchoCanceller, FixedDigital, GainController, GainController2,
        HighPassFilter, NoiseSuppression, NoiseSuppressionLevel,
    };
    if options == ProcessingOptions::NONE {
        return Ok(None);
    }
    let processor = webrtc_audio_processing::Processor::new(crate::SAMPLE_RATE_HZ)
        .map_err(|err| AudioError::Backend(format!("procesado de WebRTC: {err}")))?;
    processor.set_config(webrtc_audio_processing::Config {
        high_pass_filter: Some(HighPassFilter::default()),
        echo_canceller: options.echo_cancellation.then_some(EchoCanceller::Full {
            stream_delay_ms: None,
        }),
        noise_suppression: options.noise_suppression.then_some(NoiseSuppression {
            level: NoiseSuppressionLevel::High,
            analyze_linear_aec_output: false,
        }),
        gain_controller: options
            .gain_control
            .then_some(GainController::GainController2(GainController2 {
                input_volume_controller_enabled: false,
                adaptive_digital: Some(AdaptiveDigital::default()),
                fixed_digital: FixedDigital::default(),
            })),
        ..Default::default()
    });
    Ok(Some(std::sync::Arc::new(processor)))
}

/// Alimentación del AEC con lo que se reproduce, para el hilo de reproducción (`audio-play`).
#[derive(Clone)]
pub struct RenderFeed {
    #[cfg(all(target_os = "linux", feature = "webrtc-apm"))]
    apm: Option<Apm>,
    scratch: [f32; SAMPLES_PER_CHANNEL],
}

impl std::fmt::Debug for RenderFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderFeed").finish_non_exhaustive()
    }
}

impl RenderFeed {
    /// Pasa al AEC una trama de 10 ms de lo que se va a reproducir (mono o estéreo intercalado;
    /// se mezcla a mono). Sin AEC, no hace nada.
    pub fn feed_render(&mut self, frame: &[i16], channels: u16) {
        #[cfg(all(target_os = "linux", feature = "webrtc-apm"))]
        if let Some(apm) = &self.apm {
            if frame.len() < SAMPLES_PER_CHANNEL * usize::from(channels.max(1)) {
                return;
            }
            match channels {
                2 => {
                    for (out, pair) in self.scratch.iter_mut().zip(frame.as_chunks::<2>().0) {
                        *out = (f32::from(pair[0]) + f32::from(pair[1])) / 65_536.0;
                    }
                }
                _ => i16_to_f32(frame, &mut self.scratch),
            }
            // Un fallo del AEC en una trama de referencia no para la voz: se ignora.
            let _ = apm.analyze_render_frame([&self.scratch[..]]);
        }
        #[cfg(not(all(target_os = "linux", feature = "webrtc-apm")))]
        let _ = (frame, channels, &mut self.scratch);
    }
}

/// Procesado de la voz del micrófono.
pub struct VoiceProcessor {
    options: ProcessingOptions,
    #[cfg(all(target_os = "linux", feature = "webrtc-apm"))]
    apm: Option<Apm>,
    fallback: Fallback,
    vad: Vad,
    scratch: [f32; SAMPLES_PER_CHANNEL],
    render: RenderFeed,
}

impl std::fmt::Debug for VoiceProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceProcessor")
            .field("options", &self.options)
            .field("backend", &self.backend())
            .finish_non_exhaustive()
    }
}

impl VoiceProcessor {
    /// Procesador con `options`.
    pub fn new(options: ProcessingOptions) -> Result<Self, AudioError> {
        #[cfg(all(target_os = "linux", feature = "webrtc-apm"))]
        let apm = create_apm(options)?;
        Ok(Self {
            options,
            render: RenderFeed {
                #[cfg(all(target_os = "linux", feature = "webrtc-apm"))]
                apm: apm.clone(),
                scratch: [0.0; SAMPLES_PER_CHANNEL],
            },
            #[cfg(all(target_os = "linux", feature = "webrtc-apm"))]
            apm,
            fallback: Fallback::new(),
            vad: Vad::new(),
            scratch: [0.0; SAMPLES_PER_CHANNEL],
        })
    }

    /// Qué procesa de verdad: `"webrtc"`, `"respaldo"` o `"ninguno"`.
    pub fn backend(&self) -> &'static str {
        #[cfg(all(target_os = "linux", feature = "webrtc-apm"))]
        if self.apm.is_some() {
            return "webrtc";
        }
        if self.options == ProcessingOptions::NONE {
            "ninguno"
        } else {
            "respaldo"
        }
    }

    /// Si cancela el eco.
    pub fn echo_cancellation(&self) -> bool {
        #[cfg(all(target_os = "linux", feature = "webrtc-apm"))]
        if self.apm.is_some() {
            return self.options.echo_cancellation;
        }
        false
    }

    /// Detector de voz.
    pub fn vad(&self) -> &Vad {
        &self.vad
    }

    /// Asa para alimentar el AEC desde otro hilo.
    pub fn render_feed(&self) -> RenderFeed {
        self.render.clone()
    }

    /// Pasa al AEC una trama de lo que se reproduce (ver [`RenderFeed::feed_render`]).
    pub fn feed_render(&mut self, frame: &[i16], channels: u16) {
        self.render.feed_render(frame, channels);
    }

    /// Procesa en sitio una trama mono de 10 ms del micrófono y dice si se habla.
    pub fn process(&mut self, frame: &mut [i16]) -> Result<VoiceFrame, AudioError> {
        let available = frame.len();
        let frame = frame
            .get_mut(..SAMPLES_PER_CHANNEL)
            .ok_or(AudioError::BufferTooSmall {
                needed: SAMPLES_PER_CHANNEL,
                available,
            })?;
        i16_to_f32(frame, &mut self.scratch);
        #[cfg(all(target_os = "linux", feature = "webrtc-apm"))]
        let processed = match &self.apm {
            Some(apm) => {
                apm.process_capture_frame([&mut self.scratch[..]])
                    .map_err(|err| AudioError::Backend(format!("procesado de WebRTC: {err}")))?;
                true
            }
            None => false,
        };
        #[cfg(not(all(target_os = "linux", feature = "webrtc-apm")))]
        let processed = false;
        if !processed && self.options != ProcessingOptions::NONE {
            let level = rms_dbfs(frame);
            self.fallback
                .process(&mut self.scratch, self.options, &self.vad, level);
        }
        f32_to_i16(&self.scratch, frame);
        let level_dbfs = rms_dbfs(frame);
        let speaking = self.vad.update(level_dbfs);
        Ok(VoiceFrame {
            speaking,
            level_dbfs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_frame(freq: f64, amplitude: f64, index: usize) -> Vec<i16> {
        (index * SAMPLES_PER_CHANNEL..(index + 1) * SAMPLES_PER_CHANNEL)
            .map(|n| {
                ((2.0 * std::f64::consts::PI * freq * n as f64 / 48_000.0).sin()
                    * amplitude
                    * 32_767.0) as i16
            })
            .collect()
    }

    /// Ruido pseudoaleatorio de amplitud `amplitude`.
    fn noise_frame(amplitude: f64, seed: &mut u64) -> Vec<i16> {
        (0..SAMPLES_PER_CHANNEL)
            .map(|_| {
                *seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let v = ((*seed >> 33) as f64 / f64::from(u32::MAX >> 1)) * 2.0 - 1.0;
                (v * amplitude * 32_767.0) as i16
            })
            .collect()
    }

    #[test]
    fn vad_tells_a_tone_from_silence_with_hysteresis() {
        let mut vad = Vad::new();
        let mut seed = 1;
        // Ruido de fondo bajo: no se habla.
        for _ in 0..100 {
            assert!(!vad.update(rms_dbfs(&noise_frame(0.001, &mut seed))));
        }
        // Tono: entra en dos tramas.
        let mut first = None;
        for i in 0..50 {
            if vad.update(rms_dbfs(&tone_frame(300.0, 0.3, i))) && first.is_none() {
                first = Some(i);
            }
        }
        assert_eq!(first, Some(1), "entra en la segunda trama");
        // Silencio: se mantiene 300 ms y después sale.
        let mut off = None;
        for i in 0..60 {
            if !vad.update(rms_dbfs(&noise_frame(0.001, &mut seed))) && off.is_none() {
                off = Some(i);
            }
        }
        assert_eq!(off, Some(VAD_HANGOVER_FRAMES as usize - 1));
        assert!(vad.floor_dbfs() < -50.0, "{}", vad.floor_dbfs());
    }

    #[test]
    fn the_processor_detects_voice_through_its_pipeline() {
        for options in [ProcessingOptions::default(), ProcessingOptions::NONE] {
            let mut processor = VoiceProcessor::new(options).expect("procesador");
            let mut feed = processor.render_feed();
            let mut seed = 7;
            let mut speaking_frames = 0;
            let mut silent_speaking = 0;
            for i in 0..300 {
                // 1 s de tono modulado como sílabas, 1 s de ruido bajo, 1 s de tono.
                let mut frame = if (100..200).contains(&i) {
                    noise_frame(0.002, &mut seed)
                } else {
                    let envelope = 0.5 + 0.5 * ((i as f64) * 0.6).sin().abs();
                    let mut tone = tone_frame(250.0, 0.3, i);
                    for s in &mut tone {
                        *s = (f64::from(*s) * envelope) as i16;
                    }
                    tone
                };
                feed.feed_render(&[0; SAMPLES_PER_CHANNEL * 2], 2);
                let result = processor.process(&mut frame).expect("procesar");
                if std::env::var_os("COROOM_VAD_DEBUG").is_some() && i % 5 == 0 {
                    println!(
                        "{i}: {:.1} dBFS suelo {:.1} {}",
                        result.level_dbfs,
                        processor.vad().floor_dbfs(),
                        result.speaking
                    );
                }
                if (10..100).contains(&i) && result.speaking {
                    speaking_frames += 1;
                }
                if (140..200).contains(&i) && result.speaking {
                    silent_speaking += 1;
                }
            }
            println!(
                "{}: {speaking_frames}/90 tramas de voz detectadas, {silent_speaking}/60 en silencio",
                processor.backend()
            );
            assert!(speaking_frames >= 80, "{options:?}: {speaking_frames}");
            assert_eq!(silent_speaking, 0, "{options:?}");
        }
    }

    #[test]
    fn processing_reports_its_backend() {
        let processor = VoiceProcessor::new(ProcessingOptions::NONE).expect("procesador");
        assert_eq!(processor.backend(), "ninguno");
        assert!(!processor.echo_cancellation());
        let full = VoiceProcessor::new(ProcessingOptions::default()).expect("procesador");
        if cfg!(all(target_os = "linux", feature = "webrtc-apm")) {
            assert_eq!(full.backend(), "webrtc");
            assert!(full.echo_cancellation());
        } else {
            assert_eq!(full.backend(), "respaldo");
        }
        assert!(!backend_label().is_empty());
        let mut short = [0i16; 10];
        let mut processor = processor;
        assert!(processor.process(&mut short).is_err());
    }
}
