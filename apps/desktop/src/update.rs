//! Versiones nuevas en el escritorio: la parte de cada sistema alrededor de
//! [`coroom_session::update`] (la búsqueda en GitHub, los paquetes, la descarga y las reglas de
//! cuándo descargar y reiniciar, que comparte con Android).
//!
//! Cómo se instala ([`channel`]):
//!
//! - **Windows, zip portátil**: baja `coroom-X.Y.Z-windows-x64.zip` a la carpeta de datos
//!   (`updates/`), comprueba su SHA-256 contra el que publica GitHub y lo descomprime al lado del
//!   ejecutable, en `.coroom-update/` ([`zip::stage`]), con cada fichero escrito a disco antes de
//!   apuntar la versión como preparada. Al reiniciar (o al arrancar la próxima vez, antes que nada)
//!   cambia los ficheros ([`zip::swap_in`]): apunta antes en un diario (`.coroom-update.journal`)
//!   qué va a mover, renombra los de ahora a `*.old` (Windows deja renombrar un ejecutable o una DLL
//!   en uso), mueve los nuevos y abre el ejecutable nuevo; si algo falla, deshace los cambios. Si
//!   el proceso muere a mitad, el arranque siguiente termina el cambio desde el diario (o lo
//!   deshace). Solo se borran los `*.old` que apunta el diario. Una versión que no se pudo instalar
//!   no se vuelve a bajar sola ([`blocked_install`]).
//! - **Windows, MSIX**: solo el aviso (cada versión va firmada con un certificado nuevo).
//! - **Linux, Flatpak**: el portal de Flatpak (`UpdateMonitor.Update`, con su progreso) instala la
//!   versión nueva desde el repositorio de la instalación, y «Reiniciar y actualizar» abre la
//!   nueva con `Spawn` y la marca de la última versión. Un Flatpak instalado de un fichero, sin
//!   repositorio, solo avisa.
//! - **Compilado a mano**: solo el aviso.
//!
//! Varios procesos de coroom a la vez (la ventana, `coroom host`, `coroom doctor`…) se coordinan
//! con ficheros bloqueados ([`locks`]): cada proceso del zip tiene un bloqueo compartido de su
//! carpeta y los ficheros solo se cambian con uno exclusivo (ningún otro coroom abierto); una sola
//! descarga a la vez por carpeta de datos; y una sala abierta en cualquier proceso pausa la
//! descarga de los demás.
//!
//! `COROOM_NO_UPDATE_CHECK` (cualquier valor salvo `0`) lo apaga todo, por ejemplo en paquetes que
//! se actualizan por su propio canal.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

pub use coroom_session::update::{
    Channel, Command, DownloadControl, Failure, HomeNotice, NoticeOnly, Package, Progress,
    ProtocolGap, RELEASES_PAGE, REPOSITORY, Release, Stage, Updater, Wait, check, due,
    protocol_gap, unix_now,
};

/// Variable de entorno que desactiva la búsqueda (y con ella la actualización).
pub const DISABLE_ENV: &str = "COROOM_NO_UPDATE_CHECK";

/// Intervalos que se ofrecen en Ajustes, en horas (un día, una semana).
pub const INTERVAL_CHOICES: [u32; 2] = [24, 168];

/// Esta versión.
const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Cuánto espera el arranque a que otro proceso suelte el bloqueo de la instalación.
const STARTUP_LOCK_WAIT: Duration = Duration::from_secs(5);

/// Cada cuánto mira la descarga si otro proceso de coroom abrió una sala.
const ROOM_POLL: Duration = Duration::from_secs(2);

/// Si `COROOM_NO_UPDATE_CHECK` desactiva la búsqueda.
pub fn disabled_by_env() -> bool {
    std::env::var_os(DISABLE_ENV).is_some_and(|value| value != "0" && !value.is_empty())
}

/// Cómo se actualiza esta copia.
pub fn channel() -> Channel {
    #[cfg(target_os = "windows")]
    {
        if windows_package::is_msix() {
            return Channel::Notice(NoticeOnly::Msix);
        }
        if zip::is_portable() {
            return Channel::Package(Package::WindowsZip);
        }
    }
    #[cfg(target_os = "linux")]
    {
        if crate::portal::in_flatpak() {
            return Channel::Flatpak;
        }
    }
    Channel::Notice(NoticeOnly::Unpackaged)
}

/// Carpeta de las descargas de versiones nuevas, en la de datos.
pub fn downloads_dir() -> PathBuf {
    crate::remote::default_data_dir().join("updates")
}

/// Carpeta del ejecutable.
pub fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
}

/// Nombre del ejecutable (`coroom.exe`, o el que le haya puesto el usuario).
fn exe_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| zip::EXE_IN_ZIP.to_owned())
}

/// Nota de un fallo al instalar en el arranque o al reiniciar, para enseñarlo en la aplicación.
fn failure_note() -> PathBuf {
    downloads_dir().join("fallo.txt")
}

/// La versión que no se pudo instalar y por qué (primera línea, la versión; el resto, el motivo).
fn blocked_note() -> PathBuf {
    downloads_dir().join("no-instalar.txt")
}

/// Lo que falló al instalar la versión nueva la vez anterior (y se borra la nota).
pub fn take_install_failure() -> Option<String> {
    let path = failure_note();
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn note_install_failure(err: &str) {
    let path = failure_note();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, err);
}

