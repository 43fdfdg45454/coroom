//! Audio por **PipeWire** nativo en Linux (feature `pipewire`; sin ALSA, PulseAudio ni `cpal`).
//!
//! Cada fuente o sumidero abre un `Stream` de PipeWire en su propio hilo (`pipewire-audio`), con el
//! mismo patrón que la captura de vídeo (`coroom-capture`): `MainLoop` + `Context` + `Core` por
//! hilo, un canal de `pipewire` para pararlo y el callback `process` copiando entre el búfer de
//! PipeWire y un anillo compartido con el hilo de audio de la sesión (`audio-cap`, `voice-cap`,
//! `audio-play`). El anillo se reserva al abrir: el callback no asigna.
//!
//! - **Formato**: `SPA_AUDIO_FORMAT_S16LE` a 48 kHz con los canales pedidos (FL, FR o MONO); el
//!   adaptador de PipeWire convierte si el dispositivo usa otro. Latencia pedida
//!   `node.latency` de [`PLAYBACK_LATENCY`] y [`CAPTURE_LATENCY`] (128 tramas, 2,7 ms): el
//!   pipeline sigue en tramas de 10 ms, pero el cuanto del grafo es menor y lo que se escribe o
//!   se lee espera menos en el dispositivo (medidas en los comentarios de las constantes).
//! - **Lo que pide el grafo**: en cada ciclo de reproducción se escriben justo las tramas que pide
//!   (`pw_buffer.requested`, feature `v0_3_49` de `pipewire-rs`): lo que le falta al cuanto,
//!   descontado lo que aún tiene en cola del ciclo anterior. Escribir siempre una trama de 480
//!   dejaba cola en el flujo: con `480/48000` el cuanto es 256 (PipeWire redondea hacia abajo a
//!   una potencia de dos), el grafo pedía 32-256 tramas, se saltaba el callback 2-4 ciclos (con
//!   cuanto 128, huecos de 18-21 ms entre callbacks), lo escrito sonaba hasta 4,7 ms después de lo
//!   que decía `played_at` y bajar `node.latency` no quitaba nada. Con lo pedido exacto, la cola
//!   es cero y lo escrito suena a `pw_time.delay` del ciclo. Requiere PipeWire ≥ 0.3.49.
//! - **Tiempo real**: los flujos se conectan con `PW_STREAM_FLAG_RT_PROCESS`, así `process` corre
//!   en el hilo de datos de PipeWire, que ya tiene prioridad de tiempo real (`module-rt`, con
//!   RealtimeKit o el portal), en vez de en el bucle principal de este hilo, que competía con el
//!   resto del sistema y añadía jitter. El callback no asigna ni espera: copia entre el búfer y el
//!   anillo (o pide la trama a la reproducción a demanda) y lee los tiempos del grafo.
//! - **Marcas de captura**: cada búfer capturado se fecha con los tiempos del grafo
//!   (`pw_time.now` del ciclo y `pw_time.delay` hasta el dispositivo, comparados con
//!   `pw_stream_get_nsec`), no con la llegada al anillo.
//! - [`MonitorSource::system`]: captura (`media.category = Capture`) con
//!   `stream.capture.sink = true`: el monitor del *sink* por defecto, lo que suena.
//! - [`MonitorSource::app`]: el nodo de reproducción (`Stream/Output/Audio`) de una aplicación,
//!   buscado en el registro por `application.name`, `application.process.binary` o `node.name`
//!   (sin distinguir mayúsculas; vale un trozo) y enlazado con `target.object` = su `object.serial`.
//! - [`MicSource`]: captura del micrófono por defecto o del nodo `--mic NOMBRE` (`target.object`).
//! - [`PlaybackSink`]: reproducción (`media.category = Playback`, `media.role = Game`) en la salida
//!   por defecto o en `--speaker NOMBRE`. Por defecto **a demanda** ([`AudioSink::start_pull`]):
//!   el callback `process` pide la trama a la sesión justo cuando el grafo la necesita, sin anillo
//!   de por medio, y sonará «ahora + el retardo del grafo» (`pw_stream_get_time_n`, `delay`). Con
//!   [`AudioSink::write`], la trama pasa por el anillo y sonará además después de lo que haya en él.
//!
//! Sin un servidor de PipeWire en marcha, `start` falla con [`AudioError::Unsupported`] y el motivo.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use pipewire as pw;
use pw::spa;
use pw::spa::param::audio::{AudioFormat as SpaAudioFormat, AudioInfoRaw};
use pw::spa::pod::Pod;

