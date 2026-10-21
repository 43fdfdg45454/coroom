//! Ajustes: lo que no depende de una sala.
//!
//! Nueve secciones con su navegación lateral, cada cosa en un solo sitio y con un solo nombre:
//! **Servidor y red** (el servidor con su certificado, el uso del relay del mes, el diagnóstico y
//! la red), **Audio y voz** (salida y micrófono con la lista en vivo, tu voz y el volumen del
//! juego), **Vídeo** (la calidad por defecto y Avanzado, y cómo ves el vídeo de los demás),
//! **Sala** (cómo se comporta la sala como anfitrión y como invitado), **Notificaciones** (el
//! volumen y el tono de cada aviso, todo lo del chat y los sonidos de la interfaz), **Mandos** (el
//! que envías y la ranura que ofreces), **Atajos de teclado** (todos en una tabla), **Apariencia**
//! (tema, acento, fuente y animaciones) y **Acerca de** (versión, versiones nuevas y licencias).
//! La cuenta tiene su página en el riel (`screens::account`); sus acciones (desvincular un
//! dispositivo, ver un número de seguridad) siguen siendo mensajes de aquí ([`Msg`]).
//!
//! Todas las filas son iguales ([`ui::setting_row`]): título y descripción a la izquierda, la
//! etiqueta de cuándo surte efecto («Al momento», «En la próxima sala», «Al entrar») en una
//! columna de ancho fijo y el control a la derecha. Ninguna cambia de alto al cambiar su valor
//! (lo que cambia con él va en una ranura del tamaño de su variante mayor) ni al llegar una
//! lista de dispositivos nueva (su hora va en un icono, con el detalle al pasar el ratón).
//!
//! Cada cambio se guarda solo, poco después ([`App::settings_changed`]).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use coroom_session::audio::AudioError;
use coroom_session::capture::ShareKind;
use coroom_session::codec::Codec;
use coroom_session::input::gamepad::PadChoice;
use coroom_session::remote::audio::{AudioIssue, DeviceSwitchError};
use coroom_transport::proto::PROTOCOL_VERSION;
use coroom_transport::proto::api::Devices;
use coroom_ui::Element;
use coroom_ui::icon::{Icon, icon_with};
use coroom_ui::style::{
    ButtonClass, ContainerClass, InputClass, ProgressClass, SliderClass, SvgClass, TextClass,
};
use coroom_ui::tag;
use coroom_ui::theme::{Accent, Tone};
use coroom_ui::tokens::{size, space, text as type_size};
use coroom_ui::widget::{self as ui, Kind};
use iced_core::alignment::{Horizontal, Vertical};
use iced_core::text::Wrapping;
use iced_core::widget::Id;
use iced_core::{Alignment, Length, Padding};
use iced_widget::{Column, Row, button, column, container, progress_bar, row, text, text_input};

use crate::portal::{REMEMBERS_SHARE_PERMISSION, RestoreTokens};
use crate::ui::app::{
    App, AudioDevice, Dialog, Effect, Event, Load, Message, MicTest, MicTestError, Page, Screen,
    VideoSupport,
};
use crate::ui::screens::{diagnostics, guest, host};
use crate::ui::settings::{
    CodecChoice, DEFAULT_INTERFACE_VOLUME, DEFAULT_NOTIFICATION_VOLUME, DEFAULT_VOICE_VOLUME,
    DecoderChoice, FPS_CHOICES, MAX_VOICE_VOLUME, MotionChoice, QualityPreset, Resolution,
    Shortcuts, SoundChoice, SoundEvent, ThemeChoice, Video, VideoOption, VoiceMode, When,
    voice_gain,
};
use crate::ui::tones::Builtin;

/// Cada cuánto se vuelve a pedir la lista de micrófonos y salidas con «Audio y voz» abierta.
pub const AUDIO_REFRESH: Duration = Duration::from_secs(3);

/// Secciones de Ajustes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SettingsPage {
    /// El servidor, su certificado, el uso del relay, el diagnóstico y la red.
    #[default]
    Network,
    /// Dispositivos de audio, tu voz y el volumen del juego.
    Audio,
    /// La calidad por defecto, Avanzado y cómo se ve el vídeo de los demás.
    Video,
    /// Cómo se comporta la sala como anfitrión y como invitado.
    Room,
    /// Los avisos con sonido, el chat y los sonidos de la interfaz.
    Notifications,
    /// El mando que se envía y la ranura que se ofrece.
    Gamepads,
    /// Todos los atajos de teclado.
    Shortcuts,
    /// Tema, acento, fuente y animaciones.
    Appearance,
    /// Versión, versiones nuevas y licencias.
    About,
}

impl SettingsPage {
    /// Todas, en el orden de la navegación.
    pub const ALL: [SettingsPage; 9] = [
        SettingsPage::Network,
        SettingsPage::Audio,
        SettingsPage::Video,
        SettingsPage::Room,
        SettingsPage::Notifications,
        SettingsPage::Gamepads,
        SettingsPage::Shortcuts,
        SettingsPage::Appearance,
        SettingsPage::About,
    ];

    /// Nombre en la navegación.
    pub fn label(self) -> &'static str {
        match self {
            SettingsPage::Network => t!("settings-servidor-y-red"),
            SettingsPage::Audio => t!("settings-audio-y-voz"),
            SettingsPage::Video => t!("settings-video"),
            SettingsPage::Room => t!("settings-sala"),
            SettingsPage::Notifications => t!("settings-notificaciones"),
            SettingsPage::Gamepads => t!("settings-mandos"),
            SettingsPage::Shortcuts => t!("settings-atajos-de-teclado"),
            SettingsPage::Appearance => t!("settings-apariencia"),
            SettingsPage::About => t!("settings-acerca-de"),
        }
    }

    fn icon(self) -> Icon {
        match self {
            SettingsPage::Network => Icon::Server,
            SettingsPage::Audio => Icon::Headphones,
            SettingsPage::Video => Icon::Monitor,
            SettingsPage::Room => Icon::Users,
            SettingsPage::Notifications => Icon::Bell,
            SettingsPage::Gamepads => Icon::Gamepad,
            SettingsPage::Shortcuts => Icon::Keyboard,
            SettingsPage::Appearance => Icon::Palette,
            SettingsPage::About => Icon::Info,
        }
    }
}

/// Atajo que se está cambiando.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capture {
    /// Una letra de los atajos del invitado (índice en [`Shortcuts::letters`]).
    Shortcut(usize),
    /// La tecla de pulsar para hablar.
    PushToTalk,
}

/// Estado.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// Página.
    pub page: SettingsPage,
    /// Dispositivos de la cuenta.
    pub devices: Load<Devices>,
    /// Salida de `coroom doctor` (la pide y la muestra el diagnóstico; llega aquí con
    /// [`Event::Doctor`]).
    pub doctor: Load<Vec<String>>,
    /// Micrófonos y salidas (la última lista que llegó; no se vacía al volver a pedirla).
    pub audio_devices: Load<Vec<AudioDevice>>,
    /// La lista de audio está pedida y aún no ha llegado.
    pub audio_refreshing: bool,
    /// Cuándo llegó la última respuesta (lista o error): la siguiente se pide
    /// [`AUDIO_REFRESH`] después.
    pub audio_answered_at: Option<Instant>,
    /// Cuándo llegó la última lista (en el detalle del icono de la lista).
    pub audio_updated_at: Option<Instant>,
    /// Error de la última petición si ya había una lista (se sigue mostrando la anterior).
    pub audio_error: Option<String>,
    /// Esperando una tecla.
    pub capturing: Option<Capture>,
    /// Texto del puerto local.
    pub port: Option<String>,
    /// Error del puerto.
    pub port_error: Option<String>,
    /// El selector de ficheros está abierto para el tono de este aviso (no se abre otro).
    pub sound_picking: Option<SoundEvent>,
    /// Error de la última elección de un tono propio (fichero que no vale o selector que falló),
    /// en la fila de su aviso.
    pub sound_error: Option<(SoundEvent, String)>,
    /// La prueba del micrófono en marcha: el último nivel (0-1).
    pub mic_test: Option<f32>,
    /// Oírse durante la prueba del micrófono.
    pub mic_test_hear: bool,
    /// La prueba en marcha suena por la salida (con una sala abierta, nunca).
    pub mic_test_hearing: bool,
    /// Por qué paró la última prueba del micrófono.
    pub mic_test_error: Option<String>,
}

/// Mensajes.
#[derive(Debug, Clone)]
pub enum Msg {
    /// Página.
    Page(SettingsPage),
    /// Volver a cargar los dispositivos de la cuenta.
    ReloadDevices,
    /// Desvincular un dispositivo de la cuenta (pide confirmación).
    Revoke {
        /// Dispositivo.
        id: u64,
        /// Nombre.
        name: String,
    },
    /// Usar siempre el relay.
    ForceRelay(bool),
    /// Pedir al router que abra el puerto.
    PortMapping(bool),
    /// Puerto local.
    Port(String),
    /// Ver el número de seguridad de un par conocido.
    Peer(usize),
    /// Salida de audio.
    Speaker(Choice<Option<String>>),
    /// Micrófono.
    Microphone(Choice<Option<String>>),
    /// Como anfitrión, compartir el micrófono con la sala.
    HostVoice(bool),
    /// Como invitado, pulsar para hablar (`false`: micrófono abierto).
    PushToTalk(bool),
    /// Supresión de ruido del micrófono.
    NoiseSuppression(bool),
    /// Los invitados entran callados.
    GuestsMuted(bool),
    /// El tono de un aviso, elegido en su selector (suena para oírlo).
    Tone(SoundEvent, Choice<ToneOption>),
    /// El selector de ficheros volvió para un aviso: un fichero ya comprobado
    /// (`sound::pick_checked`, en la tarea del selector), nada (cancelado) o el motivo en palabras.
    SoundPicked(SoundEvent, Result<Option<PathBuf>, String>),
    /// Oír el tono de un aviso («Probar»).
    TestSound(SoundEvent),
    /// Volumen de los avisos (porcentaje), mientras se arrastra.
    NotificationVolume(u8),
    /// Al soltar el deslizador del volumen de los avisos (suena uno para oírlo).
    NotificationVolumeDone,
    /// Volumen de los sonidos de la interfaz (porcentaje), mientras se arrastra.
    InterfaceVolume(u8),
    /// Al soltar el deslizador del volumen de la interfaz (suena «Confirmar» para oírlo).
    InterfaceVolumeDone,
    /// Oír un sonido de la interfaz.
    PreviewCue(coroom_ui::sound::Cue),
    /// Olvidar el permiso de pantalla del portal: el diálogo del sistema volverá a salir.
    ForgetSharePermission,
    /// Notificación del escritorio con la ventana sin foco.
    ChatNotifications(bool),
    /// Burbuja sobre el vídeo.
    ChatBubbles(bool),
    /// Empezar o parar la prueba del micrófono.
    MicTest(bool),
    /// Oírse (o no) durante la prueba del micrófono.
    MicTestHear(bool),
    /// Volumen de la voz propia (porcentaje), mientras se arrastra.
    MicVolume(u8),
    /// Al soltar el deslizador de la voz propia.
    MicVolumeDone,
    /// Como invitado, quién habla sobre el vídeo.
    TalkingOverlay(bool),
    /// Volumen del audio del juego como invitado (porcentaje), mientras se arrastra.
    GameVolume(u8),
    /// Al soltar el deslizador del volumen del juego.
    GameVolumeDone,
    /// Como invitado, sincronizar la presentación con el refresco de la pantalla.
    Vsync(bool),
    /// Como invitado, decodificar por software.
    SoftwareDecoder(bool),
    /// Sonidos de la interfaz.
    InterfaceSounds(bool),
    /// Como invitado, volver a enviar el último mando que se conecte.
    PadAuto,
    /// Ranura que se ofrece primero al dar mando.
    PreferredSlot(Choice<u8>),
    /// Cambiar un atajo.
    Capture(Capture),
    /// La tecla pulsada mientras se cambiaba un atajo (nombre de `winit`, o una letra).
    KeyCaptured(String),
    /// Dejar de esperar la tecla.
    CancelCapture,
    /// Atajos por defecto.
    ResetShortcuts,
    /// Tema.
    Theme(ThemeChoice),
    /// Acento (`"system"` o una clave).
    Accent(String),
    /// Animaciones.
    Motion(MotionChoice),
    /// Comprobar ya si hay una versión nueva.
    CheckUpdates,
    /// Buscar versiones nuevas cada cierto tiempo.
    UpdateChecks(bool),
    /// Cada cuántas horas.
    UpdateInterval(Choice<u32>),
}

/// Una opción del selector de tono de un aviso.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToneOption {
    /// Un tono propio.
    Tone(Builtin),
    /// Sin sonido.
    Off,
    /// El fichero propio que ya se eligió.
    File,
    /// Elegir un fichero (abre el selector).
    Pick,
}

/// Opción de un selector con su texto.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice<T> {
    /// Valor.
    pub value: T,
    /// Texto.
    pub label: String,
}

impl<T> std::fmt::Display for Choice<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

fn choice<T>(value: T, label: impl Into<String>) -> Choice<T> {
    Choice {
        value,
        label: label.into(),
    }
}

/// Subtítulo de la cabecera con Ajustes abierto.
pub fn subtitle() -> &'static str {
    t!("settings-se-guardan-al-cambiarlos")
}

/// Lo que se pide al abrir Ajustes.
pub fn on_open(app: &mut App) -> Vec<Effect> {
    page_effects(app, app.settings_page.page)
}

fn page_effects(app: &mut App, page: SettingsPage) -> Vec<Effect> {
    match page {
        // El uso del relay del mes, al día cada vez que se abre.
        SettingsPage::Network if app.account.is_some() => vec![Effect::LoadUsage],
        SettingsPage::Audio => request_audio(app),
        _ => Vec::new(),
    }
}

/// Pide la lista de audio (si no está ya pedida); la anterior se sigue viendo mientras.
pub(crate) fn request_audio(app: &mut App) -> Vec<Effect> {
    let state = &mut app.settings_page;
    if state.audio_refreshing {
        return Vec::new();
    }
    state.audio_refreshing = true;
    if matches!(state.audio_devices, Load::Idle | Load::Failed(_)) {
        state.audio_devices = Load::Loading;
    }
    vec![Effect::LoadAudioDevices]
}

/// Si «Audio y voz» se está viendo (solo entonces se refresca la lista).
fn audio_visible(app: &App) -> bool {
    app.screen == Screen::Main
        && app.page == Page::Settings
        && app.settings_page.page == SettingsPage::Audio
}

/// Cuándo necesita Ajustes el próximo [`Message::Tick`]: con «Audio y voz» abierta, a los
/// [`AUDIO_REFRESH`] de la última respuesta para volver a pedir la lista (ningún texto cambia
/// cada segundo: la hora de la lista va en el detalle de su icono). Nunca una hora pasada
/// mientras la lista está pedida (no hay bucle).
pub fn next_tick(app: &App) -> Option<Instant> {
    if !audio_visible(app) {
        return None;
    }
    let state = &app.settings_page;
    if state.audio_refreshing {
        return None;
    }
    state.audio_answered_at.map(|at| at + AUDIO_REFRESH)
}

/// En cada [`Message::Tick`]: vuelve a pedir la lista de audio si toca.
pub fn tick(app: &mut App) -> Vec<Effect> {
    if !audio_visible(app) {
        return Vec::new();
    }
    let due = app
        .settings_page
        .audio_answered_at
        .is_some_and(|at| app.now.saturating_duration_since(at) >= AUDIO_REFRESH);
    if due { request_audio(app) } else { Vec::new() }
}

