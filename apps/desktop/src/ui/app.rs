//! Estado de la aplicación, mensajes y efectos.
//!
//! [`App`] es **puro**: [`App::update`] reduce un [`Message`] a estado nuevo y devuelve los
//! [`Effect`] que el resto del programa tiene que hacer (llamar a la API, abrir una sala, guardar
//! los ajustes, poner la ventana a pantalla completa…); [`App::view`] dibuja el estado. Así la
//! lógica de cada pantalla se prueba sin ventana ni red (`tests` de cada pantalla) y las capturas
//! se hacen con datos de ejemplo. El [`super::backend::Backend`] ejecuta los efectos y devuelve sus
//! resultados como [`Message::Event`]; el [`super::runner`] lleva la ventana, el teclado y el ratón.

use std::sync::Arc;
use std::time::{Duration, Instant};

use coroom_session::input::gamepad::PadChoice;
use coroom_session::remote::{GuestStatus, QualityLevel, SessionEvent};
use coroom_session::{GuestId, Permissions};
use coroom_transport::api::ServerProbe;
use coroom_transport::proto::api::{Devices, Friends, Usage};
use coroom_ui::theme::{Accent, Mode, Tone};
use coroom_ui::widget::{self as ui};
use coroom_ui::{Element, Theme, font::Fonts};
use iced_core::{Alignment, Length, Padding};
use iced_widget::{column, container, row};

use super::screens::{friends, guest, home, host, login, settings as settings_screen};
use super::settings::{ServerEntry, Settings, ThemeChoice};
use super::system::SystemAppearance;
use super::video::VideoFeed;
use crate::update::Update;

/// Lo que este equipo puede hacer con 4:4:4 y HDR, para explicarlo en Ajustes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VideoSupport {
    /// Códecs que codifica en 4:4:4 y con qué (`H.264 (NVENC)`).
    pub yuv444: Vec<(coroom_session::codec::Codec, String)>,
    /// Códecs que codifica en HDR10.
    pub hdr10: Vec<(coroom_session::codec::Codec, String)>,
    /// Códecs que decodifica en 4:4:4 (como invitado).
    pub decode_yuv444: Vec<coroom_session::codec::Codec>,
    /// Códecs que decodifica en HDR10.
    pub decode_hdr10: Vec<coroom_session::codec::Codec>,
    /// Si la ventana puede presentar en HDR (`None`: aún no se sabe).
    pub hdr_display: Option<bool>,
}

/// Estado del aviso de versión nueva.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UpdateState {
    /// Comprobando ahora.
    pub checking: bool,
    /// La comprobación la pidió el usuario (se le contesta con un toast).
    pub manual: bool,
    /// Versión nueva publicada.
    pub available: Option<Update>,
    /// Último error (se reintenta en el siguiente intervalo).
    pub error: Option<String>,
    /// Cuándo toca la próxima comprobación automática.
    pub next_check: Option<Instant>,
}

/// Cuenta con la que se entró.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountInfo {
    /// Servidor.
    pub server: ServerEntry,
    /// Usuario.
    pub username: String,
    /// Nombre visible.
    pub display_name: String,
    /// Identificador del usuario.
    pub user_id: u64,
    /// Nombre de este dispositivo.
    pub device: String,
    /// Huella de la identidad de este equipo.
    pub fingerprint: String,
}

impl AccountInfo {
    /// Host del servidor (`coroom.example.com`).
    pub fn host(&self) -> String {
        host_of(&self.server.url)
    }
}

/// El host de una URL de servidor, para mostrarlo.
pub fn host_of(url: &str) -> String {
    let rest = url
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    rest.split(['/', '?']).next().unwrap_or(rest).to_owned()
}

/// Una invitación de un amigo a su sala.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invitation {
    /// Usuario que invita.
    pub from_user: u64,
    /// Su nombre.
    pub from_name: String,
    /// Código de la sala.
    pub code: String,
}

/// Algo que se carga de la red.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Load<T> {
    /// Todavía no se pidió.
    #[default]
    Idle,
    /// Pedido.
    Loading,
    /// Llegó.
    Loaded(T),
    /// Falló (texto en español con la causa).
    Failed(String),
}

impl<T> Load<T> {
    /// Lo cargado, si hay.
    pub fn loaded(&self) -> Option<&T> {
        match self {
            Load::Loaded(value) => Some(value),
            _ => None,
        }
    }

    /// Si está cargando.
    pub fn is_loading(&self) -> bool {
        matches!(self, Load::Loading)
    }
}

/// Páginas con barra lateral.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    /// Inicio.
    #[default]
    Home,
    /// Amigos.
    Friends,
    /// Compartir (anfitrión).
    Host,
    /// Ajustes.
    Settings,
}

/// Pantalla.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Servidor e inicio de sesión.
    Login,
    /// Páginas con barra lateral.
    Main,
    /// Invitado: conexión y vídeo a toda la ventana.
    Guest,
}

/// Atajos globales (los traduce la ventana).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    /// `Ctrl+Q`.
    Quit,
    /// `Ctrl+,`.
    Settings,
    /// `F11`.
    Fullscreen,
    /// `Esc`.
    Escape,
}

