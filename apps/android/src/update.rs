//! La versión nueva en Android: busca en las publicaciones de GitHub, descarga el APK a la caché
//! de la aplicación, comprueba su SHA-256 y deja que la interfaz lo dé al instalador del sistema
//! (`PackageInstaller`). Las reglas (cuándo descargar, pausar con una sala abierta, cuándo se
//! puede instalar) son las de [`coroom_session::update::Updater`], las mismas del escritorio.
//!
//! La interfaz recibe el estado entero en cada cambio ([`UiEvent::Update`]) y, cuando toca
//! instalar, [`UiEvent::UpdateInstall`] con la ruta del APK. En una red con límite de datos (lo
//! dice la interfaz, `update_set_metered`) la descarga automática espera; la pedida a mano no. Una
//! compilación que no puede instalar los APK de las publicaciones (la de depuración, firmada con
//! otra clave) solo avisa (`notice_only`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};

use coroom_session::update::{
    self as shared, Channel, Command, DownloadControl, Downloaded, Failure, NoticeOnly, Package,
    Progress, Release, Stage, Updater, Wait,
};
use serde::{Deserialize, Serialize};

use crate::events::UiEvent;
use crate::guest::Emit;

/// Esta versión.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Respuesta de «Instalar ahora» con una sala abierta (o sin la versión lista).
pub const CANNOT_INSTALL: &str = "sal de la sala para instalar la versión nueva";

/// Entre dos búsquedas fuera del intervalo (la app es más vieja que el servidor), por lo menos
/// esto.
const EXTRA_CHECK_BACKOFF_SECS: u64 = 3600;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Lo que la interfaz enseña de la versión nueva (Inicio y Ajustes).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UpdateView {
    /// `idle`, `notice`, `downloading`, `paused`, `ready` o `failed`.
    pub stage: &'static str,
    /// La versión nueva, si se conoce.
    pub version: Option<String>,
    /// Su página (o la de las publicaciones).
    pub url: String,
    /// Esta versión.
    pub current: &'static str,
    /// Lo descargado, de 0 a 100.
    pub percent: u32,
    /// El tamaño del APK, si se sabe.
    pub total_bytes: Option<u64>,
    /// Por qué falló la descarga (`failed`).
    pub error: Option<String>,
    /// Por qué está en pausa (`paused`): `room` (una sala abierta) o `metered` (red con límite de
    /// datos).
    pub waiting: Option<&'static str>,
    /// Si hay una sala abierta.
    pub room_open: bool,
    /// Si se puede instalar ya.
    pub can_install: bool,
    /// «Actualizar automáticamente».
    pub auto: bool,
    /// «Buscar versiones nuevas».
    pub enabled: bool,
    /// Buscando ahora.
    pub checking: bool,
    /// La última búsqueda que contestó, en segundos desde 1970.
    pub last_check: Option<u64>,
    /// Por qué falló la última búsqueda.
    pub check_error: Option<String>,
}

/// Lo que guarda la interfaz y se le pasa al arrancar (`update_configure`).
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Carpeta de las descargas (en la caché de la aplicación).
    pub dir: String,
    /// «Buscar versiones nuevas».
    #[serde(default = "yes")]
    pub check: bool,
    /// «Actualizar automáticamente».
    #[serde(default = "yes")]
    pub auto: bool,
    /// Cada cuántas horas se busca.
    #[serde(default = "day")]
    pub interval_hours: u32,
    /// La última búsqueda que contestó.
    #[serde(default)]
    pub last_check: Option<u64>,
    /// Esta compilación no puede instalar los APK de las publicaciones (la de depuración): solo
    /// avisa.
    #[serde(default)]
    pub notice_only: bool,
}

fn yes() -> bool {
    true
}

fn day() -> u32 {
    24
}

struct State {
    updater: Updater,
    enabled: bool,
    dir: Option<PathBuf>,
    interval_hours: u32,
    checking: bool,
    last_check: Option<u64>,
    check_error: Option<String>,
    /// Hay un APK más nuevo en la caché de antes de cerrar la app: la primera búsqueda toca ya.
    cached_newer: bool,
    /// La última búsqueda fuera del intervalo ([`Updates::check_extra`]).
    last_extra: Option<u64>,
}

