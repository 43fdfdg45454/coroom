//! Audio de coroom: audio del juego y voz de la sala.
//!
//! Todo el pipeline trabaja a **48 kHz** con tramas de **10 ms** en PCM `i16` intercalado:
//!
//! - **Captura** ([`AudioSource`]): en Linux, **PipeWire** nativo ([`pipewire`]: monitor del *sink*
//!   por defecto, el nodo de una aplicación o el micrófono); en Windows, **WASAPI** ([`wasapi`]:
//!   *loopback* del dispositivo de salida, *loopback* por proceso y micrófono). Para los tests,
//!   [`synthetic::ToneSource`], una senoidal con marcas de tiempo exactas.
//! - **Reproducción** ([`AudioSink`]): PipeWire o WASAPI, y [`synthetic::CaptureSink`], que guarda
//!   lo que se reproduce. Cada escritura devuelve cuándo se oirá ([`Played`]) para la sincronía.
//! - **Códec** ([`codec`]): Opus de libopus 1.6.1 (crate `opusic-c`) con perfiles
//!   [`codec::Profile::Game`] (estéreo, `restricted low-delay`) y [`codec::Profile::Voice`] (mono,
//!   `voip`, DTX, FEC en banda y, en Linux, DRED); PLC con `decode(None)`.
//! - **Buffer de jitter** ([`jitter::JitterBuffer`]) adaptativo por emisor, **mezcla**
//!   ([`mix::Mixer`]), **procesado de voz** ([`processing::VoiceProcessor`], el módulo de WebRTC en
//!   Linux) y **sincronía A/V** ([`sync::AvSync`]).
//!
//! Los backends de plataforma van detrás de los traits y de una `feature` (`pipewire`, `wasapi`,
//! `webrtc-apm`); [`game_source`], [`mic_source`] y [`playback_sink`] eligen el de la plataforma.

use std::fmt;
use std::time::{Duration, Instant};

pub mod apps;
pub mod codec;
pub mod jitter;
pub mod mix;
pub mod processing;
pub mod sync;
pub mod synthetic;

// La mezcla por aplicación (`apps`) lo usa siempre; sin backend nativo, solo ella.
#[cfg_attr(
    not(any(
        all(target_os = "linux", feature = "pipewire"),
        all(target_os = "windows", feature = "wasapi")
    )),
    allow(dead_code)
)]
mod ring;

#[cfg(all(target_os = "linux", feature = "pipewire"))]
pub mod pipewire;

#[cfg(all(target_os = "windows", feature = "wasapi"))]
pub mod wasapi;

/// Frecuencia de muestreo de todo el pipeline de audio, en Hz.
pub const SAMPLE_RATE_HZ: u32 = 48_000;

/// Duración de una trama de audio, en milisegundos.
pub const FRAME_DURATION_MS: u32 = 10;

/// Duración de una trama de audio.
pub const FRAME_DURATION: Duration = Duration::from_millis(FRAME_DURATION_MS as u64);

/// Muestras por canal de una trama (480 a 48 kHz).
pub const SAMPLES_PER_CHANNEL: usize = (SAMPLE_RATE_HZ / 1000 * FRAME_DURATION_MS) as usize;

/// Muestras de la trama más grande (estéreo).
pub const MAX_FRAME_SAMPLES: usize = SAMPLES_PER_CHANNEL * 2;

/// Latencia de dispositivo que se pide por defecto (`node.latency = 480/48000` en PipeWire).
pub const DEVICE_LATENCY: Duration = FRAME_DURATION;

/// Formato de las tramas que entrega una fuente.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    /// Frecuencia de muestreo en Hz.
    pub sample_rate_hz: u32,
    /// Número de canales intercalados.
    pub channels: u16,
}

impl AudioFormat {
    /// Estéreo a 48 kHz, el formato del audio del juego.
    pub const GAME: AudioFormat = AudioFormat {
        sample_rate_hz: SAMPLE_RATE_HZ,
        channels: 2,
    };

    /// Mono a 48 kHz, el formato de la voz.
    pub const VOICE: AudioFormat = AudioFormat {
        sample_rate_hz: SAMPLE_RATE_HZ,
        channels: 1,
    };

    /// Muestras (sumando todos los canales) de una trama de [`FRAME_DURATION_MS`].
    pub const fn samples_per_frame(self) -> usize {
        (self.sample_rate_hz / 1000 * FRAME_DURATION_MS) as usize * self.channels as usize
    }

