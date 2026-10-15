//! Sonido de mensaje nuevo del chat y el selector de su tono.
//!
//! - El tono por defecto: en Linux, `message-new-instant` del tema de sonido freedesktop (en los
//!   directorios XDG) y, si el tema no lo tiene, la campanilla propia ([`chime_wav`]) escrita una
//!   vez en el directorio de datos. En Windows, siempre la campanilla, desde memoria (`PlaySound`
//!   de `winmm`). La campanilla se sintetiza: dos notas cortas con envolvente suave, sin fichero
//!   con derechos de nadie.
//! - El tono propio (`room.chat_sound_file`): en Linux lo que `pw-play` o `paplay` sepan leer
//!   (WAV, OGG, FLAC…); en Windows solo WAV, el único formato que `PlaySound` reproduce.
//! - El selector: el portal `FileChooser` en Linux (también dentro de Flatpak) e
//!   `IFileOpenDialog` en Windows.
//!
//! Nada de esto corre en el hilo de la interfaz: [`play`] y [`pick_tone`] se llaman desde una
//! tarea de `Backend::spawn`, y la reproducción es síncrona dentro de ella (así ni el fichero ni
//! la memoria del WAV tienen que sobrevivir a la llamada).

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Frecuencia de muestreo de la campanilla.
pub const CHIME_RATE: u32 = 48_000;
/// Duración de la campanilla (dos notas de 90 ms).
pub const CHIME_DURATION: Duration = Duration::from_millis(180);
/// Nombre del fichero con la campanilla en el directorio de datos (Linux sin tema de sonido).
pub const CHIME_FILE: &str = "chat-message.wav";

/// Las dos notas (Hz): sol₅ y do₆, una cuarta ascendente.
const NOTES: [f64; 2] = [783.99, 1046.50];
/// Ataque de cada nota.
const ATTACK: Duration = Duration::from_millis(4);
/// Constante de tiempo de la caída de cada nota.
const DECAY: Duration = Duration::from_millis(45);
/// Final de cada nota: baja a cero para que la siguiente arranque sin chasquido.
const RELEASE: Duration = Duration::from_millis(8);
/// Pico de amplitud sobre el máximo de 16 bits.
const PEAK: f64 = 0.4;

/// La campanilla como fichero WAV completo (PCM de 16 bits, mono, [`CHIME_RATE`]).
pub fn chime_wav() -> Vec<u8> {
    wav_pcm16(CHIME_RATE, &chime_samples())
}

/// Las muestras de la campanilla: cada nota con un ataque breve y una caída exponencial, y un
/// segundo armónico suave que la hace sonar a campana y no a pitido. Empieza y acaba en cero.
fn chime_samples() -> Vec<i16> {
    let rate = f64::from(CHIME_RATE);
    let total = (CHIME_DURATION.as_secs_f64() * rate).round() as usize;
    let per_note = total / NOTES.len();
    let attack = ATTACK.as_secs_f64();
    let decay = DECAY.as_secs_f64();
    let release = RELEASE.as_secs_f64();
    let note_len = per_note as f64 / rate;
    (0..total)
        .map(|index| {
            let note = (index / per_note).min(NOTES.len() - 1);
            let t = (index - note * per_note) as f64 / rate;
            let envelope = (t / attack).min(1.0)
                * (-(t - attack).max(0.0) / decay).exp()
                * ((note_len - t) / release).clamp(0.0, 1.0);
            let phase = 2.0 * std::f64::consts::PI * NOTES[note] * t;
            let tone = phase.sin() + 0.25 * (2.0 * phase).sin();
            let sample = PEAK * envelope * tone / 1.25;
            (sample * f64::from(i16::MAX)).round() as i16
        })
        .collect()
}

/// Un WAV (RIFF) de PCM de 16 bits mono con las muestras dadas.
fn wav_pcm16(rate: u32, samples: &[i16]) -> Vec<u8> {
    let data_len = u32::try_from(samples.len() * 2).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes()); // bytes por segundo
    out.extend_from_slice(&2u16.to_le_bytes()); // bytes por muestra
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