type Job = Box<dyn FnOnce() + Send>;

/// La versión nueva de esta instalación.
pub struct Updates {
    state: Mutex<State>,
    running: Mutex<Option<Arc<DownloadControl>>>,
    /// Sube con cada descarga nueva y con cada descarte: lo que avisa una anterior se tira.
    generation: AtomicU64,
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
}

impl Default for Updates {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                updater: Updater::new(Channel::Package(Package::AndroidApk), true),
                enabled: false,
                dir: None,
                interval_hours: day(),
                checking: false,
                last_check: None,
                check_error: None,
                cached_newer: false,
                last_extra: None,
            }),
            running: Mutex::new(None),
            generation: AtomicU64::new(0),
            jobs: Mutex::new(None),
        }
    }
}

impl Updates {
    /// El estado para la interfaz.
    pub fn view(&self) -> UpdateView {
        let state = lock(&self.state);
        let updater = &state.updater;
        let release = updater.release();
        let (stage, progress, error) = match updater.stage() {
            Stage::Idle => ("idle", Progress::default(), None),
            Stage::Notice => ("notice", Progress::default(), None),
            Stage::Downloading(progress) => ("downloading", *progress, None),
            Stage::Paused(progress) => ("paused", *progress, None),
            Stage::Ready => ("ready", Progress::default(), None),
            Stage::Failed(err) | Stage::InstallFailed(err) | Stage::Unwritable(err) => {
                ("failed", Progress::default(), Some(err.clone()))
            }
        };
        let waiting = updater.waiting().map(|wait| match wait {
            Wait::Room => "room",
            Wait::Metered => "metered",
        });
        UpdateView {
            stage,
            version: release.map(|release| release.version.clone()),
            url: release.map_or_else(|| shared::RELEASES_PAGE.to_owned(), |r| r.url.clone()),
            current: CURRENT,
            percent: progress.percent(),
            total_bytes: progress.total_bytes,
            error,
            waiting,
            room_open: updater.room_open(),
            can_install: updater.can_restart(),
            auto: updater.auto(),
            enabled: state.enabled,
            checking: state.checking,
            last_check: state.last_check,
            check_error: state.check_error.clone(),
        }
    }

    fn publish(&self, emit: &Emit) {
        emit(UiEvent::Update(self.view()));
    }

    /// Los ajustes guardados, al arrancar.
    pub fn configure(self: &Arc<Self>, config: Config, emit: &Emit) {
        let commands = {
            let mut state = lock(&self.state);
            let dir = PathBuf::from(config.dir);
            remove_installed(&dir);
            // Uno más nuevo bajado antes de cerrar la app se vuelve a ofrecer en la primera
            // búsqueda, sin esperar al intervalo (una sola vez).
            state.cached_newer = state.dir.is_none() && newest_apk(&dir).is_some();
            state.dir = Some(dir);
            if config.notice_only
                && state.updater.channel() != Channel::Notice(NoticeOnly::Unpackaged)
            {
                state.updater = Updater::new(Channel::Notice(NoticeOnly::Unpackaged), config.auto);
            }
            state.interval_hours = config.interval_hours.max(1);
            // La guardada puede ir por detrás de la de ahora (se guarda después).
            state.last_check = config.last_check.max(state.last_check);
            let mut commands = state.updater.set_auto(config.auto);
            if state.enabled && !config.check {
                commands.extend(state.updater.disable());
            }
            state.enabled = config.check;
            commands
        };
        self.run(commands, emit);
        self.publish(emit);
    }

