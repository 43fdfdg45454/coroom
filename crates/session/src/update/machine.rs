//! Cuándo se descarga, se pausa y se instala una versión nueva.
//!
//! [`Updater`] no hace nada por sí mismo: recibe lo que pasa (la búsqueda encontró una versión, se
//! abrió o cerró una sala, avanzó o terminó la descarga, el usuario pidió reiniciar) y devuelve
//! [`Command`]s que la plataforma ejecuta (descargar con el portal de Flatpak o por HTTPS, pausar,
//! descartar lo bajado, reiniciar). Las reglas, iguales en el escritorio y en Android:
//!
//! - Solo se descarga sin una sala abierta (propia o ajena, como anfitrión o invitado, también
//!   mientras se entra, y también la de otro proceso de coroom en este equipo): para no quitarle
//!   ancho de banda al juego. Abrir una sala pausa la descarga y cerrarla la reanuda.
//! - En una red con límite de datos (Android) la descarga automática espera a una sin límite; la
//!   que se pide a mano sí descarga.
//! - Nunca se reinicia con una sala abierta ni sin que lo pida el usuario. «Descargar y
//!   actualizar» reinicia al terminar solo si no se abrió ninguna sala entretanto.
//! - Con la actualización automática apagada, o en un paquete que no se actualiza solo, solo se
//!   avisa.
//! - Una publicación que todavía no trae el paquete (se sube después de publicarla) no es un
//!   error: se vuelve a mirar en una hora ([`Updater::recheck_hours`]).
//! - Una versión que no se pudo instalar ([`Updater::block`]) no se vuelve a bajar sola: solo con
//!   «Reintentar».
//! - Cada descarga lleva su generación ([`Command::Download`]); lo que avise una anterior (ya
//!   descartada o reemplazada) se ignora.

use super::Release;

/// Cómo se actualiza esta copia de la aplicación.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// Descarga el paquete de la publicación y lo instala (el zip portátil de Windows, el APK).
    Package(super::Package),
    /// El portal de Flatpak descarga e instala desde el repositorio Flatpak del proyecto.
    Flatpak,
    /// Solo avisa: este paquete no puede actualizarse solo.
    Notice(NoticeOnly),
}

/// Por qué una copia solo avisa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeOnly {
    /// El MSIX de Windows: cada versión va firmada con un certificado nuevo y no se instala encima.
    Msix,
    /// Compilada a mano (o un paquete sin camino de actualización).
    Unpackaged,
}

/// Avance de una descarga.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Progress {
    /// Fracción hecha, de 0 a 1.
    pub fraction: f32,
    /// Tamaño del paquete en bytes, si se sabe (el portal de Flatpak solo da el porcentaje).
    pub total_bytes: Option<u64>,
}

impl Progress {
    /// `done` de `total` bytes.
    pub fn bytes(done: u64, total: u64) -> Self {
        Self {
            fraction: if total == 0 {
                0.0
            } else {
                (done as f64 / total as f64).clamp(0.0, 1.0) as f32
            },
            total_bytes: Some(total),
        }
    }

    /// Porcentaje entero (lo que se enseña).
    pub fn percent(self) -> u32 {
        (self.fraction * 100.0).floor().clamp(0.0, 100.0) as u32
    }
}

/// En qué punto está.
#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    /// Sin versión nueva (o sin buscar todavía, o con el paquete aún sin subir).
    Idle,
    /// Hay una versión nueva y solo se avisa.
    Notice,
    /// Descargando.
    Downloading(Progress),
    /// Descarga en pausa ([`Updater::waiting`] dice por qué).
    Paused(Progress),
    /// Descargada y lista: se usa al reiniciar o la próxima vez que se abra la aplicación.
    Ready,
    /// La descarga falló (se reintenta en la próxima búsqueda): por qué.
    Failed(String),
    /// Se descargó pero no se pudo instalar: no se reintenta sola, solo con «Reintentar».
    InstallFailed(String),
    /// La carpeta de la aplicación no se puede escribir (Windows, dentro de Archivos de programa):
    /// la carpeta.
    Unwritable(String),
}