/// La versión que no se pudo instalar (más nueva que esta) y por qué: no se vuelve a bajar sola
/// ([`Updater::block`]). Una nota de una versión que ya no es más nueva se borra.
pub fn blocked_install() -> Option<(String, String)> {
    let path = blocked_note();
    let text = std::fs::read_to_string(&path).ok()?;
    let (version, reason) = text.split_once('\n').unwrap_or((text.as_str(), ""));
    let newer = coroom_session::update::Version::parse(version)
        .zip(coroom_session::update::Version::parse(CURRENT))
        .is_some_and(|(blocked, current)| blocked > current);
    if !newer {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    Some((version.trim().to_owned(), reason.trim().to_owned()))
}

/// Apunta que `version` no se pudo instalar por `err` (y lo enseña al volver).
fn block_install(version: &str, err: &str) {
    note_install_failure(err);
    let _ = std::fs::write(blocked_note(), format!("{version}\n{err}"));
}

#[cfg(target_os = "windows")]
mod windows_package {
    #![allow(unsafe_code)]

    use windows::Win32::Foundation::{APPMODEL_ERROR_NO_PACKAGE, ERROR_INSUFFICIENT_BUFFER};
    use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;

    /// Si el proceso tiene identidad de paquete (corre instalado como MSIX).
    pub fn is_msix() -> bool {
        let mut length = 0u32;
        // SAFETY: con un búfer nulo, `GetCurrentPackageFullName` solo escribe en `length` (que
        // vive durante la llamada) cuánto mediría el nombre; no lee ni escribe nada más.
        let result = unsafe { GetCurrentPackageFullName(&mut length, None) };
        if result == APPMODEL_ERROR_NO_PACKAGE {
            return false;
        }
        // Con paquete responde que el búfer (nulo) no alcanza.
        result == ERROR_INSUFFICIENT_BUFFER
    }
}

// --- Bloqueos entre procesos ---------------------------------------------------------------------

/// Bloqueos entre procesos de coroom, con ficheros (`flock` en Linux, `LockFileEx` en Windows, por
/// `std::fs::File::try_lock`): el sistema los suelta solo si el proceso muere.
pub mod locks {
    use std::fs::{File, OpenOptions, TryLockError};
    use std::path::Path;
    use std::sync::{Mutex, PoisonError};
    use std::time::{Duration, Instant};

    /// En la carpeta del ejecutable: cada proceso del zip lo tiene compartido mientras corre; los
    /// ficheros solo se cambian con él exclusivo (no hay otro coroom abierto de esta carpeta).
    pub const INSTANCE: &str = ".coroom-instance.lock";
    /// En la carpeta del ejecutable: exclusivo mientras se prepara o se instala una versión.
    pub const INSTALL: &str = ".coroom-update.lock";
    /// En la de descargas: exclusivo mientras se descarga (una descarga a la vez).
    pub const DOWNLOAD: &str = "descarga.lock";
    /// En la de descargas: compartido mientras un proceso tiene una sala abierta.
    pub const ROOM: &str = "sala.lock";

    fn open(path: &Path) -> Option<File> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .ok()
    }

    /// El bloqueo exclusivo de `path`, si nadie más lo tiene.
    pub fn try_exclusive(path: &Path) -> Option<File> {
        let file = open(path)?;
        file.try_lock().ok().map(|()| file)
    }

    /// El bloqueo compartido de `path`, si nadie lo tiene exclusivo.
    pub fn try_shared(path: &Path) -> Option<File> {
        let file = open(path)?;
        file.try_lock_shared().ok().map(|()| file)
    }

    /// Como [`try_exclusive`], reintentando hasta `within`.
    pub fn wait_exclusive(path: &Path, within: Duration) -> Option<File> {
        let deadline = Instant::now() + within;
        loop {
            if let Some(file) = try_exclusive(path) {
                return Some(file);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Si otro (otro proceso, u otro fichero abierto de este) tiene bloqueado `path`.
    pub fn held_elsewhere(path: &Path) -> bool {
        let Some(file) = open(path) else {
            return false;
        };
        matches!(file.try_lock(), Err(TryLockError::WouldBlock))
    }

    /// El bloqueo compartido de la carpeta de este proceso ([`INSTANCE`]), mientras corre.
    static INSTANCE_LOCK: Mutex<Option<File>> = Mutex::new(None);

    /// Toma (o vuelve a tomar) el bloqueo compartido de esta copia en `dir`.
    pub fn hold_instance(dir: &Path) {
        let file = try_shared(&dir.join(INSTANCE));
        *INSTANCE_LOCK.lock().unwrap_or_else(PoisonError::into_inner) = file;
    }

    /// Suelta el bloqueo compartido de esta copia (para instalar al cerrar).
    pub fn release_instance() {
        INSTANCE_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }
}

/// Mientras vive, este proceso tiene una sala abierta para los demás procesos de coroom: sus
/// descargas de la versión nueva esperan (`coroom host` y `coroom join`).
#[derive(Debug)]
pub struct RoomGuard(#[allow(dead_code)] Option<std::fs::File>);

impl RoomGuard {
    /// Avisa de la sala (si se puede escribir en la carpeta de datos).
    pub fn hold() -> Self {
        Self(locks::try_shared(&downloads_dir().join(locks::ROOM)))
    }
}

// --- Reinicio ----------------------------------------------------------------------------------

/// Se pidió «Reiniciar y actualizar»: al cerrarse la ventana, [`finish_restart`] abre la nueva.
static RESTART: AtomicBool = AtomicBool::new(false);

/// Pide abrir la versión nueva al cerrar la aplicación.
pub fn request_restart() {
    RESTART.store(true, Ordering::Release);
}

/// Si se pidió reiniciar.
pub fn restart_requested() -> bool {
    RESTART.load(Ordering::Acquire)
}

/// Por qué no se abrió la versión nueva al reiniciar.
enum RestartError {
    /// Hay otro coroom abierto de esta carpeta: lo preparado se queda para la próxima vez.
    Busy,
    /// No se pudo instalar esta versión: por qué.
    Failed {
        /// La versión.
        version: Option<String>,
        /// Por qué.
        err: String,
    },
}

/// Tras cerrar la ventana: si se pidió reiniciar, instala la versión preparada (zip) y abre la
/// nueva, o se la pide al portal de Flatpak. Un fallo se apunta para enseñarlo al volver, y se
/// vuelve a abrir esta versión.
pub fn finish_restart() {
    if !restart_requested() {
        return;
    }
    match channel() {
        Channel::Package(Package::WindowsZip) => {
            let err = match restart_zip() {
                Ok(()) => return,
                Err(RestartError::Busy) => {
                    "hay otro coroom abierto de esta carpeta; la versión nueva se usa la próxima vez \
                     que abras coroom sin otro abierto"
                        .to_owned()
                }
                Err(RestartError::Failed { version, err }) => {
                    if let Some(version) = version {
                        block_install(&version, &err);
                    }
                    if let Some(dir) = exe_dir() {
                        zip::discard_staged(&dir);
                    }
                    err
                }
            };
            tracing::warn!(error = %err, "no se pudo abrir la versión nueva");
            note_install_failure(&err);
            // Que no se quede sin aplicación: se vuelve a abrir esta.
            if let Ok(exe) = std::env::current_exe() {
                let _ = std::process::Command::new(exe).spawn();
            }
        }
        #[cfg(target_os = "linux")]
        Channel::Flatpak => {
            if let Err(err) = flatpak::spawn(true) {
                tracing::warn!(error = %err, "no se pudo abrir la versión nueva");
                note_install_failure(&err);
                // Dentro del sandbox, un hijo moriría con este proceso: también por el portal.
                if let Err(err) = flatpak::spawn(false) {
                    tracing::warn!(error = %err, "no se pudo volver a abrir coroom");
                }
            }
        }
        _ => {}
    }
}

/// «Reiniciar y actualizar» del zip: con la carpeta para este proceso solo, instala y abre la
/// nueva.
fn restart_zip() -> Result<(), RestartError> {
    let failed = |err: String| RestartError::Failed { version: None, err };
    let exe = std::env::current_exe().map_err(|err| failed(err.to_string()))?;
    let dir = exe
        .parent()
        .ok_or_else(|| failed("no se sabe en qué carpeta está coroom".to_owned()))?
        .to_path_buf();
    locks::release_instance();
    let Some(install) = locks::wait_exclusive(&dir.join(locks::INSTALL), STARTUP_LOCK_WAIT) else {
        return Err(RestartError::Busy);
    };
    let Some(alone) = locks::try_exclusive(&dir.join(locks::INSTANCE)) else {
        return Err(RestartError::Busy);
    };
    let Some(version) = zip::staged_version(&dir) else {
        return Err(failed("no hay ninguna versión preparada".to_owned()));
    };
    zip::apply_staged(&dir).map_err(|err| RestartError::Failed {
        version: Some(version),
        err,
    })?;
    drop(alone);
    drop(install);
    std::process::Command::new(&exe)
        .spawn()
        .map(|_| ())
        .map_err(|err| failed(format!("no se pudo abrir la versión nueva: {err}")))
}

/// Al arrancar, antes que nada (zip portátil): si no hay otro coroom abierto de esta carpeta,
/// termina (o deshace) un cambio de ficheros que quedó a medias, borra los `*.old` del diario y,
/// si hay una versión preparada más nueva, la instala y la abre con los mismos argumentos.
/// Devuelve el código de salida de la nueva si la abrió (este proceso tiene que terminar con él).
pub fn on_startup() -> Option<i32> {
    if channel() != Channel::Package(Package::WindowsZip) {
        return None;
    }
    let dir = exe_dir()?;
    if disabled_by_env() {
        // No instala, pero que otro proceso no le cambie los ficheros mientras corre.
        locks::hold_instance(&dir);
        return None;
    }
    let relaunch = {
        let install = locks::wait_exclusive(&dir.join(locks::INSTALL), STARTUP_LOCK_WAIT);
        let alone = install
            .as_ref()
            .and_then(|_| locks::try_exclusive(&dir.join(locks::INSTANCE)));
        let relaunch = alone.as_ref().is_some_and(|_| install_at_startup(&dir));
        drop(alone);
        // Compartido antes de soltar el de la instalación: nadie cambia los ficheros mientras
        // este proceso corre.
        locks::hold_instance(&dir);
        relaunch
    };
    if !relaunch {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    match std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .status()
    {
        Ok(status) => Some(status.code().unwrap_or(1)),
        Err(err) => {
            tracing::warn!(error = %err, "no se pudo abrir la versión nueva");
            None
        }
    }
}

/// Lo del arranque con la carpeta para este proceso solo: si hay que abrir la versión nueva (los
/// ficheros ya son los suyos y este proceso es el viejo).
fn install_at_startup(dir: &Path) -> bool {
    let Some(current) = coroom_session::update::Version::parse(CURRENT) else {
        return false;
    };
    let newer = |version: &str| {
        coroom_session::update::Version::parse(version).is_some_and(|v| v > current)
    };
    let mut relaunch = false;
    match zip::recover(dir) {
        zip::Recovery::Nothing => {}
        zip::Recovery::Finished(version) => relaunch = newer(&version),
        zip::Recovery::RolledBack { version, err } => {
            tracing::warn!(error = %err, "se deshizo una instalación a medias");
            block_install(&version, &err);
        }
    }
    let Some(staged) = zip::staged_version(dir) else {
        return relaunch;
    };
    if !newer(&staged) {
        zip::discard_staged(dir);
        return relaunch;
    }
    match zip::apply_staged(dir) {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!(error = %err, "no se pudo instalar la versión preparada");
            zip::discard_staged(dir);
            block_install(&staged, &err);
            relaunch
        }
    }
}

// --- El trabajo de cada orden --------------------------------------------------------------------

/// Lo que avisa una descarga, con su generación ([`Command::Download`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// La generación de la descarga que avisa.
    pub generation: u64,
    /// Qué.
    pub kind: ReportKind,
}

/// Qué avisa la descarga.
#[derive(Debug, Clone, PartialEq)]
pub enum ReportKind {
    /// Avanzó.
    Progress(Progress),
    /// Se detuvo por la pausa.
    Paused(Progress),
    /// Espera porque otro proceso de coroom tiene una sala abierta.
    Held(Progress),
    /// Sigue: el otro proceso cerró su sala.
    Resumed,
    /// Terminó: lista, o por qué no.
    Finished(Result<(), Failure>),
}

impl Report {
    /// Lo aplica a `updater` (lo de otra generación no cambia nada); devuelve las órdenes nuevas.
    pub fn apply(self, updater: &mut Updater) -> Vec<Command> {
        let generation = self.generation;
        match self.kind {
            ReportKind::Progress(progress) => updater.progress(generation, progress),
            ReportKind::Paused(progress) => updater.paused(generation, progress),
            ReportKind::Held(progress) => updater.held(generation, progress),
            ReportKind::Resumed => updater.resumed(generation),
            ReportKind::Finished(result) => return updater.finished(generation, result),
        }
        Vec::new()
    }
}

/// Ejecuta las órdenes de [`Updater`] que tocan la red o el disco (las descargas, en un hilo).
///
/// Los trabajos van en orden por un solo hilo (`coroom-version-nueva`): una descarga no empieza
/// hasta que la anterior (ya en pausa) suelta los ficheros, y un descarte borra antes de que empiece
/// la descarga que lo sigue. Lo que avisa una descarga reemplazada o descartada no se envía, y si
/// llega tarde lo ignora [`Updater`] por su generación.
#[derive(Debug, Default)]
pub struct Worker {
    running: Mutex<Option<Arc<DownloadControl>>>,
    /// La generación de la última descarga (0 tras un descarte).
    current: Arc<AtomicU64>,
    jobs: Mutex<Option<std::sync::mpsc::Sender<Job>>>,
    /// El aviso de sala abierta para los demás procesos ([`locks::ROOM`]).
    room: Mutex<Option<RoomGuard>>,
}

type Job = Box<dyn FnOnce() + Send>;

impl Worker {
    /// Ejecuta `command`; `report` recibe el avance y el final desde el hilo de la descarga.
    pub fn execute(
        &self,
        command: Command,
        channel: Channel,
        report: impl Fn(Report) + Send + Sync + 'static,
    ) {
        match command {
            Command::Download {
                release,
                generation,
            } => {
                let control = Arc::new(DownloadControl::new());
                let previous = self
                    .running
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .replace(Arc::clone(&control));
                if let Some(previous) = previous {
                    previous.pause();
                }
                self.current.store(generation, Ordering::Release);
                let current = Arc::clone(&self.current);
                self.queue(Box::new(move || {
                    let live = || current.load(Ordering::Acquire) == generation;
                    if !live() {
                        return;
                    }
                    let forward = |kind: ReportKind| {
                        if live() {
                            report(Report { generation, kind });
                        }
                    };
                    let end = run_job(&release, channel, &control, &forward);
                    forward(end);
                }));
            }
            Command::Pause => self.pause(),
            Command::Discard => {
                self.pause();
                self.current.store(0, Ordering::Release);
                if channel == Channel::Package(Package::WindowsZip) {
                    self.queue(Box::new(discard_downloads));
                }
            }
            // Lo hace la ventana al cerrarse ([`finish_restart`]).
            Command::Restart => request_restart(),
        }
    }

    /// Hay (o ya no hay) una sala abierta en esta aplicación: se avisa a los demás procesos.
    pub fn set_room_open(&self, open: bool) {
        let mut room = self.room.lock().unwrap_or_else(PoisonError::into_inner);
        if room.is_some() == open {
            return;
        }
        *room = open.then(RoomGuard::hold);
    }

    /// Pone `job` en la cola del hilo (que arranca la primera vez).
    fn queue(&self, job: Job) {
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        let job = match jobs.as_ref() {
            Some(sender) => match sender.send(job) {
                Ok(()) => return,
                // El hilo se fue (un pánico): se arranca otro.
                Err(std::sync::mpsc::SendError(job)) => job,
            },
            None => job,
        };
        let (sender, receiver) = std::sync::mpsc::channel::<Job>();
        let spawned = std::thread::Builder::new()
            .name("coroom-version-nueva".to_owned())
            .spawn(move || {
                for job in receiver {
                    job();
                }
            });
        match spawned {
            Ok(_) => {
                let _ = sender.send(job);
                *jobs = Some(sender);
            }
            Err(err) => tracing::warn!(error = %err, "no arrancó el hilo de la versión nueva"),
        }
    }

    fn pause(&self) {
        if let Some(control) = self
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            control.pause();
        }
    }
}

/// Borra lo bajado y lo preparado (si ningún otro proceso lo está usando).
fn discard_downloads() {
    let downloads = downloads_dir();
    if let Some(_lock) = locks::try_exclusive(&downloads.join(locks::DOWNLOAD))
        && let Ok(entries) = std::fs::read_dir(&downloads)
    {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("coroom-") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    if let Some(dir) = exe_dir()
        && let Some(_lock) = locks::try_exclusive(&dir.join(locks::INSTALL))
    {
        zip::discard_staged(&dir);
    }
}

/// Una descarga entera: una a la vez por carpeta de datos, y en pausa mientras otro proceso de
/// coroom tenga una sala abierta.
fn run_job(
    release: &Release,
    channel: Channel,
    control: &DownloadControl,
    forward: &dyn Fn(ReportKind),
) -> ReportKind {
    let downloads = downloads_dir();
    // Otro proceso ya descarga: no es un error, se vuelve a mirar en una hora.
    let Some(_download) = locks::try_exclusive(&downloads.join(locks::DOWNLOAD)) else {
        return ReportKind::Finished(Err(Failure::Later));
    };
    let room = downloads.join(locks::ROOM);
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            return ReportKind::Finished(Err(Failure::Failed(format!(
                "no se pudo preparar la descarga: {err}"
            ))));
        }
    };
    let last = std::cell::Cell::new(Progress::default());
    let track = |kind: ReportKind| {
        if let ReportKind::Progress(progress) = &kind {
            last.set(*progress);
        }
        forward(kind);
    };
    loop {
        if locks::held_elsewhere(&room) {
            forward(ReportKind::Held(last.get()));
            while locks::held_elsewhere(&room) {
                if control.is_paused() {
                    return ReportKind::Paused(last.get());
                }
                let _ = runtime.block_on(tokio::time::timeout(ROOM_POLL, control.wait_paused()));
            }
            if control.is_paused() {
                return ReportKind::Paused(last.get());
            }
            forward(ReportKind::Resumed);
        }
        // Esta vuelta se para con la pausa o si otro proceso abre una sala.
        let attempt = DownloadControl::new();
        let stop = DownloadControl::new();
        let held = AtomicBool::new(false);
        let end = std::thread::scope(|scope| {
            scope.spawn(|| {
                let Ok(watch) = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                else {
                    return;
                };
                loop {
                    let tick = watch.block_on(async {
                        tokio::select! {
                            () = control.wait_paused() => Some(true),
                            () = stop.wait_paused() => Some(false),
                            () = tokio::time::sleep(ROOM_POLL) => None,
                        }
                    });
                    match tick {
                        Some(true) => {
                            attempt.pause();
                            return;
                        }
                        Some(false) => return,
                        None if locks::held_elsewhere(&room) => {
                            held.store(true, Ordering::Release);
                            attempt.pause();
                            return;
                        }
                        None => {}
                    }
                }
            });
            let end = run_download(release, channel, &attempt, &track);
            stop.pause();
            end
        });
        match end {
            ReportKind::Paused(progress)
                if held.load(Ordering::Acquire) && !control.is_paused() =>
            {
                last.set(progress);
            }
            end => return end,
        }
    }
}

fn run_download(
    release: &Release,
    channel: Channel,
    control: &DownloadControl,
    report: &dyn Fn(ReportKind),
) -> ReportKind {
    match channel {
        Channel::Package(Package::WindowsZip) => zip::download_and_stage(release, control, report),
        #[cfg(target_os = "linux")]
        Channel::Flatpak => flatpak::update(control, report),
        _ => ReportKind::Finished(Err(Failure::Failed(
            "esta copia no se actualiza sola".to_owned(),
        ))),
    }
}

/// Abre `url` en el navegador: el portal `OpenURI` en Linux (funciona dentro de Flatpak), con
/// `xdg-open` de respaldo; el explorador en Windows.
pub fn open_url(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err("solo se abren enlaces https".to_owned());
    }
    open(url)
}