use crate::ring::{Pull, Shared};
use crate::{
    AudioConfig, AudioError, AudioFormat, AudioSink, AudioSource, Captured, DeviceCounters,
    DeviceInfo, DeviceKind, Played, Render, SAMPLE_RATE_HZ,
};

/// Espera máxima a que el hilo de PipeWire conecte el flujo.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// `node.latency` que se pide para reproducir: 128 tramas a 48 kHz (2,7 ms). PipeWire redondea el
/// cuanto hacia **abajo** a una potencia de dos (480 → 256, 240 → 128, 128 → 128). Medido en un
/// PC con PipeWire 1.6.9 y auriculares USB (cuanto del grafo 1024, mínimo 32, nodo ALSA con
/// `api.alsa.headroom = 512` y periodo 512): `pw_time.delay` hasta el dispositivo 16,0 ms con 480
/// y 13,33 ms con 240 o 128; los 10,67 ms que quedan son el headroom del nodo ALSA, fuera de
/// coroom. El callback corre cada cuanto (2,7 ms) y pide una trama de 10 ms a la sesión cada
/// cuatro ciclos ([`Pull`] reparte la trama entre cuantos).
const PLAYBACK_LATENCY: &str = "128/48000";

/// `node.latency` que se pide para capturar: 128 tramas (2,7 ms). La trama de 10 ms está completa
/// igual a los 10 ms de su primera muestra, pero el trozo que la completa llega antes: en el mismo
/// equipo, leer una trama (p50) bajó de 12,7 a 11,3 ms en el monitor de la salida y de 28,7 a
/// 24,7 ms en el micrófono. Con cuantos menores que la trama llegan varios trozos por trama: el
/// anillo los junta ([`Shared::read_frame`] espera a tener 480 muestras por canal).
const CAPTURE_LATENCY: &str = "128/48000";

fn backend(what: impl std::fmt::Display) -> AudioError {
    AudioError::Backend(format!("PipeWire: {what}"))
}

fn no_daemon(err: impl std::fmt::Display) -> AudioError {
    AudioError::Unsupported(format!(
        "no se pudo conectar con PipeWire ({err}); ¿está en marcha el servidor de PipeWire?"
    ))
}

fn valid_name(name: &str) -> Result<&str, AudioError> {
    if name.is_empty() || name.contains('\0') {
        Err(AudioError::NoSuchDevice(name.to_owned()))
    } else {
        Ok(name)
    }
}

/// Qué flujo abrir.
#[derive(Debug, Clone)]
enum Kind {
    Capture {
        capture_sink: bool,
        role: &'static str,
    },
    Playback,
}

