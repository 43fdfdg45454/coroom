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

use coroom_session::audio::AudioError;
use coroom_session::input::gamepad::PadChoice;
use coroom_session::remote::audio::DeviceSwitchError;
use coroom_session::remote::{GuestStatus, QualityLevel, SessionEvent};
use coroom_session::{GuestId, Permissions, SessionError};
use coroom_transport::api::ServerProbe;
use coroom_transport::proto::api::{Devices, Friends, Usage};
use coroom_transport::proto::signal::Activity;
use coroom_ui::theme::{Accent, Mode, Palette, Tone};
use coroom_ui::widget::{self as ui};
use coroom_ui::{Element, Theme, font::Fonts};
use iced_core::{Alignment, Length, Padding};
use iced_widget::{column, container, row};

use super::avatars;
use super::notify;
use super::screens::{
    chat, diagnostics, friends, guest, home, host, login, settings as settings_screen,
};
use super::settings::{ServerEntry, Settings, SoundEvent, ThemeChoice};
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
    /// Versión de su avatar (`None`: no tiene).
    pub avatar: Option<u64>,
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

/// Páginas bajo la cabecera.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    /// Inicio: compartir, entrar con código, invitaciones y amigos.
    #[default]
    Home,
    /// Compartir (anfitrión): preparar la sala y, abierta, la sala misma.
    Host,
    /// Ajustes.
    Settings,
}

/// Estado del servidor para la insignia de la cabecera (lo mide la sala de espera).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ServerStatus {
    /// RTT con el servidor en ms (`None` hasta medirlo).
    pub rtt_ms: Option<f64>,
    /// Si la señalización responde (`None` hasta saberlo).
    pub online: Option<bool>,
}

/// Pantalla.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Servidor e inicio de sesión.
    Login,
    /// Páginas bajo la cabecera.
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
    /// Alguien te pidió amistad ahora mismo (la lista de amigos se vuelve a pedir).
    FriendRequested {
        /// Quien la pide.
        from_user: u64,
        /// Su nombre.
        from_name: String,
    },
    /// Se eligió (o no: `Ok(None)`) la imagen del avatar propio.
    AvatarPicked(Result<Option<std::path::PathBuf>, String>),
    /// El avatar propio cambió en el servidor (`Ok(None)`: se quitó), o por qué no.
    AvatarChanged(Result<Option<u64>, String>),
    /// Llegó un avatar (de la caché o del servidor), o por qué no.
    AvatarLoaded {
        /// Versión.
        version: u64,
        /// La imagen.
        image: Result<coroom_ui::avatar::AvatarImage, avatars::Unavailable>,
    },
    /// Alguien (un amigo, o esta cuenta en otro equipo) cambió de avatar.
    Avatar {
        /// Usuario.
        user: u64,
        /// Versión nueva (`None`: lo quitó).
        avatar: Option<u64>,
    },
    /// Un amigo se conectó o desconectó.
    Presence {
        /// Usuario.
        user: u64,
        /// Si está conectado.
        online: bool,
    },
    /// Qué hace un amigo conectado (abre una sala, juega en una, nada).
    Activity {
        /// Usuario.
        user: u64,
        /// Lo que hace.
        activity: Activity,
    },
    /// Un amigo pide entrar en la sala que compartes.
    JoinRequested {
        /// Quien lo pide.
        from_user: u64,
        /// Su nombre.
        from_name: String,
    },
    /// El anfitrión al que pediste entrar dijo que ahora no.
    JoinDeclined {
        /// El anfitrión.
        from_user: u64,
        /// Su nombre.
        from_name: String,
    },
    /// Pedir entrar no llegó al servidor (o este lo rechazó): el motivo, en español.
    JoinRequestFailed {
        /// Motivo.
        message: String,
    },
    /// El servidor responde (o no) y con qué RTT.
    ServerStatus {
        /// RTT con el servidor en ms, si responde.
        rtt_ms: Option<f64>,
        /// Si la señalización responde.
        online: bool,
    },
    /// Salida de `coroom doctor`.
    Doctor(Vec<String>),
    /// Micrófonos y salidas.
    AudioDevices(Result<Vec<AudioDevice>, String>),
    /// Anfitrión: los invitados ya saben a qué sala ir (el código), o por qué no.
    HandedOver(Result<String, String>),
    /// Prueba del micrófono: el nivel (0-1, el pico de los últimos 50 ms) o por qué paró.
    MicLevel(Result<f32, MicTestError>),
    /// Con la sala abierta, el micrófono (`input`) o la salida elegidos se cambiaron en ella, o
    /// por qué no ([`Effect::SwitchAudioDevice`]).
    AudioDeviceSwitched {
        /// Micrófono (`true`) o salida.
        input: bool,
        /// El elegido.
        device: Option<String>,
        /// El que había, para volver a él si el elegido no abre.
        previous: Option<String>,
        /// Si la sala ya lo usa.
        result: Result<(), DeviceSwitchError>,
    },
    /// Anfitrión: la sala está arrancando (con su vista previa) o no pudo.
    HostStarted(Result<Arc<VideoFeed>, String>),
    /// Anfitrión: un evento de la sesión.
    Host(SessionEvent),
    /// Anfitrión: estado de los invitados.
    HostGuests(Vec<GuestStatus>, HostVoice),
    /// Anfitrión: la vista previa no está disponible (captura en la GPU).
    PreviewUnavailable,
    /// Anfitrión: la sala se cerró (`Some` con el motivo si fue un error).
    HostEnded(Option<String>),
    /// Anfitrión: se cambió lo que se comparte con la sala abierta, o no se pudo.
    SourceSwitched(Result<(), String>),
    /// Anfitrión: la sala abierta ya usa la codificación de los ajustes (códec, codificador,
    /// resolución máxima y fps), o no se pudo.
    VideoChanged(Result<(), String>),
    /// Invitado: la sesión arrancó (con la fuente del vídeo) o no pudo.
    GuestStarted(Result<Arc<VideoFeed>, String>),
    /// Invitado: un evento de la sesión.
    Guest(SessionEvent),
    /// Invitado: estadísticas de ahora.
    GuestStats(Box<guest::Stats>),
    /// Invitado: la sesión terminó (`Some` con el motivo si no fue normal).
    GuestEnded(Option<GuestEnd>),
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
    /// Anfitrión: al arrancar la captura, el portal dio un token de restauración nuevo para esa
    /// fuente (`Some`) o rechazó el guardado (`None`).
    ShareRestoreToken(coroom_session::capture::ShareKind, Option<String>),
    /// Un efecto no pudo hacerse (aviso para el usuario).
    Failed(String),
    /// El mensaje del chat no salió: el motivo, para enseñarlo bajo el campo.
    ChatFailed(String),
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

