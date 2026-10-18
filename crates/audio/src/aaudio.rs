//! Audio por **AAudio** en Android (feature `aaudio`), con los enlaces de `ndk` 0.9 (rust-mobile)
//! a la API 28 del NDK: Android 9 o posterior (uso, tipo de contenido y preset de entrada son de
//! esa versión). Es el invitado de Android: la salida del juego y de las voces y el micrófono.
//!
//! - [`PlaybackSink`]: flujo de salida en **baja latencia** (`AAUDIO_PERFORMANCE_MODE_LOW_LATENCY`),
//!   **exclusivo** (MMAP, sin el mezclador del sistema) y, si el sistema no lo concede, compartido;
//!   uso `GAME`, contenido `MOVIE` (la banda sonora de un juego con voces; `SPEECH` si es mono).
//!   Se pide `PCM_I16` a 48 kHz con los canales del formato y se mira lo que concedió el flujo: a
//!   otra frecuencia falla (no remuestreamos, como en WASAPI); en coma flotante o con otros canales
//!   convierte [`crate::convert::DeviceFormat`]. Va **a demanda** ([`AudioSink::start_pull`]): el
//!   callback de AAudio, que corre en el hilo del dispositivo (ya con prioridad de audio,
//!   `SCHED_FIFO`), pide cada trama a la sesión justo cuando la necesita ([`Pull`] reparte cada
//!   trama de 10 ms entre las ráfagas). Con [`AudioSink::write`], el callback toma del anillo.
//! - **Búfer**: empieza en **una ráfaga** (`AAudioStream_getFramesPerBurst`) y crece una ráfaga cada
//!   vez que sube el contador de faltas del propio flujo (`AAudioStream_getXRunCount`), hasta su
//!   capacidad ([`crate::tuning::BufferTuner`], dentro del callback, como el `LatencyTuner` de
//!   Oboe). Las faltas del flujo son las de [`AudioSink::device_counters`].
//! - **Cuándo suena cada trama**: con la marca del dispositivo (`AAudioStream_getTimestamp` con
//!   `CLOCK_MONOTONIC`, el reloj de [`Instant`] en Android): la trama `frame_position` sonó en
//!   `time_nanoseconds`, así que la siguiente que se escribe (`AAudioStream_getFramesWritten`)
//!   sonará las tramas de diferencia después. Incluye el búfer, el mezclador y el retardo del
//!   dispositivo hasta el altavoz. Hasta la primera marca (los primeros callbacks), el búfer.
//! - **Desconexión** (auriculares fuera, cambio a Bluetooth: `AAUDIO_ERROR_DISCONNECTED` en el
//!   callback de error): el flujo se cierra desde el hilo de la sesión ([`AudioSink::check`], o la
//!   lectura del micrófono), nunca desde el callback, y se **reabre en el dispositivo por defecto
//!   nuevo** con el mismo `Render`. Si no abre, o se vuelve a perder enseguida, el error sale por
//!   el camino de siempre (`AudioIssue` en la sesión).
//! - [`MicSource`]: flujo de entrada con el preset **`VOICE_COMMUNICATION`**, para que Android le
//!   aplique su cancelación de eco, supresión de ruido y ganancia (si el equipo las tiene: AAudio
//!   no dice si las aplicó; [`AudioSource::platform_processing`] lo declara y la aplicación lo
//!   puede corregir con [`MicSource::with_platform_processing`]). Baja latencia, **compartido**:
//!   el modo exclusivo (MMAP) se salta los efectos de preprocesado. Mono a 48 kHz en `PCM_I16`;
//!   cada [`AudioSource::read`] es una lectura bloqueante de 10 ms, fechada con la marca del
//!   dispositivo. Sin el permiso `RECORD_AUDIO` (lo pide la aplicación en Kotlin) AAudio no abre el
//!   flujo: el error lo dice.
//!
//! Los dispositivos se eligen por su id de Android (`AudioDeviceInfo.getId()`, en texto); AAudio no
//! enumera dispositivos, así que la lista la da la aplicación. Las cuentas que no dependen de la
//! plataforma están en [`crate::tuning`] (con pruebas en el anfitrión).

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, TryLockError};
use std::time::{Duration, Instant};

use ndk::audio as aa;

use crate::convert::{DeviceFormat, Sample};
use crate::ring::{Pull, SILENCE_AFTER, Shared};
use crate::tuning::{self, BufferTuner};
use crate::{
    AudioConfig, AudioError, AudioFormat, AudioSink, AudioSource, Captured, DeviceCounters,
    MAX_FRAME_SAMPLES, PlatformProcessing, Played, Render, SAMPLE_RATE_HZ, SAMPLES_PER_CHANNEL,
};