    /// Duración de `samples` muestras intercaladas.
    pub fn duration_of(self, samples: usize) -> Duration {
        let frames = samples / usize::from(self.channels.max(1));
        Duration::from_micros(frames as u64 * 1_000_000 / u64::from(self.sample_rate_hz.max(1)))
    }
}

/// Error de un backend de audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioError {
    /// El dispositivo o el proceso de origen ya no existe.
    DeviceLost,
    /// El búfer no tiene sitio para una trama completa.
    BufferTooSmall {
        /// Muestras necesarias.
        needed: usize,
        /// Muestras disponibles en el búfer.
        available: usize,
    },
    /// No se puede en esta plataforma o con estas opciones (el texto explica por qué).
    Unsupported(String),
    /// No hay ningún dispositivo con ese nombre.
    NoSuchDevice(String),
    /// La fuente o el sumidero no está en marcha (o ya se detuvo).
    Stopped,
    /// Error del códec Opus.
    Codec(String),
    /// Error interno del backend.
    Backend(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::DeviceLost => f.write_str("el dispositivo de audio ya no existe"),
            AudioError::BufferTooSmall { needed, available } => write!(
                f,
                "búfer de audio demasiado pequeño: hacen falta {needed} muestras y hay {available}"
            ),
            AudioError::Unsupported(what) => write!(f, "audio no disponible: {what}"),
            AudioError::NoSuchDevice(name) => {
                write!(f, "no hay ningún dispositivo de audio llamado «{name}»")
            }
            AudioError::Stopped => f.write_str("el flujo de audio no está en marcha"),
            AudioError::Codec(what) => write!(f, "error de Opus: {what}"),
            AudioError::Backend(what) => write!(f, "error del backend de audio: {what}"),
        }
    }
}

impl std::error::Error for AudioError {}

/// Configuración de un flujo de captura o reproducción.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioConfig {
    /// Formato pedido (el backend convierte si el dispositivo usa otro).
    pub format: AudioFormat,
    /// Dispositivo por nombre (`None`: el del sistema).
    pub device: Option<String>,
    /// Latencia de dispositivo pedida.
    pub latency: Duration,
}

impl AudioConfig {
    /// Estéreo del juego en el dispositivo por defecto.
    pub fn game() -> Self {
        Self {
            format: AudioFormat::GAME,
            device: None,
            latency: DEVICE_LATENCY,
        }
    }

    /// Mono de la voz en el dispositivo por defecto.
    pub fn voice() -> Self {
        Self {
            format: AudioFormat::VOICE,
            device: None,
            latency: DEVICE_LATENCY,
        }
    }

    /// La misma configuración en el dispositivo `device`.
    pub fn with_device(mut self, device: Option<String>) -> Self {
        self.device = device;
        self
    }
}

/// Una trama capturada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Captured {
    /// Muestras intercaladas escritas en el búfer.
    pub samples: usize,
    /// Instante (reloj monótono local) en que se capturó la **primera** muestra de la trama.
    pub captured_at: Instant,
    /// Canales intercalados.
    pub channels: u16,
}

/// Una trama entregada al dispositivo de reproducción.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Played {
    /// Cuándo se oirá su primera muestra: ahora más lo que ya hay en cola más la latencia del
    /// dispositivo (estimada).
    pub played_at_estimate: Instant,
}

/// Fuente de tramas de audio PCM.
///
/// Es `Send` porque cada fuente vive en su hilo de audio (`audio-cap`, `voice-cap`).
pub trait AudioSource: Send {
    /// Formato de las tramas que entrega.
    fn format(&self) -> AudioFormat;

    /// Abre el dispositivo y empieza a capturar.
    fn start(&mut self, config: &AudioConfig) -> Result<(), AudioError>;

    /// Espera la siguiente trama de 10 ms y la deja en `out` (se redimensiona a
    /// [`AudioFormat::samples_per_frame`] y se reutiliza su capacidad).
    fn read(&mut self, out: &mut Vec<i16>) -> Result<Captured, AudioError>;

    /// Deja de capturar y suelta el dispositivo.
    fn stop(&mut self);

    /// Qué captura, en palabras («monitor de la salida por defecto (PipeWire)»…).
    fn describe(&self) -> String {
        "fuente de audio".to_owned()
    }