/// Abre una carpeta en el gestor de archivos («Abrir la carpeta» cuando no se puede actualizar en
/// ella).
pub fn open_folder(path: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|err| format!("no se pudo abrir la carpeta: {err}"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|err| format!("no se pudo abrir la carpeta: {err}"))
    }
}

fn open(url: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let portal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| err.to_string())
            .and_then(|runtime| {
                runtime.block_on(async {
                    let uri = ashpd::Uri::parse(url).map_err(|err| err.to_string())?;
                    ashpd::desktop::open_uri::OpenFileRequest::default()
                        .send_uri(&uri)
                        .await
                        .map(|_| ())
                        .map_err(|err| err.to_string())
                })
            });
        match portal {
            Ok(()) => Ok(()),
            Err(err) => {
                tracing::debug!(error = %err, "OpenURI no respondió; se prueba xdg-open");
                std::process::Command::new("xdg-open")
                    .arg(url)
                    .spawn()
                    .map(|_| ())
                    .map_err(|err| format!("no se pudo abrir el navegador: {err}"))
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|err| format!("no se pudo abrir el navegador: {err}"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err(format!("abre {url} en el navegador"))
    }
}

// --- Flatpak -------------------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod flatpak {
    use std::collections::HashMap;
    use std::time::Duration;

    use ashpd::flatpak::update_monitor::UpdateStatus;
    use ashpd::flatpak::{Flatpak, SpawnFlags, SpawnOptions};
    use futures_util::StreamExt;

    use super::{DownloadControl, Failure, Progress, ReportKind};

    /// Cuánto se espera cada llamada al portal.
    const CALL_TIMEOUT: Duration = Duration::from_secs(30);

    /// Sin noticias del portal en este tiempo, se da la instalación por colgada (se puede
    /// reintentar).
    const STALL_TIMEOUT: Duration = Duration::from_secs(5 * 60);

    fn portal_error(err: impl std::fmt::Display) -> ReportKind {
        ReportKind::Finished(Err(Failure::Failed(format!(
            "el portal de Flatpak no respondió: {err}"
        ))))
    }

    /// `call`, o un error si el portal no contesta en [`CALL_TIMEOUT`].
    async fn timed<T, E: std::fmt::Display>(
        call: impl std::future::Future<Output = Result<T, E>>,
    ) -> Result<T, ReportKind> {
        match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(err)) => Err(portal_error(err)),
            Err(_) => Err(portal_error("no contestó a tiempo")),
        }
    }

    /// Si el fallo del portal dice que esta instalación no tiene un repositorio del que
    /// actualizarse (un Flatpak instalado de un fichero `.flatpak` sin repositorio).
    pub fn without_repository(error_name: Option<&str>, message: Option<&str>) -> bool {
        error_name.is_some_and(|name| name.ends_with(".RemoteNotFound"))
            || message.is_some_and(|message| message.contains("disabled remote"))
    }

    /// Instala la versión nueva con el portal (bloquea): `UpdateMonitor.Update` y su progreso. La
    /// pausa cierra el monitor, que cancela la instalación; lo bajado se queda en el repositorio y
    /// la siguiente vez sigue desde ahí.
    pub fn update(control: &DownloadControl, report: &dyn Fn(ReportKind)) -> ReportKind {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(err) => return portal_error(err),
        };
        runtime.block_on(async {
            let proxy = match timed(Flatpak::new()).await {
                Ok(proxy) => proxy,
                Err(end) => return end,
            };
            let monitor = match timed(proxy.create_update_monitor(Default::default())).await {
                Ok(monitor) => monitor,
                Err(end) => return end,
            };
            let end = watch(&monitor, control, report).await;
            let _ = tokio::time::timeout(CALL_TIMEOUT, monitor.close()).await;
            end
        })
    }

    async fn watch(
        monitor: &ashpd::flatpak::update_monitor::UpdateMonitor,
        control: &DownloadControl,
        report: &dyn Fn(ReportKind),
    ) -> ReportKind {
        let mut stream = match timed(monitor.receive_progress()).await {
            Ok(stream) => Box::pin(stream),
            Err(end) => return end,
        };
        if let Err(end) = timed(monitor.update(None, Default::default())).await {
            return end;
        }
        let mut last = Progress::default();
        loop {
            tokio::select! {
                () = control.wait_paused() => return ReportKind::Paused(last),
                progress = tokio::time::timeout(STALL_TIMEOUT, stream.next()) => {
                    let progress = match progress {
                        Err(_) => return ReportKind::Finished(Err(Failure::Failed(
                            "el portal de Flatpak dejó de dar noticias de la instalación".to_owned(),
                        ))),
                        Ok(None) => return portal_error("el monitor se cerró"),
                        Ok(Some(progress)) => progress,
                    };
                    match progress.status() {
                        Some(UpdateStatus::Done) => return ReportKind::Finished(Ok(())),
                        // El repositorio todavía no tiene la versión nueva (se publica después
                        // que la versión en GitHub): no es un error.
                        Some(UpdateStatus::Empty) => {
                            return ReportKind::Finished(Err(Failure::Later));
                        }
                        Some(UpdateStatus::Failed) => {
                            if without_repository(progress.error(), progress.error_message()) {
                                return ReportKind::Finished(Err(Failure::Unsupported));
                            }
                            let why = progress
                                .error_message()
                                .or(progress.error())
                                .unwrap_or("sin motivo");
                            return ReportKind::Finished(Err(Failure::Failed(format!(
                                "Flatpak no pudo instalarla: {why}"
                            ))));
                        }
                        Some(UpdateStatus::Running) | None => {
                            let ops = progress.n_ops().unwrap_or(1).max(1) as f32;
                            let op = progress.op().unwrap_or(0) as f32;
                            let part = progress.progress().unwrap_or(0) as f32 / 100.0;
                            last = Progress {
                                fraction: ((op + part) / ops).clamp(0.0, 1.0),
                                total_bytes: None,
                            };
                            report(ReportKind::Progress(last));
                        }
                    }
                }
            }
        }
    }

    /// Abre coroom de nuevo con `Spawn` del portal (fuera de este sandbox, que se cierra con este
    /// proceso): la versión instalada más nueva si `latest`.
    pub fn spawn(latest: bool) -> Result<(), String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| err.to_string())?;
        runtime.block_on(async {
            let proxy = tokio::time::timeout(CALL_TIMEOUT, Flatpak::new())
                .await
                .map_err(|_| "el portal de Flatpak no contestó a tiempo".to_owned())?
                .map_err(|err| format!("el portal de Flatpak no respondió: {err}"))?;
            let exe = std::env::current_exe().map_err(|err| err.to_string())?;
            let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
            let flags = if latest {
                SpawnFlags::LatestVersion.into()
            } else {
                Default::default()
            };
            tokio::time::timeout(
                CALL_TIMEOUT,
                proxy.spawn(
                    cwd,
                    &[exe],
                    HashMap::<u32, std::os::fd::OwnedFd>::new(),
                    HashMap::new(),
                    flags,
                    SpawnOptions::default(),
                ),
            )
            .await
            .map_err(|_| "el portal de Flatpak no contestó a tiempo".to_owned())?
            .map(|_| ())
            .map_err(|err| format!("el portal de Flatpak no abrió la versión nueva: {err}"))
        })
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn a_missing_repository_only_announces() {
            assert!(super::without_repository(
                Some("org.freedesktop.Flatpak.Error.RemoteNotFound"),
                None
            ));
            assert!(super::without_repository(
                None,
                Some("Can't pull from disabled remote 'coroom-origin'")
            ));
            assert!(!super::without_repository(
                Some("org.freedesktop.DBus.Error.Failed"),
                Some("Could not resolve hostname")
            ));
        }
    }
}

