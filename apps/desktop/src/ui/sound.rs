//! Los sonidos de los avisos y el selector de un tono propio.
//!
//! - **Dónde suenan**: por la salida elegida en «Audio y voz» (la del sistema si no hay o ya no
//!   está), con el reproductor de audio de la sala (`coroom_session::audio::playback_sink`:
//!   PipeWire en Linux, WASAPI en Windows). Así **nunca llegan a la sala**: el audio del juego que
//!   envía el anfitrión deja fuera los flujos de coroom (PipeWire por proceso y por
//!   `node.name = coroom-…`, WASAPI por el loopback que excluye este proceso).
//! - **Qué suena**: uno de los tonos propios ([`super::tones`]) o un fichero elegido. Un WAV (PCM
//!   o coma flotante, cualquier frecuencia) lo lee coroom y suena igual que los propios, con el
//!   volumen de los avisos; en Linux, otros formatos (OGG, FLAC…) los reproduce `pw-play` o
//!   `paplay` marcados como de coroom (fuera de la sala también) con la salida y el volumen
//!   elegidos. En Windows solo WAV. Un fichero propio suena como mucho [`MAX_CUSTOM`].
//! - **El selector**: el portal `FileChooser` en Linux (también dentro de Flatpak) e
//!   `IFileOpenDialog` en Windows.
//!
//! Nada de esto corre en el hilo de la interfaz: [`play`] y [`pick_checked`] se llaman desde una
//! tarea de `Backend::spawn` y bloquean mientras suena, mientras el diálogo está abierto o mientras
//! se lee el fichero elegido. Un WAV propio se lee como mucho de [`MAX_WAV_BYTES`], y solo si
//! empieza como un WAV: lo demás no se carga.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::settings::{Settings, SoundChoice, SoundEvent};
use super::tones::{self, Builtin};

/// Lo más que suena un fichero propio (con un fundido al final): un aviso no es una canción.
pub const MAX_CUSTOM: Duration = Duration::from_secs(3);

/// Lo más que se lee de un WAV propio: sobra para un aviso (a 48 kHz, estéreo y 16 bits, más de
/// un minuto) y evita cargar entero un fichero enorme elegido por error.
pub const MAX_WAV_BYTES: u64 = 16 * 1024 * 1024;

/// Fundido al cortar un fichero propio largo.
const CUT_FADE: Duration = Duration::from_millis(60);

/// Lo que suena en un aviso.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// Un tono propio.
    Tone(Builtin),
    /// Un fichero elegido.
    File(PathBuf),
}

/// Un aviso listo para sonar.
#[derive(Debug, Clone, PartialEq)]
pub struct Playback {
    /// Qué suena.
    pub source: Source,
    /// Lo que suena si el fichero ya no está o no se puede reproducir (el tono del aviso).
    pub fallback: Builtin,
    /// Ganancia lineal (el volumen de los avisos).
    pub gain: f32,
    /// Salida por nombre (`None`: la del sistema).
    pub device: Option<String>,
}

/// Lo que tiene que sonar con `event` según los ajustes; `None` si está apagado o sin volumen.
pub fn playback(settings: &Settings, event: SoundEvent) -> Option<Playback> {
    let notifications = &settings.notifications;
    let sound = notifications.sound(event);
    if !sound.is_on() || notifications.volume == 0 {
        return None;
    }
    let fallback = event.default_tone();
    let source = match (sound.tone, &sound.file) {
        (SoundChoice::Tone(tone), _) => Source::Tone(tone),
        (SoundChoice::File, Some(file)) => Source::File(file.clone()),
        (SoundChoice::File, None) | (SoundChoice::Off, _) => return None,
    };
    Some(Playback {
        source,
        fallback,
        gain: notifications.gain(),
        device: settings.audio.speaker.clone(),
    })
}