/// Reduce un mensaje. Lo que cambia los ajustes los guarda con retardo
/// ([`App::settings_changed`]).
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    let settings = &mut app.settings;
    match msg {
        Msg::Page(page) => {
            app.settings_page.page = page;
            app.settings_page.capturing = None;
            return page_effects(app, page);
        }
        Msg::ReloadDevices => {
            app.settings_page.devices = Load::Loading;
            return vec![Effect::LoadDevices];
        }
        Msg::Revoke { id, name } => {
            app.dialog = Some(Dialog::RevokeDevice { device: id, name });
            return Vec::new();
        }
        Msg::ForceRelay(on) => settings.network.force_relay = on,
        Msg::PortMapping(on) => settings.network.port_mapping = on,
        Msg::Port(text) => {
            let digits: String = text.chars().filter(char::is_ascii_digit).take(5).collect();
            app.settings_page.port_error = None;
            if digits.is_empty() {
                settings.network.local_port = 0;
            } else {
                match digits.parse::<u16>() {
                    Ok(port) if port == 0 || port >= 1024 => settings.network.local_port = port,
                    _ => {
                        app.settings_page.port_error =
                            Some(t!("settings-un-puerto-de-1024-a-65535").to_owned());
                        app.settings_page.port = Some(digits);
                        return Vec::new();
                    }
                }
            }
            app.settings_page.port = Some(digits);
        }
        Msg::Peer(index) => {
            if let Some(peer) = app.settings.peers.get(index) {
                app.dialog = Some(Dialog::SafetyNumber {
                    name: peer.name.clone(),
                    fingerprint: peer.fingerprint.clone(),
                    number: peer.safety_number.clone(),
                });
            }
            return Vec::new();
        }
        Msg::Speaker(choice) => {
            let previous = std::mem::replace(&mut settings.audio.speaker, choice.value.clone());
            let mut effects = switch_device(app, false, choice.value, previous);
            if app.settings_page.mic_test.is_some() && app.settings_page.mic_test_hearing {
                effects.push(start_mic_test(app));
            }
            return effects;
        }
        Msg::Microphone(choice) => {
            let previous = std::mem::replace(&mut settings.audio.microphone, choice.value.clone());
            let mut effects = switch_device(app, true, choice.value, previous);
            // La prueba sigue con el micrófono nuevo.
            if app.settings_page.mic_test.is_some() {
                effects.push(start_mic_test(app));
            }
            return effects;
        }
        Msg::HostVoice(on) => settings.audio.host_voice = on,
        Msg::PushToTalk(on) => {
            settings.audio.voice_mode = if on {
                VoiceMode::PushToTalk
            } else {
                VoiceMode::Activity
            };
        }
        Msg::NoiseSuppression(on) => settings.audio.noise_suppression = on,
        Msg::GuestsMuted(on) => settings.room.guests_muted = on,
        Msg::Tone(event, choice) => return choose_tone(app, event, choice.value),
        Msg::SoundPicked(event, picked) => return sound_picked(app, event, picked),
        Msg::TestSound(event) => return vec![Effect::Sound(event)],
        Msg::NotificationVolume(percent) => {
            settings.notifications.volume = percent.min(100);
            return Vec::new();
        }
        Msg::NotificationVolumeDone => {
            // Suena el primer aviso que tenga sonido, ya con el volumen nuevo.
            let mut effects = app.settings_changed();
            let notifications = &app.settings.notifications;
            effects.extend(
                SoundEvent::ALL
                    .into_iter()
                    .find(|event| notifications.sound(*event).is_on())
                    .map(Effect::Sound),
            );
            return effects;
        }
        Msg::ForgetSharePermission => {
            // No es un ajuste: va a `portal.json`, y borrarlo ahí es borrar el fichero.
            app.host.restore_tokens = RestoreTokens::default();
            return vec![Effect::SaveRestoreTokens(RestoreTokens::default())];
        }
        Msg::ChatNotifications(on) => settings.room.chat_notifications = on,
        Msg::ChatBubbles(on) => settings.room.chat_bubbles = on,
        Msg::TalkingOverlay(on) => settings.room.talking_overlay = on,
        Msg::GameVolume(percent) => {
            let percent = percent.min(MAX_VOICE_VOLUME);
            settings.audio.game_volume = percent;
            // Como invitado, suena así ya (el panel de la sala lo ve igual).
            if let Some(guest) = &mut app.guest {
                guest.game_volume = percent;
                return vec![Effect::SetGameGain {
                    gain: voice_gain(percent),
                }];
            }
            return Vec::new();
        }
        Msg::GameVolumeDone => {}
        // Al momento: la ventana lo lee de los ajustes.
        Msg::Vsync(on) => settings.video.vsync = on,
        Msg::SoftwareDecoder(software) => {
            settings.video.decoder = if software {
                DecoderChoice::Software
            } else {
                DecoderChoice::Auto
            };
            let mut effects = app.settings_changed();
            effects.push(Effect::SetSoftwareDecoder(software));
            return effects;
        }
        Msg::InterfaceSounds(on) => settings.notifications.interface_sounds = on,
        Msg::InterfaceVolume(percent) => {
            settings.notifications.interface_volume = percent.min(100);
            return Vec::new();
        }
        Msg::InterfaceVolumeDone => {
            let effects = app.settings_changed();
            preview_cue(app, coroom_ui::sound::Cue::Confirm);
            return effects;
        }
        Msg::PreviewCue(cue) => {
            preview_cue(app, cue);
            return Vec::new();
        }
        Msg::MicTest(on) => {
            app.settings_page.mic_test_error = None;
            app.settings_page.mic_test = on.then_some(0.0);
            if !on {
                return vec![Effect::MicTest(None)];
            }
            return vec![start_mic_test(app)];
        }
        Msg::MicTestHear(hear) => {
            if room_open(app) {
                // Con una sala abierta no se puede oírse (ver `mic_test`).
                return Vec::new();
            }
            app.settings_page.mic_test_hear = hear;
            // Se vuelve a abrir con la salida o sin ella.
            if app.settings_page.mic_test.is_some() {
                return vec![start_mic_test(app)];
            }
            return Vec::new();
        }
        Msg::MicVolume(percent) => {
            settings.audio.mic_volume = percent.min(MAX_VOICE_VOLUME);
            let gain = voice_gain(settings.audio.mic_volume);
            let mut effects = Vec::new();
            if app.host.room.is_some() || app.guest.is_some() {
                effects.push(Effect::SetMicGain {
                    host: app.host.room.is_some(),
                    gain,
                });
            }
            // La prueba en marcha suena con el volumen nuevo (el backend lo cambia en caliente,
            // sin volver a abrir los dispositivos).
            if app.settings_page.mic_test.is_some() {
                effects.push(start_mic_test(app));
            }
            return effects;
        }
        Msg::MicVolumeDone => {}
        Msg::PadAuto => {
            settings.input.pad = PadChoice::Auto;
            let mut effects = app.settings_changed();
            effects.push(Effect::SetPadChoice(PadChoice::Auto));
            return effects;
        }
        Msg::PreferredSlot(choice) => settings.input.preferred_slot = choice.value,
        Msg::Capture(capture) => {
            app.settings_page.capturing = Some(capture);
            return Vec::new();
        }
        Msg::CancelCapture => {
            app.settings_page.capturing = None;
            return Vec::new();
        }
        Msg::KeyCaptured(name) => return key_captured(app, name),
        Msg::ResetShortcuts => settings.input.shortcuts = Shortcuts::default(),
        Msg::Theme(theme) => {
            settings.appearance.theme = theme;
            app.refresh_theme();
        }
        Msg::Accent(key) => {
            settings.appearance.accent = key;
            app.refresh_theme();
        }
        // Se aplica al momento: [`App::motion`] lo lee de los ajustes.
        Msg::Motion(choice) => settings.appearance.motion = choice,
        Msg::CheckUpdates => return app.check_updates(true),
        Msg::UpdateChecks(on) => {
            settings.updates.check = on;
            // Al activarla, se busca enseguida; al quitarla, no se vuelve a preguntar y se para y
            // se olvida la versión nueva (también la descarga).
            let due = on && app.update.updater.release().is_none();
            app.schedule_update_check(due);
            if !on {
                let commands = app.update.updater.disable();
                let mut effects = app.settings_changed();
                effects.extend(app.update_commands(commands));
                return effects;
            }
        }
        Msg::UpdateInterval(choice) => settings.updates.interval_hours = choice.value,
    }
    app.settings_changed()
}

/// Lo elegido en el selector de tono de `event`: un tono (suena para oírlo), ninguno, el fichero
/// propio de antes o elegir otro (abre el selector de ficheros).
fn choose_tone(app: &mut App, event: SoundEvent, option: ToneOption) -> Vec<Effect> {
    let state = &mut app.settings_page;
    if state
        .sound_error
        .as_ref()
        .is_some_and(|(at, _)| *at == event)
    {
        state.sound_error = None;
    }
    let sound = app.settings.notifications.sound_mut(event);
    sound.tone = match option {
        ToneOption::Pick => {
            if state.sound_picking.is_some() {
                return Vec::new();
            }
            state.sound_picking = Some(event);
            return vec![Effect::PickSound(event)];
        }
        ToneOption::File if sound.file.is_none() => return Vec::new(),
        ToneOption::File => SoundChoice::File,
        ToneOption::Off => SoundChoice::Off,
        ToneOption::Tone(tone) => SoundChoice::Tone(tone),
    };
    let on = sound.is_on();
    let mut effects = app.settings_changed();
    if on {
        effects.push(Effect::Sound(event));
    }
    effects
}

/// El selector de ficheros volvió para `event`: el fichero (ya comprobado fuera del hilo de la
/// interfaz, `sound::pick_checked`) pasa a ser su tono y suena; si no valía o el selector falló,
/// el motivo queda en su fila y el ajuste como estaba.
fn sound_picked(
    app: &mut App,
    event: SoundEvent,
    picked: Result<Option<PathBuf>, String>,
) -> Vec<Effect> {
    let state = &mut app.settings_page;
    state.sound_picking = None;
    let file = match picked {
        Ok(None) => return Vec::new(),
        Ok(Some(file)) => file,
        Err(error) => {
            state.sound_error = Some((event, error));
            return Vec::new();
        }
    };
    state.sound_error = None;
    let sound = app.settings.notifications.sound_mut(event);
    sound.file = Some(file);
    sound.tone = SoundChoice::File;
    let mut effects = app.settings_changed();
    effects.push(Effect::Sound(event));
    effects
}

/// La tecla que llegó mientras se cambiaba un atajo o la de pulsar para hablar.
fn key_captured(app: &mut App, name: String) -> Vec<Effect> {
    let Some(capture) = app.settings_page.capturing.take() else {
        return Vec::new();
    };
    match capture {
        Capture::PushToTalk => {
            if coroom_session::input::keymap::keycode_from_name(&name).is_none() {
                app.toast(
                    Tone::Warning,
                    t!("settings-la-tecla-no-sirve-para-hablar", name = name),
                );
                return Vec::new();
            }
            app.settings.audio.push_to_talk_key = name;
        }
        Capture::Shortcut(index) => {
            let letter = name.chars().next().filter(|_| name.chars().count() == 1);
            let Some(letter) = letter.filter(char::is_ascii_alphabetic) else {
                app.toast(Tone::Warning, t!("settings-los-atajos-usan-una-letra"));
                return Vec::new();
            };
            let mut shortcuts = app.settings.input.shortcuts.clone();
            let letter = letter.to_ascii_uppercase();
            *shortcuts.letter_mut(index) = letter;
            if !shortcuts.is_valid() {
                app.toast(
                    Tone::Warning,
                    t!("settings-ya-es-otro-atajo", letter = letter),
                );
                return Vec::new();
            }
            app.settings.input.shortcuts = shortcuts;
        }
    }
    app.settings_changed()
}

/// Resultados para Ajustes.
pub fn on_event(app: &mut App, event: Event) -> Vec<Effect> {
    match event {
        Event::Devices(result) => {
            app.settings_page.devices = match result {
                Ok(devices) => Load::Loaded(devices),
                Err(err) => Load::Failed(err),
            };
            Vec::new()
        }
        Event::DeviceRevoked(result) => match result {
            Ok(text) => {
                app.toast(Tone::Success, text);
                app.settings_page.devices = Load::Loading;
                vec![Effect::LoadDevices]
            }
            Err(err) => {
                app.toast(Tone::Error, err);
                Vec::new()
            }
        },
        Event::Doctor(lines) => {
            app.settings_page.doctor = Load::Loaded(lines);
            Vec::new()
        }
        Event::MicLevel(result) => {
            let state = &mut app.settings_page;
            if state.mic_test.is_none() {
                // Llegó tarde, tras pararla.
                return Vec::new();
            }
            match result {
                Ok(level) => {
                    state.mic_test = Some(level);
                    // Fuera de «Audio y voz» la prueba se para sola.
                    if !audio_visible(app) {
                        app.settings_page.mic_test = None;
                        return vec![Effect::MicTest(None)];
                    }
                    // Se abrió una sala mientras se oía: se deja de oír (la voz volvería a
                    // entrar por el micrófono de la sala).
                    if app.settings_page.mic_test_hearing && room_open(app) {
                        return vec![start_mic_test(app)];
                    }
                }
                Err(err) => {
                    state.mic_test = None;
                    state.mic_test_error = Some(match &err {
                        MicTestError::Mic(err) => t!(
                            "settings-la-prueba-paro-microfono",
                            err = audio_error_text(err)
                        ),
                        MicTestError::Speaker(err) => {
                            t!(
                                "settings-la-prueba-paro-salida",
                                err = audio_error_text(err)
                            )
                        }
                    });
                    return vec![Effect::MicTest(None)];
                }
            }
            Vec::new()
        }
        Event::AudioDeviceSwitched {
            input,
            device,
            previous,
            result,
        } => device_switched(app, input, device, previous, result),
        Event::AudioDevices(result) => {
            let now = app.now;
            let state = &mut app.settings_page;
            state.audio_refreshing = false;
            state.audio_answered_at = Some(now);
            match result {
                Ok(devices) => {
                    state.audio_devices = Load::Loaded(devices);
                    state.audio_updated_at = Some(now);
                    state.audio_error = None;
                }
                Err(err) => {
                    if matches!(state.audio_devices, Load::Loaded(_)) {
                        state.audio_error = Some(err);
                    } else {
                        state.audio_devices = Load::Failed(err);
                    }
                }
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn on(msg: Msg) -> Message {
    Message::Settings(msg)
}

/// Prefijo del identificador de cada fila de ajuste (las pruebas miden sus rectángulos): el
/// prefijo y el título de la fila.
pub const ROW_ID: &str = "ajuste:";

/// Prefijo del identificador de cada entrada de la navegación: el prefijo y su índice en
/// [`SettingsPage::ALL`].
pub const NAV_ID: &str = "ajustes-nav:";

/// Dibuja la página: la navegación a la izquierda y la sección elegida a la derecha. El fondo
/// del elegido se desliza con un resorte hasta el nuevo ([`coroom_ui::animate::Segments`]); el
/// contenido cambia en su sitio, sin desplazarse.
pub fn view(app: &App) -> Element<'_, Message> {
    let state = &app.settings_page;
    let offline = app.server_status.online == Some(false);
    let chosen = SettingsPage::ALL
        .iter()
        .position(|page| *page == state.page);
    let entries =
        Column::with_children(SettingsPage::ALL.iter().enumerate().map(|(index, page)| {
            let selected = state.page == *page;
            // El punto del servidor que no responde tiene su sitio siempre.
            let alert = (*page == SettingsPage::Network && offline).then(|| ui::dot(Tone::Error));
            let line = row![
                icon_with(page.icon(), 18.0, SvgClass::Text),
                text(page.label())
                    .size(type_size::BODY)
                    .font(if selected {
                        coroom_ui::font::SEMIBOLD
                    } else {
                        coroom_ui::font::UI
                    })
                    .width(Length::Fill),
                ui::reserved(size::DOT, size::DOT, alert),
            ]
            .spacing(space::S)
            .align_y(Alignment::Center);
            // El elegido no pinta su fondo: lo pinta el que se desliza debajo.
            container(
                coroom_ui::animate::button(line)
                    .padding(Padding::from([9.0, 12.0]))
                    .width(Length::Fill)
                    .class(if selected {
                        ButtonClass::Segment { selected: true }
                    } else {
                        ButtonClass::Nav { selected: false }
                    })
                    .on_press(on(Msg::Page(*page))),
            )
            .id(Id::from(format!("{NAV_ID}{index}")))
            .into()
        }))
        .spacing(2);
    let nav = container(coroom_ui::animate::Segments::new(
        entries,
        chosen,
        ButtonClass::Nav { selected: true },
    ))
    .padding(Padding::from([12.0, 8.0]))
    .width(Length::Fixed(NAV_WIDTH))
    .height(Length::Fill)
    .class(ContainerClass::Sidebar);
    let content: Element<'_, Message> = match state.page {
        SettingsPage::Network => network(app),
        SettingsPage::Audio => audio(app),
        SettingsPage::Video => video(app),
        SettingsPage::Room => room(app),
        SettingsPage::Notifications => notifications(app),
        SettingsPage::Gamepads => gamepads(app),
        SettingsPage::Shortcuts => shortcuts(app),
        SettingsPage::Appearance => appearance(app),
        SettingsPage::About => about(app),
    };
    row![
        nav,
        iced_widget::rule::vertical(1),
        container(ui::page(("settings", state.page), content)).width(Length::Fill)
    ]
    .height(Length::Fill)
    .into()
}

/// Ancho de la navegación de Ajustes.
const NAV_WIDTH: f32 = 220.0;

// --- Filas ---------------------------------------------------------------------------------------

/// Cuándo surte efecto un ajuste: la etiqueta de su fila, en la columna de «cuándo aplica».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applies {
    /// Ya, también en la sala abierta («Al momento»).
    Now,
    /// En la próxima sala que se abra o en la que se entre («En la próxima sala»).
    NextRoom,
    /// Al entrar en una sala como invitado («Al entrar»).
    OnJoin,
}

impl Applies {
    /// Todas: la columna mide lo que la más ancha.
    pub const ALL: [Applies; 3] = [Applies::Now, Applies::NextRoom, Applies::OnJoin];

    /// Texto de la etiqueta.
    pub fn label(self) -> &'static str {
        match self {
            Applies::Now => t!("settings-al-momento"),
            Applies::NextRoom => t!("settings-en-la-proxima-sala"),
            Applies::OnJoin => t!("settings-al-entrar"),
        }
    }

    fn when(self) -> tag::When {
        match self {
            Applies::Now => tag::When::Live,
            Applies::NextRoom | Applies::OnJoin => tag::When::Later,
        }
    }

    /// Cuándo surte efecto una opción de vídeo ([`Video::when`]).
    pub fn of_video(option: VideoOption) -> Applies {
        match Video::when(option) {
            When::Live => Applies::Now,
            When::NextRoom => Applies::NextRoom,
        }
    }
}

