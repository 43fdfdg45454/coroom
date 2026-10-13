//! Audio por **WASAPI** en Windows (feature `wasapi`) con la crate `wasapi` 0.26 (HEnquist, MIT,
//! mantenida: publicada en octubre de 2026, sobre `windows` 0.62 como el resto del proyecto).
//!
//! Todo en **modo compartido por eventos** con el búfer mínimo (10 ms) y `S16LE` 48 kHz con
//! conversión automática (`AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM`). Cada flujo vive en su hilo
//! (`wasapi-audio`), que inicializa COM (MTA), espera los eventos del dispositivo y copia entre su
//! búfer y el anillo compartido con el hilo de audio de la sesión (como el backend de PipeWire).
//!
//! - [`LoopbackSource`]: *loopback* del dispositivo de salida por defecto (lo que suena).
//! - [`ProcessLoopbackSource`]: **loopback por proceso** (`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`
//!   por `ActivateAudioInterfaceAsync`, que hace `wasapi::AudioClient::new_application_loopback_client`):
//!   solo el audio del proceso `--audio process PID` y de sus hijos. Necesita Windows 10 20348 o
//!   posterior; [`process_loopback_status`] lo comprueba para `coroom doctor`.
//! - [`MicSource`] y [`PlaybackSink`]: el dispositivo por defecto o el que se nombre (`--mic`,
//!   `--speaker`, por su nombre visible). La hora a la que suena cada trama es «ahora + lo que hay
//!   en el anillo + el relleno del búfer del dispositivo» (`GetCurrentPadding`).
//!
//! En *loopback* WASAPI no da eventos mientras no suena nada: el anillo entrega silencio si en
//! 40 ms no llega audio (ver [`crate::ring`]). No se puede ejecutar en la CI de Linux; se comprueba
//! con `cargo check --target x86_64-pc-windows-msvc` y en Windows.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use wasapi::{
    AudioCaptureClient, AudioClient, AudioRenderClient, DeviceEnumerator, Direction, Handle,
    SampleType, StreamMode, WaveFormat,
};

use crate::ring::Shared;
use crate::{
    AudioConfig, AudioError, AudioFormat, AudioSink, AudioSource, Captured, DeviceInfo, DeviceKind,
    Played, SAMPLE_RATE_HZ, SAMPLES_PER_CHANNEL,
};

/// Espera máxima a que el hilo abra el dispositivo.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// Duración del búfer compartido que se pide (10 ms, en unidades de 100 ns).
const BUFFER_HNS: i64 = 100_000;

fn backend(err: impl std::fmt::Display) -> AudioError {
    AudioError::Backend(format!("WASAPI: {err}"))
}

/// Qué abrir.
#[derive(Debug, Clone)]
enum Endpoint {
    /// Loopback de la salida por defecto.
    Loopback,
    /// Loopback de un proceso (y su árbol si `tree`).
    Process { pid: u32, tree: bool },
    /// Captura de un micrófono.
    Capture(Option<String>),
    /// Reproducción.
    Render(Option<String>),
}

enum Io {
    Capture(AudioCaptureClient),
    Render(AudioRenderClient),
}

fn device(
    enumerator: &DeviceEnumerator,
    direction: Direction,
    name: Option<&str>,
) -> Result<wasapi::Device, AudioError> {
    match name {
        None => enumerator.get_default_device(&direction).map_err(backend),
        Some(name) => enumerator
            .get_device_collection(&direction)
            .map_err(backend)?
            .get_device_with_name(name)
            .map_err(|_| AudioError::NoSuchDevice(name.to_owned())),
    }
}

fn open(endpoint: &Endpoint, format: AudioFormat) -> Result<(AudioClient, Handle, Io), AudioError> {
    let enumerator = DeviceEnumerator::new().map_err(backend)?;
    let mut client = match endpoint {
        Endpoint::Loopback => device(&enumerator, Direction::Render, None)?
            .get_iaudioclient()
            .map_err(backend)?,
        Endpoint::Process { pid, tree } => {
            AudioClient::new_application_loopback_client(*pid, *tree).map_err(|err| {
                AudioError::Unsupported(format!(
                    "no se pudo abrir el loopback del proceso {pid} ({err}); hace falta Windows 10 \
                     20348 o posterior y que el proceso exista"
                ))
            })?
        }
        Endpoint::Capture(name) => device(&enumerator, Direction::Capture, name.as_deref())?
            .get_iaudioclient()
            .map_err(backend)?,
        Endpoint::Render(name) => device(&enumerator, Direction::Render, name.as_deref())?
            .get_iaudioclient()
            .map_err(backend)?,
    };
    let wave = WaveFormat::new(
        16,
        16,
        &SampleType::Int,
        SAMPLE_RATE_HZ as usize,
        usize::from(format.channels),
        None,
    );
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: BUFFER_HNS,
    };
    let direction = match endpoint {
        Endpoint::Render(_) => Direction::Render,
        _ => Direction::Capture,
    };
    client
        .initialize_client(&wave, &direction, &mode)
        .map_err(backend)?;
    let handle = client.set_get_eventhandle().map_err(backend)?;
    let io = match direction {
        Direction::Render => Io::Render(client.get_audiorenderclient().map_err(backend)?),
        Direction::Capture => Io::Capture(client.get_audiocaptureclient().map_err(backend)?),
    };
    client.start_stream().map_err(backend)?;
    Ok((client, handle, io))
}