/// Lo que ha llegado de la red o de una sesión (lo envía el backend).
#[derive(Debug, Clone)]
pub enum Event {
    /// Comprobación de un servidor.
    Probed {
        /// URL comprobada.
        url: String,
        /// Resultado.
        result: Result<ServerProbe, String>,
    },
    /// Sesión iniciada (o recuperada al arrancar).
    SignedIn(Result<AccountInfo, String>),
    /// No había sesión guardada que recuperar.
    NoSavedSession,
    /// Sesión cerrada.
    SignedOut(Result<String, String>),
    /// Lista de amigos.
    Friends(Result<Friends, String>),
    /// Resultado de pedir, aceptar o quitar un amigo (texto para un toast).
    FriendChanged(Result<String, String>),
    /// Dispositivos de la cuenta.
    Devices(Result<Devices, String>),
    /// Dispositivo desvinculado.
    DeviceRevoked(Result<String, String>),
    /// Uso del relay.
    Usage(Result<Usage, String>),
    /// Un amigo te invita (por la señalización de espera).
    Invited(Invitation),
    /// Un amigo se conectó o desconectó.
    Presence {
        /// Usuario.
        user: u64,
        /// Si está conectado.
        online: bool,
    },
    /// Salida de `coroom doctor`.
    Doctor(Vec<String>),
    /// Micrófonos y salidas.
    AudioDevices(Result<Vec<AudioDevice>, String>),
    /// Anfitrión: la sala está arrancando (con su vista previa) o no pudo.
    HostStarted(Result<Arc<VideoFeed>, String>),
    /// Anfitrión: un evento de la sesión.
    Host(SessionEvent),
    /// Anfitrión: estado de los invitados.
    HostGuests(Vec<GuestStatus>),
    /// Anfitrión: la vista previa no está disponible (captura en la GPU).
    PreviewUnavailable,
    /// Anfitrión: la sala se cerró (`Some` con el motivo si fue un error).
    HostEnded(Option<String>),
    /// Anfitrión: se cambió lo que se comparte con la sala abierta, o no se pudo.
    SourceSwitched(Result<(), String>),
    /// Anfitrión: el vídeo de la sala abierta ya usa el bitrate máximo (kbit/s) y el códec de los
    /// ajustes, o no se pudo.
    VideoChanged(Result<u32, String>),
    /// Invitado: la sesión arrancó (con la fuente del vídeo) o no pudo.
    GuestStarted(Result<Arc<VideoFeed>, String>),
    /// Invitado: un evento de la sesión.
    Guest(SessionEvent),
    /// Invitado: estadísticas de ahora.
    GuestStats(Box<guest::Stats>),
    /// Invitado: la sesión terminó (`Some` con el motivo si no fue normal).
    GuestEnded(Option<String>),
    /// Resultado de buscar una versión nueva, con la hora (segundos desde 1970) de la consulta.
    UpdateChecked {
        /// `Ok(None)`: esta es la última.
        result: Result<Option<Update>, String>,
        /// Cuándo se consultó.
        at: u64,
    },
    /// Lo que este equipo hace con 4:4:4 y HDR.
    VideoSupport(VideoSupport),
    /// Lo que se puede compartir.
    ShareOptions(host::ShareOptions),
    /// Lo que se eligió en el selector del sistema (`Ok(None)`: nada, o lo canceló).
    SharePicked(
        host::SourceChoice,
        Result<Option<coroom_session::capture::PickedShare>, String>,
    ),
    /// Un efecto no pudo hacerse (aviso para el usuario).
    Failed(String),
}

/// Micrófono o salida.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    /// Nombre con el que se elige.
    pub name: String,
    /// Descripción.
    pub description: String,
    /// Micrófono (`true`) o salida.
    pub input: bool,
    /// Si es el del sistema.
    pub default: bool,
}

/// Mensaje de la interfaz.
#[derive(Debug, Clone)]
pub enum Message {
    /// Cambiar de página.
    Navigate(Page),
    /// Pasa el tiempo (animaciones, toasts, barra del vídeo).
    Tick,
    /// Atajo global.
    Shortcut(Shortcut),
    /// La ventana entró o salió de pantalla completa.
    FullscreenChanged(bool),
    /// El ratón se movió (la barra del vídeo reaparece).
    PointerMoved,
    /// Cierra el diálogo abierto.
    CloseDialog,
    /// Confirma el diálogo abierto.
    ConfirmDialog,
    /// Quita un toast.
    DismissToast(u64),
    /// Copiar al portapapeles.
    Copy(String),
    /// Abrir un enlace en el navegador (la página de una versión nueva).
    OpenUrl(String),
    /// Cambió la apariencia del sistema.
    System(SystemAppearance),
    /// Cerrar sesión (pide confirmación).
    SignOut,
    /// Volver a la lista de servidores.
    SwitchServer,
    /// Abrir o cerrar el menú de la cuenta.
    ToggleAccountMenu,
    /// Inicio de sesión.
    Login(login::Msg),
    /// Inicio.
    Home(home::Msg),
    /// Amigos.
    Friends(friends::Msg),
    /// Anfitrión.
    Host(host::Msg),
    /// Invitado.
    Guest(guest::Msg),
    /// Ajustes.
    Settings(settings_screen::Msg),
    /// Resultado de la red o de una sesión.
    Event(Event),
}

/// Petición de abrir una sala.
#[derive(Debug, Clone, PartialEq)]
pub struct HostRequest {
    /// Fuente.
    pub source: host::SourceChoice,
    /// Pantalla o aplicación, y el destino elegido.
    pub share: coroom_session::capture::ShareChoice,
    /// Audio del juego.
    pub audio: coroom_session::audio::GameCapture,
    /// Voz del anfitrión.
    pub voice: bool,
    /// Los invitados entran callados.
    pub guests_muted: bool,
}