/// La campanilla como fichero en `dir` (se escribe la primera vez; después solo se comprueba).
pub fn chime_file(dir: &Path) -> Result<PathBuf, String> {
    let path = dir.join(CHIME_FILE);
    if path.is_file() {
        return Ok(path);
    }
    std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    std::fs::write(&path, chime_wav()).map_err(|err| err.to_string())?;
    Ok(path)
}

/// Si `path` sirve como tono: existe, se puede leer y, en Windows, es un WAV.
pub fn check_tone(path: &Path) -> Result<(), String> {
    if cfg!(target_os = "windows")
        && !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
    {
        return Err(t!("settings-tono-solo-wav").to_owned());
    }
    let readable = std::fs::File::open(path)
        .and_then(|file| file.metadata())
        .is_ok_and(|meta| meta.is_file());
    if readable {
        Ok(())
    } else {
        Err(t!("settings-tono-no-se-puede-leer").to_owned())
    }
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

/// Reproduce el tono del chat: `custom` si está y se puede leer, si no el de por defecto. Bloquea
/// mientras suena; se llama desde una tarea del `Backend`, nunca desde la interfaz.
pub fn play(custom: Option<&Path>) {
    let custom = custom.filter(|file| {
        let ok = file.is_file();
        if !ok {
            tracing::debug!(file = %file.display(), "el tono elegido ya no está; suena el de por defecto");
        }
        ok
    });
    #[cfg(unix)]
    play_unix(custom);
    #[cfg(target_os = "windows")]
    play_windows(custom);
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        let _ = custom;
        tracing::debug!("sin reproductor de sonido en este sistema");
    }
}

/// `message-new-instant` del tema de sonido freedesktop, si algún directorio XDG lo tiene.
#[cfg(unix)]
fn theme_sound() -> Option<PathBuf> {
    const SOUND: &str = "sounds/freedesktop/stereo/message-new-instant.oga";
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("XDG_DATA_HOME").filter(|dir| !dir.is_empty()) {
        dirs.push(PathBuf::from(home));
    }
    let shared = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    dirs.extend(shared.split(':').map(PathBuf::from));
    dirs.into_iter()
        .map(|dir| dir.join(SOUND))
        .find(|path| path.is_file())
}

#[cfg(unix)]
fn play_unix(custom: Option<&Path>) {
    use std::process::{Command, Stdio};
    let file = match custom.map(Path::to_path_buf).or_else(theme_sound) {
        Some(file) => file,
        None => match chime_file(&super::settings::data_dir()) {
            Ok(file) => file,
            Err(err) => {
                tracing::debug!(error = %err, "sin tema de sonido y sin poder escribir la campanilla");
                return;
            }
        },
    };
    for player in ["pw-play", "paplay"] {
        let started = Command::new(player)
            .arg(&file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = started {
            let _ = child.wait();
            return;
        }
    }
    tracing::debug!("sin pw-play ni paplay para el sonido del chat");
}

/// `text` en UTF-16 terminado en cero (rutas con cualquier codificación incluidas).
#[cfg(target_os = "windows")]
fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    text.encode_wide().chain(std::iter::once(0)).collect()
}

/// `PlaySound` de `winmm`: el fichero WAV elegido o, si no hay o no se pudo, la campanilla desde
/// memoria. Síncrono (sin `SND_ASYNC`): así ni la ruta ni el WAV tienen que sobrevivir a la
/// llamada, y la tarea que nos llama ya está fuera del hilo de la interfaz.
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
fn play_windows(custom: Option<&Path>) {
    use std::sync::OnceLock;

    use windows::Win32::Media::Audio::{PlaySoundW, SND_FILENAME, SND_MEMORY, SND_NODEFAULT};
    use windows::core::PCWSTR;

    if let Some(file) = custom {
        let name = wide(file.as_os_str());
        // SAFETY: `name` es una cadena UTF-16 terminada en cero que vive hasta que la llamada
        // vuelve; sin `SND_ASYNC`, PlaySound no guarda el puntero.
        let played =
            unsafe { PlaySoundW(PCWSTR(name.as_ptr()), None, SND_FILENAME | SND_NODEFAULT) };
        if played.as_bool() {
            return;
        }
        tracing::debug!(file = %file.display(), "PlaySound no pudo con el tono elegido; suena la campanilla");
    }
    static CHIME: OnceLock<Vec<u8>> = OnceLock::new();
    let bytes = CHIME.get_or_init(chime_wav);
    // SAFETY: con `SND_MEMORY` el puntero señala una imagen WAV completa; el `static` vive todo
    // el proceso y la reproducción es síncrona, así que la memoria no se libera mientras suena.
    let played = unsafe {
        PlaySoundW(
            PCWSTR(bytes.as_ptr().cast()),
            None,
            SND_MEMORY | SND_NODEFAULT,
        )
    };
    if !played.as_bool() {
        tracing::debug!("PlaySound no pudo con la campanilla");
    }
}

