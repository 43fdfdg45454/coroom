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
//!   `node.latency = 480/48000` (10 ms).
//! - [`MonitorSource::system`]: captura (`media.category = Capture`) con
//!   `stream.capture.sink = true`: el monitor del *sink* por defecto, lo que suena.
//! - [`MonitorSource::app`]: el nodo de reproducción (`Stream/Output/Audio`) de una aplicación,
//!   buscado en el registro por `application.name`, `application.process.binary` o `node.name`
//!   (sin distinguir mayúsculas; vale un trozo) y enlazado con `target.object` = su `object.serial`.
//! - [`MicSource`]: captura del micrófono por defecto o del nodo `--mic NOMBRE` (`target.object`).
//! - [`PlaybackSink`]: reproducción (`media.category = Playback`, `media.role = Game`) en la salida
//!   por defecto o en `--speaker NOMBRE`. La hora a la que sonará cada trama es «ahora + lo que hay
//!   en el anillo + el retardo del grafo» (`pw_stream_get_time_n`, `delay`).
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

use crate::ring::Shared;
use crate::{
    AudioConfig, AudioError, AudioFormat, AudioSink, AudioSource, Captured, DeviceInfo, DeviceKind,
    Played, SAMPLE_RATE_HZ,
};

/// Espera máxima a que el hilo de PipeWire conecte el flujo.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

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

/// Lo que el callback necesita.
struct StreamData {
    shared: Arc<Shared>,
    kind: Kind,
    channels: usize,
    info: AudioInfoRaw,
    /// Una trama de reproducción (se reutiliza).
    scratch: [i16; crate::MAX_FRAME_SAMPLES],
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

fn process(stream: &pw::stream::Stream, data: &mut StreamData) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let datas = buffer.datas_mut();
    let Some(first) = datas.first_mut() else {
        return;
    };
    let stride = 2 * data.channels;
    match data.kind {
        Kind::Capture { .. } => {
            let offset = first.chunk().offset() as usize;
            let size = first.chunk().size() as usize;
            let Some(bytes) = first.data() else {
                return;
            };
            let Some(bytes) = bytes.get(offset..offset + size) else {
                return;
            };
            data.shared.push(
                bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&pair| i16::from_le_bytes(pair)),
            );
        }
        Kind::Playback => {
            let Some(bytes) = first.data() else {
                return;
            };
            // Un cuanto (10 ms con `node.latency`): llenar el búfer entero añadiría latencia.
            let frames = (bytes.len() / stride).min(crate::SAMPLES_PER_CHANNEL);
            let wanted = frames * data.channels;
            let scratch = &mut data.scratch[..wanted];
            data.shared.pop_into(scratch);
            for (chunk, sample) in bytes[..wanted * 2]
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .zip(scratch.iter())
            {
                *chunk = sample.to_le_bytes();
            }
            let chunk = first.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as i32;
            *chunk.size_mut() = (wanted * 2) as u32;
            if let Ok(time) = stream.time() {
                let rate = time.rate();
                if rate.denom > 0 && time.delay() > 0 {
                    let us = time.delay() as u64 * 1_000_000 * u64::from(rate.num)
                        / u64::from(rate.denom);
                    data.shared.delay_us.store(us, Ordering::Relaxed);
                }
            }
        }
    }
}

/// Cuerpo del hilo `pipewire-audio`.
fn run_stream(
    kind: Kind,
    format: AudioFormat,
    target: Option<String>,
    shared: Arc<Shared>,
    quit: pw::channel::Receiver<()>,
    ready: std::sync::mpsc::Sender<Result<(), AudioError>>,
) {
    let setup = || -> Result<(), AudioError> {
        pw::init();
        let main_loop = pw::main_loop::MainLoopRc::new(None).map_err(no_daemon)?;
        let context = pw::context::ContextRc::new(&main_loop, None).map_err(no_daemon)?;
        let core = context.connect_rc(None).map_err(no_daemon)?;
        let (category, role, name) = match &kind {
            Kind::Capture { role, .. } => ("Capture", *role, "coroom-captura"),
            Kind::Playback => ("Playback", "Game", "coroom-reproduccion"),
        };
        let mut props = pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => category,
            *pw::keys::MEDIA_ROLE => role,
            *pw::keys::NODE_LATENCY => "480/48000",
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
            channels: usize::from(format.channels),
            info: AudioInfoRaw::default(),
            scratch: [0; crate::MAX_FRAME_SAMPLES],
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
                pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
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
) -> Result<Running, AudioError> {
    let (quit_tx, quit_rx) = pw::channel::channel::<()>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let thread_shared = Arc::clone(shared);
    let thread = thread::Builder::new()
        .name("pipewire-audio".into())
        .spawn(move || run_stream(kind, format, target, thread_shared, quit_rx, ready_tx))
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
        self.running = Some(spawn(kind, self.format, target.clone(), &shared)?);
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
        self.running = Some(spawn(kind, self.format, device, &shared)?);
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
        self.running = Some(spawn(Kind::Playback, self.format, device, &shared)?);
        self.shared = Some(shared);
        Ok(())
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
                .map(|node| {
                    if node.app.is_empty() {
                        node.name.clone()
                    } else {
                        node.app.clone()
                    }
                })
                .collect();
            AudioError::NoSuchDevice(if playing.is_empty() {
                format!("{name} (ninguna aplicación reproduce audio ahora)")
            } else {
                format!("{name} (reproducen ahora: {})", playing.join(", "))
            })
        })
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
                    let name = if node.app.is_empty() {
                        key.clone()
                    } else {
                        node.app.clone()
                    };
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
        let running = spawn(kind, format, Some(stream.to_owned()), &shared)?;
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
            let name = if kind == DeviceKind::Application && !node.app.is_empty() {
                node.app.clone()
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