/// Prueba del micrófono de Ajustes ([`Effect::MicTest`]).
#[derive(Debug, Clone, PartialEq)]
pub struct MicTest {
    /// Micrófono (`None`: el del sistema).
    pub mic: Option<String>,
    /// Salida para oírse (`None`: la del sistema).
    pub speaker: Option<String>,
    /// Oírse por la salida mientras se prueba.
    pub hear: bool,
    /// Volumen de la voz propia (ganancia lineal), el mismo que se envía en la sala.
    pub gain: f32,
}

/// Por qué paró la prueba del micrófono.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MicTestError {
    /// El micrófono no abrió o dejó de funcionar.
    Mic(AudioError),
    /// La salida para oírse no abrió o dejó de funcionar.
    Speaker(AudioError),
}

/// La voz del anfitrión en su sala, con cada lista de invitados.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostVoice {
    /// Si comparte el micrófono en esta sala.
    pub enabled: bool,
    /// Micrófono silenciado.
    pub mic_muted: bool,
    /// Quién habla ahora (ids de invitado; 0 = el anfitrión).
    pub talkers: Vec<u32>,
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
    /// El avatar propio (Ajustes → Cuenta).
    Avatar(avatars::Msg),
    /// Diagnóstico (encima de cualquier pantalla).
    Diagnostics(diagnostics::Msg),
    /// Chat de la sala (anfitrión o invitado).
    Chat(chat::Msg),
    /// La ventana ganó o perdió el foco del escritorio.
    WindowFocused(bool),
    /// El sistema dice si la ventana está tapada u oculta del todo (minimizada).
    WindowOccluded(bool),
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
    /// Traer estos avatares `(usuario, versión)`: de la caché de disco o del servidor.
    FetchAvatars(Vec<(u64, u64)>),
    /// Abrir el selector de ficheros para elegir la imagen del avatar propio.
    PickAvatar,
    /// Preparar (recortar, reducir) y subir esta imagen como avatar propio.
    UploadAvatar(std::path::PathBuf),
    /// Quitar el avatar propio.
    RemoveAvatar,
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
    /// Anfitrión: si la vista previa de la sala está a la vista (página Compartir, ventana sin ocultar).
    PreviewVisible(bool),
    /// Aplicar a la sala abierta la codificación de los ajustes: códec, codificador, resolución
    /// máxima y fps (la captura adopta la cadencia y el tamaño, el codificador se reabre con un
    /// intra y se renegocia con los invitados).
    SetEncoding,
    /// Enviar un mensaje al chat de la sala (anfitrión o invitado).
    SendChat(String),
    /// Anfitrión: vaciar el chat para todos.
    ClearChat,
    /// Dar el foco al campo del chat (`true`) o quitárselo: lo hace la ventana.
    FocusChat(bool),
    /// Notificación del escritorio de un mensaje del chat (con la ventana sin foco).
    ChatNotify {
        /// «Ana · Sala de Bea».
        title: String,
        /// El texto.
        body: String,
    },
    /// Sonar un aviso con su tono y el volumen de los avisos de Ajustes (también «Probar»). Los
    /// de los eventos pasan antes por [`notify::sound`], que los junta.
    Sound(SoundEvent),
    /// Abrir el selector de ficheros para elegir el tono de un aviso; vuelve con
    /// `Message::Settings(Msg::SoundPicked)`.
    PickSound(SoundEvent),
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
    /// Volumen de una voz en este equipo (misma clave que [`Effect::SetVoiceMuted`]): ganancia
    /// lineal, 1.0 lo normal y 2.0 el doble ([`coroom_session::remote::audio::MAX_VOICE_GAIN`]).
    SetVoiceGain {
        /// Si es la sala que este equipo comparte.
        host: bool,
        /// Emisor de la voz.
        source: u32,
        /// Ganancia.
        gain: f32,
    },
    /// Silenciar o no el micrófono propio para toda la sala.
    SetMicMuted {
        /// Si es la sala que este equipo comparte.
        host: bool,
        /// Silenciado.
        muted: bool,
    },
    /// Volumen de la voz propia que se envía: ganancia lineal, 1.0 lo normal y 2.0 el doble.
    SetMicGain {
        /// Si es la sala que este equipo comparte.
        host: bool,
        /// Ganancia.
        gain: f32,
    },
    /// Invitado: decodificar solo por software (o no), en marcha.
    SetSoftwareDecoder(bool),
    /// Anfitrión: ofrecer a un invitado pasarle la sala.
    OfferHandoff(GuestId),
    /// Anfitrión: retirar esa oferta (o decir que no a una respuesta que ya no se espera).
    CancelHandoff(GuestId),
    /// Invitado: contestar a la oferta de la sala (el código de la que ya abrió, o `None`).
    ReplyHandoff(Option<String>),
    /// Anfitrión: mandar a todos a la sala `code` del anfitrión nuevo, esperar a que salgan y
    /// avisar con [`Event::HandedOver`].
    HandOver {
        /// Código de la sala nueva.
        code: String,
        /// Nombre del anfitrión nuevo.
        host_name: String,
    },
    /// Cambiar de micrófono (`input`) o de salida en la sala abierta, sin reconectar
    /// (`None`: el del sistema).
    SwitchAudioDevice {
        /// Si es la sala que este equipo comparte.
        host: bool,
        /// Micrófono (`true`) o salida.
        input: bool,
        /// Nombre del dispositivo.
        device: Option<String>,
        /// El que había (se vuelve a él si el nuevo no abre).
        previous: Option<String>,
    },
    /// Volumen del audio del juego en este equipo, como invitado (el anfitrión no lo oye por
    /// coroom): ganancia lineal, 1.0 lo normal y 2.0 el doble
    /// ([`coroom_session::remote::audio::MAX_VOICE_GAIN`]). Aparte de silenciarlo
    /// ([`Effect::ToggleGameAudio`]), que manda.
    SetGameGain {
        /// Ganancia.
        gain: f32,
    },
    /// Permisos de un invitado.
    SetPermissions(GuestId, Permissions),
    /// Expulsar.
    Kick(GuestId),
    /// Pausar o reanudar la entrada de todos.
    PauseInput(bool),
    /// Invitar a un amigo a la sala.
    Invite(u64),
    /// Pedir entrar en la sala abierta de un amigo (por la señalización de espera).
    AskToJoin(u64),
    /// Anfitrión: no dejar entrar ahora a quien lo pidió.
    DeclineJoin(u64),
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
    /// Guardar en `portal.json` los tokens con los que el portal de captura recuerda el permiso de
    /// pantalla (o borrarlo, si no queda ninguno).
    SaveRestoreTokens(crate::portal::RestoreTokens),
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
    /// Empezar (`Some`) o parar la prueba del micrófono de Ajustes.
    MicTest(Option<MicTest>),
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
    /// Salir de la aplicación (cerrar la ventana o `Ctrl+Q`) dentro de una sala como invitado.
    QuitWhileGuest,
    /// Cambiar de servidor con la sala abierta.
    SwitchServerWhileHosting,
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
    /// Anfitrión: pasar la sala a un invitado.
    OfferHandoff {
        /// Invitado.
        guest: GuestId,
        /// Nombre.
        name: String,
    },
    /// Invitado: el anfitrión le pasa la sala.
    HandoffOffered {
        /// Nombre del anfitrión.
        host: String,
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

/// Por qué terminó la sesión del invitado (cuando no fue normal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestEnd {
    /// Qué pasó, para elegir el título del diálogo.
    pub kind: EndKind,
    /// El motivo en palabras, tal cual lo dio la sesión.
    pub text: String,
}

