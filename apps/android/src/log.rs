//! Registro: en Android, a logcat con la etiqueta `coroom`; en el equipo de desarrollo (pruebas),
//! a la salida de error. El nivel, con `RUST_LOG` (por defecto `info`).
//!
//! Además, como en el escritorio, a un fichero en el directorio de datos de la aplicación
//! (`<filesDir>/logs/coroom.log`), siempre con el nivel `info` de coroom (las crates del
//! workspace y esta escriben con objetivos que empiezan por `coroom`: `coroom_android::guest`,
//! `coroom_transport::quic`, `coroom::stats`…) y los avisos del resto, con la hora de cada línea.
//! Es lo que el usuario comparte desde Ajustes («Compartir registro»): logcat no se puede leer
//! desde otra aplicación.
//!
//! Al arrancar, el registro anterior pasa a `coroom.1.log` (se guarda una ejecución); si el de
//! ahora pasa de [`MAX_LOG_BYTES`], se rota igual y sigue en uno nuevo.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

/// Tamaño a partir del cual se rota el registro.
pub const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;

/// Niveles del fichero: `info` de coroom, avisos del resto.
pub const FILE_FILTER: &str = "warn,coroom=info";

/// Directorio de los registros (`<datos>/logs`).
pub fn log_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("logs")
}

/// El registro de esta ejecución.
pub fn log_path(data_dir: &Path) -> PathBuf {
    log_dir(data_dir).join("coroom.log")
}

/// El de la ejecución anterior.
fn previous_path(path: &Path) -> PathBuf {
    path.with_file_name("coroom.1.log")
}

/// Los registros que se pueden compartir, el de esta ejecución primero: los que existen y tienen
/// algo escrito.
pub fn files(data_dir: &Path) -> Vec<PathBuf> {
    let current = log_path(data_dir);
    let previous = previous_path(&current);
    [current, previous]
        .into_iter()
        .filter(|path| fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 0))
        .collect()
}

/// Fichero de registro con tope de tamaño.
pub struct LogFile {
    path: PathBuf,
    state: Mutex<(File, u64)>,
    max: u64,
}

impl LogFile {
    /// Abre `path` nuevo (el que había pasa a `coroom.1.log`); rota al pasar de `max` bytes.
    pub fn create(path: &Path, max: u64) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = rotate(path)?;
        Ok(Self {
            path: path.to_owned(),
            state: Mutex::new((file, 0)),
            max,
        })
    }

    fn write_line(&self, buf: &[u8]) -> io::Result<usize> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.1 > 0 && state.1 + buf.len() as u64 > self.max {
            // Si no se puede rotar se sigue en el mismo: mejor largo que perdido.
            if let Ok(file) = rotate(&self.path) {
                *state = (file, 0);
            }
        }
        state.0.write_all(buf)?;
        state.1 += buf.len() as u64;
        Ok(buf.len())
    }
}

/// Pasa `path` a `coroom.1.log` (si existe) y abre uno vacío.
fn rotate(path: &Path) -> io::Result<File> {
    if path.exists() {
        let previous = previous_path(path);
        let _ = fs::remove_file(&previous);
        let _ = fs::rename(path, &previous);
    }
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
}

/// Lo que escribe cada evento en [`LogFile`].
pub struct LogWriter<'a>(&'a LogFile);

impl Write for LogWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write_line(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .0
            .flush()
    }
}

impl<'a> MakeWriter<'a> for LogFile {
    type Writer = LogWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter(self)
    }
}

/// Instala el registro del proceso (solo la primera vez): logcat y el fichero de [`log_path`] en
/// `data_dir` (si no se puede abrir, se avisa por logcat y se sigue sin él). Devuelve la ruta del
/// fichero en uso.
pub fn init(data_dir: &Path) -> Option<PathBuf> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // Logcat pone la hora él mismo.
    let console = fmt::layer().with_ansi(false).without_time();
    #[cfg(target_os = "android")]
    let console = console.with_writer(logcat::Logcat);
    #[cfg(not(target_os = "android"))]
    let console = console.with_writer(io::stderr);
    let console = console.with_filter(filter);

    let path = log_path(data_dir);
    let (file, failed) = match LogFile::create(&path, MAX_LOG_BYTES) {
        Ok(file) => (Some(file), None),
        Err(err) => (None, Some(err)),
    };
    let opened = file.is_some();
    let file = file.map(|file| {
        fmt::layer()
            .with_writer(file)
            .with_ansi(false)
            .with_thread_names(true)
            .with_filter(EnvFilter::new(FILE_FILTER))
    });
    if tracing_subscriber::registry()
        .with(console)
        .with(file)
        .try_init()
        .is_err()
    {
        // Ya había uno (otra llamada): ese sigue.
        return None;
    }
    if let Some(err) = failed {
        tracing::warn!(path = %path.display(), error = %err, "no se pudo abrir el registro");
    }
    if !opened {
        return None;
    }
    log_panics();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        "coroom arranca"
    );
    Some(path)
}

/// Los pánicos también al registro (logcat los corta y no llegan al fichero).
fn log_panics() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        tracing::error!(thread = thread.name().unwrap_or("?"), "pánico: {info}");
        previous(info);
    }));
}