    /// Busca la versión nueva con `fetch` (bloquea). Sin `manual`, solo si está encendida y toca
    /// por el intervalo; devuelve si buscó.
    pub fn check_with(
        self: &Arc<Self>,
        manual: bool,
        emit: &Emit,
        fetch: impl FnOnce() -> Result<Option<Release>, String>,
    ) -> bool {
        {
            let mut state = lock(&self.state);
            let hours = state.updater.recheck_hours(state.interval_hours);
            let due =
                shared::due(state.last_check, shared::unix_now(), hours) || state.cached_newer;
            if state.checking || (!manual && !(state.enabled && due)) {
                return false;
            }
            state.checking = true;
            state.cached_newer = false;
        }
        self.publish(emit);
        let result = fetch();
        let commands = {
            let mut state = lock(&self.state);
            state.checking = false;
            match result {
                Ok(release) => {
                    state.check_error = None;
                    state.last_check = Some(shared::unix_now());
                    // Una búsqueda a mano con la búsqueda apagada solo contesta.
                    if state.enabled {
                        state.updater.found(release)
                    } else {
                        Vec::new()
                    }
                }
                Err(err) => {
                    state.check_error = Some(err);
                    Vec::new()
                }
            }
        };
        self.run(commands, emit);
        self.publish(emit);
        true
    }

    /// Busca en GitHub (bloquea).
    pub fn check(self: &Arc<Self>, manual: bool, emit: &Emit) -> bool {
        self.check_with(manual, emit, || shared::check(CURRENT))
    }

    /// La app es más vieja que el servidor: busca la versión nueva fuera del intervalo, si está
    /// encendida, no se sabe aún de ella y no se buscó así en la última hora (bloquea).
    pub fn check_extra(self: &Arc<Self>, emit: &Emit) -> bool {
        self.check_extra_with(emit, || shared::check(CURRENT))
    }

    /// [`Updates::check_extra`] con `fetch`.
    pub fn check_extra_with(
        self: &Arc<Self>,
        emit: &Emit,
        fetch: impl FnOnce() -> Result<Option<Release>, String>,
    ) -> bool {
        {
            let mut state = lock(&self.state);
            let now = shared::unix_now();
            let recent = state
                .last_extra
                .is_some_and(|at| at <= now && now - at < EXTRA_CHECK_BACKOFF_SECS);
            if !state.enabled || state.updater.release().is_some() || recent {
                return false;
            }
            state.last_extra = Some(now);
        }
        self.check_with(true, emit, fetch)
    }

    /// La red pasó a tener límite de datos (o dejó de tenerlo).
    pub fn set_metered(self: &Arc<Self>, metered: bool, emit: &Emit) {
        let commands = lock(&self.state).updater.set_metered(metered);
        self.run(commands, emit);
        self.publish(emit);
    }

    /// «Buscar versiones nuevas».
    pub fn set_enabled(self: &Arc<Self>, enabled: bool, emit: &Emit) {
        let commands = {
            let mut state = lock(&self.state);
            state.enabled = enabled;
            if enabled {
                Vec::new()
            } else {
                state.updater.disable()
            }
        };
        self.run(commands, emit);
        self.publish(emit);
    }

    /// «Actualizar automáticamente».
    pub fn set_auto(self: &Arc<Self>, auto: bool, emit: &Emit) {
        let commands = lock(&self.state).updater.set_auto(auto);
        self.run(commands, emit);
        self.publish(emit);
    }

    /// Se entró en una sala (o se salió).
    pub fn set_room_open(self: &Arc<Self>, open: bool, emit: &Emit) {
        let commands = {
            let mut state = lock(&self.state);
            if state.updater.room_open() == open {
                return;
            }
            state.updater.set_room_open(open)
        };
        self.run(commands, emit);
        self.publish(emit);
    }

    /// «Reintentar».
    pub fn retry(self: &Arc<Self>, emit: &Emit) {
        let commands = lock(&self.state).updater.retry();
        self.run(commands, emit);
        self.publish(emit);
    }

    /// «Descargar y actualizar» (versiones distintas): la descarga e instala al terminar.
    pub fn update_now(self: &Arc<Self>, emit: &Emit) {
        let commands = lock(&self.state).updater.update_now();
        self.run(commands, emit);
        self.publish(emit);
    }

    /// «Instalar ahora»: la ruta del APK lista, o por qué no se puede.
    pub fn install(&self) -> Result<String, String> {
        let state = lock(&self.state);
        if !state.updater.can_restart() {
            return Err(CANNOT_INSTALL.to_owned());
        }
        self.apk_path(&state)
            .ok_or_else(|| CANNOT_INSTALL.to_owned())
    }