/// Por qué está en pausa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// Hay una sala abierta (en esta aplicación o en otro proceso de coroom).
    Room,
    /// La red tiene límite de datos (la descarga automática espera a una sin límite).
    Metered,
}

/// Por qué no terminó una descarga.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// La red, GitHub o la comprobación: el texto.
    Failed(String),
    /// No se puede escribir en esta carpeta.
    Unwritable(String),
    /// Todavía no (la publicación aún no trae el paquete, el repositorio de Flatpak aún no tiene la
    /// versión, u otro proceso de coroom ya está descargando): sin aviso, se vuelve a mirar en una
    /// hora.
    Later,
    /// Esta instalación no puede actualizarse sola (un Flatpak instalado de un fichero, sin
    /// repositorio): desde ahora solo se avisa.
    Unsupported,
}

/// Lo que la plataforma tiene que hacer.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Descargar (o seguir descargando) esta versión y dejarla lista. Lo que avise esta descarga
    /// lleva `generation` ([`Updater::progress`] y compañía ignoran el de otra).
    Download {
        /// La versión.
        release: Release,
        /// La generación de esta descarga.
        generation: u64,
    },
    /// Pausar la descarga en curso (lo bajado se queda para seguir después).
    Pause,
    /// Parar y borrar lo que se haya bajado o preparado.
    Discard,
    /// Cerrar la aplicación y abrir la versión nueva.
    Restart,
}

/// El aviso de Inicio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeNotice {
    /// Nada (también mientras descarga: el progreso está en Acerca de).
    None,
    /// Hay una versión nueva y solo se avisa, con «Ver la versión nueva».
    Available,
    /// Lista para instalar; `can_restart` es falso con una sala abierta.
    Ready {
        /// Si se puede reiniciar ya.
        can_restart: bool,
    },
    /// No se pudo descargar: por qué.
    Failed(String),
    /// No se pudo instalar: por qué.
    InstallFailed(String),
    /// No se puede actualizar en esta carpeta.
    Unwritable(String),
}

/// La versión nueva de esta copia: qué hay, en qué punto está y qué toca hacer.
#[derive(Debug, Clone, PartialEq)]
pub struct Updater {
    channel: Channel,
    auto: bool,
    room_open: bool,
    /// Otro proceso de coroom tiene una sala abierta (lo avisa la descarga en curso).
    held: bool,
    metered: bool,
    release: Option<Release>,
    stage: Stage,
    /// Se pidió «Descargar y actualizar»: al quedar lista, se reinicia (si no se abrió una sala).
    restart_when_ready: bool,
    /// La descarga en curso la pidió el usuario (descarga también con límite de datos).
    manual: bool,
    generation: u64,
    /// La última descarga contestó [`Failure::Later`].
    later: bool,
    /// Versión que no se pudo instalar, y por qué.
    blocked: Option<(String, String)>,
}

impl Updater {
    /// Para esta copia, con la actualización automática encendida o no.
    pub fn new(channel: Channel, auto: bool) -> Self {
        Self {
            channel,
            auto,
            room_open: false,
            held: false,
            metered: false,
            release: None,
            stage: Stage::Idle,
            restart_when_ready: false,
            manual: false,
            generation: 0,
            later: false,
            blocked: None,
        }
    }

    /// Cómo se actualiza esta copia.
    pub fn channel(&self) -> Channel {
        self.channel
    }

    /// La versión nueva conocida.
    pub fn release(&self) -> Option<&Release> {
        self.release.as_ref()
    }

    /// En qué punto está.
    pub fn stage(&self) -> &Stage {
        &self.stage
    }

    /// Si hay una sala abierta.
    pub fn room_open(&self) -> bool {
        self.room_open
    }

    /// Si se actualiza sola.
    pub fn auto(&self) -> bool {
        self.auto
    }

    /// La generación de la última descarga pedida (o del último descarte).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Por qué está en pausa (`None` si no lo está).
    pub fn waiting(&self) -> Option<Wait> {
        if !matches!(self.stage, Stage::Paused(_)) {
            return None;
        }
        if self.metered && !self.manual && !self.room_open && !self.held {
            Some(Wait::Metered)
        } else {
            Some(Wait::Room)
        }
    }