/// Espera máxima a que un flujo se detenga antes de cerrarlo.
const STOP_TIMEOUT: Duration = Duration::from_millis(200);

/// Si un flujo reabierto tras una desconexión se vuelve a perder antes de esto, no se reabre otra
/// vez: se avisa (la sesión espera a que se elija otro dispositivo) en vez de reabrir en bucle.
const REOPEN_BACKOFF: Duration = Duration::from_secs(1);

/// Lo que Android dice que hace con el preset `VOICE_COMMUNICATION` («aprovecha la cancelación de
/// eco y el control de ganancia si los hay», `MediaRecorder.AudioSource`), con la supresión de
/// ruido que lo acompaña en los equipos.
const VOICE_COMMUNICATION: PlatformProcessing = PlatformProcessing {
    echo_cancellation: true,
    noise_suppression: true,
    gain_control: true,
};

fn backend(what: impl std::fmt::Display) -> AudioError {
    AudioError::Backend(format!("AAudio: {what}"))
}

/// Por qué no abrió un flujo, en palabras.
fn open_error(err: aa::AudioError, output: bool) -> AudioError {
    match (output, err) {
        (_, aa::AudioError::NoService) => AudioError::Unsupported(
            "el servicio de audio de Android no responde (AAudio: NoService)".to_owned(),
        ),
        (false, _) => AudioError::Unsupported(format!(
            "no se pudo abrir el micrófono (AAudio: {err:?}); ¿tiene coroom el permiso de \
             micrófono? Se concede en los ajustes de la aplicación de Android"
        )),
        (true, _) => AudioError::Unsupported(format!(
            "no se pudo abrir la salida de audio (AAudio: {err:?})"
        )),
    }
}

/// Instante actual del reloj monótono, en ns (el de las marcas de AAudio y el de [`Instant`]).
fn monotonic_ns() -> i64 {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    Duration::try_from(now)
        .ok()
        .and_then(|since_boot| i64::try_from(since_boot.as_nanos()).ok())
        .unwrap_or(0)
}

/// Tramas de una llamada de AAudio que las devuelve (`read`, `setBufferSizeInFrames`): `ndk` 0.9
/// da por error cualquier resultado distinto de cero, también las tramas
/// ([`tuning::frames_or_error`]).
fn frame_count<T: TryInto<usize>>(result: aa::Result<T>) -> Result<usize, aa::AudioError> {
    match result {
        Ok(value) => Ok(value.try_into().unwrap_or(0)),
        Err(err) => tuning::frames_or_error(i32::from(err)).map_err(|_| err),
    }
}

/// El id de Android de `device` (`None`: el del sistema).
fn device_id(device: Option<&str>) -> Result<Option<i32>, AudioError> {
    match device {
        None => Ok(None),
        Some(name) => tuning::parse_device_id(name)
            .map(Some)
            .ok_or_else(|| AudioError::NoSuchDevice(name.to_owned())),
    }
}

/// Un flujo de AAudio abierto, que se puede mover de hilo. Al soltarlo se detiene (esperando a que
/// el callback termine) y se cierra.
struct Owned(aa::AudioStream);

// SAFETY: un `AAudioStream` no está atado al hilo que lo abrió: AAudio admite sus llamadas desde
// cualquier hilo mientras no sean a la vez, y este solo lo usa quien lo posee (el hilo de la sesión
// o el de la voz); el callback recibe su propio puntero prestado del mismo flujo, que AAudio
// documenta como seguro frente a `requestStop` y los getters del dueño. Las cajas de los callbacks
// que lleva dentro ya son `Send`. `ndk` solo lo deja `!Send` por el `NonNull`.
#[allow(unsafe_code)]
unsafe impl Send for Owned {}

impl Drop for Owned {
    fn drop(&mut self) {
        // `requestStop` es asíncrono: se espera a que salga de `STOPPING` (el callback ya no corre)
        // antes de que `AudioStream` lo cierre. Desconectado o ya parado, vuelve enseguida.
        let _ = self.0.request_stop();
        let _ = self.0.wait_for_state_change(
            aa::AudioStreamState::Stopping,
            i64::try_from(STOP_TIMEOUT.as_nanos()).unwrap_or(i64::MAX),
        );
    }
}