    /// Donde está (o estará) el APK de la versión conocida.
    fn apk_path(&self, state: &State) -> Option<String> {
        let release = state.updater.release()?;
        let dir = state.dir.as_ref()?;
        Some(
            dir.join(Package::AndroidApk.asset_name(&release.version))
                .to_string_lossy()
                .into_owned(),
        )
    }

    /// Ejecuta las órdenes de [`Updater`].
    fn run(self: &Arc<Self>, commands: Vec<Command>, emit: &Emit) {
        for command in commands {
            match command {
                Command::Download {
                    release,
                    generation,
                } => self.download(release, generation, emit),
                Command::Pause => self.pause(),
                Command::Discard => {
                    self.pause();
                    self.generation.store(0, Ordering::Release);
                    if let Some(dir) = lock(&self.state).dir.clone() {
                        self.queue(Box::new(move || {
                            let _ = std::fs::remove_dir_all(dir);
                        }));
                    }
                }
                Command::Restart => {
                    let path = self.apk_path(&lock(&self.state));
                    if let Some(path) = path {
                        emit(UiEvent::UpdateInstall { path });
                    }
                }
            }
        }
    }

    fn pause(&self) {
        if let Some(control) = lock(&self.running).take() {
            control.pause();
        }
    }

    fn download(self: &Arc<Self>, release: Release, mine: u64, emit: &Emit) {
        let Some(dir) = lock(&self.state).dir.clone() else {
            self.finished(
                mine,
                Err(Failure::Failed(
                    "falta la carpeta de las descargas".to_owned(),
                )),
                emit,
            );
            return;
        };
        let control = Arc::new(DownloadControl::new());
        if let Some(previous) = lock(&self.running).replace(Arc::clone(&control)) {
            previous.pause();
        }
        self.generation.store(mine, Ordering::Release);
        let emit = Arc::clone(emit);
        let updates = Arc::clone(self);
        self.queue(Box::new(move || {
            let current = || updates.generation.load(Ordering::Acquire) == mine;
            if !current() {
                return;
            }
            let result = download_apk(&release, &dir, &control, &mut |progress| {
                if current() {
                    updates.progress(mine, progress, &emit);
                }
            });
            if !current() {
                return;
            }
            match result {
                Ok(Downloaded::Paused { done, total }) => {
                    updates.paused(mine, Progress::bytes(done, total), &emit);
                }
                Ok(Downloaded::Done(_)) => updates.finished(mine, Ok(()), &emit),
                // Todavía no trae el APK: sin aviso, se vuelve a mirar en una hora.
                Err(DownloadError::Missing) => updates.finished(mine, Err(Failure::Later), &emit),
                Err(DownloadError::Failed(err)) => {
                    updates.finished(mine, Err(Failure::Failed(err)), &emit);
                }
            }
        }));
    }

    /// La descarga avanzó (lo de otra generación no cuenta).
    pub fn progress(&self, generation: u64, progress: Progress, emit: &Emit) {
        let changed = {
            let mut state = lock(&self.state);
            let before = state.updater.stage().clone();
            state.updater.progress(generation, progress);
            // Solo cada punto: la interfaz no necesita cada trozo.
            match (&before, state.updater.stage()) {
                (Stage::Downloading(old), Stage::Downloading(new)) => {
                    old.percent() != new.percent()
                }
                _ => true,
            }
        };
        if changed {
            self.publish(emit);
        }
    }

    /// La descarga se detuvo por la pausa.
    pub fn paused(&self, generation: u64, progress: Progress, emit: &Emit) {
        lock(&self.state).updater.paused(generation, progress);
        self.publish(emit);
    }

    /// La descarga terminó.
    pub fn finished(self: &Arc<Self>, generation: u64, result: Result<(), Failure>, emit: &Emit) {
        let commands = lock(&self.state).updater.finished(generation, result);
        self.run(commands, emit);
        self.publish(emit);
    }