/// La columna de «cuándo aplica» de una fila con su etiqueta: del ancho de la más ancha, así las
/// etiquetas de las filas con el mismo control quedan una debajo de otra.
pub fn when_column<'a>(applies: Applies) -> Element<'a, Message> {
    let labels = Applies::ALL.map(|applies| (applies.when(), applies.label()));
    tag::column(&labels, Some((applies.when(), applies.label())))
}

/// Identificador de la fila de ajuste `title` ([`ROW_ID`]).
pub fn row_id(title: &str) -> Id {
    Id::from(format!("{ROW_ID}{title}"))
}

/// Fila de ajuste, la misma en todas las secciones: título y `below` a la izquierda, la columna
/// de «cuándo aplica» y el control a la derecha ([`ui::setting_row`]). Las filas de lo que no
/// tiene que ver con una sala (`applies` vacío: el tema, un tono) no llevan la columna y dejan
/// ese ancho a su descripción; ninguna fila la pone o la quita según su valor.
fn setting<'a>(
    title: impl Into<std::borrow::Cow<'a, str>>,
    below: Option<Element<'a, Message>>,
    applies: Option<Applies>,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    let title = title.into();
    let id = row_id(&title);
    container(ui::setting_row(
        title,
        below,
        applies.map(when_column),
        control,
    ))
    .id(id)
    .width(Length::Fill)
    .into()
}

/// Descripción de una fila.
fn help<'a>(content: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    ui::secondary(content)
        .wrapping(Wrapping::WordOrGlyph)
        .into()
}

/// Descripción en rojo (un error en el sitio de la descripción).
fn help_error<'a>(content: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    ui::secondary(content)
        .class(TextClass::Tone(Tone::Error))
        .wrapping(Wrapping::WordOrGlyph)
        .into()
}

/// Descripción que cambia con el valor («Sigue al sistema», «Siempre claro»): ocupa lo que la
/// más larga de `variants`, así la fila no cambia de alto al cambiar el valor.
fn varying<'a>(variants: Vec<String>, current: Element<'a, Message>) -> Element<'a, Message> {
    ui::reserve(variants.into_iter().map(help), Some(current))
        .align_y(Vertical::Top)
        .into()
}

/// Control que cambia con el valor («Probar» o «Parar»), en una ranura del tamaño del mayor de
/// `variants` y alineado a la derecha: la descripción no se estrecha ni corta línea de otra forma.
fn steady<'a>(
    variants: Vec<Element<'a, Message>>,
    current: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    ui::reserve(variants, Some(current.into()))
        .align_x(Horizontal::Right)
        .into()
}

/// Valor monoespaciado con «Copiar» (huellas).
fn copy_row<'a>(title: &'a str, subtitle: &'a str, value: &'a str) -> Element<'a, Message> {
    setting(
        title,
        Some(
            column![
                help(subtitle),
                container(ui::mono(value, type_size::SECONDARY).wrapping(Wrapping::WordOrGlyph))
                    .padding(Padding::from([6.0, 10.0]))
                    .width(Length::Fill)
                    .class(ContainerClass::Code),
            ]
            .spacing(space::XS)
            .into(),
        ),
        None,
        ui::button_with(
            Kind::Normal,
            t!("settings-copiar"),
            Some(Icon::Copy),
            Some(Message::Copy(value.to_owned())),
        ),
    )
}

// --- Tiempo ---------------------------------------------------------------------------------------

/// Cuánto hace, redondeado para que no cambie cada segundo (ni el ancho del texto con él):
/// «ahora mismo» durante el primer minuto, después «hace 5 min», «hace 3 h», «ayer»…
pub fn ago_secs(seconds: u64) -> String {
    match seconds {
        0..60 => t!("settings-ahora-mismo").to_owned(),
        60..3600 => t!("settings-hace-min", valor = seconds / 60),
        3600..86_400 => t!("settings-hace-h", valor = seconds / 3600),
        _ => match seconds / 86_400 {
            1 => t!("settings-ayer").to_owned(),
            days => t!("settings-hace-dias", days = days),
        },
    }
}

// --- Servidor y red ------------------------------------------------------------------------------

fn network(app: &App) -> Element<'_, Message> {
    column![server(app), net(app)].spacing(space::L).into()
}

/// El servidor: su estado con «Cambiar de servidor», su certificado, el uso del relay del mes y
/// el diagnóstico.
fn server(app: &App) -> Element<'_, Message> {
    let Some(account) = &app.account else {
        return ui::section(
            t!("settings-servidor"),
            None,
            vec![ui::empty_state(
                Icon::Server,
                t!("settings-sin-sesion"),
                t!("settings-sin-servidor-texto"),
                None,
            )],
        );
    };
    let (tone, status) = match (app.server_status.online, app.server_status.rtt_ms) {
        (Some(false), _) => (Tone::Error, t!("settings-servidor-no-responde-corto")),
        (_, Some(_)) => (Tone::Success, t!("settings-servidor-conectado")),
        _ => (Tone::Neutral, t!("settings-servidor-comprobando")),
    };
    let mut rows = vec![setting(
        account.host(),
        Some(
            row![ui::status_badge(tone, status)]
                .padding(Padding {
                    top: 2.0,
                    ..Padding::ZERO
                })
                .into(),
        ),
        None,
        ui::normal(
            t!("settings-cambiar-de-servidor"),
            Some(Message::SwitchServer),
        ),
    )];
    if let Some(certificate) = &account.server.fingerprint {
        rows.push(copy_row(
            t!("settings-certificado-del-servidor"),
            t!("settings-certificado-del-servidor-ayuda"),
            certificate.as_str(),
        ));
    }
    rows.push(relay_usage(app));
    // El diagnóstico, con lo que pasa cuando el servidor no responde en el sitio de su
    // descripción (del alto de la más larga: la fila no cambia cuando deja de responder).
    let offline = t!("settings-diagnostico-sin-respuesta").to_owned();
    let below = if app.server_status.online == Some(false) {
        help_error(offline.clone())
    } else {
        help(t!("settings-diagnostico-ayuda"))
    };
    rows.push(setting(
        t!("settings-diagnostico"),
        Some(varying(
            vec![t!("settings-diagnostico-ayuda").to_owned(), offline],
            below,
        )),
        None,
        ui::button_with(
            Kind::Normal,
            t!("settings-abrir-diagnostico"),
            Some(Icon::Stethoscope),
            Some(Message::Diagnostics(diagnostics::Msg::Open)),
        ),
    ));
    ui::section(t!("settings-servidor"), None, rows)
}

/// Ancho de la barra del uso del relay.
const RELAY_BAR: f32 = 160.0;

/// El uso del relay del mes, siempre a la vista (no solo al acercarse a la cuota).
fn relay_usage(app: &App) -> Element<'_, Message> {
    let bytes = crate::ui::stats::bytes;
    let (summary, fraction) = match &app.usage {
        None => (t!("settings-relay-consultando").to_owned(), None),
        Some(usage) if !usage.relay_enabled => (t!("settings-relay-sin-relay").to_owned(), None),
        Some(usage) => match usage.quota_bytes.filter(|quota| *quota > 0) {
            Some(quota) => {
                let used = usage.bytes as f64 / quota as f64;
                let summary = if used >= 1.0 {
                    t!("settings-relay-agotado", quota = bytes(quota))
                } else {
                    t!(
                        "settings-relay-uso",
                        used = bytes(usage.bytes),
                        quota = bytes(quota),
                        percent = format!("{:.0}", used * 100.0)
                    )
                };
                (summary, Some(used.min(1.0) as f32))
            }
            None => (
                t!("settings-relay-sin-limite", used = bytes(usage.bytes)),
                None,
            ),
        },
    };
    let bar = fraction.map(|fraction| {
        progress_bar(0.0..=1.0, fraction)
            .length(Length::Fixed(RELAY_BAR))
            .girth(Length::Fixed(8.0))
            .class(if fraction >= 1.0 {
                ProgressClass::Tone(Tone::Error)
            } else if fraction >= 0.8 {
                ProgressClass::Tone(Tone::Warning)
            } else {
                ProgressClass::Accent
            })
            .into()
    });
    setting(
        t!("settings-uso-del-relay"),
        Some(
            column![
                help(summary),
                ui::caption(t!("settings-uso-del-relay-ayuda")).wrapping(Wrapping::WordOrGlyph)
            ]
            .spacing(2)
            .into(),
        ),
        None,
        ui::reserved(RELAY_BAR, 8.0, bar),
    )
}

/// Ancho del campo del puerto local.
const PORT_WIDTH: f32 = 140.0;

/// La red: relay forzado, UPnP y el puerto local, para la próxima sala.
fn net(app: &App) -> Element<'_, Message> {
    let net = &app.settings.network;
    let state = &app.settings_page;
    let port_text = state.port.clone().unwrap_or_else(|| {
        if net.local_port == 0 {
            String::new()
        } else {
            net.local_port.to_string()
        }
    });
    // El campo sin etiqueta propia (la de la fila) y el error en el sitio de la descripción,
    // del alto de la más larga: la fila no crece al aparecer.
    let error = state.port_error.as_deref();
    let shake = error.map(|error| {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        error.hash(&mut hasher);
        hasher.finish()
    });
    let input = text_input(t!("settings-cualquiera"), &port_text)
        .padding(Padding::from([8.0, 12.0]))
        .size(type_size::BODY)
        .font(coroom_ui::font::UI)
        .class(if error.is_some() {
            InputClass::Invalid
        } else {
            InputClass::Normal
        })
        .on_input(|value| on(Msg::Port(value)))
        .id("local-port")
        .width(Length::Fixed(PORT_WIDTH));
    let port_help = t!("settings-puerto-local-ayuda");
    let port_below = match error {
        Some(error) => help_error(error),
        None => help(port_help),
    };
    ui::section(
        t!("settings-red"),
        None,
        vec![
            setting(
                t!("settings-forzar-el-relay"),
                Some(help(t!("settings-forzar-el-relay-ayuda"))),
                Some(Applies::NextRoom),
                ui::switch(net.force_relay, Some(|on_off| on(Msg::ForceRelay(on_off)))),
            ),
            setting(
                t!("settings-abrir-el-puerto-en-el-router"),
                Some(help(t!("settings-abrir-el-puerto-en-el-router-ayuda"))),
                Some(Applies::NextRoom),
                ui::switch(
                    net.port_mapping,
                    Some(|on_off| on(Msg::PortMapping(on_off))),
                ),
            ),
            setting(
                t!("settings-puerto-local"),
                Some(varying(
                    vec![
                        port_help.to_owned(),
                        t!("settings-un-puerto-de-1024-a-65535").to_owned(),
                    ],
                    port_below,
                )),
                Some(Applies::NextRoom),
                coroom_ui::animate::Shake::new(input, shake),
            ),
        ],
    )
}

// --- Audio y voz ---------------------------------------------------------------------------------

/// Hay una sala abierta (compartiendo o de invitado).
fn room_open(app: &App) -> bool {
    app.host.room.is_some() || app.guest.is_some()
}

/// La prueba del micrófono con lo elegido ahora en Ajustes. Con una sala abierta no se oye uno
/// mismo: lo que suena por la salida volvería a entrar por el micrófono de la sala, y la
/// cancelación de eco no lo conoce (los demás oirían eco).
fn mic_test(app: &App) -> MicTest {
    let audio = &app.settings.audio;
    MicTest {
        mic: audio.microphone.clone(),
        speaker: audio.speaker.clone(),
        hear: app.settings_page.mic_test_hear && !room_open(app),
        gain: voice_gain(audio.mic_volume),
    }
}

/// Arranca (o actualiza) la prueba del micrófono con lo elegido ahora, y anota si se oye.
fn start_mic_test(app: &mut App) -> Effect {
    let test = mic_test(app);
    app.settings_page.mic_test_hearing = test.hear;
    Effect::MicTest(Some(test))
}

/// Guarda el micrófono o la salida elegidos y, con una sala abierta (compartiendo o de invitado),
/// los cambia en ella al momento, sin reconectar. Si allí no abre, se vuelve a `previous`
/// ([`Event::AudioDeviceSwitched`]).
fn switch_device(
    app: &mut App,
    input: bool,
    device: Option<String>,
    previous: Option<String>,
) -> Vec<Effect> {
    let mut effects = app.settings_changed();
    let host = app.host.room.is_some();
    if host || app.guest.is_some() {
        effects.push(Effect::SwitchAudioDevice {
            host,
            input,
            device,
            previous,
        });
    }
    effects
}

/// La sala dijo si usa el dispositivo elegido. Si no abrió, se vuelve al anterior (en los ajustes
/// y en el selector, si nadie eligió otro mientras) y se explica por qué.
fn device_switched(
    app: &mut App,
    input: bool,
    device: Option<String>,
    previous: Option<String>,
    result: Result<(), DeviceSwitchError>,
) -> Vec<Effect> {
    let err = match result {
        Ok(()) => return Vec::new(),
        Err(DeviceSwitchError::NotRunning) => {
            // La sala no usa ese dispositivo: queda elegido para la próxima.
            app.toast(
                Tone::Info,
                if input {
                    t!("settings-microfono-para-la-proxima")
                } else {
                    t!("settings-salida-para-la-proxima")
                },
            );
            return Vec::new();
        }
        Err(err) => err,
    };
    let reason = match &err {
        DeviceSwitchError::Open(err) => audio_error_text(err),
        DeviceSwitchError::Incompatible => t!("settings-salida-de-otro-tipo").to_owned(),
        DeviceSwitchError::Timeout | DeviceSwitchError::NotRunning => {
            t!("settings-cambio-sin-respuesta").to_owned()
        }
    };
    app.toast(
        Tone::Error,
        if input {
            t!("settings-no-se-pudo-usar-el-microfono", error = reason)
        } else {
            t!("settings-no-se-pudo-usar-la-salida", error = reason)
        },
    );
    let audio = &mut app.settings.audio;
    let chosen = if input {
        &mut audio.microphone
    } else {
        &mut audio.speaker
    };
    if *chosen != device {
        // Ya se eligió otro: manda ese.
        return Vec::new();
    }
    *chosen = previous;
    let mut effects = app.settings_changed();
    if app.settings_page.mic_test.is_some() && (input || app.settings_page.mic_test_hearing) {
        effects.push(start_mic_test(app));
    }
    effects
}

/// Un error de un dispositivo de audio, en palabras de la interfaz.
pub(crate) fn audio_error_text(err: &AudioError) -> String {
    match err {
        AudioError::DeviceLost => t!("settings-audio-error-desconectado").to_owned(),
        AudioError::NoSuchDevice(name) => {
            t!("settings-audio-error-no-existe", name = name.as_str())
        }
        AudioError::Stopped => t!("settings-audio-error-parado").to_owned(),
        AudioError::Unsupported(detail) => {
            t!(
                "settings-audio-error-no-disponible",
                detail = detail.as_str()
            )
        }
        AudioError::Codec(detail) => t!("settings-audio-error-codec", detail = detail.as_str()),
        AudioError::Backend(detail) => {
            t!("settings-audio-error-sistema", detail = detail.as_str())
        }
        AudioError::BufferTooSmall { .. } => t!("settings-audio-error-interno").to_owned(),
    }
}

/// El aviso de un micrófono o una salida que no funciona en la sala.
pub(crate) fn audio_issue_text(issue: &AudioIssue) -> String {
    match issue {
        AudioIssue::MicUnavailable(err) => {
            t!("settings-microfono-no-abrio", error = audio_error_text(err))
        }
        AudioIssue::MicLost(err) => {
            t!(
                "settings-microfono-se-detuvo",
                error = audio_error_text(err)
            )
        }
        AudioIssue::SpeakerUnavailable(err) => {
            t!("settings-salida-no-abrio", error = audio_error_text(err))
        }
        AudioIssue::SpeakerLost(err) => {
            t!("settings-salida-se-detuvo", error = audio_error_text(err))
        }
    }
}

/// Opciones de un selector de audio: «El del sistema», los de la lista y, si el elegido no está
/// conectado, él también (para que se vea cuál era).
pub(crate) fn device_choices(
    devices: &[AudioDevice],
    input: bool,
    chosen: &Option<String>,
) -> Vec<Choice<Option<String>>> {
    // El del sistema dice cuál es, si la plataforma lo sabe.
    let system = devices
        .iter()
        .find(|device| device.input == input && device.default)
        .map_or_else(
            || t!("settings-el-del-sistema").to_owned(),
            |device| {
                t!(
                    "settings-el-del-sistema-con",
                    name = device.description.as_str()
                )
            },
        );
    let mut out = vec![choice(None, system)];
    out.extend(
        devices
            .iter()
            .filter(|device| device.input == input)
            .map(|device| choice(Some(device.name.clone()), device.description.clone())),
    );
    if let Some(name) = chosen
        && !out.iter().any(|known| known.value.as_ref() == Some(name))
    {
        out.push(choice(
            Some(name.clone()),
            t!("settings-no-conectado", name = name),
        ));
    }
    out
}