/// Lo que la aplicación pide hacer.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Comprobar un servidor.
    Probe {
        /// URL.
        url: String,
        /// Puerto TCP del respaldo.
        tcp_port: Option<u16>,
    },
    /// Iniciar sesión.
    SignIn {
        /// Servidor.
        server: ServerEntry,
        /// Usuario.
        username: String,
        /// Contraseña.
        password: String,
        /// Nombre de este dispositivo.
        device: String,
        /// Código de invitación y nombre visible para crear la cuenta antes.
        invitation: Option<(String, Option<String>)>,
    },
    /// Recuperar la sesión guardada del último servidor.
    Restore,
    /// Recuperar la sesión guardada de un servidor elegido (al cambiar de servidor).
    Resume(ServerEntry),
    /// Cerrar sesión.
    SignOut,
    /// Cargar los amigos.
    LoadFriends,
    /// Pedir amistad.
    AddFriend(String),
    /// Aceptar una solicitud.
    AcceptFriend(u64),
    /// Rechazar o retirar una solicitud.
    DeclineFriend(u64),
    /// Quitar un amigo.
    RemoveFriend(u64),
    /// Cargar los dispositivos.
    LoadDevices,
    /// Desvincular un dispositivo.
    RevokeDevice(u64),
    /// Cargar el uso del relay.
    LoadUsage,
    /// Escuchar invitaciones y presencia mientras no hay sesión.
    StartLobby,
    /// Dejar de escuchar.
    StopLobby,
    /// Abrir una sala.
    StartHost(HostRequest),
    /// Cerrar la sala.
    StopHost,
    /// Compartir otra cosa con la sala abierta (`audio`: el de la fuente nueva; `voice` no
    /// cuenta).
    SwitchSource(HostRequest),
    /// Cambiar qué aplicaciones suenan en la sala abierta.
    SetAppAudio(coroom_session::audio::GameCapture),
    /// Aplicar a la sala abierta el códec, el codificador y el bitrate máximo de los ajustes.
    SetVideo,
    /// Dejar de oír (o volver a oír) una voz en este equipo: en la sala propia (`host`, por id de
    /// invitado) o en la que se está como invitado (por hoja MLS).
    SetVoiceMuted {
        /// Si es la sala que este equipo comparte.
        host: bool,
        /// Emisor de la voz.
        source: u32,
        /// Silenciada.
        muted: bool,
    },
    /// Permisos de un invitado.
    SetPermissions(GuestId, Permissions),
    /// Expulsar.
    Kick(GuestId),
    /// Pausar o reanudar la entrada de todos.
    PauseInput(bool),
    /// Invitar a un amigo a la sala.
    Invite(u64),
    /// Entrar en una sala.
    Join(String),
    /// Salir de la sala.
    Leave,
    /// Reintentar la conexión ya.
    Reconnect,
    /// Alternar el micrófono del invitado.
    ToggleMic,
    /// Alternar el audio del juego del invitado.
    ToggleGameAudio,
    /// Pasar el ratón a modo inmersivo (`true`) o escritorio.
    SetImmersive(bool),
    /// Qué mando envía el invitado.
    SetPadChoice(PadChoice),
    /// Guardar los ajustes.
    SaveSettings,
    /// Copiar al portapapeles.
    Copy(String),
    /// Pantalla completa.
    SetFullscreen(bool),
    /// Salir de la aplicación.
    Quit,
    /// Diagnóstico.
    RunDoctor,
    /// Dispositivos de audio.
    LoadAudioDevices,
    /// Buscar una versión nueva.
    CheckUpdates,
    /// Abrir un enlace en el navegador.
    OpenUrl(String),
    /// Ver qué hace este equipo con 4:4:4 y HDR.
    LoadVideoSupport,
    /// Mirar qué se puede compartir con esa fuente (monitores o ventanas y lo que suena).
    LoadShareOptions(host::SourceChoice),
    /// Abrir el selector del sistema para elegir ya el monitor o la ventana.
    PickShare(host::SourceChoice),
}

/// Toast.
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    /// Identificador.
    pub id: u64,
    /// Tono.
    pub tone: Tone,
    /// Texto.
    pub text: String,
    /// Cuándo apareció.
    pub shown_at: Instant,
}

/// Diálogo modal.
#[derive(Debug, Clone, PartialEq)]
pub enum Dialog {
    /// Quitar a un amigo.
    RemoveFriend {
        /// Usuario.
        user: u64,
        /// Nombre.
        name: String,
    },
    /// Expulsar a un invitado.
    Kick {
        /// Invitado.
        guest: GuestId,
        /// Nombre.
        name: String,
    },
    /// Dejar de compartir.
    StopSharing,
    /// Salir de la sala (invitado).
    Leave,
    /// Cerrar sesión.
    SignOut,
    /// Salir de la aplicación con la sala abierta.
    QuitWhileHosting,
    /// Desvincular un dispositivo.
    RevokeDevice {
        /// Dispositivo.
        device: u64,
        /// Nombre.
        name: String,
    },
    /// La sesión del invitado terminó.
    SessionEnded {
        /// Título.
        title: String,
        /// Explicación.
        text: String,
    },
    /// Número de seguridad de un par.
    SafetyNumber {
        /// Nombre.
        name: String,
        /// Huella.
        fingerprint: String,
        /// Número.
        number: String,
    },
}

/// La aplicación.
#[derive(Debug)]
pub struct App {
    /// Ajustes (se guardan con [`Effect::SaveSettings`]).
    pub settings: Settings,
    /// Lo que dice el sistema de su apariencia.
    pub system: SystemAppearance,
    /// Fuentes elegidas.
    pub fonts: Fonts,
    /// Tema en uso.
    pub theme: Theme,
    /// Pantalla.
    pub screen: Screen,
    /// Página de la barra lateral.
    pub page: Page,
    /// Cuenta.
    pub account: Option<AccountInfo>,
    /// Amigos (Inicio, Amigos y el anfitrión los usan).
    pub friends: Load<Friends>,
    /// Uso del relay (avisos de cuota).
    pub usage: Option<Usage>,
    /// Invitaciones recibidas.
    pub invitations: Vec<Invitation>,
    /// Inicio de sesión.
    pub login: login::State,
    /// Inicio.
    pub home: home::State,
    /// Amigos.
    pub friends_page: friends::State,
    /// Anfitrión.
    pub host: host::State,
    /// Invitado.
    pub guest: Option<guest::State>,
    /// Ajustes.
    pub settings_page: settings_screen::State,
    /// Toasts.
    pub toasts: Vec<Toast>,
    next_toast: u64,
    /// Diálogo abierto.
    pub dialog: Option<Dialog>,
    /// Menú de la cuenta abierto.
    pub account_menu: bool,
    /// A pantalla completa.
    pub fullscreen: bool,
    /// Ahora (lo mueve [`Message::Tick`]).
    pub now: Instant,
    /// Prueba local (`coroom probe` con ventana): solo el vídeo.
    pub probe: bool,
    /// Versión de la aplicación.
    pub version: &'static str,
    /// Aviso de versión nueva.
    pub update: UpdateState,
    /// 4:4:4 y HDR en este equipo (`None` hasta que se sondea).
    pub video_support: Option<VideoSupport>,
}