    /// Pone `job` en la cola del hilo de las descargas (que arranca la primera vez): una a la
    /// vez, y un descarte borra antes de que empiece la descarga que lo sigue.
    fn queue(&self, job: Job) {
        let mut jobs = lock(&self.jobs);
        let job = match jobs.as_ref() {
            Some(sender) => match sender.send(job) {
                Ok(()) => return,
                Err(mpsc::SendError(job)) => job,
            },
            None => job,
        };
        let (sender, receiver) = mpsc::channel::<Job>();
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
}

/// Por qué no se bajó el APK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    /// La publicación todavía no trae el APK (se sube después de publicarla).
    Missing,
    /// Otra cosa: el texto.
    Failed(String),
}

/// Descarga el APK de `release` en `dir` y comprueba su SHA-256 (bloquea).
pub fn download_apk(
    release: &Release,
    dir: &Path,
    control: &DownloadControl,
    progress: &mut dyn FnMut(Progress),
) -> Result<Downloaded, DownloadError> {
    let Some(asset) = release.asset(Package::AndroidApk) else {
        return Err(DownloadError::Missing);
    };
    shared::download(
        asset,
        dir,
        &shared::user_agent(CURRENT),
        control,
        &mut |done, total| progress(Progress::bytes(done, total)),
    )
    .map_err(DownloadError::Failed)
}

/// La versión de un APK de las publicaciones, o de su descarga a medias
/// (`coroom-2.1.0-android.apk`, `coroom-2.1.0-android.apk.0123456789abcdef.part` → `2.1.0`).
fn apk_version(name: &str) -> Option<shared::Version> {
    let (version, rest) = name.strip_prefix("coroom-")?.split_once("-android.apk")?;
    (rest.is_empty() || rest.ends_with(".part")).then_some(())?;
    shared::Version::parse(version)
}

/// La versión del APK entero más nuevo que esta en `dir`, si hay alguno.
pub fn newest_apk(dir: &Path) -> Option<shared::Version> {
    let current = shared::Version::parse(CURRENT)?;
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().to_string_lossy().into_owned();
            if name.ends_with(".part") {
                return None;
            }
            apk_version(&name)
        })
        .filter(|version| *version > current)
        .max()
}