/// Clase del final de una sesión de invitado, sacada del error de la sesión (no de su texto).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndKind {
    /// El anfitrión nos expulsó.
    Removed,
    /// El anfitrión cerró la sala.
    RoomClosed,
    /// Falló la red (o se agotaron las reconexiones).
    Network,
    /// Otra cosa (medios, criptografía, un pánico…).
    Other,
}

impl GuestEnd {
    /// Final por un motivo cualquiera en palabras (un pánico, la prueba local).
    pub fn other(text: impl Into<String>) -> Self {
        Self {
            kind: EndKind::Other,
            text: text.into(),
        }
    }
}

impl From<SessionError> for GuestEnd {
    fn from(err: SessionError) -> Self {
        let kind = match &err {
            SessionError::Removed => EndKind::Removed,
            SessionError::RoomClosed => EndKind::RoomClosed,
            SessionError::Network(_) => EndKind::Network,
            _ => EndKind::Other,
        };
        Self {
            kind,
            text: err.to_string(),
        }
    }
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
    /// Paleta oscura con el acento en uso: lo que flota sobre el vídeo (barra, panel, píldora)
    /// va siempre oscuro. Se recalcula con el tema, no en cada vista.
    pub dark: Palette,
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
    /// Diagnóstico (se abre encima de cualquier pantalla).
    pub diagnostics: diagnostics::State,
    /// Chat de la sala abierta o en la que se está.
    pub chat: chat::State,
    /// Avisos con sonido (cuáles sonaron hace poco, para juntarlos).
    pub notify: notify::State,
    /// Avatares en memoria y los pedidos.
    pub avatars: avatars::Store,
    /// Estado del servidor para la cabecera.
    pub server_status: ServerStatus,
    /// Toasts.
    pub toasts: Vec<Toast>,
    next_toast: u64,
    /// Desde cuándo hay ajustes sin guardar (se guardan al poco, no en cada pulsación).
    settings_dirty: Option<Instant>,
    /// Diálogo abierto.
    pub dialog: Option<Dialog>,
    /// Menú de la cuenta abierto.
    pub account_menu: bool,
    /// A pantalla completa.
    pub fullscreen: bool,
    /// La ventana está tapada u oculta del todo ([`Message::WindowOccluded`]).
    pub window_hidden: bool,
    /// Ahora (lo mueve [`Message::Tick`]).
    pub now: Instant,
    /// Lo que tardaron los últimos dibujos de la interfaz (la ventana los anota).
    pub ui_draws: super::pacing::DrawTimes,
    /// Prueba local (`coroom probe` con ventana): solo el vídeo.
    pub probe: bool,
    /// Versión de la aplicación.
    pub version: &'static str,
    /// Aviso de versión nueva.
    pub update: UpdateState,
    /// 4:4:4 y HDR en este equipo (`None` hasta que se sondea).
    pub video_support: Option<VideoSupport>,
    /// Al pasar la sala, el silencio propio de la sala que se deja, para la siguiente: cada sesión
    /// empieza con el micrófono abierto y quien estaba callado debe seguir callado.
    pub carried_mute: Option<bool>,
}