// --- Zip portátil de Windows ---------------------------------------------------------------------

/// La instalación del zip portátil: preparar, cambiar los ficheros y limpiar. Es lógica de ficheros
/// sin nada de Windows, así que se prueba en cualquier sistema con una carpeta temporal.
pub mod zip {
    use std::collections::HashSet;
    use std::fs;
    use std::io;
    use std::path::{Component, Path, PathBuf};

    use coroom_session::update::{Downloaded, Package, Release};
    use serde::{Deserialize, Serialize};

    use super::{DownloadControl, Failure, Progress, ReportKind, locks};

    /// Carpeta, junto al ejecutable, con la versión nueva descomprimida.
    pub const STAGING: &str = ".coroom-update";

    /// Fichero, junto al ejecutable, con la versión preparada: se escribe al terminar de
    /// descomprimir (sin él, lo de [`STAGING`] está a medias y no se usa).
    pub const STAGED_MARKER: &str = ".coroom-update.version";

    /// Diario del cambio de ficheros, junto al ejecutable ([`Journal`]).
    pub const JOURNAL: &str = ".coroom-update.journal";

    /// Lo que deja el paquete del zip junto a `coroom.exe` (`packaging/msix/build.ps1`): así se
    /// sabe que esta carpeta es la del zip y no, por ejemplo, la de una compilación propia.
    pub const PORTABLE_MARKER: &str = "LEEME.txt";

    /// El ejecutable dentro del zip. Se instala con el nombre del que corre: si el usuario lo
    /// renombró, sus accesos directos siguen abriendo coroom (y no queda al lado un `coroom.exe`
    /// nuevo junto al suyo viejo).
    pub const EXE_IN_ZIP: &str = "coroom.exe";

    /// Sufijo de los ficheros reemplazados, que se borran al arrancar.
    pub const OLD_SUFFIX: &str = ".old";

    /// Sufijo con el que [`check_writable`] renombra el ejecutable un momento.
    const RENAME_PROBE: &str = ".coroom-prueba";