/// Lo que concedió el flujo al abrirlo.
#[derive(Debug, Clone, Copy)]
struct Granted {
    /// Formato de las muestras del flujo (las convierte [`DeviceFormat`]).
    device: DeviceFormat,
    /// Tramas por ráfaga (0 si no lo dice).
    burst: usize,
    /// Capacidad del búfer, en tramas.
    capacity: usize,
    /// Tamaño del búfer al arrancar, en tramas.
    buffer: usize,
    sharing: aa::AudioSharingMode,
    performance: aa::AudioPerformanceMode,
    device_id: i32,
}

/// Qué flujo abrir.
#[derive(Debug, Clone, Copy)]
struct Spec {
    output: bool,
    format: AudioFormat,
    device_id: Option<i32>,
}

type Callbacks = (aa::AudioStreamDataCallback, aa::AudioStreamErrorCallback);

fn builder(
    spec: &Spec,
    sharing: aa::AudioSharingMode,
    callbacks: Option<Callbacks>,
) -> aa::Result<aa::AudioStreamBuilder> {
    let mut builder = aa::AudioStreamBuilder::new()?
        .performance_mode(aa::AudioPerformanceMode::LowLatency)
        .sharing_mode(sharing)
        .format(aa::AudioFormat::PCM_I16)
        .sample_rate(SAMPLE_RATE_HZ as i32)
        .channel_count(i32::from(spec.format.channels));
    builder = if spec.output {
        let content = if spec.format.channels >= 2 {
            aa::AudioContentType::Movie
        } else {
            aa::AudioContentType::Speech
        };
        builder
            .direction(aa::AudioDirection::Output)
            .usage(aa::AudioUsage::Game)
            .content_type(content)
    } else {
        builder
            .direction(aa::AudioDirection::Input)
            .input_preset(aa::AudioInputPreset::VoiceCommunication)
    };
    if let Some(id) = spec.device_id {
        builder = builder.device_id(id);
    }
    if let Some((data, error)) = callbacks {
        builder = builder.data_callback(data).error_callback(error);
    }
    Ok(builder)
}

/// Lo que concedió `stream`, o por qué no sirve.
fn granted(stream: &aa::AudioStream) -> Result<Granted, AudioError> {
    let rate = stream.sample_rate();
    if u32::try_from(rate).ok() != Some(SAMPLE_RATE_HZ) {
        return Err(AudioError::Unsupported(format!(
            "AAudio abrió el flujo a {rate} Hz (se pidieron 48 kHz) y coroom no remuestrea"
        )));
    }
    let sample = match stream.format() {
        aa::AudioFormat::PCM_I16 => Sample::Int16,
        aa::AudioFormat::PCM_Float => Sample::Float32,
        other => {
            return Err(AudioError::Unsupported(format!(
                "AAudio abrió el flujo con muestras {other:?} (se pidió PCM_I16)"
            )));
        }
    };
    let channels = usize::try_from(stream.channel_count())
        .ok()
        .filter(|channels| *channels > 0)
        .ok_or_else(|| backend("el flujo no dice sus canales"))?;
    Ok(Granted {
        device: DeviceFormat { sample, channels },
        burst: usize::try_from(stream.frames_per_burst()).unwrap_or(0),
        capacity: usize::try_from(stream.buffer_capacity_in_frames()).unwrap_or(0),
        buffer: usize::try_from(stream.buffer_size_in_frames()).unwrap_or(0),
        sharing: stream.sharing_mode(),
        performance: stream.performance_mode(),
        device_id: stream.device_id(),
    })
}

/// Abre el flujo de `spec` con el primer modo de `sharing` que AAudio acepte (los callbacks se
/// crean de nuevo para cada intento: el constructor se los queda).
fn open_stream(
    spec: &Spec,
    sharing: &[aa::AudioSharingMode],
    mut callbacks: impl FnMut() -> Option<Callbacks>,
) -> Result<(Owned, Granted), AudioError> {
    let mut last = aa::AudioError::Unavailable;
    for &mode in sharing {
        let opened = builder(spec, mode, callbacks()).and_then(aa::AudioStreamBuilder::open_stream);
        match opened {
            Ok(stream) => {
                let stream = Owned(stream);
                let granted = granted(&stream.0)?;
                return Ok((stream, granted));
            }
            Err(err) => {
                tracing::debug!(sharing = ?mode, error = ?err, "AAudio no abrió el flujo");
                last = err;
            }
        }
    }
    Err(open_error(last, spec.output))
}