impl App {
    /// Aplicación con estos ajustes y la apariencia del sistema.
    pub fn new(settings: Settings, system: SystemAppearance, fonts: Fonts, now: Instant) -> Self {
        let login = login::State::new(&settings);
        let mut app = Self {
            theme: Theme::new(Mode::Light, Accent::Blue, fonts),
            dark: Palette::new(Mode::Dark, Accent::Blue),
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
            diagnostics: diagnostics::State::default(),
            chat: chat::State::default(),
            notify: notify::State::default(),
            avatars: avatars::Store::default(),
            server_status: ServerStatus::default(),
            toasts: Vec::new(),
            next_toast: 1,
            settings_dirty: None,
            dialog: None,
            account_menu: false,
            fullscreen: false,
            window_hidden: false,
            now,
            ui_draws: super::pacing::DrawTimes::default(),
            probe: false,
            version: env!("CARGO_PKG_VERSION"),
            update: UpdateState::default(),
            video_support: None,
            carried_mute: None,
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

    /// Movimiento efectivo: lo que pide el sistema con lo elegido en Apariencia → Animaciones. Se
    /// calcula al pedirlo, así que un cambio en el sistema ([`Message::System`]) o en los ajustes
    /// vale desde el cuadro siguiente.
    pub fn motion(&self) -> coroom_ui::motion::Motion {
        self.settings.appearance.motion.apply(self.system.motion())
    }

    /// Recalcula el tema con los ajustes y el sistema.
    pub fn refresh_theme(&mut self) {
        let accent = self.accent();
        self.theme = Theme::new(self.mode(), accent, self.fonts);
        self.dark = Palette::new(Mode::Dark, accent);
    }

    /// Si la pantalla es el vídeo del invitado (o la prueba local) ya en marcha.
    pub fn video_in_front(&self) -> bool {
        self.screen == Screen::Guest
            && self
                .guest
                .as_ref()
                .is_some_and(|guest| guest.phase == guest::Phase::Video)
    }

    /// Si hay que impedir que el escritorio se bloquee: con el vídeo del invitado delante y el
    /// anfitrión compartiendo algo, o con la sala propia compartiendo. Quien mira (o juega con un
    /// mando, que el sistema no cuenta como actividad) no toca el teclado ni el ratón.
    pub fn wants_idle_inhibit(&self) -> bool {
        let watching = self.video_in_front()
            && self
                .guest
                .as_ref()
                .is_some_and(|guest| !guest.nothing_shared());
        watching || self.sharing_screen()
    }

    /// Con la sala propia abierta y algo compartido: quien juega desde lejos con un mando tampoco
    /// cuenta como actividad, y con la sesión bloqueada se capturaría la pantalla de bloqueo.
    pub fn sharing_screen(&self) -> bool {
        self.host.room.is_some() && self.host.source() != host::SourceChoice::Nothing
    }

    /// Fondo bajo la interfaz y el vídeo para el compositor de la ventana: el del vídeo con el
    /// vídeo delante, el de la ventana si no.
    pub fn backdrop(&self) -> iced_core::Color {
        if self.video_in_front() {
            self.theme.palette.video_backdrop.color()
        } else {
            self.theme.background()
        }
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
            || self.host.starting
    }

    /// Cuándo hace falta el próximo [`Message::Tick`].
    pub fn next_tick(&self) -> Option<Instant> {
        // Los cuadros de las animaciones no van aquí: los marca el compositor (`ui::pacing`).
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
        if let Some(at) = self.settings_dirty {
            consider(at + SETTINGS_SAVE_DELAY);
        }
        if let Some(at) = self.chat.next_tick(self.now) {
            consider(at);
        }
        if let Some(at) = home::next_tick(self) {
            consider(at);
        }
        if let Some(at) = host::next_tick(self) {
            consider(at);
        }
        if let Some(at) = settings_screen::next_tick(self) {
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

    /// Los ajustes cambiaron: se guardan en el próximo [`Message::Tick`] pasado
    /// [`SETTINGS_SAVE_DELAY`] (una ráfaga de cambios es una sola escritura). Devuelve los
    /// efectos que hacen falta ahora (ninguno): las pantallas lo devuelven tal cual.
    pub fn settings_changed(&mut self) -> Vec<Effect> {
        self.settings_dirty.get_or_insert(self.now);
        Vec::new()
    }

    /// Si hay ajustes sin guardar, el efecto de guardarlos ya (al salir o cerrar sesión).
    pub fn flush_settings(&mut self) -> Option<Effect> {
        self.settings_dirty.take().map(|_| Effect::SaveSettings)
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
                if self
                    .settings_dirty
                    .is_some_and(|since| now.duration_since(since) >= SETTINGS_SAVE_DELAY)
                {
                    self.settings_dirty = None;
                    effects.push(Effect::SaveSettings);
                }
                self.chat.tick(now);
                home::tick(self);
                host::tick(self);
                effects.extend(settings_screen::tick(self));
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
            Message::CloseDialog => match self.dialog.take() {
                // Cerrar la oferta de la sala (Esc, clic fuera) es no aceptarla: el anfitrión no
                // se queda esperando.
                Some(Dialog::HandoffOffered { .. }) => vec![Effect::ReplyHandoff(None)],
                _ => Vec::new(),
            },
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
                if self.host.is_open() {
                    // Cambiar de servidor cierra la sala para todos: se pregunta, como al salir.
                    self.dialog = Some(Dialog::SwitchServerWhileHosting);
                    return Vec::new();
                }
                self.switch_server()
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
            Message::Avatar(msg) => avatars::update(self, msg),
            Message::Diagnostics(msg) => diagnostics::update(self, msg),
            Message::Chat(msg) => chat::update(self, msg),
            Message::WindowFocused(focused) => {
                chat::update(self, chat::Msg::WindowFocused(focused))
            }
            Message::WindowOccluded(hidden) => {
                self.window_hidden = hidden;
                self.preview_visibility().into_iter().collect()
            }
            Message::Event(event) => {
                if avatars::retry_trigger(self, &event) {
                    self.avatars.retry_unreachable();
                }
                let mut effects = self.event(event);
                // Las versiones de avatar llegan con los eventos: se piden las que falten.
                effects.extend(avatars::requests(self));
                effects
            }
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
            Page::Home if !self.friends.is_loading() => {
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
        effects.extend(self.preview_visibility());
        effects
    }

    /// Si la vista previa de la sala está a la vista: en Compartir, con la ventana sin ocultar.
    pub fn preview_visible(&self) -> bool {
        self.screen == Screen::Main && self.page == Page::Host && !self.window_hidden
    }

    /// Con la sala abierta, el efecto que le dice a la sesión si la vista previa está a la vista
    /// (oculta, no se reduce ningún fotograma ni se despierta la interfaz).
    pub fn preview_visibility(&self) -> Option<Effect> {
        self.host
            .is_open()
            .then(|| Effect::PreviewVisible(self.preview_visible()))
    }

    /// Vuelve a la lista de servidores (cerrando la sala si está abierta). La cuenta sigue
    /// abierta: «Volver» lleva de nuevo a ella y elegir un servidor con la sesión guardada entra
    /// sin pedir la contraseña.
    fn switch_server(&mut self) -> Vec<Effect> {
        let mut effects = vec![Effect::StopLobby];
        if self.host.is_open() {
            effects.push(Effect::StopHost);
        }
        self.screen = Screen::Login;
        self.login.step = login::Step::Servers;
        self.login.error = None;
        effects
    }

    /// Si se está dentro de una sala como invitado (no la prueba local).
    pub fn in_room_as_guest(&self) -> bool {
        self.screen == Screen::Guest && self.guest.is_some() && !self.probe
    }

    fn shortcut(&mut self, shortcut: Shortcut) -> Vec<Effect> {
        match shortcut {
            Shortcut::Quit => {
                if self.host.is_open() {
                    self.dialog = Some(Dialog::QuitWhileHosting);
                    Vec::new()
                } else if self.in_room_as_guest() {
                    self.dialog = Some(Dialog::QuitWhileGuest);
                    Vec::new()
                } else {
                    let mut effects = Vec::new();
                    effects.extend(self.flush_settings());
                    effects.push(Effect::Quit);
                    effects
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
                if self.diagnostics.open {
                    return diagnostics::update(self, diagnostics::Msg::Close);
                }
                if self.account_menu {
                    self.account_menu = false;
                    return Vec::new();
                }
                if self.chat.takes_escape() {
                    return chat::update(self, chat::Msg::Escape);
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
                effects.extend(self.flush_settings());
                effects.push(Effect::SignOut);
                effects
            }
            Dialog::QuitWhileHosting => {
                let mut effects = vec![Effect::StopHost];
                effects.extend(self.flush_settings());
                effects.push(Effect::Quit);
                effects
            }
            Dialog::QuitWhileGuest => {
                let mut effects = guest::leave(self);
                effects.extend(self.flush_settings());
                effects.push(Effect::Quit);
                effects
            }
            Dialog::SwitchServerWhileHosting => self.switch_server(),
            Dialog::RevokeDevice { device, .. } => vec![Effect::RevokeDevice(device)],
            Dialog::OfferHandoff { guest, name } => host::offer_handoff(self, guest, name),
            Dialog::HandoffOffered { .. } => host::accept_handoff(self),
            Dialog::SessionEnded { .. } => guest::close(self),
            Dialog::SafetyNumber { .. } => Vec::new(),
        }
    }

    fn event(&mut self, event: Event) -> Vec<Effect> {
        // Inicio anota cuándo llegan las invitaciones y cuándo cambió la lista de amigos.
        home::on_event(self, &event);
        // Los avisos con sonido miran el evento antes que las pantallas (que lo consumen).
        let sounds = notify::on_event(self, &event);
        let mut effects = self.dispatch_event(event);
        effects.extend(sounds);
        effects
    }

    fn dispatch_event(&mut self, event: Event) -> Vec<Effect> {
        match event {
            event @ (Event::AvatarPicked(_)
            | Event::AvatarChanged(_)
            | Event::AvatarLoaded { .. }
            | Event::Avatar { .. }) => avatars::on_event(self, &event).unwrap_or_default(),
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
                host::on_friends(self);
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
                    if !online {
                        // Desconectado no hace nada: la actividad solo llega de los conectados.
                        friend.activity = Activity::Idle;
                    }
                }
                host::on_friends(self);
                Vec::new()
            }
            Event::Activity { user, activity } => {
                if let Load::Loaded(friends) = &mut self.friends
                    && let Some(friend) = friends.friends.iter_mut().find(|f| f.id == user)
                {
                    friend.activity = activity;
                }
                host::on_friends(self);
                Vec::new()
            }
            Event::JoinRequested {
                from_user,
                from_name,
            } => {
                host::on_join_requested(self, from_user, from_name);
                Vec::new()
            }
            Event::FriendRequested {
                from_user,
                from_name,
            } => {
                // Repetir una solicitud que ya está en la lista no vuelve a avisar.
                let known = self.friends.loaded().is_some_and(|friends| {
                    friends
                        .incoming
                        .iter()
                        .any(|request| request.user.id == from_user)
                });
                if !known {
                    self.toast(
                        Tone::Info,
                        t!("app-name-quiere-ser-tu-amigo", name = from_name),
                    );
                }
                // Aunque ya se estuviese pidiendo: esa respuesta podría ser de antes.
                if !matches!(self.friends, Load::Loaded(_)) {
                    self.friends = Load::Loading;
                }
                vec![Effect::LoadFriends]
            }
            Event::JoinDeclined { from_name, .. } => {
                // Inicio ya olvidó la petición (`home::on_event`).
                self.toast(
                    Tone::Info,
                    t!("app-name-no-puede-dejarte-entrar", name = from_name),
                );
                Vec::new()
            }
            Event::JoinRequestFailed { message } => {
                self.toast(Tone::Warning, message);
                Vec::new()
            }
            Event::ServerStatus { rtt_ms, online } => {
                self.server_status = ServerStatus {
                    rtt_ms: if online {
                        rtt_ms
                    } else {
                        self.server_status.rtt_ms
                    },
                    online: Some(online),
                };
                Vec::new()
            }
            Event::Invited(invitation) => {
                // Si se le había pedido entrar, la invitación es la respuesta: se entra ya.
                if home::take_asked(self, invitation.from_user) && !home::server_down(self) {
                    self.toast(
                        Tone::Success,
                        t!("app-name-te-deja-entrar", name = invitation.from_name),
                    );
                    return home::update(self, home::Msg::Accept(invitation.code));
                }
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
            Event::Doctor(_) => {
                diagnostics::on_event(self, &event);
                settings_screen::on_event(self, event)
            }
            Event::Devices(_)
            | Event::DeviceRevoked(_)
            | Event::AudioDevices(_)
            | Event::MicLevel(_)
            | Event::AudioDeviceSwitched { .. } => settings_screen::on_event(self, event),
            Event::HostStarted(_)
            | Event::Host(_)
            | Event::HostGuests(..)
            | Event::PreviewUnavailable
            | Event::HostEnded(_)
            | Event::HandedOver(_)
            | Event::SourceSwitched(_)
            | Event::VideoChanged(_) => {
                // El chat y el diagnóstico miran los eventos de la sesión antes que la pantalla
                // (que puede cerrarla y consumir el evento).
                let mut effects = chat::on_event(self, &event);
                diagnostics::on_event(self, &event);
                effects.extend(host::on_event(self, event));
                effects
            }
            Event::GuestStarted(_)
            | Event::Guest(_)
            | Event::GuestStats(_)
            | Event::GuestEnded(_) => {
                let mut effects = chat::on_event(self, &event);
                diagnostics::on_event(self, &event);
                effects.extend(guest::on_event(self, event));
                effects
            }
            Event::UpdateChecked { result, at } => self.update_checked(result, at),
            Event::ShareOptions(options) => {
                host::on_share_options(self, options);
                Vec::new()
            }
            Event::SharePicked(source, picked) => host::on_share_picked(self, source, picked),
            Event::ShareRestoreToken(kind, token) => host::on_restore_token(self, kind, token),
            Event::VideoSupport(support) => {
                self.video_support = Some(support);
                Vec::new()
            }
            Event::ChatFailed(_) => chat::on_event(self, &event),
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
        // Con el vídeo delante la raíz es transparente: el compositor pone el vídeo debajo de la
        // interfaz y el fondo lo da [`App::backdrop`].
        let root_class = if self.video_in_front() {
            coroom_ui::style::ContainerClass::Transparent
        } else {
            coroom_ui::style::ContainerClass::Window
        };
        let content = container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .class(root_class);
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
        // El diagnóstico va encima de cualquier pantalla; un diálogo, encima de todo.
        let (overlay, dismiss) = match &self.dialog {
            Some(dialog) => (Some(self.dialog_view(dialog)), Message::CloseDialog),
            None if self.diagnostics.open => (
                Some(diagnostics::view(self)),
                Message::Diagnostics(diagnostics::Msg::Close),
            ),
            None => (None, Message::CloseDialog),
        };
        ui::modal(with_toasts, overlay, dismiss)
    }

    /// Cabecera y página; el menú de la cuenta, desplegado bajo la cabecera.
    fn shell(&self) -> Element<'_, Message> {
        let page: Element<'_, Message> = match self.page {
            Page::Home => home::view(self),
            Page::Host => host::view(self),
            Page::Settings => settings_screen::view(self),
        };
        // Una línea fina por encima y por debajo de la cabecera, que además lleva el tono de las
        // tarjetas y más aire vertical: con la barra de título del sistema (KDE) encima, del mismo
        // gris que la barra lateral, las dos se leían como una sola barra gruesa con el título
        // pegado arriba.
        let shell = column![
            iced_widget::rule::horizontal(1),
            self.headerbar(),
            iced_widget::rule::horizontal(1),
            page
        ]
        .height(Length::Fill);
        // Siempre una pila de dos capas, con el menú o sin él: si la raíz cambiara de tipo, iced
        // tiraría el estado de la página (su desplazamiento, el foco) al abrir o cerrar el menú.
        let menu: Element<'_, Message> = if self.account_menu {
            iced_widget::opaque(
                iced_widget::mouse_area(
                    container(iced_widget::opaque(self.account_menu_view()))
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .align_x(iced_core::alignment::Horizontal::Right)
                        .align_y(iced_core::alignment::Vertical::Top)
                        .padding(Padding {
                            top: HEADERBAR_HEIGHT + 5.0,
                            right: 12.0,
                            bottom: 0.0,
                            left: 0.0,
                        }),
                )
                .on_press(Message::ToggleAccountMenu),
            )
        } else {
            iced_widget::Space::new().width(0).height(0).into()
        };
        iced_widget::stack![shell, menu].into()
    }

    /// Título de la página para la cabecera (`None` en Inicio: va la marca).
    pub fn page_title(&self) -> Option<&'static str> {
        match self.page {
            Page::Home => None,
            Page::Host => Some(t!("app-compartir")),
            Page::Settings => Some(t!("app-ajustes")),
        }
    }

    /// Subtítulo bajo el título de la cabecera (Ajustes: «Se guardan al cambiarlos»).
    pub fn page_subtitle(&self) -> Option<&'static str> {
        match self.page {
            Page::Settings => Some(settings_screen::subtitle()),
            _ => None,
        }
    }

    /// Insignia del servidor: host y RTT, o que no responde.
    pub fn server_badge_text(&self) -> String {
        let host = self
            .account
            .as_ref()
            .map(AccountInfo::host)
            .unwrap_or_default();
        match (self.server_status.online, self.server_status.rtt_ms) {
            (Some(false), _) => t!("app-host-sin-conexion", host = host),
            (_, Some(rtt)) => t!("app-host-rtt-ms", host = host, rtt = rtt.round() as u64),
            _ => host,
        }
    }

    /// Cabecera: atrás y título (o la marca) a la izquierda; a la derecha la insignia del
    /// servidor, «En vivo» si se comparte desde otra página, Ajustes y la cuenta.
    fn headerbar(&self) -> Element<'_, Message> {
        use coroom_ui::icon::{Icon, icon_with};
        use coroom_ui::style::{ContainerClass, SvgClass};
        use iced_widget::button;
        let mut left = row![].spacing(8).align_y(Alignment::Center);
        match self.page_title() {
            None => {
                left = left.push(icon_with(Icon::RadioTower, BRAND_ICON, SvgClass::Accent));
                left = left.push(ui::heading("coroom"));
            }
            Some(title) => {
                left = left.push(ui::icon_button(
                    Icon::ArrowLeft,
                    t!("app-inicio"),
                    Some(Message::Navigate(Page::Home)),
                ));
                // El subtítulo va al lado del título y no debajo: en dos líneas no cabría con aire
                // en el alto de la cabecera.
                left = left.push(ui::heading(title));
                if let Some(subtitle) = self.page_subtitle() {
                    left = left.push(ui::caption(subtitle));
                }
            }
        }
        let server_tone = match self.server_status.online {
            Some(false) => Tone::Error,
            Some(true) => Tone::Success,
            None => Tone::Neutral,
        };
        let server = button(ui::status_badge(server_tone, self.server_badge_text()))
            .padding(0)
            .class(coroom_ui::style::ButtonClass::Flat)
            .on_press(Message::Diagnostics(diagnostics::Msg::Open));
        let mut right = row![server].spacing(8).align_y(Alignment::Center);
        if self.host.is_open() && self.page != Page::Host {
            right = right.push(
                button(ui::status_badge(Tone::Success, t!("app-en-vivo")))
                    .padding(0)
                    .class(coroom_ui::style::ButtonClass::Flat)
                    .on_press(Message::Navigate(Page::Host)),
            );
        }
        right = right.push(ui::icon_button(
            Icon::Settings,
            t!("app-ajustes"),
            (self.page != Page::Settings).then_some(Message::Navigate(Page::Settings)),
        ));
        if let Some(account) = &self.account {
            right = right.push(
                button(super::avatars::view(
                    self,
                    &account.display_name,
                    account.avatar,
                    28.0,
                ))
                .padding(2)
                .class(coroom_ui::style::ButtonClass::Flat)
                .on_press(Message::ToggleAccountMenu),
            );
        }
        container(
            row![left, ui::fill(), right]
                .align_y(Alignment::Center)
                .width(Length::Fill),
        )
        // Centrado en vertical: sin `center_y`, la fila (del alto de un botón) quedaba arriba,
        // pegada a la barra de título del sistema.
        .padding(Padding::from([0.0, coroom_ui::tokens::space::S]))
        .center_y(Length::Fixed(HEADERBAR_HEIGHT))
        .class(ContainerClass::Headerbar)
        .into()
    }

    /// Menú de la cuenta: quién eres y en qué servidor, cambiar de servidor y cerrar sesión.
    fn account_menu_view(&self) -> Element<'_, Message> {
        use coroom_ui::icon::{Icon, icon_with};
        use coroom_ui::style::{ButtonClass, ContainerClass, SvgClass};
        use iced_core::text::Wrapping;
        use iced_widget::button;
        let Some(account) = &self.account else {
            return ui::gap(0.0);
        };
        let who = row![
            super::avatars::view(self, &account.display_name, account.avatar, 32.0),
            column![
                ui::strong(account.display_name.as_str()),
                ui::caption(account.host())
            ]
            .spacing(2)
        ]
        .spacing(10)
        .align_y(Alignment::Center);
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
        container(
            column![
                container(who).padding(Padding::from([6.0, 10.0])),
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
        .width(Length::Fixed(260.0))
        .class(ContainerClass::Dialog)
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
            Dialog::QuitWhileGuest => (
                t!("app-salir-de-la-sala-y-cerrar").to_owned(),
                t!("app-dejaras-de-ver-y-controlar-la").to_owned(),
                vec![fill(), cancel(), confirm(t!("app-salir-y-cerrar"), true)],
            ),
            Dialog::SwitchServerWhileHosting => (
                t!("app-cerrar-la-sala-y-cambiar-de-servidor").to_owned(),
                t!("app-cambiar-de-servidor-cierra-la-sala").to_owned(),
                vec![fill(), cancel(), confirm(t!("app-cerrar-y-cambiar"), true)],
            ),
            Dialog::RevokeDevice { name, .. } => (
                t!("app-cerrar-la-sesion-de-name", name = name),
                t!("app-ese-dispositivo-queda-desvinculado-de-tu").to_owned(),
                vec![fill(), cancel(), confirm(t!("app-cerrar-sesion"), true)],
            ),
            Dialog::OfferHandoff { name, .. } => (
                t!("app-pasar-la-sala-a-name", name = name),
                t!("app-pasar-la-sala-texto", name = name),
                vec![fill(), cancel(), confirm(t!("app-pasar-la-sala"), false)],
            ),
            Dialog::HandoffOffered { host } => (
                t!("app-name-te-pasa-la-sala", name = host),
                t!("app-te-pasa-la-sala-texto").to_owned(),
                vec![
                    fill(),
                    ui::normal(
                        t!("app-ahora-no"),
                        Some(Message::Guest(guest::Msg::DeclineHandoff)),
                    )
                    .into(),
                    confirm(t!("app-abrir-mi-sala"), false),
                ],
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

/// Tamaño de la marca (el icono de la antena) en la cabecera.
const BRAND_ICON: f32 = 22.0;

/// Alto de la cabecera en píxeles lógicos: la marca, centrada, queda a la misma distancia del
/// borde de arriba (la barra de título del sistema) y de abajo que del borde izquierdo de la
/// ventana (`space::S`), como en la barra de cabecera de GNOME.
pub const HEADERBAR_HEIGHT: f32 = BRAND_ICON + 2.0 * coroom_ui::tokens::space::S;

/// Cuánto se espera tras el último cambio de ajustes para guardarlos.
pub const SETTINGS_SAVE_DELAY: Duration = Duration::from_millis(600);

/// Lo que dura una petición de entrar en una sala sin respuesta: en Inicio, «Pedido…» hasta que el
/// anfitrión contesta o pasa este tiempo; en Compartir, la tarjeta de quien pide entrar.
pub const JOIN_REQUEST_TTL: Duration = Duration::from_secs(60);

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

    /// Una cuenta de ejemplo con sesión iniciada.
    pub(crate) fn account() -> AccountInfo {
        AccountInfo {
            server: ServerEntry {
                url: "https://coroom.example.com".to_owned(),
                fingerprint: None,
                tcp_port: None,
                username: None,
            },
            username: "ana".into(),
            display_name: "Ana".into(),
            user_id: 1,
            device: "pc".into(),
            fingerprint: String::new(),
            avatar: None,
        }
    }

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
            reduced_motion: Some(true),
            animation_factor: Some(0.5),
            source: "prueba",
        }));
        assert!(app.motion().is_reduced(), "el movimiento también");
        assert_eq!(app.motion().factor(), 0.5);
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
    fn settings_are_saved_once_after_a_burst_of_changes() {
        let mut app = app();
        assert!(app.settings_changed().is_empty());
        assert!(app.settings_changed().is_empty());
        assert_eq!(app.next_tick(), Some(app.now + SETTINGS_SAVE_DELAY));
        app.now += Duration::from_millis(100);
        assert!(app.update(Message::Tick).is_empty(), "todavía no");
        app.now += SETTINGS_SAVE_DELAY;
        assert_eq!(app.update(Message::Tick), [Effect::SaveSettings]);
        assert!(app.update(Message::Tick).is_empty(), "una sola vez");
        // Al salir se guardan ya.
        app.settings_changed();
        assert_eq!(
            app.update(Message::Shortcut(Shortcut::Quit)),
            [Effect::SaveSettings, Effect::Quit]
        );
    }

    #[test]
    fn the_server_badge_shows_host_and_rtt() {
        let mut app = app();
        app.account = Some(AccountInfo {
            server: ServerEntry {
                url: "https://coroom.example.com".to_owned(),
                fingerprint: None,
                tcp_port: None,
                username: None,
            },
            username: "ana".into(),
            display_name: "Ana".into(),
            user_id: 1,
            device: "pc".into(),
            fingerprint: String::new(),
            avatar: None,
        });
        assert_eq!(app.server_badge_text(), "coroom.example.com");
        app.update(Message::Event(Event::ServerStatus {
            rtt_ms: Some(12.4),
            online: true,
        }));
        assert_eq!(app.server_badge_text(), "coroom.example.com · 12 ms");
        app.update(Message::Event(Event::ServerStatus {
            rtt_ms: None,
            online: false,
        }));
        assert_eq!(app.server_badge_text(), "coroom.example.com · sin conexión");
        assert_eq!(
            app.server_status.rtt_ms,
            Some(12.4),
            "se recuerda el último RTT"
        );
    }

    /// En reposo no se pide ningún cuadro: mientras algo se mueve los marca el compositor
    /// (`ui::pacing`) y el próximo plazo de la aplicación nunca es un cuadro supuesto.
    #[test]
    fn nothing_redraws_at_rest() {
        let mut app = app();
        assert!(!app.animating());
        assert_eq!(app.next_tick(), None);
        let shown_at = app.now;
        app.toast(Tone::Info, "hola");
        assert!(app.animating(), "el toast entra animado");
        assert_eq!(
            app.next_tick(),
            Some(shown_at + coroom_ui::tokens::motion::TOAST),
            "animando, el próximo plazo sigue siendo el de la aplicación"
        );
        app.now += Duration::from_millis(300);
        assert!(!app.animating(), "ya entró: nada se mueve");
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