    /// Si este ejecutable es el del zip portátil (compilado en release y con [`PORTABLE_MARKER`]
    /// al lado).
    pub fn is_portable() -> bool {
        !cfg!(debug_assertions)
            && super::exe_dir().is_some_and(|dir| dir.join(PORTABLE_MARKER).is_file())
    }

    /// Comprueba que se puede actualizar en `dir`: crear y borrar un fichero y renombrar el
    /// ejecutable `exe_name` (y volverlo a su nombre), que es lo que hace la instalación. Si no,
    /// la carpeta.
    pub fn check_writable(dir: &Path, exe_name: &str) -> Result<(), String> {
        let folder = || dir.display().to_string();
        let probe = dir.join(format!(".coroom-escritura-{}", std::process::id()));
        fs::write(&probe, b"").map_err(|_| folder())?;
        let _ = fs::remove_file(&probe);
        let exe = dir.join(exe_name);
        if !exe.is_file() {
            return Ok(());
        }
        let moved = dir.join(format!("{exe_name}{RENAME_PROBE}"));
        fs::rename(&exe, &moved).map_err(|_| folder())?;
        if fs::rename(&moved, &exe).is_err() && fs::rename(&moved, &exe).is_err() {
            // Lo arregla el arranque siguiente ([`recover`]).
            tracing::error!("no se pudo devolver su nombre al ejecutable");
            return Err(folder());
        }
        Ok(())
    }