/// Cuerpo del hilo `wasapi-audio`.
fn run(
    endpoint: Endpoint,
    format: AudioFormat,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    ready: std::sync::mpsc::Sender<Result<(), AudioError>>,
) {
    // El hilo que mueve el dispositivo, con MMCSS «Pro Audio» como el motor de audio de Windows.
    match crate::realtime::promote_current_thread() {
        Ok(via) => tracing::debug!(via, "wasapi-audio en tiempo real"),
        Err(reason) => tracing::debug!(%reason, "wasapi-audio sin tiempo real"),
    }
    let _ = wasapi::initialize_mta();
    let (client, handle, io) = match open(&endpoint, format) {
        Ok(opened) => opened,
        Err(err) => {
            shared.fail(err.clone());
            let _ = ready.send(Err(err));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    let channels = usize::from(format.channels);
    let bytes_per_frame = 2 * channels;
    // Búferes reservados una vez: 100 ms de bytes y una trama de muestras.
    let mut bytes = vec![0u8; SAMPLES_PER_CHANNEL * 10 * bytes_per_frame];
    let mut samples = vec![0i16; SAMPLES_PER_CHANNEL * 10 * channels];
    while !stop.load(Ordering::Acquire) {
        let event = handle.wait_for_event(100);
        match &io {
            Io::Capture(capture) => {
                if event.is_err() {
                    continue;
                }
                loop {
                    match capture.get_next_packet_size() {
                        Ok(Some(0)) | Ok(None) => break,
                        Ok(Some(_)) => {}
                        Err(err) => {
                            shared.fail(backend(err));
                            return;
                        }
                    }
                    match capture.read_from_device(&mut bytes) {
                        Ok((0, _)) => break,
                        Ok((frames, info)) => {
                            let len = frames as usize * bytes_per_frame;
                            let silent = info.flags.silent;
                            shared.push(
                                bytes[..len]
                                    .as_chunks::<2>()
                                    .0
                                    .iter()
                                    .map(|&pair| if silent { 0 } else { i16::from_le_bytes(pair) }),
                            );
                        }
                        Err(err) => {
                            shared.fail(backend(err));
                            return;
                        }
                    }
                }
            }
            Io::Render(render) => {
                let available = match client.get_available_space_in_frames() {
                    Ok(frames) => frames as usize,
                    Err(err) => {
                        shared.fail(backend(err));
                        return;
                    }
                };
                // Un cuanto como mucho: llenar el búfer entero de silencio añadiría latencia.
                let frames = available
                    .min(SAMPLES_PER_CHANNEL)
                    .min(samples.len() / channels);
                if frames == 0 {
                    continue;
                }
                let wanted = frames * channels;
                shared.pop_into(&mut samples[..wanted]);
                for (chunk, sample) in bytes[..wanted * 2]
                    .as_chunks_mut::<2>()
                    .0
                    .iter_mut()
                    .zip(&samples[..wanted])
                {
                    *chunk = sample.to_le_bytes();
                }
                if let Err(err) = render.write_to_device(frames, &bytes[..wanted * 2], None) {
                    shared.fail(backend(err));
                    return;
                }
                if let Ok(padding) = client.get_current_padding() {
                    let us = u64::from(padding) * 1_000_000 / u64::from(SAMPLE_RATE_HZ);
                    shared.delay_us.store(us, Ordering::Relaxed);
                }
            }
        }
    }
    let _ = client.stop_stream();
}

/// Flujo en marcha en su hilo.
struct Running {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Running {
    fn start(
        endpoint: Endpoint,
        format: AudioFormat,
        shared: &Arc<Shared>,
    ) -> Result<Self, AudioError> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let thread = {
            let (stop, shared) = (Arc::clone(&stop), Arc::clone(shared));
            thread::Builder::new()
                .name("wasapi-audio".into())
                .spawn(move || run(endpoint, format, shared, stop, ready_tx))
                .map_err(|err| backend(format!("no se pudo crear el hilo: {err}")))?
        };
        let mut running = Self {
            stop,
            thread: Some(thread),
        };
        match ready_rx.recv_timeout(OPEN_TIMEOUT) {
            Ok(Ok(())) => Ok(running),
            Ok(Err(err)) => {
                running.halt();
                Err(err)
            }
            Err(_) => {
                running.halt();
                Err(backend("el dispositivo no se abrió a tiempo"))
            }
        }
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.halt();
    }
}

/// Fuente de captura genérica sobre un [`Endpoint`].
struct Capture {
    endpoint: Endpoint,
    format: AudioFormat,
    shared: Option<Arc<Shared>>,
    running: Option<Running>,
}

impl Capture {
    fn new(endpoint: Endpoint, format: AudioFormat) -> Self {
        Self {
            endpoint,
            format,
            shared: None,
            running: None,
        }
    }

    fn start(&mut self, config: &AudioConfig) -> Result<(), AudioError> {
        self.stop();
        self.format = config.format;
        if let (Endpoint::Capture(name), Some(device)) = (&mut self.endpoint, &config.device) {
            *name = Some(device.clone());
        }
        let shared = Shared::new(self.format);
        self.running = Some(Running::start(self.endpoint.clone(), self.format, &shared)?);
        self.shared = Some(shared);
        Ok(())
    }

    fn read(&mut self, out: &mut Vec<i16>) -> Result<Captured, AudioError> {
        let shared = self.shared.as_ref().ok_or(AudioError::Stopped)?;
        shared.read_frame(self.format, out)
    }

    fn stop(&mut self) {
        if let Some(mut running) = self.running.take() {
            running.halt();
        }
        self.shared = None;
    }
}

macro_rules! capture_source {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        pub struct $name(Capture);

        impl AudioSource for $name {
            fn format(&self) -> AudioFormat {
                self.0.format
            }

            fn start(&mut self, config: &AudioConfig) -> Result<(), AudioError> {
                self.0.start(config)
            }

            fn read(&mut self, out: &mut Vec<i16>) -> Result<Captured, AudioError> {
                self.0.read(out)
            }

            fn stop(&mut self) {
                self.0.stop();
            }

            fn describe(&self) -> String {
                match &self.0.endpoint {
                    Endpoint::Loopback => "loopback de la salida por defecto (WASAPI)".to_owned(),
                    Endpoint::Process { pid, .. } => {
                        format!("loopback del proceso {pid} y sus hijos (WASAPI)")
                    }
                    Endpoint::Capture(Some(name)) => format!("micrófono «{name}» (WASAPI)"),
                    Endpoint::Capture(None) => "micrófono por defecto (WASAPI)".to_owned(),
                    Endpoint::Render(_) => "salida (WASAPI)".to_owned(),
                }
            }
        }
    };
}

capture_source!(LoopbackSource, "Loopback de la salida por defecto.");
capture_source!(
    ProcessLoopbackSource,
    "Loopback de un proceso y sus hijos (Windows 10 20348+)."
);
capture_source!(MicSource, "Micrófono.");

impl LoopbackSource {
    /// Lo que suena en la salida por defecto.
    pub fn new() -> Self {
        Self(Capture::new(Endpoint::Loopback, AudioFormat::GAME))
    }
}

impl Default for LoopbackSource {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessLoopbackSource {
    /// Solo el proceso `pid` (y su árbol si `include_tree`).
    pub fn new(pid: u32, include_tree: bool) -> Self {
        Self(Capture::new(
            Endpoint::Process {
                pid,
                tree: include_tree,
            },
            AudioFormat::GAME,
        ))
    }
}

impl MicSource {
    /// Micrófono `device` por su nombre visible (`None`: el del sistema).
    pub fn new(device: Option<String>) -> Self {
        Self(Capture::new(Endpoint::Capture(device), AudioFormat::VOICE))
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
        self.running = Some(Running::start(
            Endpoint::Render(device),
            self.format,
            &shared,
        )?);
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
            running.halt();
        }
        self.shared = None;
    }

    fn describe(&self) -> String {
        match &self.device {
            Some(device) => format!("salida «{device}» (WASAPI)"),
            None => "salida por defecto (WASAPI)".to_owned(),
        }
    }
}

/// Dispositivos de entrada y salida.
pub fn list_devices() -> Result<Vec<DeviceInfo>, AudioError> {
    let _ = wasapi::initialize_mta();
    let enumerator = DeviceEnumerator::new().map_err(backend)?;
    let mut out = Vec::new();
    for (direction, kind) in [
        (Direction::Render, DeviceKind::Sink),
        (Direction::Capture, DeviceKind::Source),
    ] {
        let default_id = enumerator
            .get_default_device(&direction)
            .and_then(|device| device.get_id())
            .ok();
        let collection = enumerator
            .get_device_collection(&direction)
            .map_err(backend)?;
        for device in &collection {
            let Ok(device) = device else {
                continue;
            };
            let name = device.get_friendlyname().unwrap_or_default();
            let id = device.get_id().ok();
            out.push(DeviceInfo {
                description: device.get_description().unwrap_or_else(|_| name.clone()),
                name,
                kind,
                default: id.is_some() && id == default_id,
            });
        }
    }
    Ok(out)
}

/// Si este Windows tiene loopback por proceso (se prueba activándolo para este mismo proceso).
pub fn process_loopback_status() -> String {
    let _ = wasapi::initialize_mta();
    match AudioClient::new_application_loopback_client(std::process::id(), false) {
        Ok(_) => "loopback por proceso: disponible (--audio process PID)".to_owned(),
        Err(err) => format!(
            "loopback por proceso: no disponible ({err}); hace falta Windows 10 20348 o posterior"
        ),
    }
}

/// Clave de aplicación del proceso `pid`: el nombre de su ejecutable en minúsculas
/// (`eldenring.exe`). La captura de ventanas calcula la misma para el proceso de la ventana, así
/// que audio y vídeo de una aplicación se emparejan por ella. `None` si el proceso no existe o no
/// se puede consultar.
pub fn app_key_for_pid(pid: u32) -> Option<String> {
    let path = process_image(pid)?;
    exe_file_name(&path).map(str::to_lowercase)
}

/// Nombre de fichero de una ruta de Windows (`C:\\Juegos\\X.exe` → `X.exe`).
fn exe_file_name(path: &str) -> Option<&str> {
    let name = path.rsplit(['\\', '/']).next()?.trim();
    (!name.is_empty()).then_some(name)
}

/// Nombre visible a partir de la clave: sin la extensión `.exe`.
fn exe_stem(key: &str) -> &str {
    key.len()
        .checked_sub(4)
        .filter(|&cut| key.is_char_boundary(cut) && key[cut..].eq_ignore_ascii_case(".exe"))
        .map_or(key, |cut| &key[..cut])
}

/// Nombre que se muestra: el de la sesión si lo puso la aplicación y no es una referencia a un
/// recurso (`@%SystemRoot%\\…,-123`); si no, el del ejecutable sin extensión.
fn display_name(session_name: &str, key: &str) -> String {
    let name = session_name.trim();
    if name.is_empty() || name.starts_with('@') {
        exe_stem(key).to_owned()
    } else {
        name.to_owned()
    }
}

/// Ruta completa del ejecutable del proceso `pid`.
#[allow(unsafe_code)] // Win32 sin envoltorio seguro; cada bloque lleva su `SAFETY:`.
fn process_image(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::core::PWSTR;

    // SAFETY: `OpenProcess` no toma punteros; el handle devuelto se cierra abajo en todos los
    // caminos.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = vec![0u16; 32_768];
    let mut len = buffer.len() as u32;
    // SAFETY: `buffer` es válido y tiene `len` u16 de capacidad; la función escribe como mucho
    // `len` caracteres y deja en `len` los escritos (sin el nulo).
    let queried = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        )
    };
    // SAFETY: `process` es un handle abierto por `OpenProcess` y no se usa después.
    let _ = unsafe { CloseHandle(process) };
    queried.ok()?;
    let len = (len as usize).min(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..len]))
}