/// El estado de la lista de dispositivos, en un icono con el detalle al pasar el ratón: se
/// actualiza sola (y cuándo fue la última vez) o por qué no se pudo. Ningún texto de la página
/// cambia al llegar una lista nueva.
fn devices_freshness(app: &App) -> Element<'_, Message> {
    let state = &app.settings_page;
    let lead = t!("settings-dispositivos-se-actualizan");
    let (glyph, class, tip) = match (&state.audio_error, state.audio_updated_at) {
        (Some(err), _) => (
            Icon::Warning,
            SvgClass::Tone(Tone::Warning),
            t!("settings-no-se-pudo-actualizar-la-lista", err = err),
        ),
        (None, Some(at)) => (
            Icon::Refresh,
            SvgClass::Secondary,
            t!(
                "settings-actualizado-hace",
                lead = lead,
                ago = ago_secs(app.now.saturating_duration_since(at).as_secs())
            ),
        ),
        (None, None) => (
            Icon::Refresh,
            SvgClass::Secondary,
            t!("settings-buscando-dispositivos", lead = lead),
        ),
    };
    ui::with_tooltip(icon_with(glyph, 16.0, class), tip)
}

fn audio(app: &App) -> Element<'_, Message> {
    let audio = &app.settings.audio;
    let state = &app.settings_page;
    let devices: Vec<AudioDevice> = state.audio_devices.loaded().cloned().unwrap_or_default();
    let speakers = device_choices(&devices, false, &audio.speaker);
    let speaker = speakers.iter().find(|c| c.value == audio.speaker).cloned();
    let mics = device_choices(&devices, true, &audio.microphone);
    let mic = mics.iter().find(|c| c.value == audio.microphone).cloned();
    let mut device_rows = vec![
        setting(
            t!("settings-salida"),
            Some(help(t!("settings-salida-ayuda"))),
            Some(Applies::Now),
            ui::select_fixed(
                speakers,
                speaker,
                |c| on(Msg::Speaker(c)),
                DEVICE_SELECT_WIDTH,
            ),
        ),
        setting(
            t!("settings-microfono"),
            Some(help(t!("settings-microfono-ayuda"))),
            Some(Applies::Now),
            ui::select_fixed(mics, mic, |c| on(Msg::Microphone(c)), DEVICE_SELECT_WIDTH),
        ),
    ];
    // Sin ninguna lista (la primera petición falló), el motivo debajo; con una lista, un fallo
    // al actualizarla se dice en el icono de la cabecera y la lista anterior sigue.
    if let Load::Failed(err) = &state.audio_devices {
        device_rows.push(
            container(ui::banner(
                Tone::Warning,
                t!("settings-no-se-pudieron-listar-los-dispositivos", err = err),
                None,
            ))
            .padding(Padding::from([8.0, 12.0]))
            .into(),
        );
    }
    let ptt_on = audio.voice_mode == VoiceMode::PushToTalk;
    let mic_letter = app.settings.input.shortcuts.mic.to_string();
    let voice_rows = vec![
        setting(
            t!("settings-volumen-de-tu-voz"),
            Some(help(t!("settings-volumen-de-tu-voz-ayuda"))),
            Some(Applies::Now),
            container(ui::slider(
                0..=MAX_VOICE_VOLUME,
                audio.mic_volume,
                5,
                DEFAULT_VOICE_VOLUME,
                Some(Icon::Mic),
                t!("settings-porcentaje", percent = audio.mic_volume),
                SliderClass::Accent,
                |value| on(Msg::MicVolume(value)),
                Some(on(Msg::MicVolumeDone)),
            ))
            .width(Length::Fixed(DEVICE_SELECT_WIDTH)),
        ),
        mic_test_row(app),
        setting(
            t!("settings-supresion-de-ruido"),
            Some(help(t!("settings-supresion-de-ruido-ayuda"))),
            Some(Applies::NextRoom),
            ui::switch(
                audio.noise_suppression,
                Some(|on_off| on(Msg::NoiseSuppression(on_off))),
            ),
        ),
        setting(
            t!("settings-pulsar-para-hablar"),
            Some(help(t!(
                "settings-pulsar-para-hablar-ayuda",
                key = audio.push_to_talk_key.as_str(),
                letter = mic_letter
            ))),
            Some(Applies::OnJoin),
            ui::switch(ptt_on, Some(|on_off| on(Msg::PushToTalk(on_off)))),
        ),
        setting(
            t!("settings-compartir-mi-microfono"),
            Some(help(t!("settings-compartir-mi-microfono-ayuda"))),
            Some(Applies::NextRoom),
            ui::switch(audio.host_voice, Some(|on_off| on(Msg::HostVoice(on_off)))),
        ),
    ];
    let game = audio.game_volume;
    let game_row = setting(
        t!("settings-volumen-del-juego"),
        Some(help(t!("settings-volumen-del-juego-ayuda"))),
        Some(Applies::Now),
        container(ui::slider(
            0..=MAX_VOICE_VOLUME,
            game,
            5,
            DEFAULT_VOICE_VOLUME,
            Some(if game == 0 {
                Icon::VolumeOff
            } else {
                Icon::Volume
            }),
            t!("settings-porcentaje", percent = game),
            if game == 0 {
                SliderClass::AccentMuted
            } else {
                SliderClass::Accent
            },
            |value| on(Msg::GameVolume(value)),
            Some(on(Msg::GameVolumeDone)),
        ))
        .width(Length::Fixed(DEVICE_SELECT_WIDTH)),
    );
    column![
        ui::section_with(
            t!("settings-dispositivos"),
            None,
            Some(devices_freshness(app)),
            device_rows
        ),
        ui::section(t!("settings-tu-voz"), None, voice_rows),
        ui::section(t!("settings-audio-del-juego"), None, vec![game_row]),
    ]
    .spacing(space::L)
    .into()
}

/// «Probar el micrófono»: el botón, el medidor de nivel mientras dura y «Oírte».
fn mic_test_row(app: &App) -> Element<'_, Message> {
    let state = &app.settings_page;
    let running = state.mic_test.is_some();
    let start = || {
        ui::button_with(
            Kind::Normal,
            t!("settings-probar"),
            Some(Icon::Mic),
            Some(on(Msg::MicTest(true))),
        )
    };
    let stop = || {
        ui::button_with(
            Kind::Normal,
            t!("settings-parar-la-prueba"),
            Some(Icon::Pause),
            Some(on(Msg::MicTest(false))),
        )
    };
    // «Probar» y «Parar» en una ranura del ancho del mayor.
    let button = steady(
        vec![start().into(), stop().into()],
        if running { stop() } else { start() },
    );
    let level = state.mic_test.unwrap_or(0.0);
    let meter = progress_bar(0.0..=1.0, level)
        .length(Length::Fixed(DEVICE_SELECT_WIDTH - 140.0))
        .girth(Length::Fixed(8.0))
        .class(ProgressClass::Tone(if level > 0.9 {
            Tone::Warning
        } else {
            Tone::Success
        }));
    // Con una sala abierta no se puede oírse: la voz saldría por la salida y volvería a entrar
    // por el micrófono de la sala (eco para los demás). El nivel sigue funcionando.
    let in_room = room_open(app);
    let hear = row![
        ui::switch(
            state.mic_test_hear && !in_room,
            (!in_room).then_some(|on_off| on(Msg::MicTestHear(on_off)))
        ),
        ui::caption(t!("settings-oirte")),
    ]
    .spacing(space::XS)
    .align_y(Alignment::Center);
    let mut below = column![help(t!("settings-probar-el-microfono-ayuda"))].spacing(2);
    if in_room {
        below = below.push(
            ui::caption(t!("settings-oirte-con-sala-abierta")).wrapping(Wrapping::WordOrGlyph),
        );
    }
    if let Some(err) = &state.mic_test_error {
        below = below.push(help_error(err.as_str()));
    }
    setting(
        t!("settings-probar-el-microfono"),
        Some(below.into()),
        None,
        column![
            row![button, hear]
                .spacing(space::S)
                .align_y(Alignment::Center),
            meter
        ]
        .spacing(space::XS)
        .align_x(Alignment::End),
    )
}

/// Ancho de los selectores de audio (los nombres de los dispositivos son largos).
const DEVICE_SELECT_WIDTH: f32 = 280.0;

// --- Vídeo ---------------------------------------------------------------------------------------

// Los textos de vídeo (perfiles, resoluciones, códecs, codificadores y las explicaciones de 4:4:4
// y HDR) son los de Compartir: una sola fuente, en `host`.
use host::{
    chroma_hint_for as chroma_hint, codec_choices, codec_of, encoder_choices,
    hdr_hint_for as hdr_hint, pick, preset_help, preset_name, preset_short, resolution_label,
};

/// La descripción de una opción de vídeo según el códec, del alto de la más larga de las de todos
/// los códecs (cambiar de códec no mueve la página).
fn codec_dependent(
    app: &App,
    hint: fn(Option<&VideoSupport>, Option<Codec>) -> String,
) -> Element<'_, Message> {
    let support = app.video_support.as_ref();
    let variants = [
        CodecChoice::Auto,
        CodecChoice::Av1,
        CodecChoice::Hevc,
        CodecChoice::H264,
    ]
    .map(|choice| hint(support, codec_of(choice)))
    .to_vec();
    varying(
        variants,
        help(hint(support, codec_of(app.settings.video.codec))),
    )
}

/// Vídeo: la calidad con la que se abre la sala y Avanzado (los mismos ajustes que en Compartir,
/// con sus mismos mensajes: con la sala abierta se aplican igual), y cómo se ve el vídeo de los
/// demás en tu pantalla.
fn video(app: &App) -> Element<'_, Message> {
    let video = &app.settings.video;
    let host_msg = Message::Host;
    // Los cuatro segmentos siempre, también «Personalizado»: el selector no cambia de ancho.
    let presets = ui::segmented(
        QualityPreset::PRESETS
            .into_iter()
            .chain([QualityPreset::Custom])
            .map(|preset| {
                (
                    preset_short(preset).to_owned(),
                    host_msg(host::Msg::Preset(preset)),
                    video.preset == preset,
                )
            })
            .collect(),
    );
    let preset_variants = QualityPreset::PRESETS
        .into_iter()
        .chain([QualityPreset::Custom])
        .map(|preset| {
            t!(
                "settings-perfil",
                name = preset_name(preset),
                help = preset_help(preset)
            )
        })
        .collect();
    // El selector bajo la descripción: con sus cuatro segmentos no cabe al lado.
    let quality = setting(
        t!("settings-calidad-por-defecto"),
        Some(
            column![
                varying(
                    preset_variants,
                    help(t!(
                        "settings-perfil",
                        name = preset_name(video.preset),
                        help = preset_help(video.preset)
                    )),
                ),
                presets
            ]
            .spacing(space::S)
            .into(),
        ),
        None,
        ui::gap(0.0),
    );
    let resolutions: Vec<host::Pick<Resolution>> = Resolution::ALL
        .iter()
        .rev()
        .map(|r| pick(*r, resolution_label(*r)))
        .collect();
    let resolution = resolutions
        .iter()
        .find(|c| c.value == video.max_resolution)
        .cloned();
    let fps = ui::segmented(
        FPS_CHOICES
            .iter()
            .map(|fps| {
                (
                    fps.to_string(),
                    host_msg(host::Msg::Fps(*fps)),
                    video.fps == *fps,
                )
            })
            .collect(),
    );
    let codecs = codec_choices();
    let codec = codecs.iter().find(|c| c.value == video.codec).cloned();
    let encoders = encoder_choices();
    let encoder = encoders.iter().find(|c| c.value == video.encoder).cloned();
    let advanced = vec![
        setting(
            t!("host-resolucion-maxima"),
            Some(help(t!("host-resolucion-ayuda"))),
            Some(Applies::of_video(VideoOption::Resolution)),
            ui::select(resolutions, resolution, move |c| {
                host_msg(host::Msg::Resolution(c))
            }),
        ),
        setting(
            t!("host-fotogramas-por-segundo"),
            None,
            Some(Applies::of_video(VideoOption::Fps)),
            fps,
        ),
        setting(
            t!("host-codec"),
            Some(help(t!("host-codec-ayuda"))),
            Some(Applies::of_video(VideoOption::Codec)),
            ui::select(codecs, codec, move |c| host_msg(host::Msg::Codec(c))),
        ),
        setting(
            t!("host-codificador"),
            Some(help(t!("host-codificador-ayuda"))),
            Some(Applies::of_video(VideoOption::Codec)),
            ui::select(encoders, encoder, move |c| host_msg(host::Msg::Encoder(c))),
        ),
        setting(
            t!("host-croma"),
            Some(codec_dependent(app, chroma_hint)),
            Some(Applies::of_video(VideoOption::Chroma444)),
            ui::switch(
                video.chroma_444,
                Some(move |on_off| host_msg(host::Msg::Chroma444(on_off))),
            ),
        ),
        setting(
            "HDR",
            Some(codec_dependent(app, hdr_hint)),
            Some(Applies::of_video(VideoOption::Hdr)),
            ui::switch(
                video.hdr,
                Some(move |on_off| host_msg(host::Msg::Hdr(on_off))),
            ),
        ),
    ];
    // Escalado: por ahora solo «Ajustar» (no hay nada que elegir).
    let screen = vec![
        setting(
            t!("room-escalado"),
            Some(help(t!("room-escalado-nota"))),
            None,
            ui::secondary(t!("room-ajustar")),
        ),
        setting(
            t!("room-vsync"),
            Some(help(t!("room-vsync-nota"))),
            Some(Applies::Now),
            ui::switch(video.vsync, Some(|on_off| on(Msg::Vsync(on_off)))),
        ),
        setting(
            t!("room-decodificar-por-software"),
            Some(help(t!("room-decodificar-nota"))),
            Some(Applies::Now),
            ui::switch(
                video.decoder == DecoderChoice::Software,
                Some(|on_off| on(Msg::SoftwareDecoder(on_off))),
            ),
        ),
    ];
    column![
        ui::section(
            t!("host-calidad"),
            Some(t!("settings-calidad-ayuda").to_owned()),
            vec![quality]
        ),
        ui::section(
            t!("host-avanzado"),
            Some(t!("host-avanzado-desplegado").to_owned()),
            advanced
        ),
        ui::section(
            t!("settings-en-tu-pantalla"),
            Some(t!("settings-en-tu-pantalla-ayuda").to_owned()),
            screen
        ),
    ]
    .spacing(space::L)
    .into()
}

// --- Sala ----------------------------------------------------------------------------------------

fn room(app: &App) -> Element<'_, Message> {
    let room = &app.settings.room;
    let mut as_host = vec![setting(
        t!("settings-los-invitados-entran-callados"),
        Some(help(t!("settings-los-invitados-entran-callados-ayuda"))),
        Some(Applies::NextRoom),
        ui::switch(
            room.guests_muted,
            Some(|on_off| on(Msg::GuestsMuted(on_off))),
        ),
    )];
    if REMEMBERS_SHARE_PERMISSION {
        as_host.push(share_permission_row(app));
    }
    column![
        ui::section(t!("settings-como-anfitrion"), None, as_host),
        ui::section(
            t!("settings-como-invitado"),
            None,
            vec![setting(
                t!("settings-quien-habla"),
                Some(help(t!("settings-quien-habla-ayuda"))),
                Some(Applies::Now),
                ui::switch(
                    room.talking_overlay,
                    Some(|on_off| on(Msg::TalkingOverlay(on_off))),
                ),
            )],
        ),
    ]
    .spacing(space::L)
    .into()
}

/// Fila del permiso de pantalla del portal (Linux): para qué fuentes se recuerda y «Olvidar el
/// permiso de pantalla», que borra `portal.json`; el diálogo del sistema volverá a salir una vez.
fn share_permission_row(app: &App) -> Element<'_, Message> {
    let tokens = &app.host.restore_tokens;
    let screen = tokens.get(ShareKind::Screen).is_some();
    let window = tokens.get(ShareKind::App).is_some();
    let current = match (screen, window) {
        (true, true) => t!("settings-permiso-recordado-pantalla-y-ventana"),
        (true, false) => t!("settings-permiso-recordado-pantalla"),
        (false, true) => t!("settings-permiso-recordado-ventana"),
        (false, false) => t!("settings-permiso-sin-recordar"),
    };
    let variants = [
        t!("settings-permiso-recordado-pantalla-y-ventana"),
        t!("settings-permiso-recordado-pantalla"),
        t!("settings-permiso-recordado-ventana"),
        t!("settings-permiso-sin-recordar"),
    ]
    .map(str::to_owned)
    .to_vec();
    let below = column![
        varying(variants, help(current)),
        ui::caption(t!("settings-permiso-de-pantalla-ayuda")).wrapping(Wrapping::WordOrGlyph),
    ]
    .spacing(2);
    setting(
        t!("settings-permiso-de-pantalla"),
        Some(below.into()),
        None,
        ui::button_with(
            Kind::DestructiveSoft,
            t!("settings-olvidar-el-permiso-de-pantalla"),
            None,
            (!tokens.is_empty()).then(|| on(Msg::ForgetSharePermission)),
        ),
    )
}