/// Estado de un flujo de reproducción que comparte con sus callbacks.
#[derive(Debug, Default)]
struct StreamStatus {
    /// El callback de error avisó (desconexión casi siempre): hay que cerrarlo y reabrir.
    lost: AtomicBool,
    /// Faltas del flujo (`getXRunCount`) en el último callback.
    xruns: AtomicU64,
    /// Tamaño del búfer, en tramas (lo cambia el callback al ajustarlo).
    buffer_frames: AtomicUsize,
}

/// El callback de datos de la reproducción.
struct Renderer {
    /// A demanda: la misma para todos los flujos que se abran (sobrevive a una reapertura).
    pull: Option<Arc<Mutex<Pull>>>,
    /// Con la cola ([`AudioSink::write`]): de dónde sale lo que suena; siempre, los contadores.
    shared: Arc<Shared>,
    status: Arc<StreamStatus>,
    /// Lo que concedió el flujo: se fija después de abrirlo y antes de arrancarlo.
    granted: Arc<OnceLock<Granted>>,
    tuner: Option<BufferTuner>,
    format: AudioFormat,
    /// Una trama nuestra (se reutiliza: el callback no asigna).
    scratch: [i16; MAX_FRAME_SAMPLES],
    /// Último retardo medido con la marca del dispositivo.
    measured: Option<Duration>,
}

/// La porción de bytes que AAudio le da al callback.
#[allow(unsafe_code)]
fn callback_bytes<'a>(data: *mut c_void, len: usize) -> Option<&'a mut [u8]> {
    if data.is_null() {
        return None;
    }
    // SAFETY: AAudio pasa al callback de datos un búfer de `num_frames` tramas en el formato del
    // flujo (canales × bytes por muestra: el `Granted` que se leyó del mismo flujo antes de
    // arrancarlo), válido y solo nuestro hasta que el callback vuelve. `len` es justo ese tamaño
    // y la porción no sale del callback.
    Some(unsafe { std::slice::from_raw_parts_mut(data.cast::<u8>(), len) })
}

impl Renderer {
    fn render(&mut self, stream: &aa::AudioStream, data: *mut c_void, frames: i32) {
        // Sin `Granted` no se sabe el tamaño del búfer; no pasa: se fija antes de arrancar.
        let Some(granted) = self.granted.get().copied() else {
            return;
        };
        let frames = usize::try_from(frames).unwrap_or(0);
        let Some(bytes) = callback_bytes(data, frames * granted.device.bytes_per_frame()) else {
            return;
        };
        // Faltas del flujo: una ráfaga más de búfer cada vez que suben.
        let xruns = u64::try_from(stream.x_run_count()).unwrap_or(0);
        self.status.xruns.store(xruns, Ordering::Relaxed);
        let tuner = self.tuner.get_or_insert_with(|| {
            let mut tuner = BufferTuner::new(granted.burst, granted.capacity, granted.buffer);
            tuner.granted(granted.buffer);
            tuner
        });
        if let Some(size) = tuner.observe(xruns) {
            let asked = i32::try_from(size).unwrap_or(i32::MAX);
            if let Ok(frames) = frame_count(stream.set_buffer_size_in_frames(asked)) {
                tuner.granted(frames);
                self.status.buffer_frames.store(frames, Ordering::Relaxed);
            }
        }
        // Cuándo suena la primera trama de este callback, por la marca del dispositivo; sin marca
        // aún, la última medida o, al principio, lo que hay en el búfer.
        let next = stream.frames_written();
        let measured = stream
            .timestamp(aa::Clockid::Monotonic)
            .ok()
            .and_then(|stamp| {
                tuning::time_until_presented(
                    stamp.frame_position,
                    stamp.time_nanoseconds,
                    next,
                    monotonic_ns(),
                    SAMPLE_RATE_HZ,
                )
            });
        if measured.is_some() {
            self.measured = measured;
        }
        let channels = usize::from(self.format.channels);
        let delay = self.measured.unwrap_or_else(|| {
            let queued = usize::try_from(stream.buffer_size_in_frames()).unwrap_or(0);
            self.format.duration_of(queued * channels)
        });
        let mut pull = match &self.pull {
            Some(pull) => match pull.try_lock() {
                Ok(guard) => Some(guard),
                Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
                // Solo se toma al cambiar de flujo, con este ya parado: no debería pasar.
                Err(TryLockError::WouldBlock) => {
                    bytes.fill(0);
                    return;
                }
            },
            None => {
                self.shared
                    .delay_us
                    .store(delay.as_micros() as u64, Ordering::Relaxed);
                None
            }
        };
        // Por trozos de una trama como mucho: cada trozo suena después de los anteriores.
        let frame_bytes = granted.device.bytes_per_frame();
        let mut written = 0;
        let most = SAMPLES_PER_CHANNEL.min(MAX_FRAME_SAMPLES / channels.max(1));
        while written < frames {
            let chunk = (frames - written).min(most);
            let scratch = &mut self.scratch[..chunk * channels];
            let ahead = self.format.duration_of(written * channels);
            match pull.as_deref_mut() {
                Some(pull) => pull.fill(scratch, channels, delay + ahead),
                None => self.shared.pop_into(scratch),
            }
            let start = written * frame_bytes;
            if let Some(out) = bytes.get_mut(start..start + chunk * frame_bytes) {
                granted.device.encode(scratch, channels, out);
            }
            written += chunk;
        }
    }
}