/// Borra los APK de esta versión o de antes (ya instalados) y sus descargas a medias.
fn remove_installed(dir: &Path) {
    let Some(current) = shared::Version::parse(CURRENT) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if apk_version(&name).is_some_and(|version| version <= current) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Cuál de los dos quedó atrás, para la interfaz: `client_older` o `server_older`.
pub fn gap_kind(gap: shared::ProtocolGap) -> (&'static str, u16, u16) {
    match gap {
        shared::ProtocolGap::ClientOlder { server, client } => ("client_older", server, client),
        shared::ProtocolGap::ServerOlder { server, client } => ("server_older", server, client),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// Lo que se emite, para mirarlo.
    fn recorder() -> (Arc<Mutex<Vec<UiEvent>>>, Emit) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let emit: Emit = Arc::new(move |event| lock(&sink).push(event));
        (events, emit)
    }

    fn config(dir: &Path, auto: bool) -> Config {
        Config {
            dir: dir.to_string_lossy().into_owned(),
            check: true,
            auto,
            interval_hours: 24,
            last_check: None,
            notice_only: false,
        }
    }

    /// La 2.1.0 con un APK ya descargado en `dir` (así no hace falta la red).
    fn release_in(dir: &Path) -> Release {
        let mut release = Release::new("2.1.0");
        let name = Package::AndroidApk.asset_name("2.1.0");
        std::fs::create_dir_all(dir).expect("carpeta");
        let path = dir.join(&name);
        std::fs::write(&path, b"APK de ejemplo").expect("escribir");
        release.assets.push(shared::Asset {
            url: format!("{}v2.1.0/{name}", shared::DOWNLOADS),
            name,
            size: 14,
            sha256: Some(shared::sha256_file(&path).expect("sha")),
        });
        release
    }

    fn wait_for(updates: &Updates, stage: &str) -> UpdateView {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let view = updates.view();
            if view.stage == stage || Instant::now() > deadline {
                return view;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn checks_only_when_due_and_enabled() {
        let dir = tempfile::tempdir().expect("carpeta");
        let updates = Arc::new(Updates::default());
        let (_, emit) = recorder();
        // Sin configurar está apagada: solo la búsqueda a mano.
        assert!(!updates.check_with(false, &emit, || Ok(None)));
        updates.configure(config(dir.path(), false), &emit);
        assert!(updates.check_with(false, &emit, || Ok(Some(Release::new("2.1.0")))));
        let view = updates.view();
        assert_eq!(
            view.stage, "notice",
            "sin actualización automática, solo el aviso"
        );
        assert_eq!(view.version.as_deref(), Some("2.1.0"));
        assert!(view.last_check.is_some());
        // Acaba de buscar: hasta el siguiente intervalo, nada.
        assert!(!updates.check_with(false, &emit, || panic!("no toca")));
        // Un error no borra lo que se sabía.
        assert!(updates.check_with(true, &emit, || Err("sin red".to_owned())));
        assert_eq!(updates.view().check_error.as_deref(), Some("sin red"));
        assert_eq!(updates.view().version.as_deref(), Some("2.1.0"));
        // «Buscar versiones nuevas» apagado lo olvida todo.
        updates.set_enabled(false, &emit);
        assert_eq!(updates.view().stage, "idle");
        assert_eq!(updates.view().version, None);
    }

    #[test]
    fn downloads_verifies_and_installs_without_a_room() {
        let dir = tempfile::tempdir().expect("carpeta");
        let release = release_in(dir.path());
        let updates = Arc::new(Updates::default());
        let (events, emit) = recorder();
        updates.configure(config(dir.path(), true), &emit);
        // Con una sala abierta no descarga: queda en pausa.
        updates.set_room_open(true, &emit);
        updates.check_with(false, &emit, || Ok(Some(release.clone())));
        assert_eq!(updates.view().stage, "paused");
        assert_eq!(updates.install(), Err(CANNOT_INSTALL.to_owned()));
        // Al salir, sigue, la comprueba y queda lista.
        updates.set_room_open(false, &emit);
        let view = wait_for(&updates, "ready");
        assert_eq!(view.stage, "ready", "{view:?}");
        assert!(view.can_install);
        let apk = dir.path().join(Package::AndroidApk.asset_name("2.1.0"));
        assert_eq!(updates.install(), Ok(apk.to_string_lossy().into_owned()));
        // Lista con una sala abierta: no se instala hasta salir.
        updates.set_room_open(true, &emit);
        assert!(!updates.view().can_install);
        assert_eq!(updates.install(), Err(CANNOT_INSTALL.to_owned()));
        assert!(
            lock(&events)
                .iter()
                .any(|event| matches!(event, UiEvent::Update(view) if view.stage == "ready"))
        );
    }

    #[test]
    fn update_now_installs_when_the_download_ends() {
        let dir = tempfile::tempdir().expect("carpeta");
        let release = release_in(dir.path());
        let updates = Arc::new(Updates::default());
        let (events, emit) = recorder();
        updates.configure(config(dir.path(), false), &emit);
        updates.check_with(false, &emit, || Ok(Some(release.clone())));
        assert_eq!(updates.view().stage, "notice");
        // Sin actualización automática, «Descargar y actualizar» no hace nada (es «Ver»).
        updates.update_now(&emit);
        assert_eq!(updates.view().stage, "notice");
        updates.set_auto(true, &emit);
        updates.update_now(&emit);
        let deadline = Instant::now() + Duration::from_secs(20);
        let apk = dir.path().join(Package::AndroidApk.asset_name("2.1.0"));
        let installed = loop {
            let found = lock(&events).iter().any(
                |event| matches!(event, UiEvent::UpdateInstall { path } if Path::new(path) == apk),
            );
            if found || Instant::now() > deadline {
                break found;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(installed, "al quedar lista se instala");
    }

    /// Al arrancar se borran los APK ya instalados; uno más nuevo bajado antes se vuelve a ofrecer
    /// sin esperar al intervalo.
    #[test]
    fn a_newer_downloaded_apk_is_offered_again_and_old_ones_go() {
        let dir = tempfile::tempdir().expect("carpeta");
        let old = dir.path().join(Package::AndroidApk.asset_name("0.0.1"));
        let partial = dir
            .path()
            .join(format!("{}.part", Package::AndroidApk.asset_name(CURRENT)));
        std::fs::write(&old, b"viejo").expect("escribir");
        std::fs::write(&partial, b"a medias").expect("escribir");
        assert_eq!(newest_apk(dir.path()), None);
        let release = release_in(dir.path());
        assert_eq!(newest_apk(dir.path()), shared::Version::parse("2.1.0"));
        let updates = Arc::new(Updates::default());
        let (_, emit) = recorder();
        let mut config = config(dir.path(), true);
        config.last_check = Some(shared::unix_now());
        updates.configure(config, &emit);
        assert!(
            !old.exists() && !partial.exists(),
            "los de esta versión o antes se borran"
        );
        assert!(updates.check_with(false, &emit, || Ok(Some(release.clone()))));
        assert_eq!(wait_for(&updates, "ready").stage, "ready");
    }

    /// La publicación aún no trae el APK (se sube después): no es un error, y se vuelve a mirar
    /// en una hora.
    #[test]
    fn a_release_without_the_apk_yet_is_quiet() {
        let dir = tempfile::tempdir().expect("carpeta");
        let updates = Arc::new(Updates::default());
        let (_, emit) = recorder();
        updates.configure(config(dir.path(), true), &emit);
        updates.check_with(false, &emit, || Ok(Some(Release::new("2.1.0"))));
        let deadline = Instant::now() + Duration::from_secs(20);
        while updates.view().stage != "idle" && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let view = updates.view();
        assert_eq!(view.stage, "idle", "{view:?}");
        assert_eq!(view.error, None);
        assert_eq!(lock(&updates.state).updater.recheck_hours(24), 1);
    }

    /// Con límite de datos no descarga sola; a mano, sí.
    #[test]
    fn a_metered_network_waits_unless_asked() {
        let dir = tempfile::tempdir().expect("carpeta");
        let release = release_in(dir.path());
        let updates = Arc::new(Updates::default());
        let (_, emit) = recorder();
        updates.configure(config(dir.path(), true), &emit);
        updates.set_metered(true, &emit);
        updates.check_with(false, &emit, || Ok(Some(release.clone())));
        let view = updates.view();
        assert_eq!(view.stage, "paused");
        assert_eq!(view.waiting, Some("metered"));
        updates.update_now(&emit);
        assert_eq!(wait_for(&updates, "ready").stage, "ready");
    }

    /// Una compilación que no puede instalar los APK de las publicaciones solo avisa.
    #[test]
    fn a_build_that_cannot_install_only_announces() {
        let dir = tempfile::tempdir().expect("carpeta");
        let updates = Arc::new(Updates::default());
        let (_, emit) = recorder();
        let mut config = config(dir.path(), true);
        config.notice_only = true;
        updates.configure(config, &emit);
        updates.check_with(false, &emit, || Ok(Some(release_in(dir.path()))));
        assert_eq!(updates.view().stage, "notice");
    }

    /// La búsqueda fuera del intervalo (app más vieja que el servidor): una por hora como mucho, y
    /// nunca con la búsqueda apagada.
    #[test]
    fn extra_checks_are_rate_limited() {
        let dir = tempfile::tempdir().expect("carpeta");
        let updates = Arc::new(Updates::default());
        let (_, emit) = recorder();
        assert!(!updates.check_extra_with(&emit, || panic!("apagada")));
        let mut config = config(dir.path(), false);
        config.last_check = Some(shared::unix_now());
        updates.configure(config, &emit);
        assert!(updates.check_extra_with(&emit, || Err("sin red".to_owned())));
        assert!(!updates.check_extra_with(&emit, || panic!("en menos de una hora")));
    }

    /// Las descargas a medias se reconocen por su nombre (con el SHA-256 abreviado).
    #[test]
    fn partial_downloads_are_recognised() {
        assert_eq!(
            apk_version("coroom-2.1.0-android.apk.0123456789abcdef.part"),
            shared::Version::parse("2.1.0")
        );
        assert_eq!(apk_version("coroom-2.1.0-android.apk.otra"), None);
    }
}