impl App {
    /// Aplicación con estos ajustes y la apariencia del sistema.
    pub fn new(settings: Settings, system: SystemAppearance, fonts: Fonts, now: Instant) -> Self {
        let login = login::State::new(&settings);
        let mut app = Self {
            theme: Theme::new(Mode::Light, Accent::Blue, fonts),
            settings,
            system,
            fonts,
            screen: Screen::Login,
            page: Page::Home,
            account: None,
            friends: Load::Idle,
            usage: None,
            invitations: Vec::new(),
            login,
            home: home::State::default(),
            friends_page: friends::State::default(),
            host: host::State::default(),
            guest: None,
            settings_page: settings_screen::State::default(),
            toasts: Vec::new(),
            next_toast: 1,
            dialog: None,
            account_menu: false,
            fullscreen: false,
            now,
            probe: false,
            version: env!("CARGO_PKG_VERSION"),
            update: UpdateState::default(),
            video_support: None,
        };
        app.refresh_theme();
        app
    }

    /// Modo efectivo (el elegido o el del sistema).
    pub fn mode(&self) -> Mode {
        match self.settings.appearance.theme {
            ThemeChoice::Light => Mode::Light,
            ThemeChoice::Dark => Mode::Dark,
            ThemeChoice::System => self.system.mode.unwrap_or(Mode::Light),
        }
    }

    /// Acento efectivo (el elegido o el del sistema).
    pub fn accent(&self) -> Accent {
        let key = self.settings.appearance.accent.as_str();
        if key == "system" {
            self.system.accent.unwrap_or_default()
        } else {
            Accent::from_key(key).unwrap_or_default()
        }
    }

    /// Recalcula el tema con los ajustes y el sistema.
    pub fn refresh_theme(&mut self) {
        self.theme = Theme::new(self.mode(), self.accent(), self.fonts);
    }

    /// Fuentes de vídeo que hay que dar por mostradas tras dibujar.
    pub fn feeds(&self) -> Vec<Arc<VideoFeed>> {
        let mut feeds = Vec::new();
        if let Some(guest) = &self.guest {
            feeds.push(Arc::clone(&guest.feed));
        }
        if let Some(preview) = self.host.preview() {
            feeds.push(preview);
        }
        feeds
    }

    /// Si hay algo animándose (hay que dibujar a menudo).
    pub fn animating(&self) -> bool {
        let quick = Duration::from_millis(260);
        self.toasts
            .iter()
            .any(|toast| self.now.duration_since(toast.shown_at) < quick)
            || self.login.probe.is_checking()
            || self.guest.as_ref().is_some_and(guest::State::animating)
            || self.settings_page.doctor.is_loading()
            || self.host.starting
    }

    /// Cuándo hace falta el próximo [`Message::Tick`].
    pub fn next_tick(&self) -> Option<Instant> {
        if self.animating() {
            return Some(self.now + Duration::from_millis(16));
        }
        let mut next: Option<Instant> = None;
        let mut consider = |at: Instant| {
            next = Some(next.map_or(at, |next| next.min(at)));
        };
        for toast in &self.toasts {
            consider(toast.shown_at + coroom_ui::tokens::motion::TOAST);
        }
        if let Some(at) = self.update.next_check {
            consider(at);
        }
        if self.page == Page::Host
            && self.screen == Screen::Main
            && let Some(at) = self.host.next_refresh(self.now)
        {
            // Si ya toca, el próximo `Tick` pide la lista y deja de tocar (nunca un bucle).
            consider(at);
        }
        if let Some(guest) = &self.guest
            && let Some(at) = guest.next_tick()
        {
            consider(at);
        }
        next
    }

    /// Añade un toast.
    pub fn toast(&mut self, tone: Tone, text: impl Into<String>) {
        let id = self.next_toast;
        self.next_toast += 1;
        self.toasts.push(Toast {
            id,
            tone,
            text: text.into(),
            shown_at: self.now,
        });
        if self.toasts.len() > 3 {
            self.toasts.remove(0);
        }
    }

    /// Reduce un mensaje.
    pub fn update(&mut self, message: Message) -> Vec<Effect> {
        match message {
            Message::Tick => {
                let toast_life = coroom_ui::tokens::motion::TOAST;
                let now = self.now;
                self.toasts
                    .retain(|toast| now.duration_since(toast.shown_at) < toast_life);
                if let Some(guest) = &mut self.guest {
                    guest.tick(now);
                }
                let mut effects = Vec::new();
                if self.update.next_check.is_some_and(|at| at <= now) {
                    self.update.next_check = None;
                    effects.extend(self.check_updates(false));
                }
                if self.page == Page::Host
                    && self.screen == Screen::Main
                    && self.host.refresh_due(now)
                {
                    effects.extend(host::refresh(self));
                }
                effects
            }
            Message::Navigate(page) => self.navigate(page),
            Message::Shortcut(shortcut) => self.shortcut(shortcut),
            Message::FullscreenChanged(fullscreen) => {
                self.fullscreen = fullscreen;
                Vec::new()
            }
            Message::PointerMoved => {
                if let Some(guest) = &mut self.guest {
                    guest.pointer_moved(self.now);
                }
                Vec::new()
            }
            Message::CloseDialog => {
                self.dialog = None;
                Vec::new()
            }
            Message::ConfirmDialog => self.confirm_dialog(),
            Message::DismissToast(id) => {
                self.toasts.retain(|toast| toast.id != id);
                Vec::new()
            }
            Message::Copy(text) => {
                self.toast(Tone::Success, t!("app-copiado-al-portapapeles"));
                vec![Effect::Copy(text)]
            }
            Message::OpenUrl(url) => vec![Effect::OpenUrl(url)],
            Message::System(system) => {
                self.system = system;
                self.refresh_theme();
                Vec::new()
            }
            Message::SignOut => {
                self.account_menu = false;
                self.dialog = Some(Dialog::SignOut);
                Vec::new()
            }
            Message::SwitchServer => {
                self.account_menu = false;
                let mut effects = vec![Effect::StopLobby];
                if self.host.is_open() {
                    effects.push(Effect::StopHost);
                }
                // La cuenta sigue abierta: «Volver» lleva de nuevo a ella y elegir un servidor con
                // la sesión guardada entra sin pedir la contraseña.
                self.screen = Screen::Login;
                self.login.step = login::Step::Servers;
                self.login.error = None;
                effects
            }
            Message::ToggleAccountMenu => {
                self.account_menu = !self.account_menu;
                Vec::new()
            }
            Message::Login(msg) => login::update(self, msg),
            Message::Home(msg) => home::update(self, msg),
            Message::Friends(msg) => friends::update(self, msg),
            Message::Host(msg) => host::update(self, msg),
            Message::Guest(msg) => guest::update(self, msg),
            Message::Settings(msg) => settings_screen::update(self, msg),
            Message::Event(event) => self.event(event),
        }
    }