/// Reproduce el aviso. Bloquea mientras suena; se llama desde una tarea del `Backend`.
pub fn play(request: &Playback) {
    let fallback = || stereo(&request.fallback.samples());
    let pcm = match &request.source {
        Source::Tone(tone) => stereo(&tone.samples()),
        Source::File(file) if !file.is_file() => {
            tracing::debug!(file = %file.display(), "el tono elegido ya no está; suena el del aviso");
            fallback()
        }
        Source::File(file) => match read_wav(file) {
            Ok(wav) => wav.to_stereo_48k(MAX_CUSTOM),
            Err(err) => {
                if play_external(file, request.gain, request.device.as_deref()) {
                    return;
                }
                tracing::debug!(file = %file.display(), error = ?err, "no se pudo reproducir el tono elegido; suena el del aviso");
                fallback()
            }
        },
    };
    let pcm: Vec<i16> = tones::to_i16(&pcm, request.gain);
    if let Err(err) = play_pcm(request.device.as_deref(), &pcm) {
        tracing::debug!(error = %err, "no sonó el aviso");
    }
}

/// Mono a estéreo intercalado.
fn stereo(mono: &[f32]) -> Vec<f32> {
    mono.iter().flat_map(|s| [*s, *s]).collect()
}

/// Saca `pcm` (estéreo, 48 kHz) por `device`; si ese ya no está, por la salida del sistema.
fn play_pcm(device: Option<&str>, pcm: &[i16]) -> Result<(), String> {
    use coroom_session::audio::AudioError;
    match play_on(device, pcm) {
        Err(AudioError::NoSuchDevice(name)) if device.is_some() => {
            tracing::debug!(device = %name, "la salida elegida no está; el aviso suena por la del sistema");
            play_on(None, pcm).map_err(|err| err.to_string())
        }
        other => other.map_err(|err| err.to_string()),
    }
}

/// Abre la salida, escribe el sonido trama a trama al ritmo del dispositivo y la mantiene abierta
/// hasta que se oye la última muestra (la hora que da el propio dispositivo), sin cortar la cola.
fn play_on(device: Option<&str>, pcm: &[i16]) -> Result<(), coroom_session::audio::AudioError> {
    use coroom_session::audio::{AudioConfig, AudioFormat, DEVICE_LATENCY, playback_sink};
    let format = AudioFormat::GAME;
    let config = AudioConfig {
        format,
        device: device.map(str::to_owned),
        latency: DEVICE_LATENCY,
    };
    let mut sink = playback_sink(device, format)?;
    sink.start(&config)?;
    let frame = format.samples_per_frame();
    let frame_time = format.duration_of(frame);
    let mut buffer = Vec::with_capacity(frame);
    let mut ends_at = Instant::now();
    let result = (|| {
        for chunk in pcm.chunks(frame) {
            buffer.clear();
            buffer.extend_from_slice(chunk);
            buffer.resize(frame, 0);
            ends_at = sink.write(&buffer)?.played_at_estimate + frame_time;
        }
        // Silencio hasta que el dispositivo dice que ya sonó lo último (con un tope por si la
        // estimación no avanza: un segundo más de lo que dura).
        let silence = vec![0; frame];
        let limit = Instant::now() + Duration::from_secs(1);
        while Instant::now() < ends_at && Instant::now() < limit {
            sink.write(&silence)?;
        }
        Ok(())
    })();
    sink.stop();
    result
}