/// Flujo en marcha en su hilo.
struct Running {
    quit: pw::channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Running {
    fn stop(&mut self) {
        let _ = self.quit.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Lo que el callback necesita. `process` corre en el hilo de datos de PipeWire
/// (`RT_PROCESS`) y los demás eventos en el bucle principal: `param_changed` solo toca `info`, que
/// `process` no lee, y el anillo compartido (como en los ejemplos de `pipewire-rs`).
struct StreamData {
    shared: Arc<Shared>,
    kind: Kind,
    format: AudioFormat,
    channels: usize,
    info: AudioInfoRaw,
    /// Una trama de reproducción (se reutiliza).
    scratch: [i16; crate::MAX_FRAME_SAMPLES],
    /// Reproducción a demanda.
    pull: Option<Pull>,
}

fn format_param(format: AudioFormat) -> Result<Vec<u8>, AudioError> {
    let mut info = AudioInfoRaw::new();
    info.set_format(SpaAudioFormat::S16LE);
    info.set_rate(SAMPLE_RATE_HZ);
    info.set_channels(u32::from(format.channels));
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    if format.channels >= 2 {
        position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
        position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    } else {
        position[0] = spa::sys::SPA_AUDIO_CHANNEL_MONO;
    }
    info.set_position(position);
    let object = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map(|(cursor, _)| cursor.into_inner())
    .map_err(|err| backend(format!("no se pudo serializar el formato: {err:?}")))
}

/// Tiempos del ciclo actual del grafo: cuánto hace que se calcularon (`pw_time.now` frente a
/// `pw_stream_get_nsec`, los dos del reloj monótono) y el retardo hasta el dispositivo
/// (`pw_time.delay`; negativo cuenta como cero). `None` si el grafo aún no los dio.
///
/// `pw_stream_get_nsec` existe desde PipeWire 1.1.0 (`\since 1.1.0` en `stream.h`): con la
/// biblioteca 0.3.65 de Debian 12 la crate no enlaza; con 1.4.2 sí.
#[allow(unsafe_code)] // `pw_stream_get_nsec` no tiene envoltorio en `pipewire-rs`.
fn cycle_timing(stream: &pw::stream::Stream) -> Option<(Duration, Duration)> {
    let time = stream.time().ok()?;
    let rate = time.rate();
    let computed = u64::try_from(time.now()).ok().filter(|ns| *ns > 0)?;
    if rate.denom == 0 {
        return None;
    }
    // SAFETY: `stream` es el flujo de este callback, vivo durante la llamada; la función solo lee
    // el reloj monótono del sistema (la propia documentación de `pw_time` la indica para comparar
    // con `pw_time.now`) y es segura en el hilo de tiempo real.
    let nsec = unsafe { pw::sys::pw_stream_get_nsec(stream.as_raw_ptr()) };
    let age = Duration::from_nanos(nsec.saturating_sub(computed));
    let ticks = u64::try_from(time.delay()).unwrap_or(0);
    let delay = Duration::from_nanos(
        ticks.saturating_mul(1_000_000_000) * u64::from(rate.num) / u64::from(rate.denom),
    );
    Some((age, delay))
}

fn process(stream: &pw::stream::Stream, data: &mut StreamData) {
    let timing = cycle_timing(stream);
    match data.kind {
        Kind::Capture { .. } => {
            // Todos los búferes listos, no solo uno: el monitor de la salida (`stream.capture.sink`)
            // se salta ciclos del grafo (medido: dos cuantos entre callbacks 4, 14 y 37 veces en
            // 10 s con `node.latency` 480, 240 y 128; el micrófono nunca) y entonces hay dos
            // búferes esperando; con uno por callback el otro se quedaba en cola (latencia) o se
            // perdía.
            while let Some(mut buffer) = stream.dequeue_buffer() {
                let datas = buffer.datas_mut();
                let Some(first) = datas.first_mut() else {
                    return;
                };
                let offset = first.chunk().offset() as usize;
                let size = first.chunk().size() as usize;
                let Some(bytes) = first.data() else {
                    return;
                };
                let Some(bytes) = bytes.get(offset..offset + size) else {
                    return;
                };
                let samples = bytes.as_chunks::<2>().0;
                // Lo último del búfer se capturó `delay` antes del instante del ciclo; lo primero,
                // además lo que dura el búfer. Si hay dos búferes, el anillo se queda con la marca
                // del último (la de la muestra más reciente), que es la que vale. En el monitor de
                // la salida `pw_time.delay` es siempre 0 (el micrófono sí lo da: 16 o 13,3 ms): el
                // audio del juego no tiene retardo de dispositivo que descontar.
                let captured_at = timing.and_then(|(age, delay)| {
                    std::time::Instant::now()
                        .checked_sub(age + delay + data.format.duration_of(samples.len()))
                });
                data.shared.push(
                    data.format,
                    samples.iter().map(|&pair| i16::from_le_bytes(pair)),
                    captured_at,
                );
            }
        }
        Kind::Playback => {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            // Lo que el grafo pide en este ciclo: lo que le falta al cuanto, descontado lo que aún
            // tiene en cola del ciclo anterior (0 si no lo dice: entonces una trama).
            let requested = usize::try_from(buffer.requested()).unwrap_or(0);
            let datas = buffer.datas_mut();
            let Some(first) = datas.first_mut() else {
                return;
            };
            let stride = 2 * data.channels;
            let Some(bytes) = first.data() else {
                return;
            };
            let capacity = bytes.len() / stride;
            let frames = if requested == 0 {
                crate::SAMPLES_PER_CHANNEL
            } else {
                requested
            }
            .min(capacity);
            // Lo que se escribe ahora sonará tras el retardo del grafo hasta el dispositivo,
            // descontado lo que ya pasó desde que se midió (como se escribe justo lo pedido, no hay
            // cola en el flujo que sumar). Sin medida aún (los primeros ciclos), lo que se escribe.
            let delay = match timing {
                Some((age, delay)) if !delay.is_zero() => delay.saturating_sub(age),
                _ => data.format.duration_of(frames * data.channels),
            };
            data.shared
                .delay_us
                .store(delay.as_micros() as u64, Ordering::Relaxed);
            // Por trozos de una trama como mucho (el cuanto puede ser mayor si el grafo no atiende
            // la latencia pedida): cada trozo suena después de los anteriores del mismo ciclo.
            let mut written = 0;
            while written < frames {
                let chunk = (frames - written).min(crate::SAMPLES_PER_CHANNEL);
                let wanted = chunk * data.channels;
                let ahead = data.format.duration_of(written * data.channels);
                let scratch = &mut data.scratch[..wanted];
                match &mut data.pull {
                    Some(pull) => pull.fill(scratch, data.channels, delay + ahead),
                    None => data.shared.pop_into(scratch),
                }
                let start = written * stride;
                for (out, sample) in bytes[start..start + wanted * 2]
                    .as_chunks_mut::<2>()
                    .0
                    .iter_mut()
                    .zip(scratch.iter())
                {
                    *out = sample.to_le_bytes();
                }
                written += chunk;
            }
            let chunk = first.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as i32;
            *chunk.size_mut() = (frames * stride) as u32;
        }
    }
}

/// Cuerpo del hilo `pipewire-audio`.
fn run_stream(
    kind: Kind,
    format: AudioFormat,
    target: Option<String>,
    shared: Arc<Shared>,
    pull: Option<Render>,
    quit: pw::channel::Receiver<()>,
    ready: std::sync::mpsc::Sender<Result<(), AudioError>>,
) {
    let setup = || -> Result<(), AudioError> {
        pw::init();
        let main_loop = pw::main_loop::MainLoopRc::new(None).map_err(no_daemon)?;
        let context = pw::context::ContextRc::new(&main_loop, None).map_err(no_daemon)?;
        let core = context.connect_rc(None).map_err(no_daemon)?;
        let (category, role, name, latency) = match &kind {
            Kind::Capture { role, .. } => ("Capture", *role, "coroom-captura", CAPTURE_LATENCY),
            Kind::Playback => ("Playback", "Game", "coroom-reproduccion", PLAYBACK_LATENCY),
        };
        let mut props = pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => category,
            *pw::keys::MEDIA_ROLE => role,
            *pw::keys::NODE_LATENCY => latency,
            *pw::keys::APP_NAME => "coroom",
            *pw::keys::NODE_NAME => name,
        };
        if let Kind::Capture {
            capture_sink: true, ..
        } = kind
        {
            props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
        }
        if let Some(target) = &target {
            props.insert("target.object", valid_name(target)?);
        }
        let stream = pw::stream::StreamBox::new(&core, name, props).map_err(backend)?;
        let data = StreamData {
            shared: Arc::clone(&shared),
            kind: kind.clone(),
            format,
            channels: usize::from(format.channels),
            info: AudioInfoRaw::default(),
            scratch: [0; crate::MAX_FRAME_SAMPLES],
            pull: pull.map(Pull::new),
        };
        let errors = Arc::clone(&shared);
        let _listener = stream
            .add_local_listener_with_user_data(data)
            .state_changed(move |_, _, _, new| match new {
                pw::stream::StreamState::Error(message) => {
                    errors.fail(backend(message));
                }
                pw::stream::StreamState::Unconnected => errors.fail(AudioError::DeviceLost),
                _ => {}
            })
            .param_changed(|_, data, id, param| {
                let Some(param) = param else {
                    return;
                };
                if id != spa::param::ParamType::Format.as_raw() {
                    return;
                }
                if data.info.parse(param).is_err() {
                    data.shared.fail(backend("formato de audio ilegible"));
                    return;
                }
                let channels = data.info.channels() as usize;
                if data.info.format() != SpaAudioFormat::S16LE
                    || data.info.rate() != SAMPLE_RATE_HZ
                    || channels != data.channels
                {
                    data.shared.fail(AudioError::Unsupported(format!(
                        "PipeWire negoció {:?} a {} Hz con {channels} canales (se pidió S16LE a \
                         48 kHz con {})",
                        data.info.format(),
                        data.info.rate(),
                        data.channels
                    )));
                }
            })
            .process(process)
            .register()
            .map_err(backend)?;
        let param = format_param(format)?;
        let pod = Pod::from_bytes(&param).ok_or_else(|| backend("formato no válido"))?;
        let direction = match kind {
            Kind::Capture { .. } => spa::utils::Direction::Input,
            Kind::Playback => spa::utils::Direction::Output,
        };
        stream
            .connect(
                direction,
                None,
                // `process` en el hilo de datos de PipeWire, con prioridad de tiempo real.
                pw::stream::StreamFlags::AUTOCONNECT
                    | pw::stream::StreamFlags::MAP_BUFFERS
                    | pw::stream::StreamFlags::RT_PROCESS,
                &mut [pod],
            )
            .map_err(backend)?;
        let quit_loop = main_loop.clone();
        let _quit = quit.attach(main_loop.loop_(), move |()| quit_loop.quit());
        let _ = ready.send(Ok(()));
        main_loop.run();
        let _ = stream.disconnect();
        Ok(())
    };
    if let Err(err) = setup() {
        shared.fail(err.clone());
        let _ = ready.send(Err(err));
    }
}

fn spawn(
    kind: Kind,
    format: AudioFormat,
    target: Option<String>,
    shared: &Arc<Shared>,
    pull: Option<Render>,
) -> Result<Running, AudioError> {
    let (quit_tx, quit_rx) = pw::channel::channel::<()>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let thread_shared = Arc::clone(shared);
    let thread = thread::Builder::new()
        .name("pipewire-audio".into())
        .spawn(move || {
            run_stream(kind, format, target, thread_shared, pull, quit_rx, ready_tx);
        })
        .map_err(|err| backend(format!("no se pudo crear el hilo: {err}")))?;
    let mut running = Running {
        quit: quit_tx,
        thread: Some(thread),
    };
    match ready_rx.recv_timeout(CONNECT_TIMEOUT) {
        Ok(Ok(())) => Ok(running),
        Ok(Err(err)) => {
            running.stop();
            Err(err)
        }
        Err(_) => {
            running.stop();
            Err(no_daemon("el servidor no respondió a tiempo"))
        }
    }
}

/// Monitor de la salida por defecto o el nodo de una aplicación.
pub struct MonitorSource {
    app: Option<String>,
    format: AudioFormat,
    shared: Option<Arc<Shared>>,
    running: Option<Running>,
    target: Option<String>,
}

impl MonitorSource {
    /// Lo que suena en la salida por defecto (monitor del *sink*).
    pub fn system() -> Self {
        Self {
            app: None,
            format: AudioFormat::GAME,
            shared: None,
            running: None,
            target: None,
        }
    }

    /// Solo lo que reproduce la aplicación `name`.
    pub fn app(name: &str) -> Self {
        Self {
            app: Some(name.to_owned()),
            ..Self::system()
        }
    }
}

impl AudioSource for MonitorSource {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn start(&mut self, config: &AudioConfig) -> Result<(), AudioError> {
        self.stop();
        self.format = config.format;
        let target = match &self.app {
            Some(app) => Some(resolve_app(app)?),
            None => None,
        };
        let shared = Shared::new(self.format);
        let kind = Kind::Capture {
            capture_sink: self.app.is_none(),
            role: "Game",
        };
        self.running = Some(spawn(kind, self.format, target.clone(), &shared, None)?);
        self.shared = Some(shared);
        self.target = target;
        Ok(())
    }

    fn read(&mut self, out: &mut Vec<i16>) -> Result<Captured, AudioError> {
        let shared = self.shared.as_ref().ok_or(AudioError::Stopped)?;
        shared.read_frame(self.format, out)
    }

    fn stop(&mut self) {
        if let Some(mut running) = self.running.take() {
            running.stop();
        }
        self.shared = None;
    }

    fn describe(&self) -> String {
        match &self.app {
            Some(app) => format!("aplicación «{app}» (PipeWire)"),
            None => "monitor de la salida por defecto (PipeWire)".to_owned(),
        }
    }
}

/// Micrófono.
pub struct MicSource {
    device: Option<String>,
    format: AudioFormat,
    shared: Option<Arc<Shared>>,
    running: Option<Running>,
}

impl MicSource {
    /// Micrófono `device` (`None`: el del sistema).
    pub fn new(device: Option<String>) -> Self {
        Self {
            device,
            format: AudioFormat::VOICE,
            shared: None,
            running: None,
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
        let device = config.device.clone().or_else(|| self.device.clone());
        let shared = Shared::new(self.format);
        let kind = Kind::Capture {
            capture_sink: false,
            role: "Communication",
        };
        self.running = Some(spawn(kind, self.format, device, &shared, None)?);
        self.shared = Some(shared);
        Ok(())
    }

    fn read(&mut self, out: &mut Vec<i16>) -> Result<Captured, AudioError> {
        let shared = self.shared.as_ref().ok_or(AudioError::Stopped)?;
        shared.read_frame(self.format, out)
    }

    fn stop(&mut self) {
        if let Some(mut running) = self.running.take() {
            running.stop();
        }
        self.shared = None;
    }

    fn describe(&self) -> String {
        match &self.device {
            Some(device) => format!("micrófono «{device}» (PipeWire)"),
            None => "micrófono por defecto (PipeWire)".to_owned(),
        }
    }
}

/// Reproducción.
pub struct PlaybackSink {
    device: Option<String>,
    format: AudioFormat,
    shared: Option<Arc<Shared>>,
    running: Option<Running>,
    latency: Duration,
}

impl PlaybackSink {
    /// Salida `device` (`None`: la del sistema) en `format`.
    pub fn new(device: Option<String>, format: AudioFormat) -> Self {
        Self {
            device,
            format,
            shared: None,
            running: None,
            latency: crate::DEVICE_LATENCY,
        }
    }
}

impl AudioSink for PlaybackSink {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn start(&mut self, config: &AudioConfig) -> Result<(), AudioError> {
        self.stop();
        self.format = config.format;
        self.latency = config.latency;
        let device = config.device.clone().or_else(|| self.device.clone());
        let shared = Shared::new(self.format);
        self.running = Some(spawn(Kind::Playback, self.format, device, &shared, None)?);
        self.shared = Some(shared);
        Ok(())
    }

    fn supports_pull(&self) -> bool {
        true
    }

    fn start_pull(&mut self, config: &AudioConfig, render: Render) -> Result<(), AudioError> {
        self.stop();
        self.format = config.format;
        self.latency = config.latency;
        let device = config.device.clone().or_else(|| self.device.clone());
        let shared = Shared::new(self.format);
        self.running = Some(spawn(
            Kind::Playback,
            self.format,
            device,
            &shared,
            Some(render),
        )?);
        self.shared = Some(shared);
        Ok(())
    }

    fn check(&mut self) -> Result<(), AudioError> {
        let shared = self.shared.as_ref().ok_or(AudioError::Stopped)?;
        match shared.failure() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    fn device_counters(&self) -> DeviceCounters {
        self.shared
            .as_ref()
            .map(|shared| shared.counters())
            .unwrap_or_default()
    }

    fn wait_ready(&mut self) -> Result<(), AudioError> {
        let shared = self.shared.as_ref().ok_or(AudioError::Stopped)?;
        shared.wait_ready(self.format)
    }

    fn write(&mut self, samples: &[i16]) -> Result<Played, AudioError> {
        let shared = self.shared.as_ref().ok_or(AudioError::Stopped)?;
        shared.write_frame(self.format, samples, self.latency)
    }

    fn stop(&mut self) {
        if let Some(mut running) = self.running.take() {
            running.stop();
        }
        self.shared = None;
    }

    fn describe(&self) -> String {
        match &self.device {
            Some(device) => format!("salida «{device}» (PipeWire)"),
            None => "salida por defecto (PipeWire)".to_owned(),
        }
    }
}

/// Nodo de audio del registro.
#[derive(Debug, Clone, Default)]
struct Node {
    class: String,
    name: String,
    description: String,
    app: String,
    binary: String,
    pid: String,
    serial: String,
}

/// Nodos de audio del grafo (con un recorrido completo del registro).
fn enumerate() -> Result<Vec<Node>, AudioError> {
    pw::init();
    let main_loop = pw::main_loop::MainLoopRc::new(None).map_err(no_daemon)?;
    let context = pw::context::ContextRc::new(&main_loop, None).map_err(no_daemon)?;
    let core = context.connect_rc(None).map_err(no_daemon)?;
    let registry = core.get_registry_rc().map_err(backend)?;
    let nodes = Rc::new(RefCell::new(Vec::new()));
    let found = Rc::clone(&nodes);
    let _registry = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ != pw::types::ObjectType::Node {
                return;
            }
            let Some(props) = global.props else {
                return;
            };
            let get = |key: &str| props.get(key).unwrap_or_default().to_owned();
            let class = get("media.class");
            if !class.contains("Audio") {
                return;
            }
            found.borrow_mut().push(Node {
                class,
                name: get("node.name"),
                description: get("node.description"),
                app: get("application.name"),
                binary: get("application.process.binary"),
                pid: get("application.process.id"),
                serial: get("object.serial"),
            });
        })
        .register();
    let pending = core.sync(0).map_err(backend)?;
    let done_loop = main_loop.clone();
    let _core = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE && seq == pending {
                done_loop.quit();
            }
        })
        .register();
    let timeout_loop = main_loop.clone();
    let timer = main_loop.loop_().add_timer(move |_| timeout_loop.quit());
    let _ = timer.update_timer(Some(CONNECT_TIMEOUT), None);
    main_loop.run();
    let out = nodes.borrow().clone();
    Ok(out)
}