#[cfg(target_os = "android")]
mod logcat {
    //! Escritor de `tracing` hacia logcat: cada evento es una línea con su prioridad.

    #![allow(unsafe_code)]

    use std::ffi::CString;
    use std::io;

    use tracing_subscriber::fmt::MakeWriter;

    /// Etiqueta de logcat.
    const TAG: &std::ffi::CStr = c"coroom";

    /// Fábrica de escritores hacia logcat.
    pub struct Logcat;

    /// Un evento por escribir con su prioridad.
    pub struct Line {
        priority: ndk_sys::android_LogPriority,
        buffer: Vec<u8>,
    }

    impl<'a> MakeWriter<'a> for Logcat {
        type Writer = Line;

        fn make_writer(&'a self) -> Line {
            Line {
                priority: ndk_sys::android_LogPriority::ANDROID_LOG_INFO,
                buffer: Vec::new(),
            }
        }

        fn make_writer_for(&'a self, meta: &tracing::Metadata<'_>) -> Line {
            use ndk_sys::android_LogPriority as P;
            let priority = match *meta.level() {
                tracing::Level::ERROR => P::ANDROID_LOG_ERROR,
                tracing::Level::WARN => P::ANDROID_LOG_WARN,
                tracing::Level::INFO => P::ANDROID_LOG_INFO,
                tracing::Level::DEBUG => P::ANDROID_LOG_DEBUG,
                tracing::Level::TRACE => P::ANDROID_LOG_VERBOSE,
            };
            Line {
                priority,
                buffer: Vec::new(),
            }
        }
    }

    impl io::Write for Line {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.buffer.extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for Line {
        fn drop(&mut self) {
            let text = String::from_utf8_lossy(&self.buffer);
            let text = text.trim_end().replace('\0', " ");
            let Ok(text) = CString::new(text) else {
                return;
            };
            // SAFETY: `TAG` y `text` son cadenas C terminadas en NUL que viven durante la llamada;
            // `__android_log_write` no guarda los punteros.
            unsafe {
                ndk_sys::__android_log_write(self.priority.0 as i32, TAG.as_ptr(), text.as_ptr());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_previous_run_is_kept_and_the_size_is_capped() {
        let dir = tempfile::tempdir().expect("directorio temporal");
        let path = log_path(dir.path());
        assert!(files(dir.path()).is_empty(), "sin registro todavía");
        {
            let log = LogFile::create(&path, 64).expect("registro");
            log.make_writer()
                .write_all(b"primera ejecucion\n")
                .expect("escritura");
        }
        let log = LogFile::create(&path, 64).expect("registro");
        let previous = fs::read_to_string(previous_path(&path)).expect("anterior");
        assert_eq!(previous, "primera ejecucion\n");
        assert_eq!(fs::read_to_string(&path).expect("actual"), "");
        // El de ahora está vacío: solo se comparte el anterior.
        assert_eq!(files(dir.path()), vec![previous_path(&path)]);
        let line = [b'x'; 40];
        log.make_writer().write_all(&line).expect("escritura");
        assert_eq!(files(dir.path()), vec![path.clone(), previous_path(&path)]);
        log.make_writer().write_all(&line).expect("escritura");
        // Pasó de 64 bytes: la primera línea quedó en el anterior y la segunda en uno nuevo.
        assert_eq!(fs::read(&path).expect("actual").len(), 40);
        assert_eq!(fs::read(previous_path(&path)).expect("anterior").len(), 40);
    }

    /// Lo que escriben las crates del workspace (objetivos `coroom_*` y `coroom::*`) llega al
    /// fichero en `info`; lo de otras crates, solo desde los avisos.
    #[test]
    fn the_file_filter_keeps_coroom_info_and_other_warnings() {
        let dir = tempfile::tempdir().expect("directorio temporal");
        let path = log_path(dir.path());
        let file = LogFile::create(&path, MAX_LOG_BYTES).expect("registro");
        let subscriber = tracing_subscriber::registry().with(
            fmt::layer()
                .with_writer(file)
                .with_ansi(false)
                .with_filter(EnvFilter::new(FILE_FILTER)),
        );
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "coroom::stats", "invitado: recibe 60.0 fps");
            tracing::info!(target: "coroom_transport::quic", recv = 1, "búferes del socket UDP");
            tracing::info!(target: "coroom_android::guest", "decodificador de vídeo");
            tracing::info!(target: "quinn::connection", "ruido de otra crate");
            tracing::warn!(target: "quinn::connection", "aviso de otra crate");
            tracing::debug!(target: "coroom_android::app", "operación fallida");
        });
        let text = fs::read_to_string(&path).expect("registro");
        assert!(text.contains("invitado: recibe 60.0 fps"), "{text}");
        assert!(text.contains("búferes del socket UDP"), "{text}");
        assert!(text.contains("decodificador de vídeo"), "{text}");
        assert!(text.contains("aviso de otra crate"), "{text}");
        assert!(!text.contains("ruido de otra crate"), "{text}");
        assert!(!text.contains("operación fallida"), "{text}");
        // Con la hora de cada línea, como en el escritorio (RFC 3339).
        let first = text.lines().next().expect("una línea");
        assert!(
            first.as_bytes().get(4) == Some(&b'-') && first.contains('T'),
            "{first}"
        );
    }
}