    /// Cada cuántas horas toca buscar: `interval_hours` o, si la publicación aún no traía el
    /// paquete, una.
    pub fn recheck_hours(&self, interval_hours: u32) -> u32 {
        if self.later { 1 } else { interval_hours.max(1) }
    }

    /// Si esta copia puede descargar e instalar sola (con la actualización automática encendida).
    pub fn installs(&self) -> bool {
        self.auto && !matches!(self.channel, Channel::Notice(_))
    }

    /// Si se puede reiniciar ya para usar la versión nueva.
    pub fn can_restart(&self) -> bool {
        self.stage == Stage::Ready && !self.room_open
    }

    /// Si «Descargar y actualizar» dejará reiniciar al terminar.
    pub fn restart_when_ready(&self) -> bool {
        self.restart_when_ready
    }

    /// La versión que no se pudo instalar (y por qué), si la hay.
    pub fn blocked(&self) -> Option<(&str, &str)> {
        self.blocked
            .as_ref()
            .map(|(version, reason)| (version.as_str(), reason.as_str()))
    }

    /// `version` no se pudo instalar por `reason`: no se vuelve a bajar sola (sí con
    /// «Reintentar»).
    pub fn block(&mut self, version: &str, reason: &str) {
        self.blocked = Some((version.to_owned(), reason.to_owned()));
    }

    /// El aviso de Inicio para este estado.
    pub fn home_notice(&self) -> HomeNotice {
        if self.release.is_none() {
            return HomeNotice::None;
        }
        match &self.stage {
            Stage::Idle | Stage::Downloading(_) | Stage::Paused(_) => HomeNotice::None,
            Stage::Notice => HomeNotice::Available,
            Stage::Ready => HomeNotice::Ready {
                can_restart: !self.room_open,
            },
            Stage::Failed(err) => HomeNotice::Failed(err.clone()),
            Stage::InstallFailed(err) => HomeNotice::InstallFailed(err.clone()),
            Stage::Unwritable(folder) => HomeNotice::Unwritable(folder.clone()),
        }
    }

    fn same_release(&self, release: &Release) -> bool {
        self.release
            .as_ref()
            .is_some_and(|known| known.version == release.version)
    }

    /// Si la descarga tiene que esperar ahora.
    fn must_wait(&self) -> bool {
        self.room_open || self.held || (self.metered && !self.manual)
    }

    /// Empieza (o sigue) la descarga de la versión conocida, o la deja en pausa si toca esperar.
    fn start(&mut self, from: Progress) -> Vec<Command> {
        let Some(release) = self.release.clone() else {
            return Vec::new();
        };
        if self.must_wait() {
            self.stage = Stage::Paused(from);
            return Vec::new();
        }
        self.stage = Stage::Downloading(from);
        self.generation += 1;
        vec![Command::Download {
            release,
            generation: self.generation,
        }]
    }

    /// Para y tira lo bajado: lo que avise la descarga anterior ya no cuenta.
    fn discard(&mut self) -> Command {
        self.generation += 1;
        self.held = false;
        Command::Discard
    }

    /// Resultado de una búsqueda: la versión nueva, o `None` si esta es la última.
    pub fn found(&mut self, release: Option<Release>) -> Vec<Command> {
        self.later = false;
        let Some(release) = release else {
            // Lo que ya está en marcha o listo sigue (la publicación pudo retirarse un momento).
            if matches!(
                self.stage,
                Stage::Downloading(_) | Stage::Paused(_) | Stage::Ready
            ) {
                return Vec::new();
            }
            self.release = None;
            self.stage = Stage::Idle;
            self.restart_when_ready = false;
            return Vec::new();
        };
        if self.same_release(&release)
            && matches!(
                self.stage,
                Stage::Downloading(_) | Stage::Paused(_) | Stage::Ready | Stage::InstallFailed(_)
            )
        {
            return Vec::new();
        }
        let replaced =
            matches!(self.stage, Stage::Ready | Stage::Paused(_)) && !self.same_release(&release);
        self.release = Some(release);
        let mut commands = Vec::new();
        if replaced {
            // Una más nueva que la que estaba lista o a medias: se tira y se baja la nueva.
            commands.push(self.discard());
        }
        if !self.installs() {
            self.stage = Stage::Notice;
            return commands;
        }
        let blocked = self.blocked.as_ref().and_then(|(version, reason)| {
            self.release
                .as_ref()
                .is_some_and(|release| &release.version == version)
                .then(|| reason.clone())
        });
        if let Some(reason) = blocked {
            // No se pudo instalar: solo a mano.
            self.stage = Stage::InstallFailed(reason);
            return commands;
        }
        self.manual = false;
        commands.extend(self.start(Progress::default()));
        commands
    }