/// Abre el selector de ficheros para elegir el tono. `Ok(None)` si el usuario canceló. Bloquea
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

    fn le_u32(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
    }

    fn le_u16(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([bytes[at], bytes[at + 1]])
    }

    #[test]
    fn chime_is_a_valid_wav_of_the_declared_duration() {
        let wav = chime_wav();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(le_u32(&wav, 4) as usize, wav.len() - 8);
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(le_u32(&wav, 16), 16, "fmt de PCM");
        assert_eq!(le_u16(&wav, 20), 1, "PCM");
        assert_eq!(le_u16(&wav, 22), 1, "mono");
        assert_eq!(le_u32(&wav, 24), CHIME_RATE);
        assert_eq!(le_u32(&wav, 28), CHIME_RATE * 2);
        assert_eq!(le_u16(&wav, 32), 2);
        assert_eq!(le_u16(&wav, 34), 16);
        assert_eq!(&wav[36..40], b"data");
        let data_len = le_u32(&wav, 40) as usize;
        assert_eq!(data_len, wav.len() - 44);
        let samples = data_len / 2;
        let seconds = samples as f64 / f64::from(CHIME_RATE);
        assert!(
            (seconds - CHIME_DURATION.as_secs_f64()).abs() < 0.001,
            "{seconds} s"
        );
    }

    #[test]
    fn chime_is_soft_and_free_of_clicks() {
        let samples = chime_samples();
        assert_eq!(samples[0], 0, "empieza en silencio");
        let last = *samples.last().expect("muestras");
        assert!(last.abs() < 50, "acaba en silencio: {last}");
        let peak = samples
            .iter()
            .map(|s| i32::from(*s).abs())
            .max()
            .expect("pico");
        assert!(peak > 8_000, "se oye: pico {peak}");
        assert!(
            peak <= (PEAK * f64::from(i16::MAX)) as i32 + 1,
            "no satura: pico {peak}"
        );
        // Cada nota arranca de cero (sin chasquido entre las dos).
        let second = samples.len() / 2;
        assert!(samples[second].abs() < 50, "{}", samples[second]);
        assert!(samples[second - 1].abs() < 200, "{}", samples[second - 1]);
    }

    #[test]
    fn chime_file_is_written_once() {
        let dir = tempfile::tempdir().expect("directorio");
        let path = chime_file(dir.path()).expect("escribir");
        assert_eq!(path.file_name().and_then(|n| n.to_str()), Some(CHIME_FILE));
        let written = std::fs::read(&path).expect("leer");
        assert_eq!(written, chime_wav());
        std::fs::write(&path, b"otro").expect("sobrescribir");
        chime_file(dir.path()).expect("comprobar");
        assert_eq!(
            std::fs::read(&path).expect("leer"),
            b"otro",
            "no se reescribe"
        );
    }

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
        std::fs::write(&wav, chime_wav()).expect("escribir");
        assert_eq!(check_tone(&wav), Ok(()));
        assert!(check_tone(&dir.path().join("no-esta.wav")).is_err());
        assert!(check_tone(dir.path()).is_err(), "un directorio no vale");
        let ogg = dir.path().join("tono.ogg");
        std::fs::write(&ogg, b"OggS").expect("escribir");
        assert_eq!(
            check_tone(&ogg).is_ok(),
            !cfg!(target_os = "windows"),
            "en Windows solo WAV"
        );
    }
}