/// Sesiones de audio por aplicación de la salida por defecto; cada una se captura con el
/// loopback de su proceso.
pub(crate) struct AppSessions;

impl AppSessions {
    /// (clave, nombre, pid) de cada sesión viva de la salida por defecto, salvo la de sonidos del
    /// sistema y la de coroom.
    #[allow(unsafe_code)] // COM sin envoltorio en `wasapi` (`IsSystemSoundsSession`).
    fn sessions() -> Result<Vec<(String, String, String)>, AudioError> {
        use windows::Win32::Media::Audio::{
            AudioSessionStateExpired, IAudioSessionControl2, IAudioSessionManager2,
            IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender,
        };
        use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};
        use windows::core::{HRESULT, Interface};

        // Hilo de la UI o `audio-apps`: COM en MTA (si ya estaba en STA, se usa ese).
        let _ = wasapi::initialize_mta();
        let own = std::process::id();
        // SAFETY: llamadas COM con COM inicializado en este hilo; las interfaces devueltas son
        // punteros contados que `windows` libera al soltarlos, y la cadena de `GetDisplayName`
        // se libera con `CoTaskMemFree` tras copiarla.
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(backend)?;
            let device = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(backend)?;
            let manager: IAudioSessionManager2 =
                device.Activate(CLSCTX_ALL, None).map_err(backend)?;
            let sessions = manager.GetSessionEnumerator().map_err(backend)?;
            let count = sessions.GetCount().map_err(backend)?;
            let mut out = Vec::new();
            for index in 0..count {
                let Ok(control) = sessions.GetSession(index) else {
                    continue;
                };
                let Ok(control) = control.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                // S_OK (0) solo para la sesión de sonidos del sistema; S_FALSE para el resto.
                if control.IsSystemSoundsSession() == HRESULT(0) {
                    continue;
                }
                let Ok(pid) = control.GetProcessId() else {
                    continue;
                };
                if pid == 0 || pid == own {
                    continue;
                }
                match control.GetState() {
                    Ok(state) if state != AudioSessionStateExpired => {}
                    _ => continue,
                }
                let Some(key) = app_key_for_pid(pid) else {
                    continue;
                };
                let session_name = match control.GetDisplayName() {
                    Ok(name) if !name.is_null() => {
                        let text = name.to_string().unwrap_or_default();
                        CoTaskMemFree(Some(name.0 as *const _));
                        text
                    }
                    _ => String::new(),
                };
                let name = display_name(&session_name, &key);
                out.push((key, name, pid.to_string()));
            }
            Ok(out)
        }
    }
}