    /// Programa la comprobación automática de versión: ya, si toca (`due`) y está activada.
    pub fn schedule_update_check(&mut self, due: bool) {
        self.update.next_check =
            (due && self.settings.updates.check && !crate::update::disabled_by_env())
                .then_some(self.now);
    }

    /// Pide buscar una versión nueva (`manual`: lo pidió el usuario y se le contesta).
    pub fn check_updates(&mut self, manual: bool) -> Vec<Effect> {
        if self.update.checking {
            return Vec::new();
        }
        if !manual && (!self.settings.updates.check || crate::update::disabled_by_env()) {
            return Vec::new();
        }
        self.update.checking = true;
        self.update.manual = manual;
        vec![Effect::CheckUpdates]
    }

    fn update_checked(&mut self, result: Result<Option<Update>, String>, at: u64) -> Vec<Effect> {
        let manual = std::mem::take(&mut self.update.manual);
        self.update.checking = false;
        let hours = u64::from(self.settings.updates.interval_hours.max(1));
        self.update.next_check = (self.settings.updates.check && !crate::update::disabled_by_env())
            .then(|| self.now + Duration::from_secs(hours * 3600));
        match result {
            Ok(update) => {
                self.update.error = None;
                if manual {
                    match &update {
                        Some(update) => self.toast(
                            Tone::Info,
                            t!("app-hay-una-version-nueva", version = update.version),
                        ),
                        None => self.toast(Tone::Success, t!("app-tienes-la-ultima-version")),
                    }
                }
                self.update.available = update;
                self.settings.updates.last_check = Some(at);
                vec![Effect::SaveSettings]
            }
            Err(err) => {
                if manual {
                    self.toast(Tone::Warning, t!("app-no-se-pudo-comprobar-err", err = err));
                }
                self.update.error = Some(err);
                Vec::new()
            }
        }
    }

    fn navigate(&mut self, page: Page) -> Vec<Effect> {
        self.account_menu = false;
        if self.screen != Screen::Main {
            return Vec::new();
        }
        self.page = page;
        let mut effects = Vec::new();
        match page {
            Page::Friends | Page::Home if !self.friends.is_loading() => {
                self.friends = match std::mem::take(&mut self.friends) {
                    Load::Loaded(friends) => Load::Loaded(friends),
                    _ => Load::Loading,
                };
                effects.push(Effect::LoadFriends);
            }
            Page::Settings => effects.extend(settings_screen::on_open(self)),
            Page::Host if !self.host.refreshing => effects.extend(host::refresh(self)),
            _ => {}
        }
        effects
    }

    fn shortcut(&mut self, shortcut: Shortcut) -> Vec<Effect> {
        match shortcut {
            Shortcut::Quit => {
                if self.host.is_open() {
                    self.dialog = Some(Dialog::QuitWhileHosting);
                    Vec::new()
                } else {
                    vec![Effect::Quit]
                }
            }
            Shortcut::Settings => {
                if self.screen == Screen::Main {
                    self.navigate(Page::Settings)
                } else {
                    Vec::new()
                }
            }
            Shortcut::Fullscreen => vec![Effect::SetFullscreen(!self.fullscreen)],
            Shortcut::Escape => {
                if self.dialog.take().is_some() {
                    return Vec::new();
                }
                if self.account_menu {
                    self.account_menu = false;
                    return Vec::new();
                }
                if self.fullscreen {
                    return vec![Effect::SetFullscreen(false)];
                }
                if self.screen == Screen::Guest {
                    if self.probe {
                        return vec![Effect::Quit];
                    }
                    self.dialog = Some(Dialog::Leave);
                }
                Vec::new()
            }
        }
    }

    fn confirm_dialog(&mut self) -> Vec<Effect> {
        let Some(dialog) = self.dialog.take() else {
            return Vec::new();
        };
        match dialog {
            Dialog::RemoveFriend { user, .. } => vec![Effect::RemoveFriend(user)],
            Dialog::Kick { guest, .. } => vec![Effect::Kick(guest)],
            Dialog::StopSharing => {
                self.host.stopping = true;
                vec![Effect::StopHost]
            }
            Dialog::Leave => guest::leave(self),
            Dialog::SignOut => {
                let mut effects = vec![Effect::StopLobby];
                if self.host.is_open() {
                    effects.push(Effect::StopHost);
                }
                effects.push(Effect::SignOut);
                effects
            }
            Dialog::QuitWhileHosting => vec![Effect::StopHost, Effect::Quit],
            Dialog::RevokeDevice { device, .. } => vec![Effect::RevokeDevice(device)],
            Dialog::SessionEnded { .. } => guest::close(self),
            Dialog::SafetyNumber { .. } => Vec::new(),
        }
    }