    /// Se encendió o apagó «Actualizar automáticamente».
    pub fn set_auto(&mut self, auto: bool) -> Vec<Command> {
        if self.auto == auto {
            return Vec::new();
        }
        self.auto = auto;
        if self.release.is_none() || matches!(self.channel, Channel::Notice(_)) {
            return Vec::new();
        }
        if auto {
            return match self.stage {
                Stage::Notice | Stage::Failed(_) | Stage::Unwritable(_) => {
                    self.manual = false;
                    self.start(Progress::default())
                }
                _ => Vec::new(),
            };
        }
        // Apagada: solo el aviso; lo bajado o preparado se tira.
        let commands = self.stop();
        self.stage = Stage::Notice;
        self.restart_when_ready = false;
        self.manual = false;
        commands
    }

    /// Las órdenes para parar y tirar lo que haya.
    fn stop(&mut self) -> Vec<Command> {
        match self.stage {
            Stage::Downloading(_) => vec![Command::Pause, self.discard()],
            Stage::Paused(_) | Stage::Ready => {
                if self.held {
                    vec![Command::Pause, self.discard()]
                } else {
                    vec![self.discard()]
                }
            }
            _ => Vec::new(),
        }
    }

    /// «Buscar versiones nuevas» apagado: se olvida todo y se para lo que hubiera.
    pub fn disable(&mut self) -> Vec<Command> {
        let commands = self.stop();
        self.release = None;
        self.stage = Stage::Idle;
        self.restart_when_ready = false;
        self.manual = false;
        self.later = false;
        commands
    }

    /// Se abrió (o cerró) una sala en esta aplicación.
    pub fn set_room_open(&mut self, open: bool) -> Vec<Command> {
        if self.room_open == open {
            return Vec::new();
        }
        self.room_open = open;
        if open {
            // Lo que pidió «Descargar y actualizar» ya no vale: al cerrar la sala solo queda lista.
            self.restart_when_ready = false;
            return self.hold_on();
        }
        self.go_on()
    }

    /// La red pasó a tener límite de datos (o dejó de tenerlo). Solo espera la descarga
    /// automática.
    pub fn set_metered(&mut self, metered: bool) -> Vec<Command> {
        if self.metered == metered {
            return Vec::new();
        }
        self.metered = metered;
        if metered {
            if self.manual {
                return Vec::new();
            }
            return self.hold_on();
        }
        self.go_on()
    }

    /// Pausa lo que se esté descargando.
    fn hold_on(&mut self) -> Vec<Command> {
        match &self.stage {
            Stage::Downloading(progress) => {
                self.stage = Stage::Paused(*progress);
                vec![Command::Pause]
            }
            // La descarga esperaba a que otro proceso cerrara su sala: que pare.
            Stage::Paused(_) if self.held => {
                self.held = false;
                vec![Command::Pause]
            }
            _ => Vec::new(),
        }
    }

    /// Sigue lo que estaba en pausa, si ya no hay que esperar.
    fn go_on(&mut self) -> Vec<Command> {
        match &self.stage {
            Stage::Paused(progress) if !self.must_wait() => {
                let progress = *progress;
                self.start(progress)
            }
            _ => Vec::new(),
        }
    }

    /// La descarga avanzó.
    pub fn progress(&mut self, generation: u64, progress: Progress) {
        if generation != self.generation {
            return;
        }
        if let Stage::Downloading(current) = &mut self.stage {
            *current = progress;
        }
    }

