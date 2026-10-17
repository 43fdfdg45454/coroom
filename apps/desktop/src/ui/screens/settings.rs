//! Ajustes: lo que no depende de una sala.
//!
//! Siete páginas con su navegación lateral: **Cuenta y servidor** (quién eres, el servidor y su
//! huella, los dispositivos de la cuenta, la red y los números de seguridad), **Audio y voz**
//! (salida y micrófono con la lista en vivo, la voz como anfitrión y como invitado), **Sala** (cómo
//! entran los invitados y los avisos del chat), **Notificaciones** (el volumen de los avisos con
//! sonido y el tono de cada uno, con «Probar»), **Mandos** (qué mando envías, la ranura que
//! ofreces y los atajos `Ctrl+Alt+letra`), **Apariencia** (tema, acento y la fuente del
//! escritorio) y **Acerca de** (versión, versiones nuevas y licencias). Lo de vídeo y calidad vive
//! en Compartir y en el panel de la sala; el diagnóstico se abre desde la cabecera y la sala.
//!
//! Cada cambio se guarda solo, poco después ([`App::settings_changed`]); las filas cuyo cambio no
//! llega a la sala abierta lo dicen con una etiqueta («Al abrir la sala», «Al entrar en la sala»).

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use coroom_session::audio::AudioError;
use coroom_session::capture::ShareKind;
use coroom_session::input::gamepad::PadChoice;
use coroom_session::remote::audio::{AudioIssue, DeviceSwitchError};
use coroom_transport::proto::PROTOCOL_VERSION;
use coroom_transport::proto::api::Devices;
use coroom_ui::Element;
use coroom_ui::icon::{Icon, icon_with};
use coroom_ui::style::{
    ButtonClass, ContainerClass, ProgressClass, SliderClass, SvgClass, TextClass,
};
use coroom_ui::tag;
use coroom_ui::theme::{Accent, Tone};
use coroom_ui::tokens::{space, text as type_size};
use coroom_ui::widget::{self as ui, Kind};
use iced_core::text::Wrapping;
use iced_core::{Alignment, Length, Padding};
use iced_widget::{Column, Row, button, column, container, progress_bar, row, text};

use crate::portal::{REMEMBERS_SHARE_PERMISSION, RestoreTokens};
use crate::ui::app::{
    App, AudioDevice, Dialog, Effect, Event, Load, Message, MicTest, MicTestError, Page, Screen,
};
use crate::ui::screens::diagnostics;
use crate::ui::settings::{
    DEFAULT_NOTIFICATION_VOLUME, DEFAULT_VOICE_VOLUME, MAX_VOICE_VOLUME, MotionChoice, Shortcuts,
    SoundChoice, SoundEvent, ThemeChoice, VoiceMode, voice_gain,
};
use crate::ui::tones::Builtin;

/// Cada cuánto se vuelve a pedir la lista de micrófonos y salidas con «Audio y voz» abierta.
pub const AUDIO_REFRESH: Duration = Duration::from_secs(3);

/// Páginas de Ajustes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsPage {
    /// Cuenta, servidor, dispositivos de la cuenta y red.
    #[default]
    Account,
    /// Dispositivos de audio y voz.
    Audio,
    /// Cómo entran los invitados y los avisos del chat.
    Room,
    /// Los avisos con sonido.
    Notifications,
    /// Mandos y atajos.
    Gamepads,
    /// Tema, acento y fuente.
    Appearance,
    /// Versión, versiones nuevas y licencias.
    About,
}

impl SettingsPage {
    /// Todas, en el orden de la navegación.
    pub const ALL: [SettingsPage; 7] = [
        SettingsPage::Account,
        SettingsPage::Audio,
        SettingsPage::Room,
        SettingsPage::Notifications,
        SettingsPage::Gamepads,
        SettingsPage::Appearance,
        SettingsPage::About,
    ];