/// Un flujo de reproducción en marcha.
struct Playing {
    stream: Owned,
    shared: Arc<Shared>,
    status: Arc<StreamStatus>,
    /// Tamaño del búfer que ya se registró.
    logged_buffer: usize,
}

impl Playing {
    /// Si hay que cerrarlo y reabrir.
    fn lost(&self) -> bool {
        self.status.lost.load(Ordering::Acquire)
            || self.stream.0.state() == aa::AudioStreamState::Disconnected
    }

    /// El error con el que se perdió.
    fn reason(&self) -> AudioError {
        self.shared.failure().unwrap_or(AudioError::DeviceLost)
    }

    fn counters(&self) -> DeviceCounters {
        let ring = self.shared.counters();
        DeviceCounters {
            underruns: ring.underruns + self.status.xruns.load(Ordering::Relaxed),
            overruns: ring.overruns,
        }
    }

    /// Registra el tamaño del búfer si el callback lo cambió.
    fn log_buffer(&mut self) {
        let buffer = self.status.buffer_frames.load(Ordering::Relaxed);
        if buffer != self.logged_buffer {
            self.logged_buffer = buffer;
            tracing::debug!(
                buffer_frames = buffer,
                xruns = self.status.xruns.load(Ordering::Relaxed),
                "búfer de la salida AAudio ajustado tras faltas"
            );
        }
    }
}

fn add(a: DeviceCounters, b: DeviceCounters) -> DeviceCounters {
    DeviceCounters {
        underruns: a.underruns + b.underruns,
        overruns: a.overruns + b.overruns,
    }
}

/// Reproducción por AAudio.
pub struct PlaybackSink {
    device: Option<String>,
    format: AudioFormat,
    /// A demanda: lo que pide cada trama a la sesión.
    pull: Option<Arc<Mutex<Pull>>>,
    playing: Option<Playing>,
    /// Faltas y sobras de los flujos anteriores (reabiertos) desde el último `start`.
    past: DeviceCounters,
    last_reopen: Option<Instant>,
}

impl PlaybackSink {
    /// Salida `device` (id de Android en texto; `None`: la del sistema) en `format`.
    pub fn new(device: Option<String>, format: AudioFormat) -> Self {
        Self {
            device,
            format,
            pull: None,
            playing: None,
            past: DeviceCounters::default(),
            last_reopen: None,
        }
    }