// --- Notificaciones ------------------------------------------------------------------------------

/// Nombre de un tono propio en los selectores.
fn tone_label(tone: Builtin) -> &'static str {
    match tone {
        Builtin::Chime => t!("settings-tono-campanilla"),
        Builtin::Rise => t!("settings-tono-llegada"),
        Builtin::Fall => t!("settings-tono-despedida"),
        Builtin::Bells => t!("settings-tono-campanas"),
        Builtin::Knock => t!("settings-tono-toc-toc"),
        Builtin::Arpeggio => t!("settings-tono-arpegio"),
        Builtin::Chord => t!("settings-tono-acorde"),
        Builtin::Bubble => t!("settings-tono-burbuja"),
        Builtin::Ping => t!("settings-tono-destello"),
    }
}

/// Título y explicación (quién lo oye y cuándo) de cada aviso.
fn sound_texts(event: SoundEvent) -> (&'static str, &'static str) {
    match event {
        SoundEvent::MemberJoined => (
            t!("settings-aviso-entra-alguien"),
            t!("settings-aviso-entra-alguien-ayuda"),
        ),
        SoundEvent::MemberLeft => (
            t!("settings-aviso-sale-alguien"),
            t!("settings-aviso-sale-alguien-ayuda"),
        ),
        SoundEvent::ChatMessage => (
            t!("settings-aviso-mensaje"),
            t!("settings-aviso-mensaje-ayuda"),
        ),
        SoundEvent::JoinRequest => (
            t!("settings-aviso-piden-entrar"),
            t!("settings-aviso-piden-entrar-ayuda"),
        ),
        SoundEvent::Invitation => (
            t!("settings-aviso-invitacion"),
            t!("settings-aviso-invitacion-ayuda"),
        ),
        SoundEvent::Admitted => (
            t!("settings-aviso-te-dejan-entrar"),
            t!("settings-aviso-te-dejan-entrar-ayuda"),
        ),
        SoundEvent::BecameHost => (
            t!("settings-aviso-anfitrion"),
            t!("settings-aviso-anfitrion-ayuda"),
        ),
        SoundEvent::FriendRequest => (
            t!("settings-aviso-amistad"),
            t!("settings-aviso-amistad-ayuda"),
        ),
    }
}

/// Las opciones del selector de tono de un aviso: los propios (el suyo primero, «(de serie)»),
/// «Ninguno», el fichero ya elegido si hay y «Elegir un fichero…».
fn tone_options(event: SoundEvent, file: Option<&std::path::Path>) -> Vec<Choice<ToneOption>> {
    let default = event.default_tone();
    let mut options: Vec<Choice<ToneOption>> = std::iter::once(default)
        .chain(Builtin::ALL.into_iter().filter(|tone| *tone != default))
        .map(|tone| {
            let label = if tone == default {
                t!("settings-tono-de-serie", name = tone_label(tone))
            } else {
                tone_label(tone).to_owned()
            };
            choice(ToneOption::Tone(tone), label)
        })
        .collect();
    options.push(choice(ToneOption::Off, t!("settings-tono-ninguno")));
    if let Some(file) = file {
        let name = file.file_name().map_or_else(
            || file.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        options.push(choice(
            ToneOption::File,
            ui::ellipsize(&name, 26).into_owned(),
        ));
    }
    options.push(choice(ToneOption::Pick, t!("settings-tono-elegir-fichero")));
    options
}

/// Ancho de los selectores de tono.
const TONE_SELECT_WIDTH: f32 = 230.0;

/// Fila de un aviso: qué es y quién lo oye, su tono y «Probar»; el error de la última elección
/// de un fichero va bajo el texto.
fn sound_row(app: &App, event: SoundEvent) -> Element<'_, Message> {
    let sound = app.settings.notifications.sound(event);
    let state = &app.settings_page;
    let (title, about) = sound_texts(event);
    let options = tone_options(event, sound.file.as_deref());
    let current = match sound.tone {
        SoundChoice::Tone(tone) => ToneOption::Tone(tone),
        SoundChoice::Off => ToneOption::Off,
        SoundChoice::File => ToneOption::File,
    };
    let selected = options
        .iter()
        .find(|option| option.value == current)
        .cloned();
    let mut below = column![help(about)].spacing(2);
    if let Some((_, error)) = state.sound_error.as_ref().filter(|(at, _)| *at == event) {
        below = below.push(
            row![
                icon_with(Icon::CircleAlert, 14.0, SvgClass::Tone(Tone::Error)),
                text(error)
                    .size(type_size::SECONDARY)
                    .class(TextClass::Tone(Tone::Error))
                    .wrapping(Wrapping::WordOrGlyph)
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        );
    }
    let picking = state.sound_picking.is_some();
    let select = || {
        ui::select_fixed(
            options.clone(),
            selected.clone(),
            move |option: Choice<ToneOption>| on(Msg::Tone(event, option)),
            TONE_SELECT_WIDTH,
        )
    };
    // Con el selector de ficheros abierto, los tonos esperan (el que vuelva manda), en el sitio
    // del selector.
    let control: Element<'_, Message> = if picking {
        ui::secondary(t!("settings-tono-eligiendo")).into()
    } else {
        select()
    };
    let test = ui::button_with(
        Kind::Flat,
        t!("settings-tono-probar"),
        Some(Icon::Play),
        sound.is_on().then(|| on(Msg::TestSound(event))),
    );
    setting(
        title,
        Some(below.into()),
        None,
        row![
            ui::reserve([select()], Some(control)).align_x(Horizontal::Left),
            test
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center),
    )
}

fn notifications<'a>(app: &'a App) -> Element<'a, Message> {
    let notifications = &app.settings.notifications;
    let room = &app.settings.room;
    let volume = notifications.volume;
    let volume_row = setting(
        t!("settings-volumen-de-los-avisos"),
        Some(help(t!("settings-volumen-de-los-avisos-ayuda"))),
        None,
        container(ui::slider(
            0..=100,
            volume,
            5,
            DEFAULT_NOTIFICATION_VOLUME,
            Some(if volume == 0 {
                Icon::VolumeOff
            } else {
                Icon::Volume
            }),
            t!("settings-porcentaje", percent = volume),
            if volume == 0 {
                SliderClass::AccentMuted
            } else {
                SliderClass::Accent
            },
            |percent| on(Msg::NotificationVolume(percent)),
            Some(on(Msg::NotificationVolumeDone)),
        ))
        .width(Length::Fixed(TONE_SELECT_WIDTH + 110.0)),
    );
    let rows = |events: &[SoundEvent]| -> Vec<Element<'a, Message>> {
        events.iter().map(|event| sound_row(app, *event)).collect()
    };
    let files = if cfg!(target_os = "windows") {
        t!("settings-tono-ayuda-windows")
    } else {
        t!("settings-tono-ayuda-linux")
    };
    let mut chat = rows(&[SoundEvent::ChatMessage]);
    chat.extend([
        setting(
            t!("settings-notificacion-del-escritorio"),
            Some(help(t!("settings-notificacion-del-escritorio-ayuda"))),
            Some(Applies::Now),
            ui::switch(
                room.chat_notifications,
                Some(|on_off| on(Msg::ChatNotifications(on_off))),
            ),
        ),
        setting(
            t!("settings-burbuja-sobre-el-video"),
            Some(help(t!("settings-burbuja-sobre-el-video-ayuda"))),
            Some(Applies::Now),
            ui::switch(
                room.chat_bubbles,
                Some(|on_off| on(Msg::ChatBubbles(on_off))),
            ),
        ),
    ]);
    let interface_sounds = setting(
        t!("settings-sonidos-de-la-interfaz"),
        Some(help(t!("settings-sonidos-de-la-interfaz-ayuda"))),
        None,
        ui::switch(
            notifications.interface_sounds,
            Some(|on_off| on(Msg::InterfaceSounds(on_off))),
        ),
    );
    let interface_volume = notifications.interface_volume;
    let interface_volume_row = setting(
        t!("settings-volumen-de-la-interfaz"),
        Some(help(t!("settings-volumen-de-la-interfaz-ayuda"))),
        None,
        container(ui::slider(
            0..=100,
            interface_volume,
            5,
            DEFAULT_INTERFACE_VOLUME,
            Some(if interface_volume == 0 {
                Icon::VolumeOff
            } else {
                Icon::Volume
            }),
            t!("settings-porcentaje", percent = interface_volume),
            if interface_volume == 0 || !notifications.interface_sounds {
                SliderClass::AccentMuted
            } else {
                SliderClass::Accent
            },
            |percent| on(Msg::InterfaceVolume(percent)),
            Some(on(Msg::InterfaceVolumeDone)),
        ))
        .width(Length::Fixed(TONE_SELECT_WIDTH + 110.0)),
    );
    column![
        ui::section(
            t!("settings-sonido"),
            None,
            vec![
                volume_row,
                interface_sounds,
                interface_volume_row,
                cue_previews(),
            ]
        ),
        ui::section(
            t!("settings-avisos-en-la-sala"),
            Some(t!("settings-avisos-en-la-sala-ayuda").to_owned()),
            rows(&[SoundEvent::MemberJoined, SoundEvent::MemberLeft]),
        ),
        ui::section(
            t!("settings-avisos-como-anfitrion"),
            Some(t!("settings-avisos-como-anfitrion-ayuda").to_owned()),
            rows(&[SoundEvent::JoinRequest]),
        ),
        ui::section(
            t!("settings-avisos-para-ti"),
            Some(t!("settings-avisos-para-ti-ayuda").to_owned()),
            rows(&[
                SoundEvent::Invitation,
                SoundEvent::Admitted,
                SoundEvent::BecameHost,
                SoundEvent::FriendRequest,
            ]),
        ),
        ui::section(
            t!("settings-chat-de-la-sala"),
            Some(t!("settings-chat-de-la-sala-ayuda").to_owned()),
            chat
        ),
        ui::caption(files).wrapping(Wrapping::WordOrGlyph),
    ]
    .spacing(space::L)
    .into()
}

// --- Mandos --------------------------------------------------------------------------------------

fn gamepads<'a>(app: &'a App) -> Element<'a, Message> {
    let input = &app.settings.input;
    let auto_help = t!("settings-mando-automatico-ayuda").to_owned();
    let auto_control =
        || -> Element<'a, Message> { ui::secondary(t!("settings-el-ultimo-que-conectes")).into() };
    let back_to_auto = || -> Element<'a, Message> {
        ui::normal(
            t!("settings-usar-el-ultimo-que-conectes"),
            Some(on(Msg::PadAuto)),
        )
        .into()
    };
    let (pad_help, pad_control, variants) = match &input.pad {
        PadChoice::Auto => (auto_help.clone(), auto_control(), vec![auto_help]),
        PadChoice::Pad(key) => {
            let chosen = t!("settings-mando-elegido-ayuda", name = key.name);
            (chosen.clone(), back_to_auto(), vec![auto_help, chosen])
        }
    };
    // Lo que cambia al volver a «El último que conectes» va en ranuras del tamaño de su variante
    // mayor: la fila no cambia de alto.
    let pad_row = setting(
        t!("settings-mando-que-envias"),
        Some(varying(variants, help(pad_help))),
        Some(Applies::Now),
        steady(vec![auto_control(), back_to_auto()], pad_control),
    );
    let max = coroom_session::input::max_gamepads();
    let mut slots = vec![choice(0u8, t!("settings-la-primera-libre"))];
    let connected_row = connected_pads(app);
    slots.extend((1..=max).map(|slot| choice(slot, t!("settings-ranura", slot = slot))));
    let slot = slots
        .iter()
        .find(|c| c.value == input.preferred_slot)
        .cloned();
    column![
        ui::section(
            t!("settings-como-invitado"),
            None,
            vec![pad_row, connected_row]
        ),
        ui::section(
            t!("settings-como-anfitrion"),
            None,
            vec![setting(
                t!("settings-ranura-preferida"),
                Some(help(t!("settings-ranura-preferida-ayuda"))),
                None,
                ui::select(slots, slot, |c| on(Msg::PreferredSlot(c))),
            )],
        ),
    ]
    .spacing(space::L)
    .into()
}

/// «Mandos conectados»: dentro de una sala, los mandos de este equipo (los ve la sesión) para
/// elegir cuál se envía, al momento; fuera de una sala, por qué no hay lista. La ayuda y el
/// control van en ranuras de su variante mayor: la fila no cambia de alto al entrar o salir.
fn connected_pads(app: &App) -> Element<'_, Message> {
    let choice = &app.settings.input.pad;
    let pads = app.guest.as_ref().and_then(|guest| guest.pads.as_deref());
    let inside = t!("settings-mandos-conectados-ayuda");
    let outside = t!("settings-mandos-conectados-fuera");
    let outside_control =
        || -> Element<'_, Message> { ui::secondary(t!("settings-sin-sala")).into() };
    let (current_help, control) = match pads {
        Some(pads) => {
            let mut options = vec![self::choice(PadChoice::Auto, t!("room-automatico"))];
            for pad in pads {
                options.push(self::choice(
                    PadChoice::Pad(pad.key.clone()),
                    pad.key.name.clone(),
                ));
            }
            // El elegido, aunque ahora no esté conectado.
            if let PadChoice::Pad(key) = choice
                && !pads.iter().any(|pad| &pad.key == key)
            {
                options.push(self::choice(
                    choice.clone(),
                    format!("{} · {}", key.name, t!("room-desconectado")),
                ));
            }
            let selected = options.iter().find(|c| &c.value == choice).cloned();
            let select = ui::select(options, selected, |c: Choice<PadChoice>| {
                Message::Guest(guest::Msg::ChoosePad(c.value))
            });
            (inside, select)
        }
        None => (outside, outside_control()),
    };
    setting(
        t!("settings-mandos-conectados"),
        Some(varying(
            vec![inside.to_owned(), outside.to_owned()],
            help(current_help),
        )),
        Some(Applies::Now),
        steady(vec![outside_control()], control),
    )
}

// --- Atajos de teclado ---------------------------------------------------------------------------

/// Qué hace cada atajo del invitado, en el orden de [`Shortcuts::letters`].
fn shortcut_names() -> [&'static str; 9] {
    [
        t!("settings-atajo-inmersivo"),
        t!("settings-atajo-soltar"),
        t!("settings-atajo-panel-de-conexion"),
        t!("settings-atajo-chat"),
        t!("settings-atajo-salir"),
        t!("settings-atajo-microfono"),
        t!("settings-atajo-juego"),
        t!("settings-atajo-mandos"),
        t!("settings-atajo-video"),
    ]
}

/// Una fila de la tabla de atajos: qué hace y dónde vale a la izquierda; sus teclas (o «Pulsa
/// una letra…» mientras se cambia) y «Cambiar» o «Cancelar» a la derecha, cada cosa en su
/// columna de ancho fijo. `capture`: cómo se cambia (sin él, el atajo es fijo).
fn shortcut_row<'a>(
    app: &App,
    name: &'a str,
    context: &'a str,
    keys: Element<'a, Message>,
    capture: Option<Capture>,
) -> Element<'a, Message> {
    let capturing = capture.is_some() && app.settings_page.capturing == capture;
    let prompt = match capture {
        Some(Capture::PushToTalk) => t!("settings-pulsa-una-tecla"),
        _ => t!("settings-pulsa-una-letra"),
    };
    let keys_slot = ui::reserve(
        [
            shortcut_caps('W'),
            ui::caption(t!("settings-pulsa-una-letra")).into(),
            ui::caption(t!("settings-pulsa-una-tecla")).into(),
        ],
        Some(if capturing {
            ui::caption(prompt).into()
        } else {
            keys
        }),
    )
    .align_x(Horizontal::Right);
    let action = capture.map(|capture| -> Element<'a, Message> {
        if capturing {
            ui::flat(t!("settings-cancelar"), Some(on(Msg::CancelCapture))).into()
        } else {
            ui::normal(t!("settings-cambiar"), Some(on(Msg::Capture(capture)))).into()
        }
    });
    let action_slot = ui::reserve(
        [
            ui::normal(t!("settings-cambiar"), None::<Message>).into(),
            ui::flat(t!("settings-cancelar"), None::<Message>).into(),
        ],
        action,
    )
    .align_x(Horizontal::Right);
    setting(
        name,
        Some(help(context)),
        None,
        row![keys_slot, action_slot]
            .spacing(space::S)
            .align_y(Alignment::Center),
    )
}