impl crate::apps::AppBackend for AppSessions {
    fn playing(&self) -> Result<Vec<crate::apps::PlayingApp>, AudioError> {
        // Sin loopback por proceso (antes de Windows 10 20348) no hay audio por aplicación: se
        // dice aquí para que la interfaz pase al audio de todo el sistema.
        static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let supported = *SUPPORTED.get_or_init(|| {
            let _ = wasapi::initialize_mta();
            AudioClient::new_application_loopback_client(std::process::id(), false).is_ok()
        });
        if !supported {
            return Err(AudioError::Unsupported(
                "el audio por aplicación necesita Windows 10 20348 o posterior".to_owned(),
            ));
        }
        Ok(crate::apps::group(Self::sessions()?))
    }

    fn open(&self, stream: &str, format: AudioFormat) -> Result<crate::apps::Tap, AudioError> {
        let pid: u32 = stream
            .parse()
            .map_err(|_| backend(format!("flujo de aplicación no válido: «{stream}»")))?;
        // Sin el árbol: cada proceso con sesión sale por separado, y con árboles se capturarían
        // dos veces los padres e hijos que también suenan.
        let shared = Shared::new(format);
        let running = Running::start(Endpoint::Process { pid, tree: false }, format, &shared)?;
        Ok(crate::apps::Tap {
            shared,
            _guard: Box::new(running),
        })
    }