/// Linux: un fichero que coroom no lee (OGG, FLAC…) con `pw-play` o `paplay`, marcado como de
/// coroom para que el audio del juego de la sala lo deje fuera, por `device` y con `gain`.
/// `false` si ninguno pudo.
#[cfg(target_os = "linux")]
fn play_external(file: &Path, gain: f32, device: Option<&str>) -> bool {
    use std::process::{Command, Stdio};
    let mut pw_play = Command::new("pw-play");
    pw_play
        .arg(format!("--volume={gain:.3}"))
        .arg("-P")
        .arg(r#"{ "application.name": "coroom", "node.name": "coroom-aviso" }"#);
    if let Some(device) = device {
        pw_play.arg(format!("--target={device}"));
    }
    let mut paplay = Command::new("paplay");
    paplay
        .arg(format!(
            "--volume={}",
            (gain.clamp(0.0, 1.0) * 65_536.0) as u32
        ))
        .arg("--client-name=coroom")
        .arg("--property=application.name=coroom")
        .arg("--property=node.name=coroom-aviso");
    if let Some(device) = device {
        paplay.arg(format!("--device={device}"));
    }
    for mut player in [pw_play, paplay] {
        let started = player
            .arg(file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = started else {
            continue;
        };
        // Como mucho `MAX_CUSTOM`: lo que dure más se corta.
        let deadline = Instant::now() + MAX_CUSTOM;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        // Cortado por largo también cuenta como que sonó.
        if status.is_none_or(|status| status.success()) {
            return true;
        }
    }
    false
}

/// Windows (y el resto): solo WAV, que ya lee coroom.
#[cfg(not(target_os = "linux"))]
fn play_external(_file: &Path, _gain: f32, _device: Option<&str>) -> bool {
    false
}

/// Un WAV leído: muestras intercaladas en `[-1, 1]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Wav {
    /// Frecuencia de muestreo.
    pub rate: u32,
    /// Canales.
    pub channels: u16,
    /// Muestras intercaladas.
    pub samples: Vec<f32>,
}

impl Wav {
    /// Estéreo a 48 kHz (los dos primeros canales; mono se duplica), como mucho `max` con un
    /// fundido al final si se corta. Remuestreo lineal: sobra para un aviso.
    pub fn to_stereo_48k(&self, max: Duration) -> Vec<f32> {
        let channels = usize::from(self.channels.max(1));
        let frames = self.samples.len() / channels;
        let take = |frame: usize, channel: usize| {
            self.samples[frame * channels + channel.min(channels - 1)]
        };
        let ratio = f64::from(self.rate) / f64::from(tones::RATE);
        let out_frames = ((frames as f64 / ratio).floor() as usize)
            .min((max.as_secs_f64() * f64::from(tones::RATE)) as usize);
        let cut = out_frames < (frames as f64 / ratio).floor() as usize;
        let fade = (CUT_FADE.as_secs_f64() * f64::from(tones::RATE)) as usize;
        let mut out = Vec::with_capacity(out_frames * 2);
        for index in 0..out_frames {
            let position = index as f64 * ratio;
            let at = position.floor() as usize;
            let next = (at + 1).min(frames.saturating_sub(1));
            let weight = (position - at as f64) as f32;
            let left = out_frames - index;
            let gain = if cut && left < fade {
                left as f32 / fade as f32
            } else {
                1.0
            };
            for channel in 0..2 {
                let a = take(at, channel);
                let b = take(next, channel);
                out.push((a + (b - a) * weight) * gain);
            }
        }
        out
    }
}

/// Lee un WAV: PCM de 8, 16, 24 o 32 bits, o coma flotante de 32 o 64 (también
/// `WAVE_FORMAT_EXTENSIBLE`). El error dice por qué no, en palabras.
pub fn decode_wav(bytes: &[u8]) -> Result<Wav, String> {
    let u16_at = |at: usize| {
        bytes
            .get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let u32_at = |at: usize| {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    if bytes.get(0..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err("no es un fichero WAV".to_owned());
    }
    let mut at = 12;
    let mut format: Option<(u16, u16, u32, u16)> = None;
    let mut data: Option<&[u8]> = None;
    while let (Some(id), Some(size)) = (bytes.get(at..at + 4), u32_at(at + 4)) {
        let body = at + 8;
        let end = body.saturating_add(size as usize).min(bytes.len());
        match id {
            b"fmt " => {
                let mut tag = u16_at(body).ok_or("fmt incompleto")?;
                let channels = u16_at(body + 2).ok_or("fmt incompleto")?;
                let rate = u32_at(body + 4).ok_or("fmt incompleto")?;
                let bits = u16_at(body + 14).ok_or("fmt incompleto")?;
                if tag == 0xFFFE {
                    // WAVE_FORMAT_EXTENSIBLE: el formato de verdad son los dos primeros bytes del
                    // GUID del subformato.
                    tag = u16_at(body + 24).ok_or("fmt incompleto")?;
                }
                format = Some((tag, channels, rate, bits));
            }
            b"data" => data = Some(&bytes[body..end]),
            _ => {}
        }
        // Los trozos van alineados a 2 bytes.
        at = body.saturating_add(size as usize + (size as usize & 1));
    }
    let (tag, channels, rate, bits) = format.ok_or("WAV sin formato")?;
    let data = data.ok_or("WAV sin datos")?;
    if channels == 0 || rate == 0 {
        return Err("WAV sin canales".to_owned());
    }
    let samples: Vec<f32> = match (tag, bits) {
        (1, 8) => data
            .iter()
            .map(|b| (f32::from(*b) - 128.0) / 128.0)
            .collect(),
        (1, 16) => data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| f32::from(i16::from_le_bytes(*b)) / 32_768.0)
            .collect(),
        (1, 24) => data
            .as_chunks::<3>()
            .0
            .iter()
            .map(|b| (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0)
            .collect(),
        (1, 32) => data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| i32::from_le_bytes(*b) as f32 / 2_147_483_648.0)
            .collect(),
        (3, 32) => data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b).clamp(-1.0, 1.0))
            .collect(),
        (3, 64) => data
            .as_chunks::<8>()
            .0
            .iter()
            .map(|b| f64::from_le_bytes(*b).clamp(-1.0, 1.0) as f32)
            .collect(),
        _ => {
            return Err(format!(
                "WAV con un formato que no se lee ({tag}, {bits} bits)"
            ));
        }
    };
    Ok(Wav {
        rate,
        channels,
        samples,
    })
}