    /// Nombre en la navegación.
    pub fn label(self) -> &'static str {
        match self {
            SettingsPage::Account => t!("settings-cuenta-y-servidor"),
            SettingsPage::Audio => t!("settings-audio-y-voz"),
            SettingsPage::Room => t!("settings-sala"),
            SettingsPage::Notifications => t!("settings-notificaciones"),
            SettingsPage::Gamepads => t!("settings-mandos"),
            SettingsPage::Appearance => t!("settings-apariencia"),
            SettingsPage::About => t!("settings-acerca-de"),
        }
    }

    fn icon(self) -> Icon {
        match self {
            SettingsPage::Account => Icon::UserRound,
            SettingsPage::Audio => Icon::Headphones,
            SettingsPage::Room => Icon::Users,
            SettingsPage::Notifications => Icon::Bell,
            SettingsPage::Gamepads => Icon::Gamepad,
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
    /// Cuándo llegó la última lista («actualizado hace N s»).
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
        SettingsPage::Account
            if !app.settings_page.devices.is_loading() && app.account.is_some() =>
        {
            app.settings_page.devices = match std::mem::take(&mut app.settings_page.devices) {
                Load::Loaded(devices) => Load::Loaded(devices),
                _ => Load::Loading,
            };
            vec![Effect::LoadDevices]
        }
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

/// Cuándo necesita Ajustes el próximo [`Message::Tick`]: con «Audio y voz» abierta, cada
/// segundo para «actualizado hace N s» y a los [`AUDIO_REFRESH`] de la última respuesta para
/// volver a pedir la lista. Nunca una hora pasada mientras la lista está pedida (no hay bucle).
pub fn next_tick(app: &App) -> Option<Instant> {
    if !audio_visible(app) {
        return None;
    }
    let state = &app.settings_page;
    let mut next = state.audio_updated_at.map(|at| {
        let elapsed = app.now.saturating_duration_since(at).as_secs();
        at + Duration::from_secs(elapsed + 1)
    });
    if !state.audio_refreshing
        && let Some(at) = state.audio_answered_at
    {
        let due = at + AUDIO_REFRESH;
        next = Some(next.map_or(due, |next| next.min(due)));
    }
    next
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
            // Al activarla, se comprueba enseguida; al quitarla, no se vuelve a preguntar.
            let due = on && app.update.available.is_none();
            app.schedule_update_check(due);
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
            match index {
                0 => shortcuts.immersive = letter,
                1 => shortcuts.release = letter,
                2 => shortcuts.stats = letter,
                3 => shortcuts.chat = letter,
                4 => shortcuts.leave = letter,
                5 => shortcuts.mic = letter,
                6 => shortcuts.game = letter,
                _ => shortcuts.pads = letter,
            }
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

/// Dibuja la página: la navegación a la izquierda y la página elegida a la derecha.
pub fn view(app: &App) -> Element<'_, Message> {
    let state = &app.settings_page;
    let offline = app.server_status.online == Some(false);
    let entries = Column::with_children(SettingsPage::ALL.iter().map(|page| {
        let selected = state.page == *page;
        let mut line = row![
            icon_with(page.icon(), 18.0, SvgClass::Text),
            text(page.label())
                .size(type_size::BODY)
                .font(if selected {
                    coroom_ui::font::SEMIBOLD
                } else {
                    coroom_ui::font::UI
                })
                .width(Length::Fill)
        ]
        .spacing(space::S)
        .align_y(Alignment::Center);
        if *page == SettingsPage::Account && offline {
            line = line.push(ui::dot(Tone::Error));
        }
        button(line)
            .padding(Padding::from([9.0, 12.0]))
            .width(Length::Fill)
            .class(ButtonClass::Nav { selected })
            .on_press(on(Msg::Page(*page)))
            .into()
    }))
    .spacing(2);
    let nav = container(
        column![
            entries,
            iced_widget::space::vertical(),
            container(ui::caption(t!("settings-video-esta-en-compartir")).wrapping(Wrapping::Word))
                .padding(Padding::from([8.0, 12.0]))
        ]
        .height(Length::Fill),
    )
    .padding(Padding::from([12.0, 8.0]))
    .width(Length::Fixed(NAV_WIDTH))
    .height(Length::Fill)
    .class(ContainerClass::Sidebar);
    let content: Element<'_, Message> = match state.page {
        SettingsPage::Account => account(app),
        SettingsPage::Audio => audio(app),
        SettingsPage::Room => room(app),
        SettingsPage::Notifications => notifications(app),
        SettingsPage::Gamepads => gamepads(app),
        SettingsPage::Appearance => appearance(app),
        SettingsPage::About => about(app),
    };
    row![
        nav,
        iced_widget::rule::vertical(1),
        container(ui::page(content)).width(Length::Fill)
    ]
    .height(Length::Fill)
    .into()
}

/// Ancho de la navegación de Ajustes.
const NAV_WIDTH: f32 = 220.0;

/// Fila de ajuste con una etiqueta de cuándo surte efecto delante del control.
fn tagged_row<'a>(
    title: impl text::IntoFragment<'a>,
    subtitle: Option<String>,
    when: Option<tag::When>,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut trailing = Row::new().spacing(space::S).align_y(Alignment::Center);
    if let Some(when) = when {
        trailing = trailing.push(tag::tag(
            when,
            match when {
                tag::When::Live => t!("settings-en-vivo"),
                tag::When::Later => t!("settings-al-abrir-la-sala"),
            },
        ));
    }
    ui::pref_row(title, subtitle, trailing.push(control))
}

/// Etiqueta gris con otro texto («Al entrar en la sala»).
fn later_row<'a>(
    title: impl text::IntoFragment<'a>,
    subtitle: Option<String>,
    label: &'a str,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    ui::pref_row(
        title,
        subtitle,
        row![tag::later(label), control.into()]
            .spacing(space::S)
            .align_y(Alignment::Center),
    )
}