    /// La descarga se detuvo por la pausa, con lo que llevaba.
    pub fn paused(&mut self, generation: u64, progress: Progress) {
        if generation != self.generation {
            return;
        }
        self.held = false;
        if let Stage::Paused(current) | Stage::Downloading(current) = &mut self.stage {
            *current = progress;
        }
    }

    /// La descarga espera porque otro proceso de coroom tiene una sala abierta.
    pub fn held(&mut self, generation: u64, progress: Progress) {
        if generation != self.generation {
            return;
        }
        if let Stage::Downloading(_) = self.stage {
            self.stage = Stage::Paused(progress);
            self.held = true;
        }
    }

    /// El otro proceso cerró su sala y la descarga sigue.
    pub fn resumed(&mut self, generation: u64) {
        if generation != self.generation || !self.held {
            return;
        }
        self.held = false;
        if let Stage::Paused(progress) = self.stage {
            self.stage = Stage::Downloading(progress);
        }
    }

    /// La descarga terminó: lista, o por qué no.
    pub fn finished(&mut self, generation: u64, result: Result<(), Failure>) -> Vec<Command> {
        if generation != self.generation
            || !matches!(self.stage, Stage::Downloading(_) | Stage::Paused(_))
        {
            // Ya se apagó, se descartó o se reemplazó por otra.
            return Vec::new();
        }
        self.held = false;
        self.manual = false;
        let restart = std::mem::take(&mut self.restart_when_ready);
        match result {
            Ok(()) => {
                self.stage = Stage::Ready;
                if restart && !self.room_open {
                    return vec![Command::Restart];
                }
            }
            Err(Failure::Failed(err)) => self.stage = Stage::Failed(err),
            Err(Failure::Unwritable(folder)) => self.stage = Stage::Unwritable(folder),
            Err(Failure::Later) => {
                // Sin aviso: no es un error, es que todavía no está.
                self.stage = Stage::Idle;
                self.release = None;
                self.later = true;
            }
            Err(Failure::Unsupported) => {
                self.channel = Channel::Notice(NoticeOnly::Unpackaged);
                self.stage = Stage::Notice;
            }
        }
        Vec::new()
    }

    /// «Reintentar» tras un fallo.
    pub fn retry(&mut self) -> Vec<Command> {
        if !matches!(
            self.stage,
            Stage::Failed(_) | Stage::InstallFailed(_) | Stage::Unwritable(_)
        ) || !self.installs()
        {
            return Vec::new();
        }
        self.manual = true;
        self.start(Progress::default())
    }

    /// «Reiniciar y actualizar»: solo con la versión lista y sin sala abierta.
    pub fn restart(&mut self) -> Option<Command> {
        self.can_restart().then_some(Command::Restart)
    }