    fn open(&mut self, device: Option<i32>) -> Result<(), AudioError> {
        let spec = Spec {
            output: true,
            format: self.format,
            device_id: device,
        };
        let shared = Shared::new(self.format);
        let status = Arc::new(StreamStatus::default());
        let cell = Arc::new(OnceLock::new());
        let callbacks = || {
            let mut renderer = Renderer {
                pull: self.pull.clone(),
                shared: Arc::clone(&shared),
                status: Arc::clone(&status),
                granted: Arc::clone(&cell),
                tuner: None,
                format: self.format,
                scratch: [0; MAX_FRAME_SAMPLES],
                measured: None,
            };
            let data: aa::AudioStreamDataCallback = Box::new(move |stream, data, frames| {
                renderer.render(stream, data, frames);
                aa::AudioCallbackResult::Continue
            });
            let (status, shared) = (Arc::clone(&status), Arc::clone(&shared));
            // En el hilo de AAudio que avisa: solo se marca; cerrar y reabrir lo hace la sesión.
            let error: aa::AudioStreamErrorCallback = Box::new(move |_, err| {
                status.lost.store(true, Ordering::Release);
                shared.fail(if err == aa::AudioError::Disconnected {
                    AudioError::DeviceLost
                } else {
                    backend(format!("el flujo de salida se detuvo ({err:?})"))
                });
            });
            Some((data, error))
        };
        let (stream, mut granted) = open_stream(
            &spec,
            &[
                aa::AudioSharingMode::Exclusive,
                aa::AudioSharingMode::Shared,
            ],
            callbacks,
        )?;
        // Una ráfaga de búfer para empezar; el callback la sube si hay faltas.
        let tuner = BufferTuner::new(granted.burst, granted.capacity, granted.buffer);
        let asked = i32::try_from(tuner.size()).unwrap_or(i32::MAX);
        match frame_count(stream.0.set_buffer_size_in_frames(asked)) {
            Ok(frames) => granted.buffer = frames,
            Err(err) => tracing::debug!(error = ?err, "AAudio no cambió el tamaño del búfer"),
        }
        status
            .buffer_frames
            .store(granted.buffer, Ordering::Relaxed);
        let _ = cell.set(granted);
        stream
            .0
            .request_start()
            .map_err(|err| open_error(err, true))?;
        tracing::info!(
            sharing = ?granted.sharing,
            performance = ?granted.performance,
            device_id = granted.device_id,
            burst_frames = granted.burst,
            buffer_frames = granted.buffer,
            capacity_frames = granted.capacity,
            device = ?granted.device,
            pull = self.pull.is_some(),
            "salida AAudio"
        );
        self.playing = Some(Playing {
            stream,
            shared,
            status,
            logged_buffer: granted.buffer,
        });
        Ok(())
    }

    fn start_with(&mut self, config: &AudioConfig, pull: Option<Render>) -> Result<(), AudioError> {
        self.stop();
        self.format = config.format;
        self.past = DeviceCounters::default();
        self.last_reopen = None;
        let device = config.device.clone().or_else(|| self.device.clone());
        let id = device_id(device.as_deref())?;
        self.pull = pull.map(|render| Arc::new(Mutex::new(Pull::new(render))));
        self.open(id)
    }

    /// Cierra el flujo perdido y abre otro en el dispositivo por defecto (el que el sistema elige
    /// tras la desconexión). Si no abre, o el anterior se reabrió hace nada, el error.
    fn recover(&mut self) -> Result<(), AudioError> {
        let Some(old) = self.playing.take() else {
            return Err(AudioError::Stopped);
        };
        let reason = old.reason();
        self.past = add(self.past, old.counters());
        drop(old);
        if self
            .last_reopen
            .is_some_and(|at| at.elapsed() < REOPEN_BACKOFF)
        {
            return Err(reason);
        }
        self.last_reopen = Some(Instant::now());
        match self.open(None) {
            Ok(()) => {
                tracing::info!(motivo = %reason, "salida AAudio reabierta en el dispositivo por defecto");
                Ok(())
            }
            Err(err) => {
                tracing::warn!(motivo = %reason, error = %err, "la salida AAudio no se pudo reabrir");
                Err(err)
            }
        }
    }

    /// `op` sobre el flujo en marcha, reabriéndolo una vez si se perdió.
    fn with_stream<T>(
        &mut self,
        op: impl Fn(&Playing, AudioFormat) -> Result<T, AudioError>,
    ) -> Result<T, AudioError> {
        for _ in 0..2 {
            let playing = self.playing.as_ref().ok_or(AudioError::Stopped)?;
            if !playing.lost() {
                match op(playing, self.format) {
                    Ok(value) => return Ok(value),
                    Err(err) if !playing.lost() => return Err(err),
                    Err(_) => {}
                }
            }
            self.recover()?;
        }
        Err(AudioError::DeviceLost)
    }
}

impl AudioSink for PlaybackSink {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn start(&mut self, config: &AudioConfig) -> Result<(), AudioError> {
        self.start_with(config, None)
    }

    fn supports_pull(&self) -> bool {
        true
    }

    fn start_pull(&mut self, config: &AudioConfig, render: Render) -> Result<(), AudioError> {
        self.start_with(config, Some(render))
    }