    fn event(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::Probed { .. } | Event::SignedIn(_) | Event::NoSavedSession => {
                login::on_event(self, event)
            }
            Event::SignedOut(result) => {
                self.account = None;
                self.friends = Load::Idle;
                self.invitations.clear();
                self.screen = Screen::Login;
                self.login.step = login::Step::Servers;
                self.login.password.clear();
                match result {
                    Ok(text) => self.toast(Tone::Success, text),
                    Err(err) => self.toast(Tone::Warning, err),
                }
                Vec::new()
            }
            Event::Friends(result) => {
                self.friends = match result {
                    Ok(friends) => Load::Loaded(friends),
                    Err(err) => Load::Failed(err),
                };
                Vec::new()
            }
            Event::FriendChanged(result) => {
                self.friends_page.busy = false;
                match result {
                    Ok(text) => {
                        self.friends_page.add_name.clear();
                        self.friends_page.add_error = None;
                        self.toast(Tone::Success, text);
                        vec![Effect::LoadFriends]
                    }
                    Err(err) => {
                        self.friends_page.add_error = Some(err);
                        Vec::new()
                    }
                }
            }
            Event::Presence { user, online } => {
                if let Load::Loaded(friends) = &mut self.friends
                    && let Some(friend) = friends.friends.iter_mut().find(|f| f.id == user)
                {
                    friend.online = online;
                }
                Vec::new()
            }
            Event::Invited(invitation) => {
                self.invitations
                    .retain(|known| known.from_user != invitation.from_user);
                self.toast(
                    Tone::Info,
                    t!(
                        "app-te-invita-a-su-sala",
                        from_name = invitation.from_name,
                        code = invitation.code
                    ),
                );
                self.invitations.insert(0, invitation);
                Vec::new()
            }
            Event::Usage(result) => {
                self.usage = result.ok();
                Vec::new()
            }
            Event::Devices(_)
            | Event::DeviceRevoked(_)
            | Event::Doctor(_)
            | Event::AudioDevices(_) => settings_screen::on_event(self, event),
            Event::HostStarted(_)
            | Event::Host(_)
            | Event::HostGuests(_)
            | Event::PreviewUnavailable
            | Event::HostEnded(_)
            | Event::SourceSwitched(_)
            | Event::VideoChanged(_) => host::on_event(self, event),
            Event::GuestStarted(_)
            | Event::Guest(_)
            | Event::GuestStats(_)
            | Event::GuestEnded(_) => guest::on_event(self, event),
            Event::UpdateChecked { result, at } => self.update_checked(result, at),
            Event::ShareOptions(options) => {
                host::on_share_options(self, options);
                Vec::new()
            }
            Event::SharePicked(source, picked) => host::on_share_picked(self, source, picked),
            Event::VideoSupport(support) => {
                self.video_support = Some(support);
                Vec::new()
            }
            Event::Failed(err) => {
                self.toast(Tone::Error, err);
                Vec::new()
            }
        }
    }

    /// Dibuja la aplicación.
    pub fn view(&self) -> Element<'_, Message> {
        let content: Element<'_, Message> = match self.screen {
            Screen::Login => login::view(self),
            Screen::Guest => match &self.guest {
                Some(state) => guest::view(self, state),
                None => login::view(self),
            },
            Screen::Main => self.shell(),
        };
        let content = container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .class(coroom_ui::style::ContainerClass::Window);
        let toasts = self
            .toasts
            .iter()
            .map(|toast| ui::toast(toast.tone, toast.text.as_str()))
            .collect();
        let offset = self
            .toasts
            .last()
            .map(|toast| {
                let age = self.now.duration_since(toast.shown_at).as_secs_f32();
                let t = (age / coroom_ui::tokens::motion::PANEL.as_secs_f32()).min(1.0);
                // `ease-out`: rápido al principio, suave al llegar.
                (1.0 - (1.0 - t) * (1.0 - t)) * -24.0 + 24.0
            })
            .unwrap_or(0.0);
        let with_toasts = ui::with_toasts(content, toasts, offset);
        ui::modal(
            with_toasts,
            self.dialog.as_ref().map(|dialog| self.dialog_view(dialog)),
            Message::CloseDialog,
        )
    }

    fn shell(&self) -> Element<'_, Message> {
        let page: Element<'_, Message> = match self.page {
            Page::Home => home::view(self),
            Page::Friends => friends::view(self),
            Page::Host => host::view(self),
            Page::Settings => settings_screen::view(self),
        };
        row![self.sidebar(), page].height(Length::Fill).into()
    }

    fn sidebar(&self) -> Element<'_, Message> {
        use coroom_ui::icon::Icon;
        let pending = self
            .friends
            .loaded()
            .map_or(0, |friends| friends.incoming.len());
        let item = |icon, label: &str, page: Page| ui::NavItem {
            icon,
            label: label.to_owned(),
            selected: self.page == page,
            on_press: Message::Navigate(page),
            badge: None,
        };
        let mut items = vec![item(Icon::Home, t!("app-inicio"), Page::Home)];
        let mut friends_item = item(Icon::Users, t!("app-amigos"), Page::Friends);
        if pending > 0 {
            friends_item.badge = Some(pending.to_string());
        }
        items.push(friends_item);
        if self.host.is_open() || self.page == Page::Host {
            let mut sharing = item(Icon::ShareScreen, t!("app-compartir"), Page::Host);
            if self.host.is_open() {
                sharing.badge = Some(t!("app-en-vivo").to_owned());
            }
            items.push(sharing);
        }
        items.push(item(Icon::Settings, t!("app-ajustes"), Page::Settings));
        let header = row![
            coroom_ui::icon::icon_with(Icon::RadioTower, 22.0, coroom_ui::style::SvgClass::Accent),
            ui::heading("coroom")
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .padding(Padding::from([8.0, 8.0]));
        ui::sidebar(header.into(), items, self.account_footer())
    }

    fn account_footer(&self) -> Element<'_, Message> {
        use coroom_ui::icon::{Icon, icon_with};
        use coroom_ui::style::{ButtonClass, ContainerClass, SvgClass};
        use iced_core::text::Wrapping;
        use iced_widget::button;
        let Some(account) = &self.account else {
            return ui::gap(0.0);
        };
        let who = row![
            ui::avatar(&self.theme, &account.display_name, 32.0),
            column![
                ui::strong(account.display_name.as_str()),
                ui::caption(account.host())
            ]
            .spacing(2)
            .width(Length::Fill),
            ui::icon_button(
                Icon::Menu,
                t!("app-cuenta"),
                Some(Message::ToggleAccountMenu)
            )
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        if !self.account_menu {
            return container(who).padding(Padding::from([4.0, 4.0])).into();
        }
        // Filas de menú: una sola línea, sin el alto fijo de los botones (no cabe en la barra).
        let item = |label: &'static str, icon: Icon, destructive: bool, message: Message| {
            let (class, tint) = if destructive {
                (ButtonClass::DestructiveFlat, SvgClass::Tone(Tone::Error))
            } else {
                (ButtonClass::Flat, SvgClass::Text)
            };
            button(
                row![
                    icon_with(icon, 18.0, tint),
                    ui::strong(label).wrapping(Wrapping::None)
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([8.0, 10.0]))
            .width(Length::Fill)
            .class(class)
            .on_press(message)
        };
        let menu = container(
            column![
                item(
                    t!("app-cambiar-de-servidor"),
                    Icon::Server,
                    false,
                    Message::SwitchServer
                ),
                item(
                    t!("app-cerrar-sesion"),
                    Icon::LogOut,
                    true,
                    Message::SignOut
                ),
            ]
            .spacing(2),
        )
        .padding(4)
        .width(Length::Fill)
        .class(ContainerClass::Card);
        column![menu, container(who).padding(Padding::from([4.0, 4.0]))]
            .spacing(8)
            .into()
    }

    fn dialog_view<'a>(&'a self, dialog: &'a Dialog) -> Element<'a, Message> {
        let cancel = || ui::normal(t!("app-cancelar"), Some(Message::CloseDialog)).into();
        let confirm = |label: &'static str, destructive: bool| -> Element<'_, Message> {
            if destructive {
                ui::destructive(label, Some(Message::ConfirmDialog)).into()
            } else {
                ui::primary(label, Some(Message::ConfirmDialog)).into()
            }
        };
        let fill = || ui::fill();
        let (title, text, actions): (String, String, Vec<Element<'_, Message>>) = match dialog {
            Dialog::RemoveFriend { name, .. } => (
                t!("app-quitar-a-name-de-tus-amigos", name = name),
                t!("app-dejareis-de-ver-si-el-otro", name = name),
                vec![fill(), cancel(), confirm(t!("app-quitar"), true)],
            ),
            Dialog::Kick { name, .. } => (
                t!("app-expulsar-a-name", name = name),
                t!("app-sale-de-la-sala-al-momento").to_owned(),
                vec![fill(), cancel(), confirm(t!("app-expulsar"), true)],
            ),
            Dialog::StopSharing => (
                t!("app-dejar-de-compartir").to_owned(),
                t!("app-la-sala-se-cierra-y-todos").to_owned(),
                vec![
                    fill(),
                    cancel(),
                    confirm(t!("app-dejar-de-compartir-2"), true),
                ],
            ),
            Dialog::Leave => (
                t!("app-salir-de-la-sala").to_owned(),
                t!("app-dejaras-de-ver-y-controlar-la").to_owned(),
                vec![fill(), cancel(), confirm(t!("app-salir"), true)],
            ),
            Dialog::SignOut => (
                t!("app-cerrar-sesion-2").to_owned(),
                t!("app-se-borran-las-credenciales-de-este").to_owned(),
                vec![fill(), cancel(), confirm(t!("app-cerrar-sesion"), true)],
            ),
            Dialog::QuitWhileHosting => (
                t!("app-cerrar-la-sala-y-salir").to_owned(),
                t!("app-estas-compartiendo-la-pantalla-al-salir").to_owned(),
                vec![fill(), cancel(), confirm(t!("app-cerrar-y-salir"), true)],
            ),
            Dialog::RevokeDevice { name, .. } => (
                t!("app-cerrar-la-sesion-de-name", name = name),
                t!("app-ese-dispositivo-queda-desvinculado-de-tu").to_owned(),
                vec![fill(), cancel(), confirm(t!("app-cerrar-sesion"), true)],
            ),
            Dialog::SessionEnded { title, text } => (
                title.clone(),
                text.clone(),
                vec![fill(), confirm(t!("app-volver-al-inicio"), false)],
            ),
            Dialog::SafetyNumber {
                name,
                fingerprint,
                number,
            } => {
                let body = column![
                    ui::body(t!("app-compara-estos-numeros-con-name-por", name = name))
                        .wrapping(iced_core::text::Wrapping::WordOrGlyph),
                    ui::copy_field(number.as_str(), Message::Copy(number.clone())),
                    ui::caption(t!(
                        "app-huella-de-name-fingerprint",
                        name = name,
                        fingerprint = fingerprint
                    ))
                    .wrapping(iced_core::text::Wrapping::Glyph),
                ]
                .spacing(12);
                return ui::dialog(
                    t!("app-numero-de-seguridad-con-name", name = name),
                    body,
                    vec![
                        fill(),
                        ui::normal(t!("app-cerrar"), Some(Message::CloseDialog)).into(),
                    ],
                );
            }
        };
        ui::dialog(
            title,
            ui::body(text).wrapping(iced_core::text::Wrapping::WordOrGlyph),
            actions,
        )
    }
}