/// `target.object` del nodo de reproducción de la aplicación `name`.
fn resolve_app(name: &str) -> Result<String, AudioError> {
    let wanted = valid_name(name)?.to_lowercase();
    let nodes = enumerate()?;
    let streams: Vec<&Node> = nodes
        .iter()
        .filter(|node| node.class == "Stream/Output/Audio")
        .collect();
    streams
        .iter()
        .find(|node| {
            [&node.app, &node.binary, &node.name]
                .iter()
                .any(|field| field.to_lowercase().contains(&wanted))
        })
        .map(|node| {
            if node.serial.is_empty() {
                node.name.clone()
            } else {
                node.serial.clone()
            }
        })
        .ok_or_else(|| {
            let playing: Vec<String> = streams
                .iter()
                .map(|node| app_display_name(&node.app, &node.binary, &node.name))
                .collect();
            AudioError::NoSuchDevice(if playing.is_empty() {
                format!("{name} (ninguna aplicación reproduce audio ahora)")
            } else {
                format!("{name} (reproducen ahora: {})", playing.join(", "))
            })
        })
}

/// Nombre de la aplicación para mostrar: `application.name`, salvo cuando es el envoltorio
/// genérico de pipewire-alsa («ALSA plug-in [reproductor.bin]»), que no dice nada al usuario; entonces
/// el nombre del ejecutable sin ruta ni extensión (`reproductor`), y si no hay, el nombre del nodo.
fn app_display_name(app: &str, binary: &str, name: &str) -> String {
    let app = app.trim();
    let wrapper = app.is_empty() || app.to_lowercase().starts_with("alsa plug-in");
    if !wrapper {
        return app.to_owned();
    }
    let binary = binary
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches(".exe")
        .trim_end_matches(".bin")
        .trim_end_matches(".x86_64")
        .trim_end_matches(".sh");
    if !binary.is_empty() {
        return binary.to_owned();
    }
    // «ALSA plug-in [reproductor.bin]» sin binario: lo que va entre corchetes.
    if let Some((_, rest)) = app.split_once('[')
        && let Some((inner, _)) = rest.split_once(']')
        && !inner.trim().is_empty()
    {
        return app_display_name("", inner, name);
    }
    if app.is_empty() {
        name.to_owned()
    } else {
        app.to_owned()
    }
}