/// Por qué no se leyó un WAV ([`read_wav`]).
#[derive(Debug, Clone, PartialEq)]
pub enum WavError {
    /// No se puede abrir o leer (o no es un fichero).
    Unreadable(String),
    /// No empieza como un WAV (`RIFF`…`WAVE`): es de otro formato.
    NotWav,
    /// Pesa más de [`MAX_WAV_BYTES`].
    TooLarge,
    /// Es un WAV, pero no se entiende.
    Undecodable(String),
}

/// Lee el WAV de `path` sin cargar lo que no lo es: primero mira la cabecera (`RIFF`…`WAVE`) y el
/// tamaño (como mucho [`MAX_WAV_BYTES`]) y solo entonces lee el resto.
pub fn read_wav(path: &Path) -> Result<Wav, WavError> {
    use std::io::Read;
    let unreadable = |err: std::io::Error| WavError::Unreadable(err.to_string());
    let mut file = std::fs::File::open(path).map_err(unreadable)?;
    let meta = file.metadata().map_err(unreadable)?;
    if !meta.is_file() {
        return Err(WavError::Unreadable("no es un fichero".to_owned()));
    }
    let mut header = [0; 12];
    match file.read_exact(&mut header) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(WavError::NotWav);
        }
        Err(err) => return Err(unreadable(err)),
    }
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Err(WavError::NotWav);
    }
    if meta.len() > MAX_WAV_BYTES {
        return Err(WavError::TooLarge);
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    bytes.extend_from_slice(&header);
    // Con tope también al leer: el fichero puede crecer entre mirar su tamaño y leerlo.
    file.take(MAX_WAV_BYTES - header.len() as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if bytes.len() as u64 > MAX_WAV_BYTES {
        return Err(WavError::TooLarge);
    }
    decode_wav(&bytes).map_err(WavError::Undecodable)
}

/// Si `path` sirve como tono: existe, se puede leer y, si es WAV (en Windows tiene que serlo),
/// coroom sabe leerlo y no pasa de [`MAX_WAV_BYTES`]. Lee el fichero: fuera del hilo de la
/// interfaz ([`pick_checked`]).
pub fn check_tone(path: &Path) -> Result<(), String> {
    let wav = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"));
    if cfg!(target_os = "windows") && !wav {
        return Err(t!("settings-tono-solo-wav").to_owned());
    }
    let readable = std::fs::File::open(path)
        .and_then(|file| file.metadata())
        .is_ok_and(|meta| meta.is_file());
    if !readable {
        return Err(t!("settings-tono-no-se-puede-leer").to_owned());
    }
    if wav {
        read_wav(path).map_err(|err| match err {
            WavError::Unreadable(_) => t!("settings-tono-no-se-puede-leer").to_owned(),
            WavError::TooLarge => t!(
                "settings-tono-demasiado-grande",
                mib = MAX_WAV_BYTES / (1024 * 1024)
            ),
            WavError::NotWav | WavError::Undecodable(_) => {
                t!("settings-tono-wav-no-se-lee").to_owned()
            }
        })?;
    }
    Ok(())
}

/// Abre el selector de ficheros y comprueba el elegido ([`check_tone`]): `Ok(None)` si el usuario
/// canceló; el error, ya en palabras para la fila del aviso. Bloquea; se llama desde una tarea del
/// `Backend`, así que leer el fichero no para la interfaz.
pub fn pick_checked() -> Result<Option<PathBuf>, String> {
    checked(pick_tone())
}

/// Lo que devolvió el selector, con el fichero elegido ya comprobado ([`pick_checked`]).
pub fn checked(picked: Result<Option<PathBuf>, String>) -> Result<Option<PathBuf>, String> {
    let picked = picked.map_err(|error| t!("settings-tono-no-se-pudo-elegir", error = error))?;
    if let Some(file) = &picked {
        check_tone(file)?;
    }
    Ok(picked)
}

/// Ruta de un URI `file://` (con los `%XX` decodificados); `None` si no es un fichero local.
pub fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // `file:///ruta` o `file://localhost/ruta`.
    let path = match rest.find('/') {
        Some(0) => rest,
        Some(slash) if &rest[..slash] == "localhost" => &rest[slash..],
        _ => return None,
    };
    let mut bytes = Vec::with_capacity(path.len());
    let mut chars = path.bytes();
    while let Some(byte) = chars.next() {
        if byte == b'%' {
            let hex = [chars.next()?, chars.next()?];
            let hex = std::str::from_utf8(&hex).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
        } else {
            bytes.push(byte);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
    }
    #[cfg(not(unix))]
    {
        Some(PathBuf::from(String::from_utf8(bytes).ok()?))
    }
}

/// `text` en UTF-16 terminado en cero (rutas con cualquier codificación incluidas).
#[cfg(target_os = "windows")]
fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    text.encode_wide().chain(std::iter::once(0)).collect()
}