/// Texto de la calidad para la interfaz.
pub fn quality_tone(level: QualityLevel) -> Tone {
    match level {
        QualityLevel::Excellent | QualityLevel::Good => Tone::Success,
        QualityLevel::Acceptable => Tone::Warning,
        QualityLevel::Poor => Tone::Error,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn app() -> App {
        App::new(
            Settings::default(),
            SystemAppearance::default(),
            Fonts::default(),
            Instant::now(),
        )
    }

    #[test]
    fn the_theme_follows_the_system_unless_chosen() {
        let mut app = app();
        assert_eq!(app.mode(), Mode::Light);
        app.update(Message::System(SystemAppearance {
            mode: Some(Mode::Dark),
            accent: Some(Accent::Teal),
            font: None,
            source: "prueba",
        }));
        assert_eq!(app.theme.mode(), Mode::Dark);
        assert_eq!(app.theme.palette.accent, Accent::Teal);
        app.settings.appearance.theme = ThemeChoice::Light;
        app.settings.appearance.accent = "red".into();
        app.refresh_theme();
        assert_eq!(app.theme.mode(), Mode::Light);
        assert_eq!(app.theme.palette.accent, Accent::Red);
    }

    #[test]
    fn global_shortcuts() {
        let mut app = app();
        assert_eq!(
            app.update(Message::Shortcut(Shortcut::Quit)),
            [Effect::Quit]
        );
        assert_eq!(
            app.update(Message::Shortcut(Shortcut::Fullscreen)),
            [Effect::SetFullscreen(true)]
        );
        app.update(Message::FullscreenChanged(true));
        assert_eq!(
            app.update(Message::Shortcut(Shortcut::Escape)),
            [Effect::SetFullscreen(false)],
            "Esc sale de pantalla completa"
        );
        app.dialog = Some(Dialog::SignOut);
        assert!(app.update(Message::Shortcut(Shortcut::Escape)).is_empty());
        assert_eq!(app.dialog, None, "Esc cierra el diálogo");
        // Ctrl+, solo con la barra lateral.
        assert!(app.update(Message::Shortcut(Shortcut::Settings)).is_empty());
        app.screen = Screen::Main;
        app.update(Message::Shortcut(Shortcut::Settings));
        assert_eq!(app.page, Page::Settings);
    }

    /// Aviso de versión nueva: la comprobación automática sale en el `Tick` cuando toca, el
    /// resultado se guarda y programa la siguiente, y la manual contesta con un toast.
    #[test]
    fn update_checks_are_scheduled_and_answered() {
        let mut app = app();
        app.schedule_update_check(true);
        if crate::update::disabled_by_env() {
            assert!(app.update(Message::Tick).is_empty());
            return;
        }
        assert_eq!(app.update(Message::Tick), [Effect::CheckUpdates]);
        assert!(app.update.checking);
        assert!(app.update(Message::Tick).is_empty(), "una sola a la vez");
        let update = Update {
            version: "0.2.0".to_owned(),
            url: "https://github.com/43fdfdg45454/coroom/releases/tag/v0.2.0".to_owned(),
        };
        let effects = app.update(Message::Event(Event::UpdateChecked {
            result: Ok(Some(update.clone())),
            at: 1_790_000_000,
        }));
        assert_eq!(effects, [Effect::SaveSettings]);
        assert_eq!(app.settings.updates.last_check, Some(1_790_000_000));
        assert_eq!(app.update.available, Some(update.clone()));
        assert!(app.toasts.is_empty(), "la automática no molesta");
        let next = app.update.next_check.expect("siguiente comprobación");
        assert_eq!(next - app.now, Duration::from_secs(24 * 3600));
        assert_eq!(app.next_tick(), Some(next));
        // El aviso lleva a la página de la versión.
        assert_eq!(
            app.update(Message::OpenUrl(update.url.clone())),
            [Effect::OpenUrl(update.url)]
        );
        // La manual contesta siempre; un error no borra lo que ya se sabía.
        assert_eq!(app.check_updates(true), [Effect::CheckUpdates]);
        app.update(Message::Event(Event::UpdateChecked {
            result: Err("sin red".to_owned()),
            at: 1_790_000_100,
        }));
        assert!(app.toasts.iter().any(|t| t.text.contains("sin red")));
        assert!(app.update.available.is_some());
        assert_eq!(app.settings.updates.last_check, Some(1_790_000_000));
        // Desactivada: no se programa.
        app.settings.updates.check = false;
        app.schedule_update_check(true);
        assert_eq!(app.update.next_check, None);
        assert!(app.check_updates(false).is_empty());
    }

    #[test]
    fn toasts_expire() {
        let mut app = app();
        app.toast(Tone::Info, "hola");
        assert_eq!(app.toasts.len(), 1);
        assert!(app.next_tick().is_some());
        app.now += coroom_ui::tokens::motion::TOAST + Duration::from_millis(1);
        app.update(Message::Tick);
        assert!(app.toasts.is_empty());
        for n in 0..5 {
            app.toast(Tone::Info, format!("{n}"));
        }
        assert_eq!(app.toasts.len(), 3, "como mucho tres a la vez");
    }

    #[test]
    fn invitations_replace_the_previous_one_of_the_same_friend() {
        let mut app = app();
        let invite = |code: &str| {
            Message::Event(Event::Invited(Invitation {
                from_user: 7,
                from_name: "Bea".into(),
                code: code.into(),
            }))
        };
        app.update(invite("AAAAAA"));
        app.update(invite("BBBBBB"));
        assert_eq!(app.invitations.len(), 1);
        assert_eq!(app.invitations[0].code, "BBBBBB");
    }

    #[test]
    fn hosts_of_urls() {
        assert_eq!(
            host_of("https://coroom.example.com:4433/x"),
            "coroom.example.com:4433"
        );
        assert_eq!(host_of("coroom.example.com"), "coroom.example.com");
    }
}