/// Valor monoespaciado con «Copiar», en una fila de ajuste.
fn copy_row<'a>(title: &'a str, subtitle: Option<String>, value: &'a str) -> Element<'a, Message> {
    let mut texts = column![ui::body(title)].spacing(2);
    if let Some(subtitle) = subtitle {
        texts = texts.push(ui::secondary(subtitle).wrapping(Wrapping::WordOrGlyph));
    }
    texts = texts.push(
        container(ui::mono(value, type_size::SECONDARY).wrapping(Wrapping::WordOrGlyph))
            .padding(Padding::from([6.0, 10.0]))
            .width(Length::Fill)
            .class(ContainerClass::Code),
    );
    row![
        texts.width(Length::Fill),
        ui::button_with(
            Kind::Normal,
            t!("settings-copiar"),
            Some(Icon::Copy),
            Some(Message::Copy(value.to_owned())),
        )
    ]
    .spacing(space::M)
    .align_y(Alignment::Center)
    .padding(Padding::from([10.0, 16.0]))
    .into()
}

// --- Cuenta y servidor ---------------------------------------------------------------------------

fn account(app: &App) -> Element<'_, Message> {
    let mut page = Column::new().spacing(space::L);
    if app.server_status.online == Some(false) {
        let host = app
            .account
            .as_ref()
            .map(crate::ui::app::AccountInfo::host)
            .unwrap_or_default();
        page = page.push(ui::banner(
            Tone::Error,
            t!("settings-el-servidor-no-responde", host = host),
            Some((
                t!("settings-abrir-diagnostico"),
                Message::Diagnostics(diagnostics::Msg::Open),
            )),
        ));
    }
    let Some(account) = &app.account else {
        return page
            .push(ui::empty_state(
                Icon::UserRound,
                t!("settings-sin-sesion"),
                t!("settings-sin-sesion-texto"),
                None,
            ))
            .into();
    };
    let server_status = match (app.server_status.online, app.server_status.rtt_ms) {
        (Some(false), _) => t!("settings-servidor-no-responde-corto").to_owned(),
        (_, Some(rtt)) => t!("settings-servidor-responde-en", rtt = rtt.round() as u64),
        _ => t!("settings-servidor-comprobando").to_owned(),
    };
    let mut rows = vec![ui::list_row(
        Some(crate::ui::avatars::view(
            app,
            &account.display_name,
            account.avatar,
            crate::ui::avatars::SETTINGS_SIDE,
        )),
        account.display_name.as_str(),
        Some(t!(
            "settings-usuario-en-este-equipo",
            username = account.username,
            device = account.device
        )),
        vec![
            ui::button_with(
                Kind::DestructiveFlat,
                t!("settings-cerrar-sesion"),
                Some(Icon::LogOut),
                Some(Message::SignOut),
            )
            .into(),
        ],
    )];
    rows.extend(crate::ui::avatars::settings_row(app));
    rows.extend([ui::list_row(
        Some(icon_with(Icon::Server, 22.0, SvgClass::Secondary)),
        account.host(),
        Some(server_status),
        vec![
            ui::normal(
                t!("settings-cambiar-de-servidor"),
                Some(Message::SwitchServer),
            )
            .into(),
        ],
    )]);
    if let Some(certificate) = &account.server.fingerprint {
        rows.push(copy_row(
            t!("settings-certificado-del-servidor"),
            Some(t!("settings-certificado-del-servidor-ayuda").to_owned()),
            certificate.as_str(),
        ));
    }
    rows.push(copy_row(
        t!("settings-identidad-de-este-equipo"),
        Some(t!("settings-identidad-de-este-equipo-ayuda").to_owned()),
        account.fingerprint.as_str(),
    ));
    page = page.push(ui::section(t!("settings-cuenta"), None, rows));
    page = page.push(devices(app));
    page = page.push(network(app));
    page = page.push(peers(app));
    page.into()
}

/// "hace 5 min", "hace 3 h", "hace 2 días".
pub fn ago(unix: i64, now: i64) -> String {
    let seconds = (now - unix).max(0);
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

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

fn devices(app: &App) -> Element<'_, Message> {
    let body: Element<'_, Message> = match &app.settings_page.devices {
        Load::Idle | Load::Loading => ui::skeleton(t!("settings-cargando-los-dispositivos"), 2),
        Load::Failed(err) => ui::error_state(
            t!("settings-no-se-pudieron-cargar-los-dispositivos"),
            err.as_str(),
            Some((t!("settings-reintentar"), on(Msg::ReloadDevices))),
        ),
        Load::Loaded(list) => {
            let now = now_unix();
            let rows = list
                .devices
                .iter()
                .map(|device| {
                    let current = list.current == Some(device.id);
                    let short: String = device.fingerprint.chars().take(16).collect();
                    let trailing = if current {
                        ui::status_badge(Tone::Info, t!("settings-este-equipo"))
                    } else {
                        ui::button_with(
                            Kind::DestructiveFlat,
                            t!("settings-desvincular"),
                            Some(Icon::UserX),
                            Some(on(Msg::Revoke {
                                id: device.id,
                                name: device.name.clone(),
                            })),
                        )
                        .into()
                    };
                    let seen = if current {
                        t!("settings-ahora-mismo").to_owned()
                    } else {
                        ago(device.last_seen_at, now)
                    };
                    ui::list_row(
                        Some(icon_with(Icon::Laptop, 22.0, SvgClass::Secondary)),
                        device.name.as_str(),
                        Some(t!("settings-visto-huella", seen = seen, short = short)),
                        vec![trailing],
                    )
                })
                .collect();
            ui::boxed_list(rows)
        }
    };
    column![
        column![
            ui::strong(t!("settings-dispositivos-de-tu-cuenta")),
            ui::secondary(t!("settings-dispositivos-de-tu-cuenta-ayuda"))
                .wrapping(Wrapping::WordOrGlyph)
        ]
        .spacing(2),
        body
    ]
    .spacing(space::XS)
    .into()
}