    /// Como [`AudioSource::read`], en coma flotante (`[-1, 1]`). Devuelve las muestras escritas.
    /// Es una comodidad: el pipeline usa [`AudioSource::read`].
    fn read_frame(&mut self, buffer: &mut [f32]) -> Result<usize, AudioError> {
        let needed = self.format().samples_per_frame();
        if buffer.len() < needed {
            return Err(AudioError::BufferTooSmall {
                needed,
                available: buffer.len(),
            });
        }
        let mut pcm = Vec::with_capacity(needed);
        let captured = self.read(&mut pcm)?;
        for (out, sample) in buffer.iter_mut().zip(&pcm[..captured.samples]) {
            *out = f32::from(*sample) / 32_768.0;
        }
        Ok(captured.samples)
    }
}

/// Sumidero de reproducción.
///
/// [`AudioSink::wait_ready`] (o, si no se llama, [`AudioSink::write`]) bloquea mientras el
/// dispositivo tiene en cola más de su latencia objetivo, así que el hilo que escribe
/// (`audio-play`) va al ritmo del dispositivo.
pub trait AudioSink: Send {
    /// Formato que acepta.
    fn format(&self) -> AudioFormat;

    /// Abre el dispositivo.
    fn start(&mut self, config: &AudioConfig) -> Result<(), AudioError>;

    /// Espera a que el dispositivo admita una trama más. La reproducción la llama antes de sacar
    /// la trama del buffer de jitter y decodificarla, así lo que suena es lo más reciente y
    /// [`AudioSink::write`] ya no espera. Por defecto no espera (y `write` bloquea).
    fn wait_ready(&mut self) -> Result<(), AudioError> {
        Ok(())
    }

    /// Encola una trama (muestras intercaladas en [`AudioSink::format`]).
    fn write(&mut self, samples: &[i16]) -> Result<Played, AudioError>;

    /// Suelta el dispositivo.
    fn stop(&mut self);

    /// Dónde reproduce, en palabras.
    fn describe(&self) -> String {
        "salida de audio".to_owned()
    }
}

/// Qué audio del juego captura el anfitrión (`coroom host --audio`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameCapture {
    /// Sin audio del juego.
    None,
    /// Lo que suena en la salida por defecto (monitor en PipeWire, loopback en WASAPI).
    System,
    /// Solo una aplicación, por su nombre (PipeWire: `application.name` o `node.name`).
    App(String),
    /// Solo un proceso y sus hijos (WASAPI, loopback por proceso; Windows 10 20348+).
    Process(u32),
    /// La mezcla de las aplicaciones que elige el filtro ([`apps::AppMixSource`]), igual en todas
    /// las plataformas. coroom nunca entra.
    Apps(apps::AppFilter),
}

impl fmt::Display for GameCapture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GameCapture::None => f.write_str("sin audio del juego"),
            GameCapture::System => f.write_str("salida del sistema"),
            GameCapture::App(name) => write!(f, "aplicación «{name}»"),
            GameCapture::Process(pid) => write!(f, "proceso {pid} y sus hijos"),
            GameCapture::Apps(apps::AppFilter::AllExcept(excluded)) if excluded.is_empty() => {
                f.write_str("todas las aplicaciones")
            }
            GameCapture::Apps(apps::AppFilter::AllExcept(excluded)) => {
                write!(f, "todas las aplicaciones salvo {}", excluded.join(", "))
            }
            GameCapture::Apps(apps::AppFilter::Only(included)) => {
                write!(f, "solo {}", included.join(", "))
            }
        }
    }
}

/// Clase de un dispositivo de audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// Entrada (micrófono).
    Source,
    /// Salida (altavoces, auriculares).
    Sink,
    /// Flujo de una aplicación que reproduce (capturable por nombre).
    Application,
}

/// Dispositivo de audio que lista `coroom doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Nombre con el que se elige (`--mic`, `--speaker`, `--audio app`).
    pub name: String,
    /// Descripción para personas.
    pub description: String,
    /// Clase.
    pub kind: DeviceKind,
    /// Si es el del sistema por defecto.
    pub default: bool,
}