    fn label(&self) -> &'static str {
        "WASAPI"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_names_come_from_the_executable() {
        assert_eq!(
            exe_file_name(r"C:\Juegos\Elden Ring\EldenRing.exe"),
            Some("EldenRing.exe")
        );
        assert_eq!(exe_file_name(r"C:\Juegos\"), None);
        assert_eq!(exe_stem("eldenring.exe"), "eldenring");
        assert_eq!(exe_stem("herramienta"), "herramienta");
        assert_eq!(display_name("", "eldenring.exe"), "eldenring");
        assert_eq!(
            display_name("@%SystemRoot%\\System32\\x.dll,-1", "x.exe"),
            "x"
        );
        assert_eq!(display_name("Spotify", "spotify.exe"), "Spotify");
    }

    #[test]
    fn this_process_has_a_key() {
        let key = app_key_for_pid(std::process::id()).expect("clave de este proceso");
        assert!(key.ends_with(".exe"));
        assert_eq!(key, key.to_lowercase());
    }

    #[test]
    fn coroom_never_lists_itself() {
        use crate::apps::AppBackend;
        if let Ok(apps) = AppSessions.playing() {
            let own = std::process::id().to_string();
            assert!(apps.iter().all(|app| !app.streams.contains(&own)));
        }
    }
}