fn network(app: &App) -> Element<'_, Message> {
    let net = &app.settings.network;
    let state = &app.settings_page;
    let port_text = state.port.clone().unwrap_or_else(|| {
        if net.local_port == 0 {
            String::new()
        } else {
            net.local_port.to_string()
        }
    });
    let port = container(Element::from(
        ui::field(t!("settings-puerto-udp"), port_text)
            .placeholder(t!("settings-cualquiera"))
            .on_input(|value| on(Msg::Port(value)))
            .error(state.port_error.as_deref())
            .id("local-port"),
    ))
    .width(Length::Fixed(180.0));
    ui::section(
        t!("settings-red"),
        Some(t!("settings-red-ayuda").to_owned()),
        vec![
            ui::pref_row(
                t!("settings-forzar-el-relay"),
                Some(t!("settings-forzar-el-relay-ayuda").to_owned()),
                ui::switch(net.force_relay, Some(|on_off| on(Msg::ForceRelay(on_off)))),
            ),
            ui::pref_row(
                t!("settings-abrir-el-puerto-en-el-router"),
                Some(t!("settings-abrir-el-puerto-en-el-router-ayuda").to_owned()),
                ui::switch(
                    net.port_mapping,
                    Some(|on_off| on(Msg::PortMapping(on_off))),
                ),
            ),
            ui::pref_row(
                t!("settings-puerto-local"),
                Some(t!("settings-puerto-local-ayuda").to_owned()),
                port,
            ),
        ],
    )
}