/// Abre el selector de ficheros para elegir un tono. `Ok(None)` si el usuario canceló. Bloquea
/// hasta que el diálogo se cierra; se llama desde una tarea del `Backend`.
pub fn pick_tone() -> Result<Option<PathBuf>, String> {
    #[cfg(target_os = "linux")]
    {
        pick_linux()
    }
    #[cfg(target_os = "windows")]
    {
        pick_windows()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err(t!("settings-tono-sin-selector").to_owned())
    }
}

/// El portal `FileChooser` (GNOME y KDE lo muestran con su diálogo, también dentro de Flatpak).
#[cfg(target_os = "linux")]
fn pick_linux() -> Result<Option<PathBuf>, String> {
    use ashpd::desktop::ResponseError;
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| err.to_string())?;
    let mut filter = FileFilter::new(t!("settings-tono-filtro-sonidos"));
    for pattern in ["*.wav", "*.ogg", "*.oga", "*.opus", "*.flac", "*.mp3"] {
        filter = filter.glob(pattern);
    }
    let picked = runtime.block_on(async {
        SelectedFiles::open_file()
            .title(t!("settings-elegir-el-tono"))
            .accept_label(t!("settings-tono-elegir"))
            .modal(true)
            .filter(filter)
            .send()
            .await?
            .response()
    });
    match picked {
        Ok(files) => Ok(files
            .uris()
            .first()
            .and_then(|uri| file_uri_to_path(uri.as_str()))),
        Err(ashpd::Error::Response(ResponseError::Cancelled)) => Ok(None),
        Err(err) => Err(err.to_string()),
    }
}