/// Todos los atajos en una tabla: los del invitado, pulsar para hablar y los de la aplicación.
fn shortcuts(app: &App) -> Element<'_, Message> {
    let input = &app.settings.input;
    let letters = input.shortcuts.letters();
    let mut rows: Vec<Element<'_, Message>> = shortcut_names()
        .into_iter()
        .zip(letters)
        .enumerate()
        .map(|(index, (name, letter))| {
            shortcut_row(
                app,
                name,
                t!("settings-atajo-en-la-sala"),
                shortcut_caps(letter),
                Some(Capture::Shortcut(index)),
            )
        })
        .collect();
    rows.push(shortcut_row(
        app,
        t!("settings-pulsar-para-hablar"),
        t!("settings-atajo-pulsar-para-hablar"),
        ui::shortcut(&[app.settings.audio.push_to_talk_key.as_str()]),
        Some(Capture::PushToTalk),
    ));
    for (name, keys) in [
        (t!("settings-atajo-ajustes"), &["Ctrl", ","][..]),
        (t!("settings-atajo-pantalla-completa"), &["F11"][..]),
        (t!("settings-atajo-cerrar-dialogos"), &["Esc"][..]),
        (t!("settings-atajo-salir-de-coroom"), &["Ctrl", "Q"][..]),
    ] {
        rows.push(shortcut_row(
            app,
            name,
            t!("settings-atajo-en-toda-la-app"),
            ui::shortcut(keys),
            None,
        ));
    }
    let defaults = input.shortcuts == Shortcuts::default();
    ui::section_with(
        t!("settings-atajos-de-teclado"),
        Some(t!("settings-atajos-ayuda").to_owned()),
        Some(
            ui::flat(
                t!("settings-restablecer"),
                (!defaults).then_some(on(Msg::ResetShortcuts)),
            )
            .into(),
        ),
        rows,
    )
}

fn shortcut_caps<'a>(letter: char) -> Element<'a, Message> {
    let letter: &'static str = match letter.to_ascii_uppercase() {
        c @ 'A'..='Z' => LETTERS[(c as u8 - b'A') as usize],
        _ => "?",
    };
    ui::shortcut(&["Ctrl", "Alt", letter])
}

const LETTERS: [&str; 26] = [
    "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S",
    "T", "U", "V", "W", "X", "Y", "Z",
];

// --- Apariencia ----------------------------------------------------------------------------------

fn appearance(app: &App) -> Element<'_, Message> {
    let current = &app.settings.appearance;
    let theme = ui::segmented(vec![
        (
            t!("settings-del-sistema").to_owned(),
            on(Msg::Theme(ThemeChoice::System)),
            current.theme == ThemeChoice::System,
        ),
        (
            t!("settings-claro").to_owned(),
            on(Msg::Theme(ThemeChoice::Light)),
            current.theme == ThemeChoice::Light,
        ),
        (
            t!("settings-oscuro").to_owned(),
            on(Msg::Theme(ThemeChoice::Dark)),
            current.theme == ThemeChoice::Dark,
        ),
    ]);
    let system_theme = t!(
        "settings-sigue-al-sistema",
        valor = match app.system.mode {
            Some(coroom_ui::theme::Mode::Dark) => t!("settings-ahora-oscuro"),
            Some(coroom_ui::theme::Mode::Light) => t!("settings-ahora-claro"),
            None => t!("settings-sin-preferencia-claro"),
        }
    );
    let theme_hint = match current.theme {
        ThemeChoice::System => system_theme.clone(),
        ThemeChoice::Light => t!("settings-siempre-claro").to_owned(),
        ThemeChoice::Dark => t!("settings-siempre-oscuro").to_owned(),
    };
    let theme_variants = vec![
        system_theme,
        t!("settings-siempre-claro").to_owned(),
        t!("settings-siempre-oscuro").to_owned(),
    ];
    let follows_system = current.accent == "system";
    let mut swatches = Row::new().spacing(space::XS).align_y(Alignment::Center);
    swatches = swatches.push(ui::button_with(
        if follows_system {
            Kind::Primary
        } else {
            Kind::Normal
        },
        t!("settings-del-sistema"),
        follows_system.then_some(Icon::Check),
        Some(on(Msg::Accent("system".to_owned()))),
    ));
    for accent in Accent::NAMED {
        let key = accent.key();
        let selected = current.accent == key;
        swatches = swatches.push(swatch(accent, key, selected));
    }
    let system_accent = t!(
        "settings-acento-del-sistema",
        valor = app.system.accent.unwrap_or_default().label(),
        source = app.system.source
    );
    let accent_hint = if follows_system {
        system_accent.clone()
    } else {
        app.accent().label()
    };
    let mut accent_variants = vec![system_accent];
    accent_variants.extend(Accent::NAMED.iter().map(|accent| accent.label()));
    let from_desktop = app.system.font.as_deref().is_some_and(|font| {
        coroom_ui::font::family_of(font).eq_ignore_ascii_case(app.fonts.family)
    });
    let font_hint = if from_desktop {
        t!("settings-fuente-del-escritorio", family = app.fonts.family)
    } else if app.fonts.is_embedded() {
        t!("settings-fuente-incluida", family = app.fonts.family)
    } else {
        t!("settings-fuente-instalada", family = app.fonts.family)
    };
    let motion = ui::segmented(vec![
        (
            t!("settings-del-sistema").to_owned(),
            on(Msg::Motion(MotionChoice::System)),
            current.motion == MotionChoice::System,
        ),
        (
            t!("settings-completas").to_owned(),
            on(Msg::Motion(MotionChoice::Full)),
            current.motion == MotionChoice::Full,
        ),
        (
            t!("settings-reducidas").to_owned(),
            on(Msg::Motion(MotionChoice::Reduced)),
            current.motion == MotionChoice::Reduced,
        ),
    ]);
    let system_motion = app.system.motion();
    let system_motion_hint = t!(
        "settings-sigue-al-sistema",
        valor = if system_motion.is_off() {
            t!("settings-ahora-sin-animaciones")
        } else if system_motion.is_reduced() {
            t!("settings-ahora-reducidas")
        } else {
            t!("settings-ahora-completas")
        }
    );
    let motion_hint = match current.motion {
        MotionChoice::System => system_motion_hint.clone(),
        MotionChoice::Reduced => t!("settings-solo-fundidos-cortos").to_owned(),
        MotionChoice::Full => t!("settings-siempre-completas").to_owned(),
    };
    let motion_variants = vec![
        system_motion_hint,
        t!("settings-solo-fundidos-cortos").to_owned(),
        t!("settings-siempre-completas").to_owned(),
    ];
    ui::section(
        t!("settings-apariencia"),
        None,
        vec![
            setting(
                t!("settings-tema"),
                Some(varying(theme_variants, help(theme_hint))),
                None,
                theme,
            ),
            setting(
                t!("settings-color-de-acento"),
                Some(
                    column![varying(accent_variants, help(accent_hint)), swatches.wrap()]
                        .spacing(space::XS)
                        .into(),
                ),
                None,
                ui::gap(0.0),
            ),
            setting(
                t!("settings-fuente"),
                Some(help(font_hint)),
                None,
                icon_with(Icon::Lock, 16.0, SvgClass::Secondary),
            ),
            setting(
                t!("settings-animaciones"),
                Some(varying(motion_variants, help(motion_hint))),
                None,
                motion,
            ),
        ],
    )
}

/// Muestra de un acento de GNOME, con su nombre al pasar el ratón.
fn swatch<'a>(accent: Accent, key: String, selected: bool) -> Element<'a, Message> {
    ui::with_tooltip(
        button(if selected {
            Element::from(iced_widget::center(icon_with(
                Icon::Check,
                16.0,
                SvgClass::OnAccent,
            )))
        } else {
            ui::gap(0.0)
        })
        .width(Length::Fixed(28.0))
        .height(Length::Fixed(28.0))
        .padding(0)
        .class(ButtonClass::Swatch {
            color: accent.rgb(),
            selected,
        })
        .on_press(on(Msg::Accent(key))),
        accent.label(),
    )
}

// --- Acerca de -----------------------------------------------------------------------------------

/// Página de las notas de esta versión.
pub fn release_page(version: &str) -> String {
    format!("{}/tag/v{version}", crate::update::RELEASES_PAGE)
}

fn updates<'a>(app: &'a App) -> Vec<Element<'a, Message>> {
    let updates = &app.settings.updates;
    let disabled = crate::update::disabled_by_env();
    // El texto y la acción de cada punto de la versión nueva ([`super::updates`]).
    let (status, action) = super::updates::status(app);
    // Del alto de la más larga de las que pueden salir sin más datos (y de la de ahora).
    let status_variants = vec![
        t!("settings-comprobando-en-las-publicaciones").to_owned(),
        t!("settings-pregunta-a-las-publicaciones").to_owned(),
        status.clone(),
    ];
    let check_sample: Element<'a, Message> =
        ui::normal(t!("settings-comprobar-ahora"), None::<Message>).into();
    let intervals: Vec<Choice<u32>> = crate::update::INTERVAL_CHOICES
        .iter()
        .map(|hours| {
            choice(
                *hours,
                if *hours == 24 {
                    t!("settings-una-vez-al-dia")
                } else {
                    t!("settings-una-vez-a-la-semana")
                },
            )
        })
        .collect();
    let interval = intervals
        .iter()
        .find(|c| c.value == updates.interval_hours)
        .cloned();
    let checks = updates.check && !disabled;
    let interval_sample: Element<'a, Message> = row![
        ui::select(intervals.clone(), interval.clone(), |c| on(
            Msg::UpdateInterval(c)
        )),
        ui::switch(checks, None::<fn(bool) -> Message>)
    ]
    .spacing(space::S)
    .align_y(Alignment::Center)
    .into();
    vec![
        setting(
            t!("settings-version-nueva"),
            Some(varying(status_variants, help(status))),
            None,
            steady(vec![check_sample], action),
        ),
        setting(
            t!("update-automatico"),
            Some(help(t!("update-automatico-ayuda"))),
            None,
            super::updates::auto_switch(app),
        ),
        setting(
            t!("settings-buscar-versiones-nuevas"),
            Some(help(t!("settings-buscar-versiones-nuevas-ayuda"))),
            None,
            steady(
                vec![interval_sample],
                row![]
                    .push(
                        checks.then(|| {
                            ui::select(intervals, interval, |c| on(Msg::UpdateInterval(c)))
                        }),
                    )
                    .push(ui::switch(
                        checks,
                        (!disabled).then_some(|on_off| on(Msg::UpdateChecks(on_off))),
                    ))
                    .spacing(space::S)
                    .align_y(Alignment::Center),
            ),
        ),
    ]
}

fn license_row<'a>(name: &'a str, license: &'a str) -> Element<'a, Message> {
    setting(name, Some(help(license)), None, ui::gap(0.0))
}

fn about(app: &App) -> Element<'_, Message> {
    let header = ui::card(
        row![
            icon_with(Icon::RadioTower, 44.0, SvgClass::Accent),
            column![
                ui::title("coroom"),
                ui::secondary(t!(
                    "settings-version-y-protocolo",
                    version = app.version,
                    protocol = PROTOCOL_VERSION
                )),
                ui::caption(t!("settings-juego-remoto-con-amigos"))
            ]
            .spacing(4)
            .width(Length::Fill),
            ui::button_with(
                Kind::Flat,
                t!("settings-notas-de-esta-version"),
                Some(Icon::ExternalLink),
                Some(Message::OpenUrl(release_page(app.version))),
            )
        ]
        .spacing(space::M)
        .align_y(Alignment::Center),
    );
    column![
        header,
        ui::section(t!("settings-versiones"), None, updates(app)),
        ui::section(
            t!("settings-licencias"),
            None,
            vec![
                license_row("coroom", t!("settings-licencia-coroom")),
                license_row(t!("settings-servidor"), t!("settings-licencia-servidor")),
                license_row(t!("settings-iconos-lucide"), "ISC"),
                license_row(t!("settings-fuentes"), t!("settings-licencia-fuentes")),
                license_row(t!("settings-interfaz"), t!("settings-licencia-interfaz")),
                license_row("FFmpeg", t!("settings-licencia-ffmpeg")),
                license_row(
                    t!("settings-dependencias-de-rust"),
                    t!("settings-licencia-dependencias")
                ),
            ]
        ),
    ]
    .spacing(space::L)
    .into()
}

/// Escenas de Ajustes para las capturas.
pub(crate) mod shots {
    use std::time::Duration;

    use coroom_session::input::gamepad::{PadChoice, PadKey};
    use coroom_transport::proto::api::{Device, Devices, Usage};

    use super::SettingsPage;
    use crate::ui::app::{App, AudioDevice, Event, Load, Message, Page};
    use crate::ui::screens::updates::shots as updates;
    use crate::ui::settings::KnownPeer;
    use crate::ui::shots::{Builder, signed_in, unix_ago};

    /// Ajustes abierto en `page` con una cuenta, sus dispositivos y la lista de audio.
    pub(crate) fn settings_page(app: &mut App, page: SettingsPage) {
        signed_in(app, Page::Settings);
        app.update(Message::Event(Event::ServerStatus {
            rtt_ms: Some(12.0),
            online: true,
        }));
        let day = Duration::from_secs(86_400);
        app.settings_page.page = page;
        app.settings_page.devices = Load::Loaded(Devices {
            current: Some(1),
            devices: vec![
                Device {
                    id: 1,
                    name: "Portátil de Ana".to_owned(),
                    fingerprint: "a1".repeat(32),
                    created_at: unix_ago(day * 90),
                    last_seen_at: unix_ago(Duration::from_secs(60)),
                },
                Device {
                    id: 2,
                    name: "Sobremesa del salón".to_owned(),
                    fingerprint: "c4".repeat(32),
                    created_at: unix_ago(day * 40),
                    last_seen_at: unix_ago(day * 3),
                },
            ],
        });
        app.settings_page.audio_devices = Load::Loaded(vec![
            AudioDevice {
                name: "usb-headset-mic".to_owned(),
                description: "Micrófono de los auriculares".to_owned(),
                input: true,
                default: true,
            },
            AudioDevice {
                name: "usb-headset".to_owned(),
                description: "Auriculares USB".to_owned(),
                input: false,
                default: true,
            },
            AudioDevice {
                name: "builtin-speakers".to_owned(),
                description: "Altavoces integrados".to_owned(),
                input: false,
                default: false,
            },
        ]);
        app.settings_page.audio_refreshing = false;
        app.settings_page.audio_updated_at = Some(app.now - Duration::from_secs(1));
        app.settings_page.audio_answered_at = app.settings_page.audio_updated_at;
        app.settings.audio.speaker = Some("usb-headset".to_owned());
        app.settings.audio.microphone = Some("usb-headset-mic".to_owned());
        app.settings.updates.last_check = Some(1_790_000_000);
        app.usage = Some(Usage {
            month: "2026-10".to_owned(),
            bytes: 12_400_000_000,
            quota_bytes: Some(50_000_000_000),
            relay_enabled: true,
            days: Vec::new(),
        });
        app.settings.peers = vec![KnownPeer {
            name: "Bea".to_owned(),
            fingerprint: "cd".repeat(32),
            safety_number: "12345 67890 13579 24680 11223 34455".to_owned(),
        }];
    }

    /// Escenas de esta pantalla.
    pub(crate) const BUILDERS: &[(&str, Builder)] = &[
        ("16-ajustes-servidor", |app| {
            settings_page(app, SettingsPage::Network);
            // Un servidor con certificado propio: se ve la huella en la que se confió.
            if let Some(account) = &mut app.account {
                account.server.fingerprint = Some("9c4e".repeat(16));
            }
        }),
        ("16-ajustes-sin-servidor", |app| {
            settings_page(app, SettingsPage::Network);
            app.update(Message::Event(Event::ServerStatus {
                rtt_ms: None,
                online: false,
            }));
        }),
        ("17-ajustes-audio", |app| {
            settings_page(app, SettingsPage::Audio);
            app.settings.audio.voice_mode = crate::ui::settings::VoiceMode::PushToTalk;
            app.settings.audio.push_to_talk_key = "V".to_owned();
        }),
        ("17-ajustes-audio-prueba", |app| {
            settings_page(app, SettingsPage::Audio);
            app.update(Message::Settings(super::Msg::MicTestHear(true)));
            app.update(Message::Settings(super::Msg::MicTest(true)));
            app.update(Message::Event(Event::MicLevel(Ok(0.62))));
        }),
        ("17-ajustes-video", |app| {
            settings_page(app, SettingsPage::Video);
            app.settings.video.vsync = true;
        }),
        ("18-ajustes-sala", |app| {
            settings_page(app, SettingsPage::Room)
        }),
        ("18-ajustes-notificaciones", |app| {
            use crate::ui::settings::{EventSound, SoundChoice};
            settings_page(app, SettingsPage::Notifications);
            let notifications = &mut app.settings.notifications;
            notifications.chat_message = EventSound {
                tone: SoundChoice::File,
                file: Some(std::path::PathBuf::from("/home/ana/Sonidos/campanita.ogg")),
            };
            notifications.member_left.tone = SoundChoice::Off;
            app.settings_page.sound_error = Some((
                crate::ui::settings::SoundEvent::ChatMessage,
                "Ese WAV usa un formato que coroom no lee (vale PCM o coma flotante).".to_owned(),
            ));
        }),
        ("19-ajustes-mandos", |app| {
            settings_page(app, SettingsPage::Gamepads);
            app.settings.input.pad = PadChoice::Pad(PadKey {
                vendor: 0x1234,
                product: 0x5678,
                name: "Mando de ejemplo".to_owned(),
            });
        }),
        ("19-ajustes-atajos", |app| {
            settings_page(app, SettingsPage::Shortcuts);
            app.settings.input.shortcuts.chat = 'H';
            app.update(Message::Settings(super::Msg::Capture(
                super::Capture::Shortcut(1),
            )));
        }),
        ("20-ajustes-apariencia", |app| {
            settings_page(app, SettingsPage::Appearance)
        }),
        ("21-ajustes-acerca", |app| {
            settings_page(app, SettingsPage::About)
        }),
        // Cada punto de la versión nueva ([`crate::ui::screens::updates`]).
        ("21-ajustes-acerca-al-dia", |app| {
            settings_page(app, SettingsPage::About);
            updates::up_to_date(app);
        }),
        ("21-ajustes-acerca-buscando", |app| {
            settings_page(app, SettingsPage::About);
            updates::checking(app);
        }),
        ("21-ajustes-acerca-descargando", |app| {
            settings_page(app, SettingsPage::About);
            updates::downloading(app);
        }),
        ("21-ajustes-acerca-en-pausa", |app| {
            settings_page(app, SettingsPage::About);
            updates::paused(app);
        }),
        ("21-ajustes-acerca-lista", |app| {
            settings_page(app, SettingsPage::About);
            updates::ready(app, false);
        }),
        ("21-ajustes-acerca-msix", |app| {
            settings_page(app, SettingsPage::About);
            updates::msix(app);
        }),
    ];
}