fn peers(app: &App) -> Element<'_, Message> {
    let rows: Vec<Element<'_, Message>> = if app.settings.peers.is_empty() {
        vec![
            container(ui::caption(t!("settings-numeros-de-seguridad-vacio")))
                .padding(Padding::from([12.0, 16.0]))
                .into(),
        ]
    } else {
        app.settings
            .peers
            .iter()
            .enumerate()
            .take(8)
            .map(|(index, peer)| {
                let preview: String = peer.safety_number.chars().take(17).collect();
                ui::list_row(
                    Some(ui::avatar(&app.theme, &peer.name, 28.0)),
                    peer.name.as_str(),
                    Some(t!("settings-numero-recortado", preview = preview)),
                    vec![ui::normal(t!("settings-ver"), Some(on(Msg::Peer(index)))).into()],
                )
            })
            .collect()
    };
    ui::section(
        t!("settings-numeros-de-seguridad"),
        Some(t!("settings-numeros-de-seguridad-ayuda").to_owned()),
        rows,
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

/// «En vivo: …» con la edad de la lista.
fn audio_freshness(app: &App) -> String {
    let state = &app.settings_page;
    let lead = t!("settings-dispositivos-en-vivo");
    match state.audio_updated_at {
        Some(at) => t!(
            "settings-actualizado-hace",
            lead = lead,
            seconds = app.now.saturating_duration_since(at).as_secs()
        ),
        None => t!("settings-buscando-dispositivos", lead = lead),
    }
}

fn audio(app: &App) -> Element<'_, Message> {
    let audio = &app.settings.audio;
    let state = &app.settings_page;
    let devices: Vec<AudioDevice> = state.audio_devices.loaded().cloned().unwrap_or_default();
    let speakers = device_choices(&devices, false, &audio.speaker);
    let speaker = speakers.iter().find(|c| c.value == audio.speaker).cloned();
    let mics = device_choices(&devices, true, &audio.microphone);
    let mic = mics.iter().find(|c| c.value == audio.microphone).cloned();
    let mut rows = vec![
        ui::pref_row(
            t!("settings-salida"),
            Some(t!("settings-salida-ayuda").to_owned()),
            ui::select_fixed(
                speakers,
                speaker,
                |c| on(Msg::Speaker(c)),
                DEVICE_SELECT_WIDTH,
            ),
        ),
        ui::pref_row(
            t!("settings-microfono"),
            Some(t!("settings-microfono-ayuda").to_owned()),
            ui::select_fixed(mics, mic, |c| on(Msg::Microphone(c)), DEVICE_SELECT_WIDTH),
        ),
        ui::pref_row(
            t!("settings-volumen-de-tu-voz"),
            Some(t!("settings-volumen-de-tu-voz-ayuda").to_owned()),
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
        later_row(
            t!("settings-supresion-de-ruido"),
            Some(t!("settings-supresion-de-ruido-ayuda").to_owned()),
            t!("settings-en-la-proxima-sala"),
            ui::switch(
                audio.noise_suppression,
                Some(|on_off| on(Msg::NoiseSuppression(on_off))),
            ),
        ),
    ];
    let error = match &state.audio_devices {
        Load::Failed(err) => Some(err.as_str()),
        _ => state.audio_error.as_deref(),
    };
    if let Some(err) = error {
        rows.push(
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
    let key: Element<'_, Message> = if state.capturing == Some(Capture::PushToTalk) {
        row![
            ui::caption(t!("settings-pulsa-una-tecla")),
            ui::flat(t!("settings-cancelar"), Some(on(Msg::CancelCapture)))
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center)
        .into()
    } else {
        row![
            ui::shortcut(&[audio.push_to_talk_key.as_str()]),
            ui::normal(
                t!("settings-cambiar"),
                Some(on(Msg::Capture(Capture::PushToTalk)))
            )
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center)
        .into()
    };
    let mic_letter = app.settings.input.shortcuts.mic.to_string();
    column![
        ui::section(
            t!("settings-dispositivos"),
            Some(audio_freshness(app)),
            rows
        ),
        ui::section(
            t!("settings-voz-como-anfitrion"),
            None,
            vec![tagged_row(
                t!("settings-compartir-mi-microfono"),
                Some(t!("settings-compartir-mi-microfono-ayuda").to_owned()),
                Some(tag::When::Later),
                ui::switch(audio.host_voice, Some(|on_off| on(Msg::HostVoice(on_off)))),
            )],
        ),
        ui::section(
            t!("settings-voz-como-invitado"),
            None,
            vec![later_row(
                t!("settings-pulsar-para-hablar"),
                Some(t!("settings-pulsar-para-hablar-ayuda", letter = mic_letter)),
                t!("settings-al-entrar-en-la-sala"),
                row![
                    key,
                    ui::switch(ptt_on, Some(|on_off| on(Msg::PushToTalk(on_off))))
                ]
                .spacing(space::S)
                .align_y(Alignment::Center),
            )],
        ),
    ]
    .spacing(space::L)
    .into()
}

/// «Probar el micrófono»: el botón, el medidor de nivel mientras dura y «Oírte».
fn mic_test_row(app: &App) -> Element<'_, Message> {
    let state = &app.settings_page;
    let running = state.mic_test.is_some();
    let button = if running {
        ui::button_with(
            Kind::Normal,
            t!("settings-parar-la-prueba"),
            Some(Icon::Pause),
            Some(on(Msg::MicTest(false))),
        )
    } else {
        ui::button_with(
            Kind::Normal,
            t!("settings-probar"),
            Some(Icon::Mic),
            Some(on(Msg::MicTest(true))),
        )
    };
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
    let mut help = column![
        ui::secondary(t!("settings-probar-el-microfono-ayuda")).wrapping(Wrapping::WordOrGlyph)
    ]
    .spacing(2);
    if in_room {
        help = help.push(
            ui::caption(t!("settings-oirte-con-sala-abierta")).wrapping(Wrapping::WordOrGlyph),
        );
    }
    if let Some(err) = &state.mic_test_error {
        help = help.push(
            ui::caption(err.as_str())
                .class(TextClass::Tone(Tone::Error))
                .wrapping(Wrapping::WordOrGlyph),
        );
    }
    ui::pref_row_with(
        t!("settings-probar-el-microfono"),
        help,
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

// --- Sala ----------------------------------------------------------------------------------------

fn room(app: &App) -> Element<'_, Message> {
    let room = &app.settings.room;
    let mut as_host = vec![tagged_row(
        t!("settings-los-invitados-entran-callados"),
        Some(t!("settings-los-invitados-entran-callados-ayuda").to_owned()),
        Some(tag::When::Later),
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
            vec![ui::pref_row(
                t!("settings-quien-habla"),
                Some(t!("settings-quien-habla-ayuda").to_owned()),
                ui::switch(
                    room.talking_overlay,
                    Some(|on_off| on(Msg::TalkingOverlay(on_off))),
                ),
            )],
        ),
        ui::section(
            t!("settings-chat-de-la-sala"),
            Some(t!("settings-chat-de-la-sala-ayuda").to_owned()),
            vec![
                ui::pref_row(
                    t!("settings-notificacion-del-escritorio"),
                    Some(t!("settings-notificacion-del-escritorio-ayuda").to_owned()),
                    ui::switch(
                        room.chat_notifications,
                        Some(|on_off| on(Msg::ChatNotifications(on_off))),
                    ),
                ),
                ui::pref_row(
                    t!("settings-burbuja-sobre-el-video"),
                    Some(t!("settings-burbuja-sobre-el-video-ayuda").to_owned()),
                    ui::switch(
                        room.chat_bubbles,
                        Some(|on_off| on(Msg::ChatBubbles(on_off)))
                    ),
                ),
            ],
        ),
    ]
    .spacing(space::L)
    .into()
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
    let (title, help) = sound_texts(event);
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
    let mut below = column![ui::secondary(help).wrapping(Wrapping::WordOrGlyph)].spacing(2);
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
    let select = ui::select_fixed(
        options,
        selected,
        move |option: Choice<ToneOption>| on(Msg::Tone(event, option)),
        TONE_SELECT_WIDTH,
    );
    // Con el selector de ficheros abierto, los tonos esperan (el que vuelva manda).
    let select: Element<'_, Message> = if picking {
        container(ui::secondary(t!("settings-tono-eligiendo")))
            .width(Length::Fixed(TONE_SELECT_WIDTH))
            .into()
    } else {
        select
    };
    let test = ui::button_with(
        Kind::Flat,
        t!("settings-tono-probar"),
        Some(Icon::Play),
        sound.is_on().then(|| on(Msg::TestSound(event))),
    );
    ui::pref_row_with(
        title,
        below,
        row![select, test]
            .spacing(space::XS)
            .align_y(Alignment::Center),
    )
}

fn notifications(app: &App) -> Element<'_, Message> {
    let notifications = &app.settings.notifications;
    let volume = notifications.volume;
    let volume_row = ui::pref_row(
        t!("settings-volumen-de-los-avisos"),
        Some(t!("settings-volumen-de-los-avisos-ayuda").to_owned()),
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
    let rows = |events: &[SoundEvent]| events.iter().map(|event| sound_row(app, *event)).collect();
    let files = if cfg!(target_os = "windows") {
        t!("settings-tono-ayuda-windows")
    } else {
        t!("settings-tono-ayuda-linux")
    };
    column![
        ui::section(t!("settings-avisos-volumen"), None, vec![volume_row]),
        ui::section(
            t!("settings-avisos-en-la-sala"),
            Some(t!("settings-avisos-en-la-sala-ayuda").to_owned()),
            rows(&[
                SoundEvent::MemberJoined,
                SoundEvent::MemberLeft,
                SoundEvent::ChatMessage,
            ]),
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
        ui::caption(files).wrapping(Wrapping::WordOrGlyph),
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
    let texts = column![
        ui::body(t!("settings-permiso-de-pantalla")),
        ui::secondary(current).wrapping(Wrapping::WordOrGlyph),
        ui::caption(t!("settings-permiso-de-pantalla-ayuda")).wrapping(Wrapping::WordOrGlyph),
    ]
    .spacing(2);
    let forget = ui::flat(
        t!("settings-olvidar-el-permiso-de-pantalla"),
        (!tokens.is_empty()).then(|| on(Msg::ForgetSharePermission)),
    );
    row![texts.width(Length::Fill), forget]
        .spacing(space::M)
        .align_y(Alignment::Center)
        .padding(Padding::from([10.0, 16.0]))
        .into()
}

// --- Mandos --------------------------------------------------------------------------------------

/// Qué hace cada atajo del invitado, en el orden de [`Shortcuts::letters`].
fn shortcut_names() -> [&'static str; 8] {
    [
        t!("settings-atajo-inmersivo"),
        t!("settings-atajo-soltar"),
        t!("settings-atajo-panel-de-conexion"),
        t!("settings-atajo-chat"),
        t!("settings-atajo-salir"),
        t!("settings-atajo-microfono"),
        t!("settings-atajo-juego"),
        t!("settings-atajo-mandos"),
    ]
}

fn gamepads(app: &App) -> Element<'_, Message> {
    let input = &app.settings.input;
    let state = &app.settings_page;
    let pads_letter = input.shortcuts.pads.to_string();
    let pad_row = match &input.pad {
        PadChoice::Auto => ui::pref_row(
            t!("settings-mando-que-envias"),
            Some(t!("settings-mando-automatico-ayuda", letter = pads_letter)),
            ui::secondary(t!("settings-el-ultimo-que-conectes")),
        ),
        PadChoice::Pad(key) => ui::pref_row(
            t!("settings-mando-que-envias"),
            Some(t!(
                "settings-mando-elegido-ayuda",
                name = key.name,
                letter = pads_letter
            )),
            ui::normal(
                t!("settings-usar-el-ultimo-que-conectes"),
                Some(on(Msg::PadAuto)),
            ),
        ),
    };
    let max = coroom_session::input::max_gamepads();
    let mut slots = vec![choice(0u8, t!("settings-la-primera-libre"))];
    slots.extend((1..=max).map(|slot| choice(slot, t!("settings-ranura", slot = slot))));
    let slot = slots
        .iter()
        .find(|c| c.value == input.preferred_slot)
        .cloned();
    let letters = input.shortcuts.letters();
    let mut rows: Vec<Element<'_, Message>> = shortcut_names()
        .into_iter()
        .zip(letters)
        .enumerate()
        .map(|(index, (name, letter))| {
            let control: Element<'_, Message> = if state.capturing == Some(Capture::Shortcut(index))
            {
                row![
                    ui::caption(t!("settings-pulsa-una-letra")),
                    ui::flat(t!("settings-cancelar"), Some(on(Msg::CancelCapture)))
                ]
                .spacing(space::XS)
                .align_y(Alignment::Center)
                .into()
            } else {
                row![
                    shortcut_caps(letter),
                    ui::normal(
                        t!("settings-cambiar"),
                        Some(on(Msg::Capture(Capture::Shortcut(index))))
                    )
                ]
                .spacing(space::XS)
                .align_y(Alignment::Center)
                .into()
            };
            ui::pref_row(name, None, control)
        })
        .collect();
    let defaults = input.shortcuts == Shortcuts::default();
    rows.push(
        container(row![
            ui::fill(),
            ui::flat(
                t!("settings-restablecer"),
                (!defaults).then_some(on(Msg::ResetShortcuts))
            )
        ])
        .padding(Padding::from([4.0, 8.0]))
        .into(),
    );
    column![
        ui::section(t!("settings-como-invitado"), None, vec![pad_row]),
        ui::section(
            t!("settings-como-anfitrion"),
            None,
            vec![ui::pref_row(
                t!("settings-ranura-preferida"),
                Some(t!("settings-ranura-preferida-ayuda").to_owned()),
                ui::select(slots, slot, |c| on(Msg::PreferredSlot(c))),
            )],
        ),
        ui::section(
            t!("settings-atajos-del-invitado"),
            Some(t!("settings-atajos-del-invitado-ayuda").to_owned()),
            rows
        ),
        ui::section(
            t!("settings-atajos-de-la-aplicacion"),
            None,
            vec![
                ui::pref_row(
                    t!("settings-atajo-ajustes"),
                    None,
                    ui::shortcut(&["Ctrl", ","])
                ),
                ui::pref_row(
                    t!("settings-atajo-pantalla-completa"),
                    None,
                    ui::shortcut(&["F11"])
                ),
                ui::pref_row(
                    t!("settings-atajo-cerrar-dialogos"),
                    None,
                    ui::shortcut(&["Esc"])
                ),
                ui::pref_row(
                    t!("settings-atajo-salir-de-coroom"),
                    None,
                    ui::shortcut(&["Ctrl", "Q"])
                ),
            ]
        ),
    ]
    .spacing(space::L)
    .into()
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
    let theme_hint = match current.theme {
        ThemeChoice::System => t!(
            "settings-sigue-al-sistema",
            valor = match app.system.mode {
                Some(coroom_ui::theme::Mode::Dark) => t!("settings-ahora-oscuro"),
                Some(coroom_ui::theme::Mode::Light) => t!("settings-ahora-claro"),
                None => t!("settings-sin-preferencia-claro"),
            }
        ),
        ThemeChoice::Light => t!("settings-siempre-claro").to_owned(),
        ThemeChoice::Dark => t!("settings-siempre-oscuro").to_owned(),
    };
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
    let accent_hint = if follows_system {
        t!(
            "settings-acento-del-sistema",
            valor = app.accent().label(),
            source = app.system.source
        )
    } else {
        app.accent().label()
    };
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
            t!("settings-como-el-sistema").to_owned(),
            on(Msg::Motion(MotionChoice::System)),
            current.motion == MotionChoice::System,
        ),
        (
            t!("settings-reducidas").to_owned(),
            on(Msg::Motion(MotionChoice::Reduced)),
            current.motion == MotionChoice::Reduced,
        ),
        (
            t!("settings-completas").to_owned(),
            on(Msg::Motion(MotionChoice::Full)),
            current.motion == MotionChoice::Full,
        ),
    ]);
    let system_motion = app.system.motion();
    let motion_hint = match current.motion {
        MotionChoice::System => t!(
            "settings-sigue-al-sistema",
            valor = if system_motion.is_off() {
                t!("settings-ahora-sin-animaciones")
            } else if system_motion.is_reduced() {
                t!("settings-ahora-reducidas")
            } else {
                t!("settings-ahora-completas")
            }
        ),
        MotionChoice::Reduced => t!("settings-solo-fundidos-cortos").to_owned(),
        MotionChoice::Full => t!("settings-siempre-completas").to_owned(),
    };
    column![ui::section(
        t!("settings-apariencia"),
        None,
        vec![
            ui::pref_row(t!("settings-tema"), Some(theme_hint), theme),
            container(
                column![
                    ui::body(t!("settings-color-de-acento")),
                    ui::secondary(accent_hint),
                    swatches.wrap()
                ]
                .spacing(space::XS)
            )
            .padding(Padding::from([10.0, 16.0]))
            .into(),
            ui::pref_row(
                t!("settings-fuente"),
                Some(font_hint),
                icon_with(Icon::Lock, 16.0, SvgClass::Secondary)
            ),
            ui::pref_row(t!("settings-animaciones"), Some(motion_hint), motion),
        ]
    )]
    .into()
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