    /// Devuelve su nombre a un ejecutable que [`check_writable`] dejó renombrado.
    fn undo_rename_probe(dir: &Path) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(original) = name.strip_suffix(RENAME_PROBE) {
                let original = dir.join(original);
                if !original.exists() {
                    let _ = fs::rename(entry.path(), original);
                }
            }
        }
    }

    /// La ruta relativa segura de una entrada del zip (`\` también separa: `Compress-Archive` de
    /// Windows PowerShell los escribe así), o `None` si es un directorio o sale de la carpeta.
    pub fn entry_path(name: &str) -> Option<PathBuf> {
        let name = name.replace('\\', "/");
        if name.ends_with('/') || name.starts_with('/') || name.contains(':') {
            return None;
        }
        let mut out = PathBuf::new();
        for part in name.split('/') {
            match part {
                "" | "." => {}
                ".." => return None,
                part => out.push(part),
            }
        }
        let normal = out
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
        (normal && out.components().next().is_some()).then_some(out)
    }

    /// Escribe `contents` en `path` de forma que, si se corta, queda el de antes o el nuevo entero
    /// (fichero aparte, a disco, y renombrado encima).
    fn write_durably(path: &Path, contents: &[u8]) -> io::Result<()> {
        let mut temp = path.as_os_str().to_owned();
        temp.push(".tmp");
        let temp = PathBuf::from(temp);
        {
            let mut file = fs::File::create(&temp)?;
            io::Write::write_all(&mut file, contents)?;
            file.sync_all()?;
        }
        fs::rename(&temp, path)
    }

    /// Descomprime `zip` en `dest` (que se vacía antes), con cada fichero escrito a disco. Si todo
    /// va dentro de una sola carpeta, se quita esa carpeta. Tiene que traer [`EXE_IN_ZIP`].
    pub fn extract(zip: &Path, dest: &Path) -> Result<(), String> {
        let file = fs::File::open(zip)
            .map_err(|err| format!("no se pudo abrir {}: {err}", zip.display()))?;
        let mut archive = ::zip::ZipArchive::new(file)
            .map_err(|err| format!("el zip de la versión nueva no es válido: {err}"))?;
        let mut entries = Vec::new();
        for index in 0..archive.len() {
            let entry = archive
                .by_index(index)
                .map_err(|err| format!("el zip de la versión nueva no es válido: {err}"))?;
            let name = entry
                .name()
                .map_err(|err| format!("el zip de la versión nueva no es válido: {err}"))?
                .into_owned();
            if entry.is_dir() || name.replace('\\', "/").ends_with('/') {
                continue;
            }
            let path = entry_path(&name)
                .ok_or_else(|| format!("el zip trae una ruta no válida: {name}"))?;
            entries.push((index, path));
        }
        // Una sola carpeta arriba (`coroom-2.1.0-windows-x64/coroom.exe`): se quita.
        let first: Option<PathBuf> = entries
            .first()
            .and_then(|(_, path)| path.components().next())
            .map(|first| PathBuf::from(first.as_os_str()));
        let strip = first.filter(|first| {
            entries
                .iter()
                .all(|(_, path)| path.starts_with(first) && path != first)
                && !entries
                    .iter()
                    .any(|(_, path)| path == Path::new(EXE_IN_ZIP))
        });
        let _ = fs::remove_dir_all(dest);
        fs::create_dir_all(dest)
            .map_err(|err| format!("no se pudo crear {}: {err}", dest.display()))?;
        for (index, path) in entries {
            let path = match &strip {
                Some(first) => path.strip_prefix(first).unwrap_or(&path).to_path_buf(),
                None => path,
            };
            let target = dest.join(&path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .map_err(|err| format!("no se pudo crear {}: {err}", parent.display()))?;
            }
            let mut entry = archive
                .by_index(index)
                .map_err(|err| format!("el zip de la versión nueva no es válido: {err}"))?;
            let mut out = fs::File::create(&target)
                .map_err(|err| format!("no se pudo escribir {}: {err}", target.display()))?;
            io::copy(&mut entry, &mut out)
                .and_then(|_| out.sync_all())
                .map_err(|err| format!("no se pudo descomprimir {}: {err}", path.display()))?;
        }
        if !dest.join(EXE_IN_ZIP).is_file() {
            let _ = fs::remove_dir_all(dest);
            return Err(format!("el zip de la versión nueva no trae {EXE_IN_ZIP}"));
        }
        Ok(())
    }

    /// Descomprime `zip` en `dir/.coroom-update`, con el ejecutable con el nombre `exe_name` del
    /// que corre, y apunta `version` como preparada (cuando todo está ya en el disco).
    pub fn stage(zip: &Path, dir: &Path, version: &str, exe_name: &str) -> Result<(), String> {
        discard_staged(dir);
        let staging = dir.join(STAGING);
        extract(zip, &staging)?;
        if !exe_name.eq_ignore_ascii_case(EXE_IN_ZIP) {
            fs::rename(staging.join(EXE_IN_ZIP), staging.join(exe_name))
                .map_err(|err| format!("no se pudo preparar la versión nueva: {err}"))?;
        }
        write_durably(&dir.join(STAGED_MARKER), version.as_bytes())
            .map_err(|err| format!("no se pudo preparar la versión nueva: {err}"))
    }

    /// La versión preparada en `dir`, si la hay entera.
    pub fn staged_version(dir: &Path) -> Option<String> {
        let version = fs::read_to_string(dir.join(STAGED_MARKER)).ok()?;
        let version = version.trim().to_owned();
        (!version.is_empty() && dir.join(STAGING).is_dir()).then_some(version)
    }

    /// Borra la versión preparada (o a medias) de `dir`.
    pub fn discard_staged(dir: &Path) {
        let _ = fs::remove_file(dir.join(STAGED_MARKER));
        let _ = fs::remove_dir_all(dir.join(STAGING));
    }

    /// Instala en `dir` la versión preparada ([`swap_in`]) y borra lo preparado.
    pub fn apply_staged(dir: &Path) -> Result<(), String> {
        let version = staged_version(dir).unwrap_or_default();
        swap_in_version(dir, &dir.join(STAGING), &version, &mut |from, to| {
            fs::rename(from, to)
        })?;
        discard_staged(dir);
        Ok(())
    }

    /// Los ficheros bajo `root`, relativos y en orden.
    fn files(root: &Path) -> io::Result<Vec<PathBuf>> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                if entry.file_type()?.is_dir() {
                    walk(root, &path, out)?;
                } else if let Ok(relative) = path.strip_prefix(root) {
                    out.push(relative.to_path_buf());
                }
            }
            Ok(())
        }
        let mut out = Vec::new();
        walk(root, root, &mut out)?;
        out.sort();
        Ok(out)
    }

    /// Un fichero del cambio: dónde va (relativo) y, si ya había uno, dónde se aparta.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Entry {
        /// Ruta relativa.
        pub path: PathBuf,
        /// A dónde se aparta el que había (relativo), si había.
        pub old: Option<PathBuf>,
    }

    /// El diario del cambio de ficheros: se escribe entero antes de tocar nada y, al terminar, se
    /// marca hecho con los `*.old` por borrar. Con él, un cambio cortado a mitad se termina (o se
    /// deshace) al arrancar, y solo se borran los `*.old` que creó coroom.
    #[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
    pub struct Journal {
        /// La versión que se instala.
        pub version: String,
        /// Si el cambio terminó.
        pub committed: bool,
        /// Carpetas que hay que crear (relativas), de arriba abajo.
        pub created: Vec<PathBuf>,
        /// Los ficheros, en orden.
        pub entries: Vec<Entry>,
        /// Los `*.old` por borrar (relativos).
        pub leftovers: Vec<PathBuf>,
    }

    fn read_journal(dir: &Path) -> Option<Journal> {
        let text = fs::read(dir.join(JOURNAL)).ok()?;
        serde_json::from_slice(&text).ok()
    }

    fn write_journal(dir: &Path, journal: &Journal) -> Result<(), String> {
        let text = serde_json::to_vec(journal).map_err(|err| err.to_string())?;
        write_durably(&dir.join(JOURNAL), &text)
            .map_err(|err| format!("no se pudo apuntar la instalación: {err}"))
    }

    /// Un nombre libre para apartar `relative`: `x.old`, `x.1.old`, `x.2.old`… que no exista ni
    /// esté ya en `taken`.
    fn free_old_name(dir: &Path, relative: &Path, taken: &mut HashSet<PathBuf>) -> PathBuf {
        let name = relative
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        for n in 0u32.. {
            let candidate = if n == 0 {
                relative.with_file_name(format!("{name}{OLD_SUFFIX}"))
            } else {
                relative.with_file_name(format!("{name}.{n}{OLD_SUFFIX}"))
            };
            if !dir.join(&candidate).exists() && !taken.contains(&candidate) {
                taken.insert(candidate.clone());
                return candidate;
            }
        }
        relative.with_file_name(format!("{name}{OLD_SUFFIX}"))
    }

    /// Qué se va a mover de `staged` a `target`.
    fn plan(
        target: &Path,
        staged: &Path,
        version: &str,
        leftovers: Vec<PathBuf>,
    ) -> Result<Journal, String> {
        let list =
            files(staged).map_err(|err| format!("no se pudo leer la versión preparada: {err}"))?;
        if list.is_empty() {
            return Err("la versión preparada está vacía".to_owned());
        }
        let mut taken: HashSet<PathBuf> = leftovers.iter().cloned().collect();
        let mut journal = Journal {
            version: version.to_owned(),
            leftovers,
            ..Journal::default()
        };
        for relative in list {
            let mut missing = Vec::new();
            let mut parent = relative.parent();
            while let Some(dir) = parent {
                if dir.as_os_str().is_empty()
                    || target.join(dir).exists()
                    || journal.created.iter().any(|created| created == dir)
                {
                    break;
                }
                missing.push(dir.to_path_buf());
                parent = dir.parent();
            }
            journal.created.extend(missing.into_iter().rev());
            let to = target.join(&relative);
            if to.is_dir() {
                return Err(format!("{} es una carpeta", to.display()));
            }
            let old = to
                .exists()
                .then(|| free_old_name(target, &relative, &mut taken));
            journal.entries.push(Entry {
                path: relative,
                old,
            });
        }
        Ok(journal)
    }

    /// Hace (o termina) el cambio apuntado en `journal`. Cada paso se reconoce por lo que hay en el
    /// disco, así que sirve también para seguir uno cortado a mitad.
    fn roll_forward(
        target: &Path,
        staged: &Path,
        journal: &Journal,
        rename: &mut dyn FnMut(&Path, &Path) -> io::Result<()>,
    ) -> Result<(), String> {
        for dir in &journal.created {
            let dir = target.join(dir);
            if !dir.is_dir() {
                fs::create_dir(&dir)
                    .map_err(|err| format!("no se pudo crear {}: {err}", dir.display()))?;
            }
        }
        for entry in &journal.entries {
            let from = staged.join(&entry.path);
            let to = target.join(&entry.path);
            if !from.exists() {
                if to.exists() {
                    continue;
                }
                return Err(format!(
                    "falta {} en la versión preparada",
                    entry.path.display()
                ));
            }
            match &entry.old {
                Some(old) if to.exists() => {
                    let old = target.join(old);
                    if old.exists() {
                        return Err(format!("{} ya existe", old.display()));
                    }
                    rename(&to, &old)
                        .map_err(|err| format!("no se pudo apartar {}: {err}", to.display()))?;
                }
                None if to.exists() => {
                    return Err(format!("{} apareció durante la instalación", to.display()));
                }
                _ => {}
            }
            rename(&from, &to)
                .map_err(|err| format!("no se pudo poner {}: {err}", to.display()))?;
        }
        Ok(())
    }

    /// Deshace lo hecho de `journal` (en orden inverso), según lo que hay en el disco.
    fn roll_back(target: &Path, staged: &Path, journal: &Journal) {
        let undo = |from: &Path, to: &Path| {
            if let Err(err) = fs::rename(from, to) {
                tracing::error!(error = %err, "no se pudo deshacer un paso de la actualización");
            }
        };
        for entry in journal.entries.iter().rev() {
            let from = staged.join(&entry.path);
            let to = target.join(&entry.path);
            if to.exists() && !from.exists() {
                if let Some(parent) = from.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                undo(&to, &from);
            }
            if let Some(old) = &entry.old {
                let old = target.join(old);
                if old.exists() && !to.exists() {
                    undo(&old, &to);
                }
            }
        }
        for dir in journal.created.iter().rev() {
            let _ = fs::remove_dir(target.join(dir));
        }
    }

    /// El diario después de terminar: hecho, con los `*.old` por borrar.
    fn committed(mut journal: Journal) -> Journal {
        journal.committed = true;
        let olds: Vec<PathBuf> = journal
            .entries
            .iter()
            .filter_map(|entry| entry.old.clone())
            .collect();
        journal.leftovers.extend(olds);
        journal.created.clear();
        journal.entries.clear();
        journal
    }

    /// Deja el diario con solo los `*.old` pendientes de antes (o lo borra si no hay).
    fn restore_leftovers(dir: &Path, version: String, leftovers: Vec<PathBuf>) {
        if leftovers.is_empty() {
            let _ = fs::remove_file(dir.join(JOURNAL));
        } else {
            let _ = write_journal(
                dir,
                &Journal {
                    version,
                    committed: true,
                    leftovers,
                    ..Journal::default()
                },
            );
        }
    }

    /// Mueve los ficheros de `staged` a `target`: aparta cada uno que ya exista a `*.old` (en
    /// Windows se puede renombrar el ejecutable y las DLL en uso, no sobrescribirlos) y pone el
    /// nuevo. Apunta antes lo que va a hacer en el diario. Si algo falla, deshace todo en orden
    /// inverso y la carpeta queda como estaba.
    pub fn swap_in(target: &Path, staged: &Path) -> Result<(), String> {
        swap_in_version(target, staged, "", &mut |from, to| fs::rename(from, to))
    }

    /// [`swap_in`] con el renombrado dado (las pruebas lo hacen fallar a mitad).
    pub fn swap_in_with(
        target: &Path,
        staged: &Path,
        rename: &mut dyn FnMut(&Path, &Path) -> io::Result<()>,
    ) -> Result<(), String> {
        swap_in_version(target, staged, "", rename)
    }

    fn swap_in_version(
        target: &Path,
        staged: &Path,
        version: &str,
        rename: &mut dyn FnMut(&Path, &Path) -> io::Result<()>,
    ) -> Result<(), String> {
        let previous = read_journal(target)
            .filter(|journal| journal.committed)
            .map(|journal| journal.leftovers)
            .unwrap_or_default();
        let journal = plan(target, staged, version, previous.clone())?;
        write_journal(target, &journal)?;
        if let Err(err) = roll_forward(target, staged, &journal, rename) {
            roll_back(target, staged, &journal);
            restore_leftovers(target, journal.version, previous);
            return Err(err);
        }
        // Si no se puede apuntar que terminó, el arranque siguiente lo ve todo hecho y lo apunta.
        if let Err(err) = write_journal(target, &committed(journal)) {
            tracing::warn!(error = %err, "no se pudo apuntar el final de la instalación");
        }
        Ok(())
    }

    /// Lo que encontró [`recover`].
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Recovery {
        /// Ningún cambio a medias.
        Nothing,
        /// Terminó un cambio cortado a mitad: los ficheros ya son los de esta versión.
        Finished(String),
        /// No se pudo terminar y se deshizo: la carpeta queda como antes.
        RolledBack {
            /// La versión que no se pudo instalar.
            version: String,
            /// Por qué.
            err: String,
        },
    }

    /// Al arrancar (con la carpeta para este proceso solo): termina o deshace un cambio cortado a
    /// mitad y borra los `*.old` del diario que ya no estén en uso.
    pub fn recover(dir: &Path) -> Recovery {
        undo_rename_probe(dir);
        let Some(journal) = read_journal(dir) else {
            return Recovery::Nothing;
        };
        if journal.committed {
            // Cortado después de terminar y antes de limpiar lo preparado.
            if staged_version(dir).as_deref() == Some(journal.version.as_str()) {
                discard_staged(dir);
            }
            let pending: Vec<PathBuf> = journal
                .leftovers
                .iter()
                .filter(|old| {
                    let path = dir.join(old);
                    path.exists() && fs::remove_file(&path).is_err()
                })
                .cloned()
                .collect();
            restore_leftovers(dir, journal.version, pending);
            return Recovery::Nothing;
        }
        let staged = dir.join(STAGING);
        let previous = journal.leftovers.clone();
        let version = journal.version.clone();
        match roll_forward(dir, &staged, &journal, &mut |from, to| fs::rename(from, to)) {
            Ok(()) => {
                if let Err(err) = write_journal(dir, &committed(journal)) {
                    tracing::warn!(error = %err, "no se pudo apuntar el final de la instalación");
                }
                discard_staged(dir);
                Recovery::Finished(version)
            }
            Err(err) => {
                roll_back(dir, &staged, &journal);
                restore_leftovers(dir, version.clone(), previous);
                discard_staged(dir);
                Recovery::RolledBack { version, err }
            }
        }
    }

    /// Descarga el zip de `release`, lo comprueba y lo deja preparado junto al ejecutable.
    pub fn download_and_stage(
        release: &Release,
        control: &DownloadControl,
        report: &dyn Fn(ReportKind),
    ) -> ReportKind {
        let failed = |err: String| ReportKind::Finished(Err(Failure::Failed(err)));
        let later = ReportKind::Finished(Err(Failure::Later));
        let Some(dir) = super::exe_dir() else {
            return failed("no se sabe en qué carpeta está coroom".to_owned());
        };
        let exe_name = super::exe_name();
        {
            // Otro proceso instala o prepara ahora mismo: más tarde.
            let Some(_install) = locks::try_exclusive(&dir.join(locks::INSTALL)) else {
                return later;
            };
            if let Err(folder) = check_writable(&dir, &exe_name) {
                return ReportKind::Finished(Err(Failure::Unwritable(folder)));
            }
            if staged_version(&dir).as_deref() == Some(release.version.as_str()) {
                return ReportKind::Finished(Ok(()));
            }
        }
        // La publicación todavía no trae el zip (se sube después de publicarla): más tarde.
        let Some(asset) = release.asset(Package::WindowsZip) else {
            return later;
        };
        let downloads = super::downloads_dir();
        let agent = coroom_session::update::user_agent(super::CURRENT);
        let result = coroom_session::update::download(
            asset,
            &downloads,
            &agent,
            control,
            &mut |done, total| report(ReportKind::Progress(Progress::bytes(done, total))),
        );
        match result {
            Ok(Downloaded::Paused { done, total }) => {
                ReportKind::Paused(Progress::bytes(done, total))
            }
            Ok(Downloaded::Done(zip)) => {
                let Some(_install) = locks::try_exclusive(&dir.join(locks::INSTALL)) else {
                    return later;
                };
                match stage(&zip, &dir, &release.version, &exe_name) {
                    Ok(()) => {
                        let _ = fs::remove_file(&zip);
                        ReportKind::Finished(Ok(()))
                    }
                    Err(err) => failed(err),
                }
            }
            Err(err) => failed(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;

    use super::zip::*;

    fn write_zip(path: &std::path::Path, entries: &[(&str, &[u8])]) {
        let file = fs::File::create(path).expect("zip");
        let mut writer = ::zip::ZipWriter::new(file);
        for (name, body) in entries {
            writer
                .start_file(*name, ::zip::write::SimpleFileOptions::default())
                .expect("entrada");
            writer.write_all(body).expect("escribir");
        }
        writer.finish().expect("cerrar");
    }

    fn read(path: &std::path::Path) -> String {
        fs::read_to_string(path).expect("leer")
    }

    /// Una carpeta de coroom de ejemplo: el ejecutable, dos DLL y las licencias.
    fn install(dir: &std::path::Path, version: &str) {
        fs::create_dir_all(dir.join("licencias")).expect("carpeta");
        for name in ["coroom.exe", "avcodec-62.dll", "avutil-60.dll", "LEEME.txt"] {
            fs::write(dir.join(name), format!("{name} {version}")).expect("fichero");
        }
        fs::write(dir.join("licencias/FFmpeg-LGPL.txt"), version).expect("licencia");
    }

    /// Lo preparado de la 2.1.0 en `dir/.coroom-update`, como lo deja [`stage`].
    fn staged_210(dir: &std::path::Path) {
        let staged = dir.join(STAGING);
        fs::create_dir_all(staged.join("licencias")).expect("carpeta");
        fs::create_dir_all(staged.join("nueva")).expect("carpeta");
        for name in [
            "avcodec-62.dll",
            "coroom.exe",
            "licencias/FFmpeg-LGPL.txt",
            "nueva/x.txt",
        ] {
            fs::write(staged.join(name), "2.1.0").expect("fichero");
        }
        fs::write(dir.join(STAGED_MARKER), "2.1.0").expect("marca");
    }

    #[test]
    fn zip_entry_paths_stay_inside_the_folder() {
        assert_eq!(entry_path("coroom.exe"), Some("coroom.exe".into()));
        assert_eq!(
            entry_path("licencias\\FFmpeg-LGPL.txt"),
            Some(["licencias", "FFmpeg-LGPL.txt"].iter().collect())
        );
        for bad in ["../x", "/etc/x", "C:\\x", "a/../../x", "licencias/", ""] {
            assert_eq!(entry_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_zip_is_extracted_with_or_without_a_top_folder() {
        let temp = tempfile::tempdir().expect("directorio");
        let zip = temp.path().join("v.zip");
        write_zip(
            &zip,
            &[
                ("coroom.exe", b"nuevo"),
                ("licencias\\FFmpeg-LGPL.txt", b"lgpl"),
            ],
        );
        let dest = temp.path().join("fuera");
        extract(&zip, &dest).expect("extraer");
        assert_eq!(read(&dest.join("coroom.exe")), "nuevo");
        assert_eq!(read(&dest.join("licencias/FFmpeg-LGPL.txt")), "lgpl");

        write_zip(
            &zip,
            &[
                ("coroom-2.1.0-windows-x64/coroom.exe", b"nuevo"),
                ("coroom-2.1.0-windows-x64/avutil-60.dll", b"dll"),
            ],
        );
        extract(&zip, &dest).expect("extraer con carpeta");
        assert_eq!(read(&dest.join("avutil-60.dll")), "dll");

        write_zip(&zip, &[("otra.exe", b"x")]);
        assert!(extract(&zip, &dest).is_err(), "sin coroom.exe");
        write_zip(&zip, &[("../fuera.exe", b"x")]);
        assert!(extract(&zip, &dest).is_err(), "fuera de la carpeta");
        fs::write(&zip, b"no es un zip").expect("escribir");
        assert!(extract(&zip, &dest).is_err());
    }

    /// Si el usuario renombró el ejecutable, el `coroom.exe` del zip se instala con su nombre.
    #[test]
    fn a_renamed_executable_keeps_its_name() {
        let temp = tempfile::tempdir().expect("directorio");
        let dir = temp.path().join("coroom");
        fs::create_dir_all(&dir).expect("carpeta");
        fs::write(dir.join("juego.exe"), "2.0.6").expect("fichero");
        let zip = temp.path().join("v.zip");
        write_zip(&zip, &[("coroom.exe", b"2.1.0"), ("LEEME.txt", b"leeme")]);
        stage(&zip, &dir, "2.1.0", "juego.exe").expect("preparar");
        apply_staged(&dir).expect("instalar");
        assert_eq!(read(&dir.join("juego.exe")), "2.1.0");
        assert!(!dir.join("coroom.exe").exists());
    }

    #[test]
    fn staged_files_replace_the_old_ones_which_are_removed_on_the_next_start() {
        let temp = tempfile::tempdir().expect("directorio");
        let dir = temp.path().join("coroom");
        install(&dir, "2.0.6");
        // Un `*.old` que no es de coroom (la carpeta está entre otras cosas del usuario) se queda.
        fs::create_dir_all(dir.join("partidas")).expect("carpeta");
        fs::write(dir.join("partidas/guardado.old"), "del usuario").expect("fichero");
        let zip = temp.path().join("v.zip");
        write_zip(
            &zip,
            &[
                ("coroom.exe", b"coroom.exe 2.1.0"),
                ("avcodec-62.dll", b"avcodec-62.dll 2.1.0"),
                ("swscale-9.dll", b"nueva"),
                ("licencias/FFmpeg-LGPL.txt", b"2.1.0"),
            ],
        );
        stage(&zip, &dir, "2.1.0", "coroom.exe").expect("preparar");
        assert_eq!(staged_version(&dir).as_deref(), Some("2.1.0"));
        apply_staged(&dir).expect("instalar");
        assert_eq!(read(&dir.join("coroom.exe")), "coroom.exe 2.1.0");
        assert_eq!(read(&dir.join("swscale-9.dll")), "nueva");
        assert_eq!(read(&dir.join("licencias/FFmpeg-LGPL.txt")), "2.1.0");
        // Lo que la versión nueva no trae se queda; lo reemplazado queda aparte hasta arrancar.
        assert_eq!(read(&dir.join("avutil-60.dll")), "avutil-60.dll 2.0.6");
        assert_eq!(read(&dir.join("coroom.exe.old")), "coroom.exe 2.0.6");
        assert_eq!(staged_version(&dir), None);
        assert!(!dir.join(STAGING).exists());
        assert_eq!(recover(&dir), Recovery::Nothing);
        assert!(!dir.join("coroom.exe.old").exists());
        assert!(!dir.join("licencias/FFmpeg-LGPL.txt.old").exists());
        assert!(dir.join("coroom.exe").exists());
        assert_eq!(read(&dir.join("partidas/guardado.old")), "del usuario");
        assert!(
            !dir.join(JOURNAL).exists(),
            "sin nada pendiente, sin diario"
        );
    }

    #[test]
    fn a_failed_swap_leaves_the_folder_as_it_was() {
        let temp = tempfile::tempdir().expect("directorio");
        let dir = temp.path().join("coroom");
        install(&dir, "2.0.6");
        let staged = temp.path().join("preparada");
        fs::create_dir_all(staged.join("licencias")).expect("carpeta");
        fs::create_dir_all(staged.join("nueva")).expect("carpeta");
        for name in [
            "avcodec-62.dll",
            "coroom.exe",
            "licencias/FFmpeg-LGPL.txt",
            "nueva/x.txt",
        ] {
            fs::write(staged.join(name), "2.1.0").expect("fichero");
        }
        // Cada renombrado del cambio, hasta que falla uno: en cada punto la carpeta vuelve a estar
        // igual que antes.
        for fail_at in 0..7 {
            let mut calls = 0;
            let result = swap_in_with(&dir, &staged, &mut |from, to| {
                calls += 1;
                if calls > fail_at {
                    Err(std::io::Error::other("en uso"))
                } else {
                    fs::rename(from, to)
                }
            });
            assert!(result.is_err(), "falla en el renombrado {fail_at}");
            for name in ["coroom.exe", "avcodec-62.dll", "avutil-60.dll"] {
                assert_eq!(read(&dir.join(name)), format!("{name} 2.0.6"), "{fail_at}");
            }
            assert_eq!(read(&dir.join("licencias/FFmpeg-LGPL.txt")), "2.0.6");
            assert!(!dir.join("nueva").exists(), "la carpeta creada se quita");
            assert!(!dir.join("coroom.exe.old").exists());
            assert!(!dir.join(JOURNAL).exists(), "sin diario");
            assert_eq!(
                read(&staged.join("coroom.exe")),
                "2.1.0",
                "lo preparado sigue"
            );
        }
    }

    /// Un corte a mitad del cambio (el proceso muere): el arranque siguiente lo termina desde el
    /// diario, también con la versión preparada igual a la que ya corre.
    #[test]
    fn a_swap_cut_halfway_is_finished_at_the_next_start() {
        for cut_after in 0..7 {
            let temp = tempfile::tempdir().expect("directorio");
            let dir = temp.path().join("coroom");
            install(&dir, "2.0.6");
            staged_210(&dir);
            let mut calls = 0;
            // El «corte»: los renombrados después de `cut_after` no llegan a hacerse, y tampoco el
            // deshacer (se simula con un pánico que lo salta todo).
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                swap_in_with(&dir, &dir.join(STAGING), &mut |from, to| {
                    calls += 1;
                    if calls > cut_after {
                        panic!("corte");
                    }
                    fs::rename(from, to)
                })
            }));
            assert!(result.is_err(), "{cut_after}");
            assert!(dir.join(JOURNAL).exists(), "el diario se escribió antes");
            assert_eq!(
                recover(&dir),
                Recovery::Finished(String::new()),
                "{cut_after}"
            );
            for name in [
                "coroom.exe",
                "avcodec-62.dll",
                "licencias/FFmpeg-LGPL.txt",
                "nueva/x.txt",
            ] {
                assert_eq!(read(&dir.join(name)), "2.1.0", "{cut_after}: {name}");
            }
            assert_eq!(read(&dir.join("avutil-60.dll")), "avutil-60.dll 2.0.6");
            assert_eq!(staged_version(&dir), None);
            // Y el arranque después borra los `*.old` apuntados.
            assert_eq!(recover(&dir), Recovery::Nothing);
            assert!(!dir.join("coroom.exe.old").exists());
            assert!(!dir.join(JOURNAL).exists());
        }
    }

    /// Si lo preparado ya no está entero, el cambio a medias se deshace.
    #[test]
    fn a_swap_that_cannot_be_finished_is_rolled_back() {
        let temp = tempfile::tempdir().expect("directorio");
        let dir = temp.path().join("coroom");
        install(&dir, "2.0.6");
        staged_210(&dir);
        let mut calls = 0;
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            swap_in_with(&dir, &dir.join(STAGING), &mut |from, to| {
                calls += 1;
                if calls > 3 {
                    panic!("corte");
                }
                fs::rename(from, to)
            })
        }));
        fs::remove_file(dir.join(STAGING).join("nueva/x.txt")).expect("borrar");
        let Recovery::RolledBack { err, .. } = recover(&dir) else {
            panic!("tenía que deshacerse");
        };
        assert!(err.contains("x.txt"), "{err}");
        for name in ["coroom.exe", "avcodec-62.dll", "avutil-60.dll"] {
            assert_eq!(read(&dir.join(name)), format!("{name} 2.0.6"));
        }
        assert!(!dir.join("coroom.exe.old").exists());
        assert!(!dir.join(JOURNAL).exists());
        assert_eq!(staged_version(&dir), None, "lo preparado se tira");
    }

    #[test]
    fn an_old_file_in_use_gets_another_name() {
        let temp = tempfile::tempdir().expect("directorio");
        let dir = temp.path().join("coroom");
        install(&dir, "2.0.6");
        // Un `coroom.exe.old` que sigue ahí (en uso) de la vez anterior.
        fs::write(dir.join("coroom.exe.old"), "anterior").expect("ocupado");
        let staged = temp.path().join("preparada");
        fs::create_dir_all(&staged).expect("carpeta");
        fs::write(staged.join("coroom.exe"), "2.1.0").expect("fichero");
        swap_in(&dir, &staged).expect("instalar");
        assert_eq!(read(&dir.join("coroom.exe")), "2.1.0");
        assert_eq!(read(&dir.join("coroom.exe.1.old")), "coroom.exe 2.0.6");
        assert_eq!(
            read(&dir.join("coroom.exe.old")),
            "anterior",
            "no es del diario"
        );
    }

    #[test]
    fn half_extracted_or_missing_versions_are_not_staged() {
        let temp = tempfile::tempdir().expect("directorio");
        let dir = temp.path();
        fs::create_dir_all(dir.join(STAGING)).expect("carpeta");
        assert_eq!(staged_version(dir), None, "sin la marca está a medias");
        fs::write(dir.join(STAGED_MARKER), "2.1.0").expect("marca");
        assert_eq!(staged_version(dir).as_deref(), Some("2.1.0"));
        discard_staged(dir);
        assert_eq!(staged_version(dir), None);
    }

    /// Se puede actualizar si se puede escribir y renombrar el ejecutable; el ejecutable queda con
    /// su nombre.
    #[test]
    fn the_folder_must_let_the_executable_be_renamed() {
        let temp = tempfile::tempdir().expect("directorio");
        let dir = temp.path();
        fs::write(dir.join("coroom.exe"), "exe").expect("fichero");
        assert!(check_writable(dir, "coroom.exe").is_ok());
        assert_eq!(read(&dir.join("coroom.exe")), "exe");
        assert_eq!(fs::read_dir(dir).expect("leer").count(), 1, "sin restos");
        assert!(check_writable(&dir.join("no-existe"), "coroom.exe").is_err());
        // Un renombrado de prueba que quedó a medias se arregla al arrancar.
        fs::rename(dir.join("coroom.exe"), dir.join("coroom.exe.coroom-prueba")).expect("mover");
        assert_eq!(recover(dir), Recovery::Nothing);
        assert_eq!(read(&dir.join("coroom.exe")), "exe");
    }

    /// Los bloqueos entre procesos: el exclusivo excluye a los demás (también a otro fichero
    /// abierto del mismo proceso, como haría otro proceso).
    #[test]
    fn locks_exclude_each_other() {
        let temp = tempfile::tempdir().expect("directorio");
        let path = temp.path().join("x.lock");
        let shared = super::locks::try_shared(&path).expect("compartido");
        assert!(
            super::locks::try_shared(&path).is_some(),
            "varios compartidos"
        );
        assert!(super::locks::try_exclusive(&path).is_none());
        assert!(super::locks::held_elsewhere(&path));
        drop(shared);
        assert!(!super::locks::held_elsewhere(&path));
        let exclusive = super::locks::try_exclusive(&path).expect("exclusivo");
        assert!(super::locks::try_shared(&path).is_none());
        drop(exclusive);
    }
}
