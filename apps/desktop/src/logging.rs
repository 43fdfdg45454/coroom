//! Registro: a la salida de error (`RUST_LOG` elige el nivel; por defecto, avisos y errores) y,
//! en la aplicación y en las órdenes que abren sesiones, también a un fichero en el directorio de
//! datos (`<datos>/logs/coroom.log`), siempre con el nivel `info` de coroom y los avisos del
//! resto. En Windows la ventana no tiene consola: el fichero es lo que se manda al pedir ayuda.
//!
//! Al arrancar, el registro anterior pasa a `coroom.1.log` (se guarda una ejecución); si el de
//! ahora pasa de [`MAX_LOG_BYTES`], se rota igual y sigue en uno nuevo.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, fmt};

/// Tamaño a partir del cual se rota el registro.
pub const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;

/// Niveles del fichero: `info` de coroom, avisos del resto.
pub const FILE_FILTER: &str = "warn,coroom=info";

/// Directorio de los registros (`<datos>/logs`).
pub fn log_dir() -> PathBuf {
    crate::remote::default_data_dir().join("logs")
}

/// El registro de esta ejecución.
pub fn log_path() -> PathBuf {
    log_dir().join("coroom.log")
}

/// El de la ejecución anterior.
fn previous_path(path: &Path) -> PathBuf {
    path.with_file_name("coroom.1.log")
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
        // En Windows `rename` no sustituye un fichero que ya existe.
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

/// Arranca el registro. Con `to_file`, también al fichero de [`log_path`] (si no se puede abrir,
/// se avisa por la salida de error y se sigue sin él). Devuelve la ruta del fichero en uso.
pub fn init(to_file: bool) -> Option<PathBuf> {
    use std::io::IsTerminal;
    let stderr_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let stderr = fmt::layer()
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .with_filter(stderr_filter);
    let path = log_path();
    let file = to_file
        .then(|| match LogFile::create(&path, MAX_LOG_BYTES) {
            Ok(file) => Some(file),
            Err(err) => {
                eprintln!(
                    "Aviso: no se pudo abrir el registro {}: {err}",
                    path.display()
                );
                None
            }
        })
        .flatten();
    let opened = file.is_some();
    let file = file.map(|file| {
        fmt::layer()
            .with_writer(file)
            .with_ansi(false)
            .with_thread_names(true)
            .with_filter(EnvFilter::new(FILE_FILTER))
    });
    let _ = tracing_subscriber::registry()
        .with(stderr)
        .with(file)
        .try_init();
    if !opened {
        return None;
    }
    log_panics();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        // De dónde se cargó FFmpeg (en el Flatpak, la extensión `codecs-extra` o la Platform):
        // antes del sondeo de codificadores, por si un driver o una biblioteca lo tumba.
        ffmpeg = %coroom_session::codec::ffmpeg::library_dir(),
        "coroom arranca"
    );
    Some(path)
}

/// Los pánicos también al registro (sin consola en Windows, si no, se pierden).
fn log_panics() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        tracing::error!(thread = thread.name().unwrap_or("?"), "pánico: {info}");
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_previous_run_is_kept_and_the_size_is_capped() {
        let dir = tempfile::tempdir().expect("directorio temporal");
        let path = dir.path().join("logs").join("coroom.log");
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
        let line = [b'x'; 40];
        log.make_writer().write_all(&line).expect("escritura");
        log.make_writer().write_all(&line).expect("escritura");
        // Pasó de 64 bytes: la primera línea quedó en el anterior y la segunda en uno nuevo.
        assert_eq!(fs::read(&path).expect("actual").len(), 40);
        assert_eq!(fs::read(previous_path(&path)).expect("anterior").len(), 40);
    }
}