fn updates(app: &App) -> Vec<Element<'_, Message>> {
    let state = &app.update;
    let updates = &app.settings.updates;
    let disabled = crate::update::disabled_by_env();
    let status = if disabled {
        t!(
            "settings-desactivado-con-este-paquete",
            valor = crate::update::DISABLE_ENV
        )
    } else if state.checking {
        t!("settings-comprobando-en-las-publicaciones").to_owned()
    } else if let Some(update) = &state.available {
        t!(
            "settings-hay-una-version-nueva",
            version = update.version,
            current = app.version
        )
    } else if let Some(err) = &state.error {
        t!("settings-la-ultima-comprobacion-fallo", err = err)
    } else if updates.last_check.is_some() {
        t!("settings-tienes-la-ultima-version").to_owned()
    } else {
        t!("settings-pregunta-a-las-publicaciones").to_owned()
    };
    let mut action = row![].spacing(space::S).align_y(Alignment::Center);
    if let Some(update) = &state.available {
        action = action.push(ui::button_with(
            Kind::Primary,
            t!("settings-ver-la-version-nueva"),
            Some(Icon::ExternalLink),
            Some(Message::OpenUrl(update.url.clone())),
        ));
    }
    action = action.push(ui::normal(
        if state.checking {
            t!("settings-comprobando")
        } else {
            t!("settings-comprobar-ahora")
        },
        (!state.checking && !disabled).then_some(on(Msg::CheckUpdates)),
    ));
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
    vec![
        ui::pref_row(t!("settings-version-nueva"), Some(status), action),
        ui::pref_row(
            t!("settings-buscar-versiones-nuevas"),
            Some(t!("settings-buscar-versiones-nuevas-ayuda").to_owned()),
            row![]
                .push(
                    checks.then(|| ui::select(intervals, interval, |c| on(Msg::UpdateInterval(c)))),
                )
                .push(ui::switch(
                    checks,
                    (!disabled).then_some(|on_off| on(Msg::UpdateChecks(on_off))),
                ))
                .spacing(space::S)
                .align_y(Alignment::Center),
        ),
    ]
}