/// Fuente del audio del juego de la plataforma; `Ok(None)` con [`GameCapture::None`].
pub fn game_source(spec: &GameCapture) -> Result<Option<Box<dyn AudioSource>>, AudioError> {
    if *spec == GameCapture::None {
        return Ok(None);
    }
    if let GameCapture::Apps(filter) = spec {
        return Ok(Some(Box::new(apps::AppMixSource::new(
            app_backend()?,
            filter.clone(),
        ))));
    }
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    {
        match spec {
            GameCapture::Process(pid) => Err(AudioError::Unsupported(format!(
                "el loopback por proceso (--audio process {pid}) es de Windows; en Linux usa \
                 --audio app NOMBRE"
            ))),
            GameCapture::App(name) => Ok(Some(Box::new(pipewire::MonitorSource::app(name)))),
            _ => Ok(Some(Box::new(pipewire::MonitorSource::system()))),
        }
    }
    #[cfg(all(target_os = "windows", feature = "wasapi"))]
    {
        match spec {
            GameCapture::Process(pid) => Ok(Some(Box::new(wasapi::ProcessLoopbackSource::new(
                *pid, true,
            )))),
            GameCapture::App(name) => Err(AudioError::Unsupported(format!(
                "en Windows el audio de una aplicación se elige por proceso: --audio process PID \
                 (no «{name}»)"
            ))),
            _ => Ok(Some(Box::new(wasapi::LoopbackSource::new()))),
        }
    }
    #[cfg(not(any(
        all(target_os = "linux", feature = "pipewire"),
        all(target_os = "windows", feature = "wasapi")
    )))]
    {
        Err(AudioError::Unsupported(format!(
            "este binario no tiene captura de audio del sistema ({spec})"
        )))
    }
}

/// Backend del audio por aplicación de la plataforma.
fn app_backend() -> Result<std::sync::Arc<dyn apps::AppBackend>, AudioError> {
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    {
        Ok(std::sync::Arc::new(pipewire::AppStreams))
    }
    #[cfg(all(target_os = "windows", feature = "wasapi"))]
    {
        Ok(std::sync::Arc::new(wasapi::AppSessions))
    }
    #[cfg(not(any(
        all(target_os = "linux", feature = "pipewire"),
        all(target_os = "windows", feature = "wasapi")
    )))]
    {
        Err(AudioError::Unsupported(
            "este binario no tiene audio por aplicación".to_owned(),
        ))
    }
}

/// Aplicaciones que reproducen audio ahora, sin coroom, ordenadas por nombre.
pub fn playing_apps() -> Result<Vec<apps::PlayingApp>, AudioError> {
    app_backend()?.playing()
}

/// Micrófono de la plataforma (`None`: el del sistema).
pub fn mic_source(device: Option<&str>) -> Result<Box<dyn AudioSource>, AudioError> {
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    {
        Ok(Box::new(pipewire::MicSource::new(
            device.map(str::to_owned),
        )))
    }
    #[cfg(all(target_os = "windows", feature = "wasapi"))]
    {
        Ok(Box::new(wasapi::MicSource::new(device.map(str::to_owned))))
    }
    #[cfg(not(any(
        all(target_os = "linux", feature = "pipewire"),
        all(target_os = "windows", feature = "wasapi")
    )))]
    {
        let _ = device;
        Err(AudioError::Unsupported(
            "este binario no tiene captura de micrófono".to_owned(),
        ))
    }
}

/// Salida de la plataforma en `format` (`None`: la del sistema).
pub fn playback_sink(
    device: Option<&str>,
    format: AudioFormat,
) -> Result<Box<dyn AudioSink>, AudioError> {
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    {
        Ok(Box::new(pipewire::PlaybackSink::new(
            device.map(str::to_owned),
            format,
        )))
    }
    #[cfg(all(target_os = "windows", feature = "wasapi"))]
    {
        Ok(Box::new(wasapi::PlaybackSink::new(
            device.map(str::to_owned),
            format,
        )))
    }
    #[cfg(not(any(
        all(target_os = "linux", feature = "pipewire"),
        all(target_os = "windows", feature = "wasapi")
    )))]
    {
        let _ = (device, format);
        Err(AudioError::Unsupported(
            "este binario no tiene reproducción de audio".to_owned(),
        ))
    }
}

/// Dispositivos de audio de la plataforma.
pub fn list_devices() -> Result<Vec<DeviceInfo>, AudioError> {
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    {
        pipewire::list_devices()
    }
    #[cfg(all(target_os = "windows", feature = "wasapi"))]
    {
        wasapi::list_devices()
    }
    #[cfg(not(any(
        all(target_os = "linux", feature = "pipewire"),
        all(target_os = "windows", feature = "wasapi")
    )))]
    {
        Err(AudioError::Unsupported(
            "este binario no tiene backend de audio".to_owned(),
        ))
    }
}