/// Hace sonar `cue` con el volumen de la interfaz (aunque estén apagados: es la vista previa).
fn preview_cue(app: &App, cue: coroom_ui::sound::Cue) {
    let notifications = &app.settings.notifications;
    let fraction = f32::from(notifications.interface_volume.min(100)) / 100.0;
    crate::ui::ui_sound::preview(
        cue,
        fraction * fraction,
        app.settings.audio.speaker.as_deref(),
    );
}

/// Los sonidos de la interfaz, cada uno con su botón para oírlo.
fn cue_previews<'a>() -> Element<'a, Message> {
    use coroom_ui::sound::Cue;
    let cues = [
        (t!("settings-sonido-pasar"), Cue::Hover { x: 0.5 }),
        (t!("settings-sonido-pulsar"), Cue::Press),
        (t!("settings-sonido-confirmar"), Cue::Confirm),
        (t!("settings-sonido-encender"), Cue::Toggle(true)),
        (t!("settings-sonido-apagar"), Cue::Toggle(false)),
        (t!("settings-sonido-abrir"), Cue::Open),
        (t!("settings-sonido-cerrar"), Cue::Close),
        (t!("settings-sonido-cambiar-de-pantalla"), Cue::Navigate),
        (t!("settings-sonido-copiado"), Cue::Copied),
        (t!("settings-sonido-error"), Cue::Error),
    ];
    let buttons = cues.into_iter().map(|(label, cue)| {
        coroom_ui::animate::button(
            row![
                icon_with(Icon::Volume, 14.0, SvgClass::Text),
                text(label).size(type_size::SECONDARY)
            ]
            .spacing(space::XS)
            .align_y(Alignment::Center),
        )
        .class(ButtonClass::Flat)
        .padding(Padding::from([4.0, 10.0]))
        // Sin el «toc» del botón: se oye solo el sonido elegido.
        .cue(None)
        .on_press(Message::Settings(Msg::PreviewCue(cue)))
        .into()
    });
    setting(
        t!("settings-escuchar-los-sonidos"),
        Some(help(t!("settings-escuchar-los-sonidos-ayuda"))),
        None,
        iced_widget::Row::with_children(buttons)
            .spacing(space::XS)
            .wrap(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::tests::app;

    /// Ajustes abierto en `page`, como tras navegar desde la cabecera.
    fn open(page: SettingsPage) -> App {
        let mut app = app();
        app.screen = Screen::Main;
        app.page = Page::Settings;
        app.settings_page.page = page;
        app
    }

    #[test]
    fn forgetting_the_share_permission_clears_the_tokens_file() {
        use coroom_session::capture::ShareKind;

        let mut app = open(SettingsPage::Room);
        app.host
            .restore_tokens
            .set(ShareKind::App, Some("ventana".to_owned()));
        let effects = app.update(Message::Settings(Msg::ForgetSharePermission));
        assert!(
            matches!(&effects[..], [Effect::SaveRestoreTokens(tokens)] if tokens.is_empty()),
            "sin tokens, guardar borra el fichero: {effects:?}"
        );
        assert!(app.host.restore_tokens.is_empty());
        assert!(
            app.flush_settings().is_none(),
            "no es un ajuste: settings.toml no se toca"
        );
    }

    /// Cada interruptor cambia el modelo y no guarda en el acto: se guarda en el próximo
    /// `Tick` pasado el retardo (una ráfaga de cambios es una sola escritura).
    #[test]
    fn every_change_is_saved_after_a_delay() {
        let mut app = open(SettingsPage::Network);
        let change = |app: &mut App, msg: Msg| {
            let effects = app.update(Message::Settings(msg));
            assert!(effects.is_empty(), "{effects:?}");
        };
        change(&mut app, Msg::ForceRelay(true));
        assert!(app.settings.network.force_relay);
        change(&mut app, Msg::PortMapping(false));
        assert!(!app.settings.network.port_mapping);
        change(&mut app, Msg::Port("47000".into()));
        assert_eq!(app.settings.network.local_port, 47_000);
        change(&mut app, Msg::Port("80".into()));
        assert!(
            app.settings_page.port_error.is_some(),
            "puertos privilegiados no"
        );
        assert_eq!(app.settings.network.local_port, 47_000);
        change(&mut app, Msg::Port(String::new()));
        assert_eq!(app.settings.network.local_port, 0);
        change(&mut app, Msg::HostVoice(false));
        assert!(!app.settings.audio.host_voice);
        change(&mut app, Msg::PushToTalk(true));
        assert_eq!(app.settings.audio.voice_mode, VoiceMode::PushToTalk);
        change(&mut app, Msg::PushToTalk(false));
        assert_eq!(app.settings.audio.voice_mode, VoiceMode::Activity);
        change(&mut app, Msg::NoiseSuppression(false));
        assert!(!app.settings.audio.noise_suppression);
        change(&mut app, Msg::Speaker(choice(Some("usb".into()), "USB")));
        assert_eq!(app.settings.audio.speaker.as_deref(), Some("usb"));
        change(&mut app, Msg::Microphone(choice(None, "Sistema")));
        assert_eq!(app.settings.audio.microphone, None);
        change(&mut app, Msg::GuestsMuted(true));
        assert!(app.settings.room.guests_muted);
        change(&mut app, Msg::NotificationVolume(40));
        assert_eq!(app.settings.notifications.volume, 40);
        change(&mut app, Msg::ChatNotifications(false));
        assert!(!app.settings.room.chat_notifications);
        change(&mut app, Msg::ChatBubbles(false));
        assert!(!app.settings.room.chat_bubbles);
        assert!(
            app.settings.room.talking_overlay,
            "de partida se ve quién habla"
        );
        change(&mut app, Msg::TalkingOverlay(false));
        assert!(!app.settings.room.talking_overlay);
        change(&mut app, Msg::GameVolume(250));
        assert_eq!(
            app.settings.audio.game_volume, MAX_VOICE_VOLUME,
            "sin sala, solo se guarda (y como mucho el máximo)"
        );
        change(&mut app, Msg::Vsync(true));
        assert!(app.settings.video.vsync);
        assert!(
            app.settings.notifications.interface_sounds,
            "encendidos de partida"
        );
        change(&mut app, Msg::InterfaceSounds(false));
        assert!(!app.settings.notifications.interface_sounds);
        change(&mut app, Msg::PreferredSlot(choice(2, "Ranura 2")));
        assert_eq!(app.settings.input.preferred_slot, 2);
        change(&mut app, Msg::Theme(ThemeChoice::Dark));
        assert_eq!(
            app.theme.mode(),
            coroom_ui::theme::Mode::Dark,
            "el tema cambia al momento"
        );
        change(&mut app, Msg::Accent("purple".into()));
        assert_eq!(app.theme.palette.accent, Accent::Purple);
        assert!(!app.motion().is_reduced());
        change(&mut app, Msg::Motion(MotionChoice::Reduced));
        assert!(
            app.motion().is_reduced(),
            "las animaciones cambian al momento"
        );
        change(&mut app, Msg::UpdateInterval(choice(168, "Semana")));
        assert_eq!(app.settings.updates.interval_hours, 168);
        change(&mut app, Msg::UpdateChecks(false));
        assert!(!app.settings.updates.check);
        // Pasado el retardo, el `Tick` guarda una sola vez.
        app.now += Duration::from_secs(5);
        let effects = app.update(Message::Tick);
        assert_eq!(
            effects
                .iter()
                .filter(|effect| **effect == Effect::SaveSettings)
                .count(),
            1,
            "{effects:?}"
        );
    }

    /// Una sala de invitado abierta (sin cambiar de pantalla).
    fn in_a_room(app: &mut App) {
        app.guest = Some(crate::ui::screens::guest::State::new(
            "K7Q2MZ".to_owned(),
            crate::ui::video::VideoFeed::detached(),
            app.now,
        ));
    }

    fn started(effects: &[Effect]) -> &MicTest {
        match effects {
            [.., Effect::MicTest(Some(test))] => test,
            _ => panic!("sin prueba: {effects:?}"),
        }
    }

    #[test]
    fn the_mic_test_follows_the_volume_and_never_hears_itself_in_a_room() {
        let mut app = open(SettingsPage::Audio);
        app.update(Message::Settings(Msg::MicTestHear(true)));
        let effects = app.update(Message::Settings(Msg::MicTest(true)));
        let test = started(&effects);
        assert!(test.hear && test.gain == 1.0, "{test:?}");
        // El volumen llega a la prueba en marcha (el backend lo cambia en caliente).
        let effects = app.update(Message::Settings(Msg::MicVolume(50)));
        let test = started(&effects);
        assert!(test.hear && test.gain == 0.5, "{test:?}");
        // Se abre una sala mientras se oye: deja de oírse en el siguiente nivel.
        in_a_room(&mut app);
        let effects = app.update(Message::Event(Event::MicLevel(Ok(0.3))));
        assert!(!started(&effects).hear, "{effects:?}");
        assert!(!app.settings_page.mic_test_hearing);
        assert!(
            app.update(Message::Event(Event::MicLevel(Ok(0.3))))
                .is_empty(),
            "una sola vez"
        );
        // Con la sala abierta, «Oírte» no se puede encender, y cambiar de volumen o de
        // micrófono sigue sin oírse.
        assert!(
            app.update(Message::Settings(Msg::MicTestHear(true)))
                .is_empty()
        );
        let effects = app.update(Message::Settings(Msg::MicVolume(80)));
        assert!(!started(&effects).hear);
        // Sin sala, vuelve a oírse como estaba elegido.
        app.guest = None;
        let effects = app.update(Message::Settings(Msg::MicVolume(90)));
        assert!(started(&effects).hear);
    }

    #[test]
    fn mic_test_failures_are_explained_in_spanish() {
        use coroom_session::audio::AudioError;
        let mut app = open(SettingsPage::Audio);
        app.update(Message::Settings(Msg::MicTest(true)));
        let effects = app.update(Message::Event(Event::MicLevel(Err(MicTestError::Mic(
            AudioError::NoSuchDevice("Micrófono de ejemplo".into()),
        )))));
        assert_eq!(effects, [Effect::MicTest(None)]);
        assert_eq!(
            app.settings_page.mic_test_error.as_deref(),
            Some(
                "La prueba se paró por el micrófono: no hay ningún dispositivo llamado \
                 «Micrófono de ejemplo»"
            )
        );
        app.update(Message::Settings(Msg::MicTest(true)));
        app.update(Message::Event(Event::MicLevel(Err(MicTestError::Speaker(
            AudioError::DeviceLost,
        )))));
        assert_eq!(
            app.settings_page.mic_test_error.as_deref(),
            Some("La prueba se paró por la salida: el dispositivo ya no está conectado")
        );
    }

    #[test]
    fn a_device_that_does_not_open_in_the_room_is_reverted_and_explained() {
        use coroom_session::audio::AudioError;
        let mut app = open(SettingsPage::Audio);
        app.settings.audio.microphone = Some("mic-a".into());
        in_a_room(&mut app);
        let effects = app.update(Message::Settings(Msg::Microphone(choice(
            Some("mic-b".into()),
            "Micrófono de ejemplo",
        ))));
        assert!(
            effects.contains(&Effect::SwitchAudioDevice {
                host: false,
                input: true,
                device: Some("mic-b".into()),
                previous: Some("mic-a".into()),
            }),
            "{effects:?}"
        );
        assert_eq!(app.settings.audio.microphone.as_deref(), Some("mic-b"));
        app.flush_settings();
        // No abre: vuelve el anterior, se guarda y se explica.
        app.update(Message::Event(Event::AudioDeviceSwitched {
            input: true,
            device: Some("mic-b".into()),
            previous: Some("mic-a".into()),
            result: Err(DeviceSwitchError::Open(AudioError::DeviceLost)),
        }));
        assert_eq!(app.settings.audio.microphone.as_deref(), Some("mic-a"));
        assert_eq!(app.flush_settings(), Some(Effect::SaveSettings));
        let toast = app.toasts.last().expect("aviso");
        assert_eq!(toast.tone, Tone::Error);
        assert_eq!(
            toast.text,
            "No se pudo usar ese micrófono: el dispositivo ya no está conectado. Vuelve a estar \
             elegido el anterior."
        );
        // Si mientras tanto se eligió otro, manda ese.
        app.update(Message::Settings(Msg::Speaker(choice(
            Some("out-b".into()),
            "Salida de ejemplo",
        ))));
        app.update(Message::Settings(Msg::Speaker(choice(
            Some("out-c".into()),
            "Otra salida de ejemplo",
        ))));
        app.update(Message::Event(Event::AudioDeviceSwitched {
            input: false,
            device: Some("out-b".into()),
            previous: None,
            result: Err(DeviceSwitchError::Timeout),
        }));
        assert_eq!(app.settings.audio.speaker.as_deref(), Some("out-c"));
        // La sala no usa micrófono: queda elegido para la próxima, y se dice.
        app.update(Message::Event(Event::AudioDeviceSwitched {
            input: true,
            device: Some("mic-a".into()),
            previous: None,
            result: Err(DeviceSwitchError::NotRunning),
        }));
        assert_eq!(app.settings.audio.microphone.as_deref(), Some("mic-a"));
        assert_eq!(app.toasts.last().expect("aviso").tone, Tone::Info);
    }

    #[test]
    fn audio_issues_in_the_room_read_in_spanish() {
        use coroom_session::audio::AudioError;
        assert_eq!(
            audio_issue_text(&AudioIssue::MicLost(AudioError::DeviceLost)),
            "El micrófono dejó de funcionar: el dispositivo ya no está conectado. Elige otro en \
             Ajustes o en el panel de la sala para que te sigan oyendo."
        );
        assert!(
            audio_issue_text(&AudioIssue::SpeakerUnavailable(AudioError::Backend(
                "sin respuesta".into()
            )))
            .contains("el sistema de audio dio un error (sin respuesta)")
        );
    }

    /// Elegir un tono lo guarda y lo hace sonar; «Ninguno» lo apaga (y «Probar» con él);
    /// «Elegir un fichero…» abre el selector, que vuelve con un fichero que vale, uno que no
    /// (error en su fila) o nada.
    #[test]
    fn notification_tones_are_chosen_picked_and_tested() {
        let mut app = open(SettingsPage::Notifications);
        let pick = |option: ToneOption| choice(option, "");
        let joined = SoundEvent::MemberJoined;
        let effects = app.update(Message::Settings(Msg::Tone(
            joined,
            pick(ToneOption::Tone(Builtin::Ping)),
        )));
        assert_eq!(effects, [Effect::Sound(joined)], "suena para oírlo");
        assert_eq!(
            app.settings.notifications.member_joined.tone,
            SoundChoice::Tone(Builtin::Ping)
        );
        assert!(
            app.update(Message::Settings(Msg::Tone(joined, pick(ToneOption::Off))))
                .is_empty()
        );
        assert!(!app.settings.notifications.member_joined.is_on());
        assert!(
            app.update(Message::Settings(Msg::Tone(joined, pick(ToneOption::File))))
                .is_empty(),
            "sin fichero elegido, «fichero» no hace nada"
        );
        assert_eq!(
            app.settings.notifications.member_joined.tone,
            SoundChoice::Off
        );

        let chat = SoundEvent::ChatMessage;
        assert_eq!(
            app.update(Message::Settings(Msg::Tone(chat, pick(ToneOption::Pick)))),
            [Effect::PickSound(chat)]
        );
        assert_eq!(app.settings_page.sound_picking, Some(chat));
        assert!(
            app.update(Message::Settings(Msg::Tone(joined, pick(ToneOption::Pick))))
                .is_empty(),
            "con el selector abierto no se abre otro"
        );
        // Cancelado: nada cambia.
        assert!(
            app.update(Message::Settings(Msg::SoundPicked(chat, Ok(None))))
                .is_empty()
        );
        assert_eq!(app.settings_page.sound_picking, None);
        assert_eq!(
            app.settings.notifications.chat_message.tone,
            SoundChoice::Tone(Builtin::Chime)
        );
        // Un fichero que no existe (lo comprueba la tarea del selector, `sound::checked`): error
        // en su fila y el ajuste como estaba.
        let dir = tempfile::tempdir().expect("directorio");
        let missing = dir.path().join("no-esta.wav");
        let checked = crate::ui::sound::checked(Ok(Some(missing)));
        assert!(checked.is_err(), "{checked:?}");
        assert!(
            app.update(Message::Settings(Msg::SoundPicked(chat, checked)))
                .is_empty()
        );
        assert!(matches!(&app.settings_page.sound_error, Some((at, _)) if *at == chat));
        assert_eq!(app.settings.notifications.chat_message.file, None);
        // El selector falló: se explica.
        app.update(Message::Settings(Msg::SoundPicked(
            chat,
            crate::ui::sound::checked(Err("sin portal".into())),
        )));
        assert!(
            app.settings_page
                .sound_error
                .as_ref()
                .is_some_and(|(_, error)| error.contains("sin portal"))
        );
        // Uno que vale quita el error, pasa a ser el tono y suena.
        let tone = dir.path().join("tono.wav");
        std::fs::write(&tone, Builtin::Bells.wav()).expect("escribir");
        assert_eq!(
            app.update(Message::Settings(Msg::SoundPicked(
                chat,
                Ok(Some(tone.clone()))
            ))),
            [Effect::Sound(chat)]
        );
        assert_eq!(app.settings_page.sound_error, None);
        assert_eq!(
            app.settings.notifications.chat_message.tone,
            SoundChoice::File
        );
        assert_eq!(
            app.settings.notifications.chat_message.file.as_deref(),
            Some(tone.as_path())
        );
        // Volver a un tono propio recuerda el fichero, y se puede volver a él.
        app.update(Message::Settings(Msg::Tone(
            chat,
            pick(ToneOption::Tone(Builtin::Chime)),
        )));
        assert!(app.settings.notifications.chat_message.file.is_some());
        app.update(Message::Settings(Msg::Tone(chat, pick(ToneOption::File))));
        assert_eq!(
            app.settings.notifications.chat_message.tone,
            SoundChoice::File
        );
        // «Probar» suena sin tocar nada; el volumen se guarda al soltar y suena un aviso.
        assert_eq!(
            app.update(Message::Settings(Msg::TestSound(chat))),
            [Effect::Sound(chat)]
        );
        assert!(
            app.update(Message::Settings(Msg::NotificationVolume(130)))
                .is_empty()
        );
        assert_eq!(app.settings.notifications.volume, 100, "como mucho 100 %");
        assert_eq!(
            app.update(Message::Settings(Msg::NotificationVolumeDone)),
            [Effect::Sound(SoundEvent::MemberLeft)],
            "el primero con sonido (entra alguien está en «Ninguno»)"
        );
        app.now += Duration::from_secs(5);
        let effects = app.update(Message::Tick);
        assert!(effects.contains(&Effect::SaveSettings), "{effects:?}");
    }

    /// La pantalla no lee el fichero elegido: lo comprobó ya la tarea del selector
    /// (`sound::pick_checked`), así que el hilo de la interfaz no se para con uno grande o lento.
    #[test]
    fn a_picked_tone_is_not_read_on_the_ui_thread() {
        let mut app = open(SettingsPage::Notifications);
        let chat = SoundEvent::ChatMessage;
        app.update(Message::Settings(Msg::Tone(
            chat,
            choice(ToneOption::Pick, ""),
        )));
        // Una ruta que no existe: si la pantalla la leyera, la rechazaría.
        let unread = PathBuf::from("/nonexistent/coroom/tono.wav");
        assert_eq!(
            app.update(Message::Settings(Msg::SoundPicked(
                chat,
                Ok(Some(unread.clone()))
            ))),
            [Effect::Sound(chat)]
        );
        assert_eq!(
            app.settings.notifications.chat_message.file.as_deref(),
            Some(unread.as_path())
        );
    }

    /// Cada aviso ofrece todos los tonos propios (el suyo primero), «Ninguno», el fichero si lo
    /// hay y elegir otro.
    #[test]
    fn every_notification_offers_every_tone() {
        for event in SoundEvent::ALL {
            let options = tone_options(event, None);
            assert_eq!(options[0].value, ToneOption::Tone(event.default_tone()));
            assert_eq!(options.len(), Builtin::ALL.len() + 2);
            assert_eq!(options.last().map(|o| o.value), Some(ToneOption::Pick));
            let with_file = tone_options(event, Some(std::path::Path::new("/home/ana/tono.ogg")));
            assert!(
                with_file
                    .iter()
                    .any(|o| o.value == ToneOption::File && o.label == "tono.ogg")
            );
        }
    }

    #[test]
    fn actions_ask_for_what_they_need() {
        let mut app = open(SettingsPage::Network);
        app.settings.input.pad = PadChoice::Pad(coroom_session::input::gamepad::PadKey {
            vendor: 1,
            product: 2,
            name: "Mando".into(),
        });
        assert_eq!(
            app.update(Message::Settings(Msg::PadAuto)),
            [Effect::SetPadChoice(PadChoice::Auto)]
        );
        assert_eq!(app.settings.input.pad, PadChoice::Auto);
        assert_eq!(
            app.update(Message::Settings(Msg::ReloadDevices)),
            [Effect::LoadDevices]
        );
        assert!(
            app.update(Message::Settings(Msg::Revoke {
                id: 2,
                name: "Sobremesa".into()
            }))
            .is_empty()
        );
        assert!(matches!(
            app.dialog,
            Some(Dialog::RevokeDevice { device: 2, .. })
        ));
        assert_eq!(
            app.update(Message::ConfirmDialog),
            [Effect::RevokeDevice(2)]
        );
        assert_eq!(
            app.update(Message::Settings(Msg::CheckUpdates)),
            [Effect::CheckUpdates]
        );
    }

    #[test]
    fn shortcuts_are_captured_and_must_not_collide() {
        let mut app = open(SettingsPage::Gamepads);
        assert_eq!(Shortcuts::default().letters().len(), 9);
        app.update(Message::Settings(Msg::Capture(Capture::Shortcut(0))));
        assert!(
            app.update(Message::Settings(Msg::KeyCaptured("k".into())))
                .is_empty()
        );
        assert_eq!(app.settings.input.shortcuts.immersive, 'K');
        // Una letra que ya usa otro atajo se rechaza y lo dice.
        app.update(Message::Settings(Msg::Capture(Capture::Shortcut(7))));
        app.update(Message::Settings(Msg::KeyCaptured("K".into())));
        assert_eq!(app.settings.input.shortcuts.pads, 'J');
        assert!(
            app.toasts
                .iter()
                .any(|toast| toast.text.contains("ya es otro atajo")),
            "{:?}",
            app.toasts
        );
        // Lo que no es una letra, también.
        app.update(Message::Settings(Msg::Capture(Capture::Shortcut(3))));
        app.update(Message::Settings(Msg::KeyCaptured("F5".into())));
        assert_eq!(app.settings.input.shortcuts.chat, 'C');
        assert_eq!(app.settings_page.capturing, None);
        app.update(Message::Settings(Msg::Capture(Capture::PushToTalk)));
        app.update(Message::Settings(Msg::KeyCaptured("CapsLock".into())));
        assert_eq!(app.settings.audio.push_to_talk_key, "CapsLock");
        // Soltar el teclado y el ratón es el segundo.
        app.update(Message::Settings(Msg::Capture(Capture::Shortcut(1))));
        app.update(Message::Settings(Msg::KeyCaptured("t".into())));
        assert_eq!(app.settings.input.shortcuts.release, 'T');
        // Apagar el vídeo es el último y choca como los demás.
        app.update(Message::Settings(Msg::Capture(Capture::Shortcut(8))));
        app.update(Message::Settings(Msg::KeyCaptured("m".into())));
        assert_eq!(app.settings.input.shortcuts.video, 'I');
        app.update(Message::Settings(Msg::Capture(Capture::Shortcut(8))));
        app.update(Message::Settings(Msg::KeyCaptured("y".into())));
        assert_eq!(app.settings.input.shortcuts.video, 'Y');
        app.update(Message::Settings(Msg::Capture(Capture::Shortcut(1))));
        app.update(Message::Settings(Msg::CancelCapture));
        assert_eq!(app.settings_page.capturing, None);
        app.update(Message::Settings(Msg::ResetShortcuts));
        assert_eq!(app.settings.input.shortcuts, Shortcuts::default());
    }

    /// La lista de audio se pide al abrir la página y, mientras siga abierta, cada 3 s tras la
    /// última respuesta; fuera de la página, nunca.
    #[test]
    fn audio_devices_refresh_every_three_seconds_while_open() {
        let mut app = open(SettingsPage::Network);
        assert_eq!(
            app.update(Message::Settings(Msg::Page(SettingsPage::Audio))),
            [Effect::LoadAudioDevices]
        );
        assert!(app.settings_page.audio_devices.is_loading());
        // Pedida: ningún `Tick` la vuelve a pedir ni hace falta uno ya.
        assert!(app.update(Message::Tick).is_empty());
        assert_eq!(next_tick(&app), None);
        app.update(Message::Event(Event::AudioDevices(Ok(vec![AudioDevice {
            name: "usb".into(),
            description: "USB".into(),
            input: true,
            default: true,
        }]))));
        let answered = app.now;
        assert_eq!(app.settings_page.audio_updated_at, Some(answered));
        // Nada se redibuja cada segundo: el próximo `Tick` es el de volver a pedirla.
        assert_eq!(next_tick(&app), Some(answered + AUDIO_REFRESH));
        app.now = answered + Duration::from_secs(2);
        assert!(app.update(Message::Tick).is_empty(), "aún no toca");
        app.now = answered + AUDIO_REFRESH;
        assert_eq!(app.update(Message::Tick), [Effect::LoadAudioDevices]);
        assert!(app.settings_page.audio_refreshing);
        assert!(
            matches!(app.settings_page.audio_devices, Load::Loaded(_)),
            "la lista anterior sigue a la vista"
        );
        assert_eq!(
            next_tick(&app),
            None,
            "pedida, ningún `Tick` hasta la respuesta (nunca una hora pasada)"
        );
        // Un error con lista: se avisa y se mantiene la anterior.
        app.update(Message::Event(Event::AudioDevices(Err(
            "sin PipeWire".into()
        ))));
        assert!(matches!(app.settings_page.audio_devices, Load::Loaded(_)));
        assert_eq!(
            app.settings_page.audio_error.as_deref(),
            Some("sin PipeWire")
        );
        // Con otra página delante, no se pide.
        app.update(Message::Settings(Msg::Page(SettingsPage::Room)));
        app.now += Duration::from_secs(10);
        assert_eq!(next_tick(&app), None);
        assert!(app.update(Message::Tick).is_empty());
        app.page = Page::Home;
        app.settings_page.page = SettingsPage::Audio;
        assert!(
            app.update(Message::Tick).is_empty(),
            "fuera de Ajustes, nunca"
        );
    }

    #[test]
    fn opening_the_server_section_asks_for_the_relay_usage() {
        let mut app = open(SettingsPage::Audio);
        assert!(
            app.update(Message::Settings(Msg::Page(SettingsPage::Network)))
                .is_empty(),
            "sin cuenta no hay uso del relay que pedir"
        );
        crate::ui::shots::signed_in(&mut app, Page::Settings);
        // Servidor y red pide el uso del relay del mes cada vez que se abre.
        app.update(Message::Settings(Msg::Page(SettingsPage::Audio)));
        assert_eq!(
            app.update(Message::Settings(Msg::Page(SettingsPage::Network))),
            [Effect::LoadUsage]
        );
        assert_eq!(ago_secs(10), "ahora mismo");
        assert_eq!(ago_secs(7200), "hace 2 h");
        assert_eq!(ago_secs(86_400), "ayer");
        assert_eq!(ago_secs(86_400 * 3), "hace 3 días");
        assert!(release_page("1.2.3").ends_with("/tag/v1.2.3"));
    }

    #[test]
    fn a_missing_device_stays_visible() {
        let devices = [AudioDevice {
            name: "usb".into(),
            description: "USB".into(),
            input: true,
            default: true,
        }];
        let choices = device_choices(&devices, true, &Some("bt".into()));
        assert_eq!(choices.len(), 3);
        assert!(choices[2].label.contains("bt"), "{}", choices[2].label);
        assert_eq!(device_choices(&devices, false, &None).len(), 1);
    }

    /// Las nueve secciones, en el orden de la especificación, con Servidor y red de partida y
    /// la cuenta fuera (tiene su página en el riel).
    #[test]
    fn the_sections_come_in_their_order() {
        let labels: Vec<&str> = SettingsPage::ALL.iter().map(|page| page.label()).collect();
        assert_eq!(
            labels,
            [
                "Servidor y red",
                "Audio y voz",
                "Vídeo",
                "Sala",
                "Notificaciones",
                "Mandos",
                "Atajos de teclado",
                "Apariencia",
                "Acerca de",
            ]
        );
        assert_eq!(SettingsPage::default(), SettingsPage::Network);
        // Cada sección se elige con su entrada y abre su contenido (sin pánicos al dibujarla).
        let mut app = open(SettingsPage::Network);
        for page in SettingsPage::ALL {
            app.update(Message::Settings(Msg::Page(page)));
            assert_eq!(app.settings_page.page, page);
            let _ = view(&app);
        }
    }

    /// Un solo nombre para cada cosa (la tabla de la especificación) y las etiquetas de cuándo
    /// aplica un ajuste.
    #[test]
    fn every_setting_has_one_name() {
        assert_eq!(t!("settings-salida"), "Salida");
        assert_eq!(t!("settings-microfono"), "Micrófono");
        assert_eq!(
            Applies::ALL.map(Applies::label),
            ["Al momento", "En la próxima sala", "Al entrar"]
        );
        assert_eq!(
            Applies::of_video(VideoOption::Resolution),
            Applies::Now,
            "lo de Avanzado lleva la etiqueta de cuándo se aplica en la sala"
        );
        assert_eq!(Applies::of_video(VideoOption::Hdr), Applies::NextRoom);
    }

    /// El volumen del juego y el decodificador cambian también en la sala abierta como invitado.
    #[test]
    fn video_and_game_audio_apply_in_the_open_room() {
        let mut app = open(SettingsPage::Audio);
        in_a_room(&mut app);
        assert_eq!(
            app.update(Message::Settings(Msg::GameVolume(60))),
            [Effect::SetGameGain {
                gain: voice_gain(60)
            }]
        );
        assert_eq!(app.guest.as_ref().map(|guest| guest.game_volume), Some(60));
        assert_eq!(app.settings.audio.game_volume, 60);
        app.update(Message::Settings(Msg::GameVolumeDone));
        assert!(app.flush_settings().is_some(), "se guarda al soltar");
        let effects = app.update(Message::Settings(Msg::SoftwareDecoder(true)));
        assert!(
            effects.contains(&Effect::SetSoftwareDecoder(true)),
            "{effects:?}"
        );
        assert_eq!(app.settings.video.decoder, DecoderChoice::Software);
        app.update(Message::Settings(Msg::SoftwareDecoder(false)));
        assert_eq!(app.settings.video.decoder, DecoderChoice::Auto);
    }

    /// Las descripciones de 4:4:4 y HDR se calculan para cada códec (la fila reserva la más
    /// larga) con los textos de Compartir, y Compartir las da para el códec elegido.
    #[test]
    fn the_codec_hints_match_share() {
        let support = VideoSupport {
            yuv444: vec![(Codec::Hevc, "HEVC (NVENC)".to_owned())],
            hdr10: vec![],
            decode_yuv444: vec![],
            decode_hdr10: vec![],
            hdr_display: Some(false),
        };
        let mut app = open(SettingsPage::Video);
        app.video_support = Some(support.clone());
        for (choice, codec) in [
            (CodecChoice::Auto, None),
            (CodecChoice::Av1, Some(Codec::Av1)),
            (CodecChoice::Hevc, Some(Codec::Hevc)),
        ] {
            app.settings.video.codec = choice;
            assert_eq!(chroma_hint(Some(&support), codec), host::chroma_hint(&app));
            assert_eq!(hdr_hint(Some(&support), codec), host::hdr_hint(&app));
        }
    }
}