/// `IFileOpenDialog` (el diálogo de abrir de Windows) en un apartamento COM propio de este hilo,
/// solo con ficheros WAV.
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
fn pick_windows() -> Result<Option<PathBuf>, String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    use windows::Win32::Foundation::ERROR_CANCELLED;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
        CoInitializeEx, CoTaskMemFree, CoUninitialize,
    };
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH,
    };
    use windows::core::{HRESULT, PCWSTR, w};

    // SAFETY: el hilo es nuestro (`Backend::spawn` lo crea para esta tarea) y no tiene COM
    // iniciado; se cierra con `CoUninitialize` antes de volver.
    let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    if init.is_err() {
        return Err(init.message());
    }
    let title = wide(t!("settings-elegir-el-tono").as_ref());
    let label = wide(t!("settings-tono-filtro-wav").as_ref());
    let filters = [COMDLG_FILTERSPEC {
        pszName: PCWSTR(label.as_ptr()),
        pszSpec: w!("*.wav"),
    }];
    // SAFETY: llamadas COM sobre objetos vivos de este hilo; `title`, `filters` y las cadenas
    // que señala viven hasta que el diálogo se cierra, y el nombre que devuelve `GetDisplayName`
    // se copia y se libera con `CoTaskMemFree` una sola vez.
    let shown: windows::core::Result<Option<PathBuf>> = unsafe {
        (|| {
            let dialog: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
            dialog.SetTitle(PCWSTR(title.as_ptr()))?;
            dialog.SetFileTypes(&filters)?;
            let options = dialog.GetOptions()?;
            dialog.SetOptions(options | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM)?;
            if let Err(err) = dialog.Show(None) {
                if err.code() == HRESULT::from_win32(ERROR_CANCELLED.0) {
                    return Ok(None);
                }
                return Err(err);
            }
            let item = dialog.GetResult()?;
            let name = item.GetDisplayName(SIGDN_FILESYSPATH)?;
            let path = PathBuf::from(OsString::from_wide(name.as_wide()));
            CoTaskMemFree(Some(name.as_ptr().cast()));
            Ok(Some(path))
        })()
    };
    // SAFETY: empareja el `CoInitializeEx` de arriba, en el mismo hilo, con todos los objetos
    // COM ya liberados (`dialog` e `item` salieron del ámbito).
    unsafe { CoUninitialize() };
    shown.map_err(|err| err.message())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::settings::EventSound;

    #[test]
    fn file_uris_become_paths() {
        assert_eq!(
            file_uri_to_path("file:///home/ana/M%C3%BAsica/Tono%20nuevo.ogg"),
            Some(PathBuf::from("/home/ana/Música/Tono nuevo.ogg"))
        );
        assert_eq!(
            file_uri_to_path("file://localhost/tmp/a.wav"),
            Some(PathBuf::from("/tmp/a.wav"))
        );
        assert_eq!(file_uri_to_path("https://coroom.example.com/a.wav"), None);
        assert_eq!(file_uri_to_path("file://otro/a.wav"), None);
        assert_eq!(file_uri_to_path("file:///a%2"), None, "escape incompleto");
    }

    #[test]
    fn tones_are_checked_before_being_kept() {
        let dir = tempfile::tempdir().expect("directorio");
        let wav = dir.path().join("tono.wav");
        std::fs::write(&wav, Builtin::Chime.wav()).expect("escribir");
        assert_eq!(check_tone(&wav), Ok(()));
        assert!(check_tone(&dir.path().join("no-esta.wav")).is_err());
        assert!(check_tone(dir.path()).is_err(), "un directorio no vale");
        let broken = dir.path().join("roto.wav");
        std::fs::write(&broken, b"RIFF....WAVE").expect("escribir");
        assert!(check_tone(&broken).is_err(), "un WAV que no se lee no vale");
        let ogg = dir.path().join("tono.ogg");
        std::fs::write(&ogg, b"OggS").expect("escribir");
        assert_eq!(
            check_tone(&ogg).is_ok(),
            !cfg!(target_os = "windows"),
            "en Windows solo WAV"
        );
    }

    /// Un WAV solo se carga si empieza como tal y no pasa del tope; lo demás se descarta mirando la
    /// cabecera y el tamaño (ficheros dispersos: no ocupan disco aunque digan medir mucho).
    #[test]
    fn custom_files_are_checked_before_being_read() {
        let dir = tempfile::tempdir().expect("directorio");
        let wav = dir.path().join("tono.wav");
        std::fs::write(&wav, Builtin::Chime.wav()).expect("escribir");
        assert!(read_wav(&wav).is_ok());

        let huge_ogg = dir.path().join("enorme.ogg");
        std::fs::write(&huge_ogg, b"OggS\0\x02\0\0\0\0\0\0").expect("escribir");
        std::fs::File::options()
            .write(true)
            .open(&huge_ogg)
            .and_then(|file| file.set_len(MAX_WAV_BYTES * 4))
            .expect("agrandar");
        assert_eq!(read_wav(&huge_ogg), Err(WavError::NotWav), "otro formato");

        let huge_wav = dir.path().join("enorme.wav");
        std::fs::write(&huge_wav, b"RIFF\xff\xff\xff\xffWAVE").expect("escribir");
        std::fs::File::options()
            .write(true)
            .open(&huge_wav)
            .and_then(|file| file.set_len(MAX_WAV_BYTES + 1))
            .expect("agrandar");
        assert_eq!(read_wav(&huge_wav), Err(WavError::TooLarge));
        let error = check_tone(&huge_wav).expect_err("demasiado grande");
        assert!(error.contains("16 MiB"), "{error}");

        let short = dir.path().join("corto.wav");
        std::fs::write(&short, b"RIFF").expect("escribir");
        assert_eq!(read_wav(&short), Err(WavError::NotWav));
        assert!(matches!(
            read_wav(&dir.path().join("no-esta.wav")),
            Err(WavError::Unreadable(_))
        ));
        assert!(matches!(read_wav(dir.path()), Err(WavError::Unreadable(_))));
    }

    /// Lo que vuelve del selector se comprueba allí mismo: cancelar no es un error, el fallo del
    /// selector se explica y un fichero que no vale lleva el motivo.
    #[test]
    fn picked_files_are_checked_in_the_picker_task() {
        assert_eq!(checked(Ok(None)), Ok(None));
        let error = checked(Err("sin portal".into())).expect_err("falló");
        assert!(
            error.contains("sin portal") && error.len() > "sin portal".len(),
            "{error}"
        );
        let dir = tempfile::tempdir().expect("directorio");
        assert!(checked(Ok(Some(dir.path().join("no-esta.wav")))).is_err());
        let wav = dir.path().join("tono.wav");
        std::fs::write(&wav, Builtin::Chime.wav()).expect("escribir");
        assert_eq!(checked(Ok(Some(wav.clone()))), Ok(Some(wav)));
    }

    /// Un WAV con un trozo `fmt ` extensible de 24 bits, otro trozo delante de los datos y bytes
    /// impares: se lee igual.
    #[test]
    fn wavs_of_every_common_format_are_read() {
        let samples = [0i16, 16_384, -16_384, 32_767];
        let pcm16 = tones::wav_pcm16(44_100, 2, &samples);
        let wav = decode_wav(&pcm16).expect("PCM 16");
        assert_eq!((wav.rate, wav.channels), (44_100, 2));
        assert_eq!(wav.samples, [0.0, 0.5, -0.5, 32_767.0 / 32_768.0]);

        let mut extensible = Vec::new();
        let data: Vec<u8> = [0i32, 4_194_304, -4_194_304]
            .iter()
            .flat_map(|s| s.to_le_bytes()[..3].to_vec())
            .collect();
        extensible.extend_from_slice(b"RIFF\0\0\0\0WAVE");
        extensible.extend_from_slice(b"LIST\x03\0\0\0abc\0");
        extensible.extend_from_slice(b"fmt \x28\0\0\0");
        extensible.extend_from_slice(&0xFFFEu16.to_le_bytes());
        extensible.extend_from_slice(&1u16.to_le_bytes());
        extensible.extend_from_slice(&96_000u32.to_le_bytes());
        extensible.extend_from_slice(&(96_000u32 * 3).to_le_bytes());
        extensible.extend_from_slice(&3u16.to_le_bytes());
        extensible.extend_from_slice(&24u16.to_le_bytes());
        extensible.extend_from_slice(&22u16.to_le_bytes());
        extensible.extend_from_slice(&24u16.to_le_bytes());
        extensible.extend_from_slice(&4u32.to_le_bytes());
        extensible.extend_from_slice(&1u16.to_le_bytes()); // subformato PCM
        extensible.extend_from_slice(&[0; 14]);
        extensible.extend_from_slice(b"data");
        extensible.extend_from_slice(&(data.len() as u32).to_le_bytes());
        extensible.extend_from_slice(&data);
        let wav = decode_wav(&extensible).expect("PCM 24 extensible");
        assert_eq!((wav.rate, wav.channels), (96_000, 1));
        assert_eq!(wav.samples, [0.0, 0.5, -0.5]);

        assert!(decode_wav(b"OggS").is_err());
        let mut adpcm = pcm16.clone();
        adpcm[20] = 2; // ADPCM de Microsoft
        assert!(decode_wav(&adpcm).is_err());
    }

    /// A 48 kHz y estéreo, con la duración que tenía; lo que pasa del tope se corta con fundido.
    #[test]
    fn custom_tones_are_resampled_and_capped() {
        let mono = Wav {
            rate: 24_000,
            channels: 1,
            samples: vec![0.5; 24_000],
        };
        let out = mono.to_stereo_48k(MAX_CUSTOM);
        assert_eq!(out.len(), 48_000 * 2, "un segundo, en estéreo");
        assert!(
            out.iter().all(|s| (*s - 0.5).abs() < 1e-6),
            "sin fundido si no se corta"
        );

        let long = Wav {
            rate: 48_000,
            channels: 2,
            samples: vec![0.25; 48_000 * 2 * 10],
        };
        let out = long.to_stereo_48k(MAX_CUSTOM);
        assert_eq!(out.len(), 48_000 * 2 * 3, "como mucho tres segundos");
        assert!(out[out.len() - 1].abs() < 0.01, "acaba en fundido");
        assert_eq!(out[0], 0.25);
    }

    #[test]
    fn playback_follows_the_settings() {
        let mut settings = Settings::default();
        settings.audio.speaker = Some("usb-headset".to_owned());
        let joined = playback(&settings, SoundEvent::MemberJoined).expect("suena");
        assert_eq!(joined.source, Source::Tone(Builtin::Rise));
        assert_eq!(
            joined.device.as_deref(),
            Some("usb-headset"),
            "la salida elegida"
        );
        assert_eq!(joined.gain, settings.notifications.gain());

        settings.notifications.chat_message = EventSound {
            tone: SoundChoice::File,
            file: Some(PathBuf::from("/home/ana/tono.ogg")),
        };
        let chat = playback(&settings, SoundEvent::ChatMessage).expect("suena");
        assert_eq!(
            chat.source,
            Source::File(PathBuf::from("/home/ana/tono.ogg"))
        );
        assert_eq!(chat.fallback, Builtin::Chime);

        settings.notifications.member_left.tone = SoundChoice::Off;
        assert_eq!(playback(&settings, SoundEvent::MemberLeft), None);
        settings.notifications.volume = 0;
        assert_eq!(playback(&settings, SoundEvent::MemberJoined), None);
    }
}