    /// «Actualizar y reiniciar» o «Descargar y actualizar» del aviso de versiones distintas: si está
    /// lista, reinicia; si no, la descarga (o sigue) y reinicia al terminar si no se abre una sala
    /// entretanto.
    pub fn update_now(&mut self) -> Vec<Command> {
        // Sin actualización automática el aviso solo lleva a la página de la versión.
        if self.release.is_none() || !self.installs() || self.room_open {
            return Vec::new();
        }
        match &self.stage {
            Stage::Ready => self.restart().into_iter().collect(),
            Stage::Downloading(_) => {
                self.restart_when_ready = true;
                self.manual = true;
                Vec::new()
            }
            Stage::Paused(progress) => {
                let progress = *progress;
                self.restart_when_ready = true;
                self.manual = true;
                if self.held {
                    // Espera a la sala del otro proceso: la descarga sigue sola al cerrarse.
                    return Vec::new();
                }
                self.start(progress)
            }
            Stage::Idle
            | Stage::Notice
            | Stage::Failed(_)
            | Stage::InstallFailed(_)
            | Stage::Unwritable(_) => {
                self.restart_when_ready = true;
                self.manual = true;
                self.start(Progress::default())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::Package;
    use super::*;

    fn zip() -> Updater {
        Updater::new(Channel::Package(Package::WindowsZip), true)
    }

    fn release(version: &str) -> Release {
        Release::new(version)
    }

    fn download(version: &str, generation: u64) -> Command {
        Command::Download {
            release: release(version),
            generation,
        }
    }

    #[test]
    fn a_new_release_downloads_on_its_own_and_waits_to_restart() {
        let mut up = zip();
        assert_eq!(up.home_notice(), HomeNotice::None);
        assert_eq!(up.found(Some(release("2.1.0"))), [download("2.1.0", 1)]);
        assert_eq!(up.stage(), &Stage::Downloading(Progress::default()));
        assert_eq!(
            up.home_notice(),
            HomeNotice::None,
            "mientras baja, Inicio calla"
        );
        up.progress(1, Progress::bytes(34, 100));
        assert_eq!(up.stage(), &Stage::Downloading(Progress::bytes(34, 100)));
        // Otra búsqueda de la misma versión no vuelve a empezar.
        assert!(up.found(Some(release("2.1.0"))).is_empty());
        assert!(
            up.finished(1, Ok(())).is_empty(),
            "no reinicia sin preguntar"
        );
        assert_eq!(up.stage(), &Stage::Ready);
        assert_eq!(up.home_notice(), HomeNotice::Ready { can_restart: true });
        assert_eq!(up.restart(), Some(Command::Restart));
    }

    #[test]
    fn opening_a_room_pauses_and_closing_it_resumes() {
        let mut up = zip();
        up.found(Some(release("2.1.0")));
        up.progress(1, Progress::bytes(34, 100));
        assert_eq!(up.set_room_open(true), [Command::Pause]);
        assert_eq!(up.stage(), &Stage::Paused(Progress::bytes(34, 100)));
        assert_eq!(up.waiting(), Some(Wait::Room));
        up.paused(1, Progress::bytes(35, 100));
        assert_eq!(up.stage(), &Stage::Paused(Progress::bytes(35, 100)));
        // El avance que llegue tarde no la saca de la pausa.
        up.progress(1, Progress::bytes(40, 100));
        assert_eq!(up.stage(), &Stage::Paused(Progress::bytes(35, 100)));
        assert_eq!(up.set_room_open(false), [download("2.1.0", 2)]);
        assert_eq!(up.stage(), &Stage::Downloading(Progress::bytes(35, 100)));
    }

    #[test]
    fn reports_of_an_older_download_are_ignored() {
        let mut up = zip();
        up.found(Some(release("2.1.0")));
        up.finished(1, Ok(()));
        // Una más nueva reemplaza a la lista: lo que avise la anterior ya no cuenta.
        assert_eq!(
            up.found(Some(release("2.2.0"))),
            [Command::Discard, download("2.2.0", 3)]
        );
        up.progress(1, Progress::bytes(90, 100));
        assert!(up.finished(1, Ok(())).is_empty());
        assert_eq!(up.stage(), &Stage::Downloading(Progress::default()));
        up.progress(3, Progress::bytes(10, 100));
        assert_eq!(up.stage(), &Stage::Downloading(Progress::bytes(10, 100)));
        up.finished(3, Ok(()));
        assert_eq!(up.stage(), &Stage::Ready);
    }

    #[test]
    fn nothing_downloads_with_a_room_open() {
        let mut up = zip();
        up.set_room_open(true);
        assert!(up.found(Some(release("2.1.0"))).is_empty());
        assert!(matches!(up.stage(), Stage::Paused(_)));
        assert!(up.update_now().is_empty(), "tampoco a mano");
        assert_eq!(up.set_room_open(false), [download("2.1.0", 1)]);
    }

    #[test]
    fn a_room_in_another_process_holds_the_download() {
        let mut up = zip();
        up.found(Some(release("2.1.0")));
        up.held(1, Progress::bytes(20, 100));
        assert_eq!(up.stage(), &Stage::Paused(Progress::bytes(20, 100)));
        assert_eq!(up.waiting(), Some(Wait::Room));
        up.resumed(1);
        assert_eq!(up.stage(), &Stage::Downloading(Progress::bytes(20, 100)));
        // Si mientras espera se abre una sala aquí, la descarga para y sigue al cerrarla.
        up.held(1, Progress::bytes(30, 100));
        assert_eq!(up.set_room_open(true), [Command::Pause]);
        up.paused(1, Progress::bytes(30, 100));
        assert_eq!(up.set_room_open(false), [download("2.1.0", 2)]);
    }

    #[test]
    fn a_metered_network_holds_automatic_downloads_only() {
        let mut up = Updater::new(Channel::Package(Package::AndroidApk), true);
        up.set_metered(true);
        assert!(up.found(Some(release("2.1.0"))).is_empty());
        assert_eq!(up.waiting(), Some(Wait::Metered));
        assert_eq!(up.set_metered(false), [download("2.1.0", 1)]);
        assert_eq!(up.set_metered(true), [Command::Pause]);
        // A mano sí descarga, también con límite de datos.
        assert_eq!(up.update_now(), [download("2.1.0", 2)]);
        assert!(up.set_metered(false).is_empty());
        assert!(up.set_metered(true).is_empty(), "la manual sigue");
    }

    #[test]
    fn never_restarts_with_a_room_open() {
        let mut up = zip();
        up.found(Some(release("2.1.0")));
        up.finished(1, Ok(()));
        up.set_room_open(true);
        assert_eq!(up.restart(), None);
        assert!(!up.can_restart());
        assert_eq!(up.home_notice(), HomeNotice::Ready { can_restart: false });
        assert!(up.update_now().is_empty());
        assert!(
            up.set_room_open(false).is_empty(),
            "cerrar la sala no reinicia solo"
        );
        assert_eq!(up.restart(), Some(Command::Restart));
    }

    #[test]
    fn download_and_update_restarts_only_if_no_room_opened_meanwhile() {
        // Sin actualización automática el aviso solo lleva a la página: no descarga.
        let mut up = zip();
        up.set_auto(false);
        up.found(Some(release("2.1.0")));
        assert_eq!(up.stage(), &Stage::Notice);
        assert!(up.update_now().is_empty());
        assert!(!up.restart_when_ready());

        // Tras un fallo, «Descargar y actualizar» la vuelve a bajar y reinicia al terminar.
        let mut up = zip();
        up.found(Some(release("2.1.0")));
        up.finished(1, Err(Failure::Failed("sin red".to_owned())));
        assert_eq!(up.update_now(), [download("2.1.0", 2)]);
        assert!(up.restart_when_ready());
        assert_eq!(up.finished(2, Ok(())), [Command::Restart]);

        // Se abre una sala a medias: pausa y se olvida el reinicio; al cerrarla sigue y queda
        // lista, sin reiniciar.
        let mut up = zip();
        up.found(Some(release("2.1.0")));
        up.update_now();
        assert!(up.restart_when_ready());
        assert_eq!(up.set_room_open(true), [Command::Pause]);
        assert!(!up.restart_when_ready());
        assert_eq!(up.set_room_open(false), [download("2.1.0", 2)]);
        assert!(up.finished(2, Ok(())).is_empty());
        assert_eq!(up.stage(), &Stage::Ready);

        // Terminó justo al abrir la sala: lista, y cerrarla no reinicia.
        let mut up = zip();
        up.found(Some(release("2.1.0")));
        up.update_now();
        up.set_room_open(true);
        assert!(up.finished(1, Ok(())).is_empty());
        assert!(up.set_room_open(false).is_empty());
        assert!(up.can_restart());
    }

    #[test]
    fn auto_off_or_notice_only_packages_just_announce() {
        let mut up = Updater::new(Channel::Notice(NoticeOnly::Msix), true);
        assert!(up.found(Some(release("2.1.0"))).is_empty());
        assert_eq!(up.stage(), &Stage::Notice);
        assert_eq!(up.home_notice(), HomeNotice::Available);
        assert!(up.update_now().is_empty());

        let mut up = Updater::new(Channel::Flatpak, false);
        assert!(up.found(Some(release("2.1.0"))).is_empty());
        assert_eq!(up.stage(), &Stage::Notice);
        assert_eq!(up.set_auto(true), [download("2.1.0", 1)]);
        assert_eq!(up.set_auto(false), [Command::Pause, Command::Discard]);
        assert_eq!(up.stage(), &Stage::Notice);
    }

    #[test]
    fn failures_are_retried_on_the_next_check_or_by_hand() {
        let mut up = zip();
        up.found(Some(release("2.1.0")));
        assert!(
            up.finished(
                1,
                Err(Failure::Failed("GitHub no respondió a tiempo".into()))
            )
            .is_empty()
        );
        assert_eq!(
            up.home_notice(),
            HomeNotice::Failed("GitHub no respondió a tiempo".into())
        );
        assert_eq!(up.retry(), [download("2.1.0", 2)]);
        up.finished(
            2,
            Err(Failure::Unwritable("C:\\Program Files\\coroom".into())),
        );
        assert_eq!(
            up.home_notice(),
            HomeNotice::Unwritable("C:\\Program Files\\coroom".into())
        );
        assert_eq!(
            up.found(Some(release("2.1.0"))),
            [download("2.1.0", 3)],
            "la búsqueda siguiente reintenta"
        );
    }

    #[test]
    fn a_package_not_uploaded_yet_is_quiet_and_rechecked_soon() {
        let mut up = zip();
        assert_eq!(up.recheck_hours(24), 24);
        up.found(Some(release("2.1.0")));
        assert!(up.finished(1, Err(Failure::Later)).is_empty());
        assert_eq!(up.stage(), &Stage::Idle);
        assert_eq!(up.home_notice(), HomeNotice::None, "no es un error");
        assert_eq!(up.recheck_hours(24), 1);
        assert_eq!(up.found(Some(release("2.1.0"))), [download("2.1.0", 2)]);
        assert_eq!(up.recheck_hours(24), 24);
    }

    #[test]
    fn a_flatpak_without_a_repository_only_announces() {
        let mut up = Updater::new(Channel::Flatpak, true);
        up.found(Some(release("2.1.0")));
        assert!(up.finished(1, Err(Failure::Unsupported)).is_empty());
        assert_eq!(up.home_notice(), HomeNotice::Available);
        assert!(!up.installs());
        assert!(up.found(Some(release("2.2.0"))).is_empty());
        assert_eq!(up.stage(), &Stage::Notice);
    }

    #[test]
    fn a_version_that_failed_to_install_is_only_retried_by_hand() {
        let mut up = zip();
        up.block("2.1.0", "en uso");
        assert!(up.found(Some(release("2.1.0"))).is_empty());
        assert_eq!(up.home_notice(), HomeNotice::InstallFailed("en uso".into()));
        assert!(
            up.found(Some(release("2.1.0"))).is_empty(),
            "ni al día siguiente"
        );
        assert_eq!(up.retry(), [download("2.1.0", 1)]);
        // Una versión más nueva sí se baja sola.
        let mut up = zip();
        up.block("2.1.0", "en uso");
        assert_eq!(up.found(Some(release("2.2.0"))), [download("2.2.0", 1)]);
    }

    #[test]
    fn a_newer_release_replaces_the_ready_one_and_disabling_forgets_it() {
        let mut up = zip();
        up.found(Some(release("2.1.0")));
        up.finished(1, Ok(()));
        assert_eq!(
            up.found(Some(release("2.2.0"))),
            [Command::Discard, download("2.2.0", 3)]
        );
        assert!(up.found(None).is_empty(), "lo que baja sigue");
        assert_eq!(up.disable(), [Command::Pause, Command::Discard]);
        assert_eq!(up.stage(), &Stage::Idle);
        assert!(up.release().is_none());
        // Un resultado tardío de la descarga ya no cambia nada.
        assert!(up.finished(3, Ok(())).is_empty());
        assert_eq!(up.stage(), &Stage::Idle);
    }

    #[test]
    fn progress_is_a_fraction_with_its_size() {
        let p = Progress::bytes(20_740_000, 61_000_000);
        assert_eq!(p.percent(), 34);
        assert_eq!(p.total_bytes, Some(61_000_000));
        assert_eq!(Progress::bytes(5, 0).percent(), 0);
        assert_eq!(Progress::bytes(100, 100).percent(), 100);
    }
}