/// Si el nodo es de coroom (por el proceso o por el nombre: en Flatpak el PID es otro).
fn is_own(node: &Node) -> bool {
    node.pid == std::process::id().to_string()
        || node.app.eq_ignore_ascii_case("coroom")
        || node.name.starts_with("coroom-")
}

/// Audio por aplicación con PipeWire: cada flujo de reproducción (`Stream/Output/Audio`) de una
/// aplicación, enlazado por su `object.serial`.
pub(crate) struct AppStreams;

impl crate::apps::AppBackend for AppStreams {
    fn playing(&self) -> Result<Vec<crate::apps::PlayingApp>, AudioError> {
        Ok(crate::apps::group(
            enumerate()?
                .into_iter()
                .filter(|node| node.class == "Stream/Output/Audio" && !is_own(node))
                .map(|node| {
                    let key = [&node.app, &node.binary, &node.name]
                        .into_iter()
                        .find(|field| !field.is_empty())
                        .cloned()
                        .unwrap_or_default();
                    let name = app_display_name(&node.app, &node.binary, &node.name);
                    let stream = if node.serial.is_empty() {
                        node.name
                    } else {
                        node.serial
                    };
                    (key, name, stream)
                }),
        ))
    }

    fn open(&self, stream: &str, format: AudioFormat) -> Result<crate::apps::Tap, AudioError> {
        let shared = Shared::new(format);
        let kind = Kind::Capture {
            capture_sink: false,
            role: "Game",
        };
        let running = spawn(kind, format, Some(stream.to_owned()), &shared, None)?;
        Ok(crate::apps::Tap {
            shared,
            _guard: Box::new(running),
        })
    }