fn license_row<'a>(name: &'a str, license: &'a str) -> Element<'a, Message> {
    ui::pref_row(name, Some(license.to_owned()), ui::gap(0.0))
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
    use coroom_transport::proto::api::{Device, Devices};

    use super::SettingsPage;
    use crate::ui::app::{App, AudioDevice, Event, Load, Message, Page};
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
        app.settings.peers = vec![KnownPeer {
            name: "Bea".to_owned(),
            fingerprint: "cd".repeat(32),
            safety_number: "12345 67890 13579 24680 11223 34455".to_owned(),
        }];
    }

    /// Escenas de esta pantalla.
    pub(crate) const BUILDERS: &[(&str, Builder)] = &[
        ("16-ajustes-cuenta", |app| {
            settings_page(app, SettingsPage::Account);
            // Un servidor con certificado propio: se ve la huella en la que se confió.
            if let Some(account) = &mut app.account {
                account.server.fingerprint = Some("9c4e".repeat(16));
            }
        }),
        ("16-ajustes-sin-servidor", |app| {
            settings_page(app, SettingsPage::Account);
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
                vendor: 0x045e,
                product: 0x0b13,
                name: "Mando inalámbrico Xbox".to_owned(),
            });
        }),
        ("20-ajustes-apariencia", |app| {
            settings_page(app, SettingsPage::Appearance)
        }),
        ("21-ajustes-acerca", |app| {
            settings_page(app, SettingsPage::About)
        }),
    ];
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
        let mut app = open(SettingsPage::Account);
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
        let mut app = open(SettingsPage::Account);
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
        assert_eq!(Shortcuts::default().letters().len(), 8);
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
        let mut app = open(SettingsPage::Account);
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
        // «actualizado hace N s» se redibuja cada segundo.
        assert_eq!(next_tick(&app), Some(answered + Duration::from_secs(1)));
        app.now = answered + Duration::from_secs(2);
        assert!(app.update(Message::Tick).is_empty(), "aún no toca");
        app.now = answered + AUDIO_REFRESH;
        assert_eq!(app.update(Message::Tick), [Effect::LoadAudioDevices]);
        assert!(app.settings_page.audio_refreshing);
        assert!(
            matches!(app.settings_page.audio_devices, Load::Loaded(_)),
            "la lista anterior sigue a la vista"
        );
        assert!(
            next_tick(&app).is_some_and(|at| at > app.now),
            "pedida, nunca una hora pasada"
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
    fn the_account_page_loads_the_devices() {
        let mut app = open(SettingsPage::Audio);
        assert!(
            app.update(Message::Settings(Msg::Page(SettingsPage::Account)))
                .is_empty(),
            "sin cuenta no hay dispositivos que pedir"
        );
        assert_eq!(ago(100, 100), "ahora mismo");
        assert_eq!(ago(0, 7200), "hace 2 h");
        assert_eq!(ago(0, 86_400), "ayer");
        assert_eq!(ago(0, 86_400 * 3), "hace 3 días");
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
}