    fn check(&mut self) -> Result<(), AudioError> {
        let playing = self.playing.as_mut().ok_or(AudioError::Stopped)?;
        playing.log_buffer();
        if playing.lost() {
            return self.recover();
        }
        match playing.shared.failure() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    fn device_counters(&self) -> DeviceCounters {
        let current = self
            .playing
            .as_ref()
            .map(Playing::counters)
            .unwrap_or_default();
        add(self.past, current)
    }

    fn wait_ready(&mut self) -> Result<(), AudioError> {
        self.with_stream(|playing, format| playing.shared.wait_ready(format))
    }

    fn write(&mut self, samples: &[i16]) -> Result<Played, AudioError> {
        // El retardo lo pone el callback con la marca del dispositivo; `latency` no se usa.
        self.with_stream(|playing, format| {
            playing
                .shared
                .write_frame(format, samples, crate::DEVICE_LATENCY)
        })
    }

    fn stop(&mut self) {
        self.playing = None;
        self.pull = None;
    }

    fn describe(&self) -> String {
        match &self.device {
            Some(device) => format!("salida {device} (AAudio)"),
            None => "salida por defecto (AAudio)".to_owned(),
        }
    }
}

/// Un flujo de captura en marcha.
struct Recording {
    stream: Owned,
    granted: Granted,
    /// Si el flujo se abrió con el preset `VOICE_COMMUNICATION`.
    voice_preset: bool,
    /// Lo leído del flujo, en su formato (una trama de 10 ms).
    bytes: Vec<u8>,
    underruns: u64,
    overruns: u64,
}

impl Recording {
    /// Lee hasta `frames` tramas en `bytes` esperando como mucho `timeout`.
    #[allow(unsafe_code)]
    fn read_raw(&mut self, frames: usize, timeout: Duration) -> Result<usize, aa::AudioError> {
        let frame_bytes = self.granted.device.bytes_per_frame();
        let frames = frames.min(self.bytes.len() / frame_bytes);
        let count = i32::try_from(frames).unwrap_or(i32::MAX);
        let timeout = i64::try_from(timeout.as_nanos()).unwrap_or(i64::MAX);
        // SAFETY: `self.bytes` tiene sitio para `frames` tramas en el formato del flujo (se
        // dimensionó con el `Granted` del mismo flujo y `frames` se recorta a lo que cabe); la
        // lectura es bloqueante y AAudio solo escribe en él durante la llamada.
        let result = unsafe {
            self.stream
                .0
                .read(self.bytes.as_mut_ptr().cast::<c_void>(), count, timeout)
        };
        frame_count(result)
    }

    /// Una trama de 10 ms de `format`, fechada con la marca del dispositivo.
    fn read(
        &mut self,
        format: AudioFormat,
        out: &mut Vec<i16>,
    ) -> Result<Captured, aa::AudioError> {
        let channels = usize::from(format.channels.max(1));
        let wanted = format.samples_per_frame() / channels;
        // Si el hilo se retrasó, lo más antiguo se descarta: la latencia no crece.
        let stream = &self.stream.0;
        let mut skip =
            tuning::capture_backlog(stream.frames_written() - stream.frames_read(), wanted);
        if skip > 0 {
            self.overruns += 1;
        }
        while skip > 0 {
            let read = self.read_raw(skip.min(wanted), Duration::ZERO)?;
            if read == 0 {
                break;
            }
            skip -= read;
        }
        let read = self.read_raw(wanted, SILENCE_AFTER)?;
        let arrived = Instant::now();
        let now_ns = monotonic_ns();
        let stream = &self.stream.0;
        let first = stream.frames_read() - read as i64;
        let stamp = if read > 0 {
            stream.timestamp(aa::Clockid::Monotonic).ok()
        } else {
            None
        };
        let age = stamp
            .and_then(|stamp| {
                tuning::time_since_captured(
                    stamp.frame_position,
                    stamp.time_nanoseconds,
                    first,
                    now_ns,
                    SAMPLE_RATE_HZ,
                )
            })
            // Sin marca aún: lo leído acaba de llegar.
            .unwrap_or_else(|| format.duration_of(read * channels));
        let captured_at = arrived.checked_sub(age).unwrap_or(arrived);
        let frame_bytes = self.granted.device.bytes_per_frame();
        self.granted
            .device
            .decode(&self.bytes[..read * frame_bytes], channels, out);
        if read < wanted {
            // No llegó entera a tiempo: el resto, silencio.
            self.underruns += 1;
            out.resize(wanted * channels, 0);
        }
        Ok(Captured {
            samples: wanted * channels,
            captured_at,
            channels: format.channels,
        })
    }
}

/// Micrófono por AAudio con el procesado de voz del sistema.
pub struct MicSource {
    device: Option<String>,
    format: AudioFormat,
    recording: Option<Recording>,
    /// Lo que la aplicación sabe del procesado del sistema (si no, lo que dice el preset).
    platform: Option<PlatformProcessing>,
    last_reopen: Option<Instant>,
}

impl MicSource {
    /// Micrófono `device` (id de Android en texto; `None`: el del sistema).
    pub fn new(device: Option<String>) -> Self {
        Self {
            device,
            format: AudioFormat::VOICE,
            recording: None,
            platform: None,
            last_reopen: None,
        }
    }