/// Estado del audio para `coroom doctor`: backend, códec, procesado y loopback por proceso.
pub fn status() -> Vec<String> {
    let mut lines = Vec::new();
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    lines.push(
        "backend: PipeWire nativo (monitor de la salida o nodo de la aplicación, micrófono y \
         reproducción)"
            .to_owned(),
    );
    #[cfg(all(target_os = "windows", feature = "wasapi"))]
    {
        lines.push(
            "backend: WASAPI (loopback de la salida, micrófono y reproducción en modo compartido \
             por eventos)"
                .to_owned(),
        );
        lines.push(wasapi::process_loopback_status());
    }
    #[cfg(target_os = "linux")]
    lines.push(
        "audio de una sola aplicación: por su nodo de PipeWire (--audio app NOMBRE); el loopback \
         por proceso es de Windows"
            .to_owned(),
    );
    lines.push(format!(
        "códec: Opus (libopus {}), tramas de 10 ms; {}",
        codec::libopus_version(),
        if codec::dred_available() {
            "DRED disponible"
        } else {
            "sin DRED (FEC en banda)"
        }
    ));
    lines.push(format!("procesado de voz: {}", processing::backend_label()));
    lines
}

/// Convierte `i16` a coma flotante en `[-1, 1)`.
pub fn i16_to_f32(input: &[i16], output: &mut [f32]) {
    for (out, sample) in output.iter_mut().zip(input) {
        *out = f32::from(*sample) / 32_768.0;
    }
}

/// Convierte coma flotante a `i16` con saturación.
pub fn f32_to_i16(input: &[f32], output: &mut [i16]) {
    for (out, sample) in output.iter_mut().zip(input) {
        *out = (sample * 32_768.0).round().clamp(-32_768.0, 32_767.0) as i16;
    }
}

/// Nivel RMS de unas muestras en dBFS (−120 en silencio).
pub fn rms_dbfs(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return -120.0;
    }
    let energy: f64 = samples
        .iter()
        .map(|s| {
            let v = f64::from(*s) / 32_768.0;
            v * v
        })
        .sum::<f64>()
        / samples.len() as f64;
    if energy <= 1e-12 {
        -120.0
    } else {
        (10.0 * energy.log10()) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Silence;

    impl AudioSource for Silence {
        fn format(&self) -> AudioFormat {
            AudioFormat::GAME
        }

        fn start(&mut self, _config: &AudioConfig) -> Result<(), AudioError> {
            Ok(())
        }

        fn read(&mut self, out: &mut Vec<i16>) -> Result<Captured, AudioError> {
            let needed = self.format().samples_per_frame();
            out.clear();
            out.resize(needed, 0);
            Ok(Captured {
                samples: needed,
                captured_at: Instant::now(),
                channels: 2,
            })
        }

        fn stop(&mut self) {}
    }

    fn assert_send<T: Send + ?Sized>() {}

    #[test]
    fn audio_source_is_send_and_object_safe() {
        assert_send::<dyn AudioSource>();
        assert_send::<dyn AudioSink>();
        let mut source: Box<dyn AudioSource> = Box::new(Silence);
        source.start(&AudioConfig::game()).expect("arranque");
        let mut buffer = vec![1.0; 1024];
        assert_eq!(source.read_frame(&mut buffer), Ok(960));
        assert!(buffer[..960].iter().all(|sample| *sample == 0.0));
        let mut short = [0.0; 10];
        assert!(matches!(
            source.read_frame(&mut short),
            Err(AudioError::BufferTooSmall { needed: 960, .. })
        ));
    }

    #[test]
    fn ten_millisecond_frames_at_48_khz() {
        assert_eq!(AudioFormat::VOICE.samples_per_frame(), 480);
        assert_eq!(AudioFormat::GAME.samples_per_frame(), 960);
        assert_eq!(SAMPLES_PER_CHANNEL, 480);
        assert_eq!(AudioFormat::GAME.duration_of(960), FRAME_DURATION);
    }

    #[test]
    fn conversions_and_levels() {
        let mut f = [0.0f32; 3];
        i16_to_f32(&[0, 16_384, -32_768], &mut f);
        assert_eq!(f, [0.0, 0.5, -1.0]);
        let mut back = [0i16; 3];
        f32_to_i16(&[0.0, 0.5, 2.0], &mut back);
        assert_eq!(back, [0, 16_384, 32_767]);
        assert_eq!(rms_dbfs(&[0; 480]), -120.0);
        let full: Vec<i16> = (0..480)
            .map(|i| if i % 2 == 0 { 32_767 } else { -32_767 })
            .collect();
        assert!(rms_dbfs(&full).abs() < 0.01);
    }

    #[test]
    fn game_capture_reads_in_spanish() {
        assert_eq!(
            GameCapture::Process(42).to_string(),
            "proceso 42 y sus hijos"
        );
        assert!(game_source(&GameCapture::None).expect("nada").is_none());
        assert!(!status().is_empty());
    }
}