    fn label(&self) -> &'static str {
        "PipeWire"
    }
}

/// Dispositivos y aplicaciones con audio.
pub fn list_devices() -> Result<Vec<DeviceInfo>, AudioError> {
    Ok(enumerate()?
        .into_iter()
        .filter_map(|node| {
            let kind = match node.class.as_str() {
                "Audio/Sink" => DeviceKind::Sink,
                "Audio/Source" => DeviceKind::Source,
                "Stream/Output/Audio" => DeviceKind::Application,
                _ => return None,
            };
            let name = if kind == DeviceKind::Application {
                app_display_name(&node.app, &node.binary, &node.name)
            } else {
                node.name.clone()
            };
            Some(DeviceInfo {
                name,
                description: if node.description.is_empty() {
                    node.name
                } else {
                    node.description
                },
                kind,
                default: false,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}

    #[test]
    fn the_alsa_wrapper_shows_the_executable_instead() {
        assert_eq!(app_display_name("Firefox", "firefox", "n"), "Firefox");
        assert_eq!(
            app_display_name("ALSA plug-in [reproductor.bin]", "reproductor.bin", "n"),
            "reproductor"
        );
        assert_eq!(
            app_display_name("ALSA plug-in [reproductor.bin]", "", "n"),
            "reproductor"
        );
        assert_eq!(app_display_name("", "/usr/bin/game.x86_64", "n"), "game");
        assert_eq!(
            app_display_name("", "", "alsa_playback.reproductor"),
            "alsa_playback.reproductor"
        );
        assert_eq!(app_display_name("  ", "", "n"), "n");
    }

    #[test]
    fn backends_are_send_and_describe_themselves() {
        assert_send::<MonitorSource>();
        assert_send::<MicSource>();
        assert_send::<PlaybackSink>();
        assert!(MonitorSource::app("juego").describe().contains("juego"));
        assert!(MicSource::new(None).describe().contains("micrófono"));
        assert!(
            PlaybackSink::new(Some("auriculares".into()), AudioFormat::GAME)
                .describe()
                .contains("auriculares")
        );
    }

    #[test]
    fn the_format_parameter_serializes() {
        assert!(format_param(AudioFormat::GAME).is_ok_and(|bytes| !bytes.is_empty()));
        assert!(format_param(AudioFormat::VOICE).is_ok_and(|bytes| !bytes.is_empty()));
        assert!(valid_name("").is_err() && valid_name("a\0b").is_err());
    }

    #[test]
    fn unstarted_streams_say_so() {
        let mut mic = MicSource::new(None);
        assert_eq!(mic.read(&mut Vec::new()), Err(AudioError::Stopped));
        let mut sink = PlaybackSink::new(None, AudioFormat::VOICE);
        assert!(matches!(sink.write(&[0; 480]), Err(AudioError::Stopped)));
    }

    /// Con un servidor de PipeWire (un escritorio Linux o la CI con `pipewire` en
    /// marcha): un segundo de monitor y de reproducción. Sin servidor, se salta.
    #[test]
    fn real_streams_when_a_daemon_is_running() {
        let mut sink = PlaybackSink::new(None, AudioFormat::GAME);
        match sink.start(&AudioConfig::game()) {
            Err(AudioError::Unsupported(why)) => {
                eprintln!("sin PipeWire: test saltado ({why})");
                return;
            }
            Err(err) => panic!("{err}"),
            Ok(()) => {}
        }
        let mut monitor = MonitorSource::system();
        monitor.start(&AudioConfig::game()).expect("monitor");
        let mut frame = Vec::new();
        for _ in 0..100 {
            sink.write(&[0; 960]).expect("reproducir");
        }
        let captured = monitor.read(&mut frame).expect("capturar");
        assert_eq!(captured.samples, 960);
        assert!(!list_devices().expect("dispositivos").is_empty());
    }
}