    /// Fija lo que el sistema aplica de verdad al micrófono, si la aplicación lo sabe (por ejemplo
    /// con `AcousticEchoCanceler.isAvailable()`, `NoiseSuppressor.isAvailable()` y
    /// `AutomaticGainControl.isAvailable()`): manda sobre lo que declara el preset.
    pub fn with_platform_processing(mut self, processing: PlatformProcessing) -> Self {
        self.platform = Some(processing);
        self
    }

    fn open(&mut self, device: Option<i32>) -> Result<(), AudioError> {
        let spec = Spec {
            output: false,
            format: self.format,
            device_id: device,
        };
        // Compartido: el modo exclusivo (MMAP) va sin los efectos del preset.
        let (stream, granted) = open_stream(&spec, &[aa::AudioSharingMode::Shared], || None)?;
        let preset = stream.0.input_preset();
        stream
            .0
            .request_start()
            .map_err(|err| open_error(err, false))?;
        tracing::info!(
            sharing = ?granted.sharing,
            performance = ?granted.performance,
            preset = ?preset,
            device_id = granted.device_id,
            burst_frames = granted.burst,
            capacity_frames = granted.capacity,
            device = ?granted.device,
            "micrófono AAudio"
        );
        self.recording = Some(Recording {
            stream,
            granted,
            voice_preset: preset == aa::AudioInputPreset::VoiceCommunication,
            bytes: vec![0; SAMPLES_PER_CHANNEL * granted.device.bytes_per_frame()],
            underruns: 0,
            overruns: 0,
        });
        Ok(())
    }

    /// Como en la salida: cierra el flujo perdido y abre el micrófono por defecto.
    fn recover(&mut self, reason: aa::AudioError) -> Result<(), AudioError> {
        self.recording = None;
        let lost = AudioError::DeviceLost;
        if self
            .last_reopen
            .is_some_and(|at| at.elapsed() < REOPEN_BACKOFF)
        {
            return Err(lost);
        }
        self.last_reopen = Some(Instant::now());
        match self.open(None) {
            Ok(()) => {
                tracing::info!(motivo = ?reason, "micrófono AAudio reabierto en el dispositivo por defecto");
                Ok(())
            }
            Err(err) => {
                tracing::warn!(motivo = ?reason, error = %err, "el micrófono AAudio no se pudo reabrir");
                Err(err)
            }
        }
    }
}

impl AudioSource for MicSource {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn start(&mut self, config: &AudioConfig) -> Result<(), AudioError> {
        self.stop();
        self.format = config.format;
        self.last_reopen = None;
        let device = config.device.clone().or_else(|| self.device.clone());
        let id = device_id(device.as_deref())?;
        self.open(id)
    }

    fn read(&mut self, out: &mut Vec<i16>) -> Result<Captured, AudioError> {
        for _ in 0..2 {
            let recording = self.recording.as_mut().ok_or(AudioError::Stopped)?;
            match recording.read(self.format, out) {
                Ok(captured) => return Ok(captured),
                Err(aa::AudioError::Disconnected) => self.recover(aa::AudioError::Disconnected)?,
                Err(err) => return Err(backend(format!("lectura del micrófono ({err:?})"))),
            }
        }
        Err(AudioError::DeviceLost)
    }

    fn stop(&mut self) {
        if let Some(recording) = self.recording.take()
            && (recording.underruns > 0 || recording.overruns > 0)
        {
            tracing::debug!(
                faltas = recording.underruns,
                sobras = recording.overruns,
                "micrófono AAudio cerrado"
            );
        }
    }

    fn describe(&self) -> String {
        match &self.device {
            Some(device) => format!("micrófono {device} (AAudio)"),
            None => "micrófono por defecto (AAudio)".to_owned(),
        }
    }

    fn platform_processing(&self) -> PlatformProcessing {
        if let Some(processing) = self.platform {
            return processing;
        }
        match &self.recording {
            Some(recording) if !recording.voice_preset => PlatformProcessing::NONE,
            // Antes de abrir, lo que se va a pedir.
            _ => VOICE_COMMUNICATION,
        }
    }
}
