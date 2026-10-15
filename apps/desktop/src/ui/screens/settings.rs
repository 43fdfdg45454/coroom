//! Pantallas 6 y 7: ajustes y diagnóstico.
//!
//! Vídeo (códec, bitrate, fps, resolución, 4:4:4 y HDR), audio (dispositivos, modo de voz y su
//! tecla, supresión de ruido), entrada (atajos editables, ranura de mando preferida), red (forzar
//! relay, puerto local), dispositivos de la cuenta (con cerrar sesión remota), apariencia (sistema,
//! claro u oscuro; acento del sistema o fijo), acerca de (versión, licencias, actualizaciones,
//! huella de la identidad y números de seguridad) y el diagnóstico de `coroom doctor` con "Copiar".
//! Todo se guarda al cambiarlo en `settings.toml`.

use std::time::{SystemTime, UNIX_EPOCH};

use coroom_transport::proto::PROTOCOL_VERSION;
use coroom_ui::Element;
use coroom_ui::icon::{Icon, icon_with};
use coroom_ui::style::{ButtonClass, ContainerClass, SvgClass};
use coroom_ui::theme::{Accent, Tone};
use coroom_ui::tokens::{size, space, text as type_size};
use coroom_ui::widget::{self as ui, Kind};
use iced_core::text::Wrapping;
use iced_core::{Alignment, Length, Padding};
use iced_widget::{Column, Row, button, column, container, row, text};

use crate::ui::app::{App, AudioDevice, Dialog, Effect, Event, Load, Message};
use crate::ui::settings::{
    BITRATE_CHOICES, CodecChoice, DecoderChoice, EncoderChoice, FPS_CHOICES, Resolution, Shortcuts,
    ThemeChoice, VoiceMode,
};
use crate::ui::stats;
use coroom_transport::proto::api::Devices;

/// Páginas de Ajustes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsPage {
    /// Vídeo.
    #[default]
    Video,
    /// Audio.
    Audio,
    /// Entrada.
    Input,
    /// Red.
    Network,
    /// Dispositivos.
    Devices,
    /// Apariencia.
    Appearance,
    /// Acerca de.
    About,
    /// Diagnóstico.
    Diagnostics,
}

impl SettingsPage {
    /// Todas, en el orden de la lista.
    pub const ALL: [SettingsPage; 8] = [
        SettingsPage::Video,
        SettingsPage::Audio,
        SettingsPage::Input,
        SettingsPage::Network,
        SettingsPage::Devices,
        SettingsPage::Appearance,
        SettingsPage::About,
        SettingsPage::Diagnostics,
    ];

    /// Nombre.
    pub fn label(self) -> &'static str {
        match self {
            SettingsPage::Video => t!("pantalla-settings-video"),
            SettingsPage::Audio => t!("pantalla-settings-audio"),
            SettingsPage::Input => t!("pantalla-settings-entrada"),
            SettingsPage::Network => t!("pantalla-settings-red"),
            SettingsPage::Devices => t!("pantalla-settings-dispositivos"),
            SettingsPage::Appearance => t!("pantalla-settings-apariencia"),
            SettingsPage::About => t!("pantalla-settings-acerca-de"),
            SettingsPage::Diagnostics => t!("pantalla-settings-diagnostico"),
        }
    }

    fn icon(self) -> Icon {
        match self {
            SettingsPage::Video => Icon::Video,
            SettingsPage::Audio => Icon::Headphones,
            SettingsPage::Input => Icon::Keyboard,
            SettingsPage::Network => Icon::Network,
            SettingsPage::Devices => Icon::Laptop,
            SettingsPage::Appearance => Icon::Palette,
            SettingsPage::About => Icon::Info,
            SettingsPage::Diagnostics => Icon::Stethoscope,
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
    /// Salida del diagnóstico.
    pub doctor: Load<Vec<String>>,
    /// Micrófonos y salidas.
    pub audio_devices: Load<Vec<AudioDevice>>,
    /// Esperando una tecla.
    pub capturing: Option<Capture>,
    /// Texto del puerto local.
    pub port: Option<String>,
    /// Error del puerto.
    pub port_error: Option<String>,
}

/// Mensajes.
#[derive(Debug, Clone)]
pub enum Msg {
    /// Página.
    Page(SettingsPage),
    /// Códec.
    Codec(Choice<CodecChoice>),
    /// Codificador.
    Encoder(Choice<EncoderChoice>),
    /// Decodificador.
    Decoder(Choice<DecoderChoice>),
    /// Bitrate.
    Bitrate(Choice<u32>),
    /// Fps.
    Fps(u32),
    /// Resolución.
    Resolution(Choice<Resolution>),
    /// 4:4:4.
    Chroma444(bool),
    /// HDR.
    Hdr(bool),
    /// Voz por defecto.
    Voice(bool),
    /// Micrófono.
    Microphone(Choice<Option<String>>),
    /// Salida.
    Speaker(Choice<Option<String>>),
    /// Modo de voz.
    VoiceMode(VoiceMode),
    /// Supresión de ruido.
    NoiseSuppression(bool),
    /// Cambiar un atajo.
    Capture(Capture),
    /// La tecla pulsada mientras se cambiaba un atajo (nombre de `winit`, o una letra).
    KeyCaptured(String),
    /// Dejar de esperar la tecla.
    CancelCapture,
    /// Atajos por defecto.
    ResetShortcuts,
    /// Ranura preferida.
    PreferredSlot(Choice<u8>),
    /// Forzar relay.
    ForceRelay(bool),
    /// Pedir al router que abra el puerto.
    PortMapping(bool),
    /// Puerto local.
    Port(String),
    /// Tema.
    Theme(ThemeChoice),
    /// Acento (`"system"` o una clave).
    Accent(String),
    /// Cerrar la sesión de un dispositivo (pide confirmación).
    Revoke {
        /// Dispositivo.
        id: u64,
        /// Nombre.
        name: String,
    },
    /// Volver a cargar los dispositivos.
    ReloadDevices,
    /// Volver a pasar el diagnóstico.
    RunDoctor,
    /// Copiar el diagnóstico.
    CopyDoctor,
    /// Ver el número de seguridad de un par conocido.
    Peer(usize),
    /// Comprobar ya si hay una versión nueva.
    CheckUpdates,
    /// Comprobar sola cada cierto tiempo.
    UpdateChecks(bool),
    /// Cada cuántas horas.
    UpdateInterval(Choice<u32>),
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

/// Lo que se pide al abrir Ajustes.
pub fn on_open(app: &mut App) -> Vec<Effect> {
    page_effects(app, app.settings_page.page)
}

fn page_effects(app: &mut App, page: SettingsPage) -> Vec<Effect> {
    let state = &mut app.settings_page;
    match page {
        SettingsPage::Devices if !state.devices.is_loading() && app.account.is_some() => {
            state.devices = Load::Loading;
            vec![Effect::LoadDevices]
        }
        SettingsPage::Diagnostics if matches!(state.doctor, Load::Idle) => {
            state.doctor = Load::Loading;
            vec![Effect::RunDoctor]
        }
        SettingsPage::Audio if matches!(state.audio_devices, Load::Idle) => {
            state.audio_devices = Load::Loading;
            vec![Effect::LoadAudioDevices]
        }
        _ => Vec::new(),
    }
}

/// Guarda y, con la sala abierta, le aplica el vídeo nuevo.
fn live_video(app: &App) -> Vec<Effect> {
    let mut effects = vec![Effect::SaveSettings];
    if app.host.room.is_some() {
        let video = &app.settings.video;
        let (codec, encoder, kbps) = (video.codec, video.encoder, video.max_bitrate_kbps);
        tracing::info!(?codec, ?encoder, kbps, "vídeo pedido para la sala abierta");
        effects.push(Effect::SetVideo);
    }
    effects
}

/// Códecs del selector, con «Automático».
pub fn codec_choices() -> Vec<Choice<CodecChoice>> {
    vec![
        choice(CodecChoice::Auto, t!("pantalla-settings-automatico")),
        choice(CodecChoice::Av1, "AV1"),
        choice(CodecChoice::Hevc, "HEVC"),
        choice(CodecChoice::H264, "H.264"),
    ]
}

/// Bitrates máximos del selector.
pub fn bitrate_choices() -> Vec<Choice<u32>> {
    BITRATE_CHOICES
        .iter()
        .map(|kbps| choice(*kbps, bitrate_label(*kbps)))
        .collect()
}

/// El bitrate dentro de una frase: «Auto hasta 80 Mbit/s» o «Auto sin tope». Auto es el único
/// modo: el control de tasa sube hasta donde la red aguanta y baja antes de que se congele; el
/// tope solo le pone techo.
pub fn bitrate_phrase(kbps: u32) -> String {
    if kbps < coroom_session::remote::UNLIMITED_KBPS {
        t!(
            "pantalla-settings-auto-hasta",
            valor = stats::kbps(f64::from(kbps))
        )
    } else {
        t!("pantalla-settings-auto-sin-tope").to_owned()
    }
}

/// Cómo se nombra un tope en el selector: «80 Mbit/s» o «Sin tope».
pub fn bitrate_label(kbps: u32) -> String {
    if kbps >= coroom_session::remote::UNLIMITED_KBPS {
        t!("pantalla-settings-sin-tope").to_owned()
    } else {
        stats::kbps(f64::from(kbps))
    }
}

/// Reduce un mensaje.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    let save = vec![Effect::SaveSettings];
    let settings = &mut app.settings;
    match msg {
        Msg::Page(page) => {
            app.settings_page.page = page;
            app.settings_page.capturing = None;
            return page_effects(app, page);
        }
        // Con la sala abierta, el códec, el codificador y el bitrate se aplican también a ella.
        Msg::Codec(choice) => {
            settings.video.codec = choice.value;
            return live_video(app);
        }
        Msg::Encoder(choice) => {
            settings.video.encoder = choice.value;
            return live_video(app);
        }
        Msg::Decoder(choice) => settings.video.decoder = choice.value,
        Msg::Bitrate(choice) => {
            settings.video.max_bitrate_kbps = choice.value;
            return live_video(app);
        }
        Msg::Fps(fps) => settings.video.fps = fps,
        Msg::Resolution(choice) => settings.video.max_resolution = choice.value,
        Msg::Chroma444(on) => settings.video.chroma_444 = on,
        Msg::Hdr(on) => settings.video.hdr = on,
        Msg::Voice(on) => settings.audio.voice = on,
        Msg::Microphone(choice) => settings.audio.microphone = choice.value,
        Msg::Speaker(choice) => settings.audio.speaker = choice.value,
        Msg::VoiceMode(mode) => settings.audio.voice_mode = mode,
        Msg::NoiseSuppression(on) => settings.audio.noise_suppression = on,
        Msg::Capture(capture) => {
            app.settings_page.capturing = Some(capture);
            return Vec::new();
        }
        Msg::CancelCapture => {
            app.settings_page.capturing = None;
            return Vec::new();
        }
        Msg::KeyCaptured(name) => {
            let Some(capture) = app.settings_page.capturing.take() else {
                return Vec::new();
            };
            match capture {
                Capture::PushToTalk => {
                    if coroom_session::input::keymap::keycode_from_name(&name).is_none() {
                        app.toast(
                            Tone::Warning,
                            t!("pantalla-settings-la-tecla-name-no-sirve-para", name = name),
                        );
                        return Vec::new();
                    }
                    app.settings.audio.push_to_talk_key = name;
                }
                Capture::Shortcut(index) => {
                    let letter = name.chars().next().filter(|_| name.chars().count() == 1);
                    let Some(letter) = letter.filter(char::is_ascii_alphabetic) else {
                        app.toast(
                            Tone::Warning,
                            t!("pantalla-settings-los-atajos-usan-una-letra-con"),
                        );
                        return Vec::new();
                    };
                    let mut shortcuts = app.settings.input.shortcuts.clone();
                    let letter = letter.to_ascii_uppercase();
                    match index {
                        0 => shortcuts.immersive = letter,
                        1 => shortcuts.stats = letter,
                        2 => shortcuts.leave = letter,
                        3 => shortcuts.mic = letter,
                        4 => shortcuts.game = letter,
                        _ => shortcuts.pads = letter,
                    }
                    if !shortcuts.is_valid() {
                        app.toast(
                            Tone::Warning,
                            t!(
                                "pantalla-settings-ctrl-alt-letter-ya-es-otro",
                                letter = letter
                            ),
                        );
                        return Vec::new();
                    }
                    app.settings.input.shortcuts = shortcuts;
                }
            }
        }
        Msg::ResetShortcuts => settings.input.shortcuts = Shortcuts::default(),
        Msg::PreferredSlot(choice) => settings.input.preferred_slot = choice.value,
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
                            Some(t!("pantalla-settings-un-puerto-de-1024-a-65535").to_owned());
                        app.settings_page.port = Some(digits);
                        return Vec::new();
                    }
                }
            }
            app.settings_page.port = Some(digits);
        }
        Msg::Theme(theme) => {
            settings.appearance.theme = theme;
            app.refresh_theme();
        }
        Msg::Accent(key) => {
            settings.appearance.accent = key;
            app.refresh_theme();
        }
        Msg::Revoke { id, name } => {
            app.dialog = Some(Dialog::RevokeDevice { device: id, name });
            return Vec::new();
        }
        Msg::ReloadDevices => {
            app.settings_page.devices = Load::Loading;
            return vec![Effect::LoadDevices];
        }
        Msg::RunDoctor => {
            app.settings_page.doctor = Load::Loading;
            return vec![Effect::RunDoctor];
        }
        Msg::CopyDoctor => {
            if let Load::Loaded(lines) = &app.settings_page.doctor {
                let text = lines.join("\n");
                return app.update(Message::Copy(text));
            }
            return Vec::new();
        }
        Msg::CheckUpdates => return app.check_updates(true),
        Msg::UpdateChecks(on) => {
            settings.updates.check = on;
            // Al activarla, se comprueba enseguida; al quitarla, no se vuelve a preguntar.
            let due = on && app.update.available.is_none();
            app.schedule_update_check(due);
        }
        Msg::UpdateInterval(choice) => settings.updates.interval_hours = choice.value,
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
    }
    save
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
        Event::AudioDevices(result) => {
            app.settings_page.audio_devices = match result {
                Ok(devices) => Load::Loaded(devices),
                Err(err) => Load::Failed(err),
            };
            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn on(msg: Msg) -> Message {
    Message::Settings(msg)
}

/// Dibuja la página.
pub fn view(app: &App) -> Element<'_, Message> {
    let state = &app.settings_page;
    let nav = Column::with_children(SettingsPage::ALL.iter().map(|page| {
        let selected = state.page == *page;
        button(
            row![
                icon_with(page.icon(), 18.0, SvgClass::Text),
                text(page.label()).size(type_size::BODY).font(if selected {
                    coroom_ui::font::SEMIBOLD
                } else {
                    coroom_ui::font::UI
                })
            ]
            .spacing(space::S)
            .align_y(Alignment::Center),
        )
        .padding(Padding::from([8.0, 12.0]))
        .width(Length::Fill)
        .class(ButtonClass::Nav { selected })
        .on_press(on(Msg::Page(*page)))
        .into()
    }))
    .spacing(2)
    .width(Length::Fixed(200.0));
    let content: Element<'_, Message> = match state.page {
        SettingsPage::Video => video(app),
        SettingsPage::Audio => audio(app),
        SettingsPage::Input => input(app),
        SettingsPage::Network => network(app),
        SettingsPage::Devices => devices(app),
        SettingsPage::Appearance => appearance(app),
        SettingsPage::About => about(app),
        SettingsPage::Diagnostics => diagnostics(app),
    };
    ui::page(
        column![
            ui::page_header(
                t!("pantalla-settings-ajustes"),
                Some(t!("pantalla-settings-se-guardan-al-cambiarlos").to_owned()),
                vec![]
            ),
            row![nav, container(content).width(Length::Fill)]
                .spacing(space::L)
                .align_y(Alignment::Start)
        ]
        .spacing(space::L),
    )
}

fn video(app: &App) -> Element<'_, Message> {
    let video = &app.settings.video;
    let codecs = codec_choices();
    let codec = codecs.iter().find(|c| c.value == video.codec).cloned();
    let bitrates = bitrate_choices();
    let bitrate = bitrates
        .iter()
        .find(|c| c.value == video.max_bitrate_kbps)
        .cloned();
    let resolutions: Vec<Choice<Resolution>> = Resolution::ALL
        .iter()
        .map(|r| {
            let (w, h) = r.size();
            choice(*r, format!("{w} × {h}"))
        })
        .collect();
    let resolution = resolutions
        .iter()
        .find(|c| c.value == video.max_resolution)
        .cloned();
    // Solo los backends que existen en esta plataforma.
    let mut encoders = vec![choice(
        EncoderChoice::Auto,
        t!("pantalla-settings-automatico"),
    )];
    encoders.extend(
        [
            (EncoderChoice::Nvenc, "NVENC (NVIDIA)"),
            (EncoderChoice::Amf, "AMF (AMD)"),
            (EncoderChoice::Qsv, "Quick Sync (Intel)"),
            (EncoderChoice::Vulkan, "Vulkan Video"),
            (EncoderChoice::Vaapi, "VA-API"),
        ]
        .into_iter()
        .filter(|(c, _)| cfg!(target_os = "linux") || *c != EncoderChoice::Vaapi)
        .map(|(c, label)| choice(c, label)),
    );
    encoders.push(choice(
        EncoderChoice::Software,
        t!("pantalla-settings-por-software"),
    ));
    let encoder = encoders.iter().find(|c| c.value == video.encoder).cloned();
    let decoders = vec![
        choice(DecoderChoice::Auto, t!("pantalla-settings-automatico")),
        choice(
            DecoderChoice::Software,
            t!("pantalla-settings-por-software"),
        ),
    ];
    let decoder = decoders.iter().find(|c| c.value == video.decoder).cloned();
    let fps = ui::segmented(
        FPS_CHOICES
            .iter()
            .map(|fps| (fps.to_string(), on(Msg::Fps(*fps)), video.fps == *fps))
            .collect(),
    );
    column![
        ui::section(
            t!("pantalla-settings-lo-que-compartes"),
            Some(t!("pantalla-settings-se-aplica-a-las-salas-que").to_owned()),
            vec![
                ui::pref_row(
                    t!("pantalla-settings-codec-preferido"),
                    Some(
                        t!("pantalla-settings-automatico-elige-el-mejor-que-decodifiquen")
                            .to_owned()
                    ),
                    ui::select(codecs, codec, |c| on(Msg::Codec(c))),
                ),
                ui::pref_row(
                    t!("pantalla-settings-bitrate-maximo"),
                    Some(
                        if video.max_bitrate_kbps >= coroom_session::remote::UNLIMITED_KBPS {
                            t!("pantalla-settings-auto-sin-tope-ayuda")
                        } else {
                            t!("pantalla-settings-auto-con-tope-ayuda")
                        }
                        .to_owned()
                    ),
                    ui::select(bitrates, bitrate, |c| on(Msg::Bitrate(c))),
                ),
                ui::pref_row(t!("pantalla-settings-fotogramas-por-segundo"), None, fps),
                ui::pref_row(
                    t!("pantalla-settings-resolucion-maxima"),
                    None,
                    ui::select(resolutions, resolution, |c| on(Msg::Resolution(c))),
                ),
            ],
        ),
        ui::section(
            t!("pantalla-settings-calidad-de-imagen"),
            Some(t!("pantalla-settings-se-usan-si-tu-equipo-y").to_owned()),
            vec![
                ui::pref_row(
                    t!("pantalla-settings-croma-4-4-4"),
                    Some(chroma_hint(app)),
                    ui::switch(video.chroma_444, Some(|on_off| on(Msg::Chroma444(on_off)))),
                ),
                ui::pref_row(
                    "HDR",
                    Some(hdr_hint(app)),
                    ui::switch(video.hdr, Some(|on_off| on(Msg::Hdr(on_off)))),
                ),
            ],
        ),
        ui::section(
            t!("pantalla-settings-avanzado"),
            Some(t!("pantalla-settings-cambialos-solo-si-el-video").to_owned()),
            vec![
                ui::pref_row(
                    t!("pantalla-settings-codificador"),
                    Some(t!("pantalla-settings-codificador-ayuda").to_owned()),
                    ui::select(encoders, encoder, |c| on(Msg::Encoder(c))),
                ),
                ui::pref_row(
                    t!("pantalla-settings-decodificador"),
                    Some(t!("pantalla-settings-decodificador-ayuda").to_owned()),
                    ui::select(decoders, decoder, |c| on(Msg::Decoder(c))),
                ),
            ],
        ),
    ]
    .spacing(space::L)
    .into()
}

/// Códec elegido en Ajustes (`None`: automático).
fn chosen_codec(app: &App) -> Option<coroom_session::codec::Codec> {
    use coroom_session::codec::Codec;
    match app.settings.video.codec {
        CodecChoice::Auto => None,
        CodecChoice::Av1 => Some(Codec::Av1),
        CodecChoice::Hevc => Some(Codec::Hevc),
        CodecChoice::H264 => Some(Codec::H264),
    }
}

/// Explicación de 4:4:4 con lo que hace este equipo.
pub fn chroma_hint(app: &App) -> String {
    let lead = t!("pantalla-settings-texto-nitido-en-escritorios-y-juegos");
    let Some(support) = &app.video_support else {
        return t!(
            "pantalla-settings-lead-comprobando-que-codecs-lo-admiten",
            lead = lead
        );
    };
    if support.yuv444.is_empty() {
        let decode = if support.decode_yuv444.is_empty() {
            String::new()
        } else {
            t!(
                "pantalla-settings-como-invitado-si-lo-recibes-en",
                valor = join_codecs(support.decode_yuv444.iter().map(ToString::to_string))
            )
        };
        return t!(
            "pantalla-settings-lead-no-disponible-para-compartir-ningun",
            lead = lead,
            decode = decode
        );
    }
    match chosen_codec(app) {
        Some(codec) if !support.yuv444.iter().any(|(c, _)| *c == codec) => t!(
            "pantalla-settings-lead-no-disponible-con-este-codec",
            lead = lead,
            codec = codec,
            valor = join_codecs(support.yuv444.iter().map(|(_, label)| label.clone()))
        ),
        _ => t!(
            "pantalla-settings-lead-disponible-con",
            lead = lead,
            valor = join_codecs(support.yuv444.iter().map(|(_, label)| label.clone()))
        ),
    }
}

/// Explicación de HDR con lo que hace este equipo.
pub fn hdr_hint(app: &App) -> String {
    let Some(support) = &app.video_support else {
        return t!("pantalla-settings-hdr10-10-bits-bt-2020-pq").to_owned();
    };
    let share = if support.hdr10.is_empty() {
        t!("pantalla-settings-no-disponible-para-compartir-ningun-codificador").to_owned()
    } else {
        match chosen_codec(app) {
            Some(codec) if !support.hdr10.iter().any(|(c, _)| *c == codec) => t!(
                "pantalla-settings-no-disponible-con-este-codec-codec",
                codec = codec,
                valor = join_codecs(support.hdr10.iter().map(|(_, label)| label.clone()))
            ),
            _ => t!(
                "pantalla-settings-compartes-en-hdr10-con-cuando-la",
                valor = join_codecs(support.hdr10.iter().map(|(_, label)| label.clone()))
            ),
        }
    };
    let display = match support.hdr_display {
        Some(true) => t!("pantalla-settings-esta-pantalla-muestra-hdr-el-video"),
        Some(false) => {
            t!("pantalla-settings-esta-pantalla-no-expone-hdr-a")
        }
        None => t!("pantalla-settings-si-la-pantalla-no-expone-hdr"),
    };
    format!("{share} {display}")
}

fn join_codecs(names: impl Iterator<Item = String>) -> String {
    let names: Vec<String> = names.collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => t!(
            "pantalla-settings-y-last",
            valor = rest.join(", "),
            last = last
        ),
    }
}

fn device_choices(devices: &[AudioDevice], input: bool) -> Vec<Choice<Option<String>>> {
    let mut out = vec![choice(None, t!("pantalla-settings-el-del-sistema"))];
    out.extend(
        devices
            .iter()
            .filter(|device| device.input == input)
            .map(|device| choice(Some(device.name.clone()), device.description.clone())),
    );
    out
}

fn audio(app: &App) -> Element<'_, Message> {
    let audio = &app.settings.audio;
    let state = &app.settings_page;
    let devices: Vec<AudioDevice> = state.audio_devices.loaded().cloned().unwrap_or_default();
    let mics = device_choices(&devices, true);
    let mic = mics.iter().find(|c| c.value == audio.microphone).cloned();
    let speakers = device_choices(&devices, false);
    let speaker = speakers.iter().find(|c| c.value == audio.speaker).cloned();
    let mut rows = vec![
        ui::pref_row(
            t!("pantalla-settings-microfono"),
            None,
            ui::select(mics, mic, |c| on(Msg::Microphone(c))),
        ),
        ui::pref_row(
            t!("pantalla-settings-salida"),
            None,
            ui::select(speakers, speaker, |c| on(Msg::Speaker(c))),
        ),
    ];
    if let Load::Failed(err) = &state.audio_devices {
        rows.push(
            container(ui::caption(t!(
                "pantalla-settings-no-se-pudieron-listar-los-dispositivos",
                err = err
            )))
            .padding(Padding::from([8.0, 16.0]))
            .into(),
        );
    }
    let capturing = state.capturing == Some(Capture::PushToTalk);
    let ptt: Element<'_, Message> = if capturing {
        row![
            ui::caption(t!("pantalla-settings-pulsa-una-tecla")),
            ui::flat(
                t!("pantalla-settings-cancelar"),
                Some(on(Msg::CancelCapture))
            )
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center)
        .into()
    } else {
        row![
            ui::shortcut(&[audio.push_to_talk_key.as_str()]),
            ui::normal(
                t!("pantalla-settings-cambiar"),
                Some(on(Msg::Capture(Capture::PushToTalk)))
            )
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center)
        .into()
    };
    column![
        ui::section(t!("pantalla-settings-dispositivos"), None, rows),
        ui::section(
            t!("pantalla-settings-voz"),
            None,
            vec![
                ui::pref_row(
                    t!("pantalla-settings-hablar-al-entrar-en-una-sala"),
                    Some(t!("pantalla-settings-solo-si-el-anfitrion-te-da").to_owned()),
                    ui::switch(audio.voice, Some(|on_off| on(Msg::Voice(on_off)))),
                ),
                ui::pref_row(
                    t!("pantalla-settings-modo"),
                    None,
                    ui::segmented(vec![
                        (
                            t!("pantalla-settings-al-hablar").to_owned(),
                            on(Msg::VoiceMode(VoiceMode::Activity)),
                            audio.voice_mode == VoiceMode::Activity,
                        ),
                        (
                            t!("pantalla-settings-pulsar-para-hablar").to_owned(),
                            on(Msg::VoiceMode(VoiceMode::PushToTalk)),
                            audio.voice_mode == VoiceMode::PushToTalk,
                        ),
                    ]),
                ),
                ui::pref_row(t!("pantalla-settings-tecla-para-hablar"), None, ptt),
                ui::pref_row(
                    t!("pantalla-settings-supresion-de-ruido"),
                    Some(t!("pantalla-settings-quita-el-ruido-de-fondo-del").to_owned()),
                    ui::switch(
                        audio.noise_suppression,
                        Some(|on_off| on(Msg::NoiseSuppression(on_off)))
                    ),
                ),
            ],
        ),
    ]
    .spacing(space::L)
    .into()
}

fn input(app: &App) -> Element<'_, Message> {
    let shortcuts = &app.settings.input.shortcuts;
    let state = &app.settings_page;
    let names = [
        t!("pantalla-settings-modo-inmersivo-o-escritorio"),
        t!("pantalla-settings-panel-de-estadisticas"),
        t!("pantalla-settings-salir-de-la-sala"),
        t!("pantalla-settings-silenciar-el-microfono"),
        t!("pantalla-settings-silenciar-el-audio-del-juego"),
        t!("pantalla-settings-panel-de-mandos"),
    ];
    let letters = shortcuts.letters();
    let mut rows: Vec<Element<'_, Message>> = names
        .iter()
        .zip(letters)
        .enumerate()
        .map(|(index, (name, letter))| {
            let control: Element<'_, Message> = if state.capturing == Some(Capture::Shortcut(index))
            {
                row![
                    ui::caption(t!("pantalla-settings-pulsa-una-letra")),
                    ui::flat(
                        t!("pantalla-settings-cancelar"),
                        Some(on(Msg::CancelCapture))
                    )
                ]
                .spacing(space::XS)
                .align_y(Alignment::Center)
                .into()
            } else {
                row![
                    shortcut_caps(letter),
                    ui::normal(
                        t!("pantalla-settings-cambiar"),
                        Some(on(Msg::Capture(Capture::Shortcut(index))))
                    )
                ]
                .spacing(space::XS)
                .align_y(Alignment::Center)
                .into()
            };
            ui::pref_row(*name, None, control)
        })
        .collect();
    rows.push(
        container(row![
            ui::fill(),
            ui::flat(
                t!("pantalla-settings-restablecer"),
                Some(on(Msg::ResetShortcuts))
            )
        ])
        .padding(Padding::from([4.0, 8.0]))
        .into(),
    );
    let max = coroom_session::input::max_gamepads();
    let mut slots = vec![choice(0u8, t!("pantalla-settings-la-primera-libre"))];
    slots.extend(
        (1..=max).map(|slot| choice(slot, t!("pantalla-settings-ranura-slot", slot = slot))),
    );
    let slot = slots
        .iter()
        .find(|c| c.value == app.settings.input.preferred_slot)
        .cloned();
    column![
        ui::section(
            t!("pantalla-settings-atajos-del-invitado"),
            Some(t!("pantalla-settings-siempre-con-ctrl-alt-para-no").to_owned()),
            rows
        ),
        ui::section(
            t!("pantalla-settings-mandos"),
            None,
            vec![ui::pref_row(
                t!("pantalla-settings-ranura-preferida-al-dar-mando"),
                Some(t!("pantalla-settings-la-que-se-ofrece-primero-al").to_owned()),
                ui::select(slots, slot, |c| on(Msg::PreferredSlot(c))),
            )]
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
        ui::field(t!("pantalla-settings-puerto-udp"), port_text)
            .placeholder(t!("pantalla-settings-cualquiera"))
            .on_input(|value| on(Msg::Port(value)))
            .error(state.port_error.as_deref())
            .id("local-port"),
    ))
    .width(Length::Fixed(180.0));
    column![ui::section(
        t!("pantalla-settings-conexion"),
        None,
        vec![
            ui::pref_row(
                t!("pantalla-settings-usar-siempre-el-relay"),
                Some(t!("pantalla-settings-sin-intentar-la-conexion-directa-util").to_owned()),
                ui::switch(net.force_relay, Some(|on_off| on(Msg::ForceRelay(on_off)))),
            ),
            ui::pref_row(
                t!("pantalla-settings-abrir-el-puerto-en-el-router"),
                Some(t!("pantalla-settings-pide-al-router-que-deje-entrar").to_owned()),
                ui::switch(
                    net.port_mapping,
                    Some(|on_off| on(Msg::PortMapping(on_off)))
                ),
            ),
            ui::pref_row(
                t!("pantalla-settings-puerto-local"),
                Some(t!("pantalla-settings-fijalo-si-tu-router-reenvia-un").to_owned()),
                port,
            ),
        ],
    )]
    .into()
}

/// "hace 5 min", "hace 3 h", "hace 2 días".
pub fn ago(unix: i64, now: i64) -> String {
    let seconds = (now - unix).max(0);
    match seconds {
        0..60 => t!("pantalla-settings-ahora-mismo").to_owned(),
        60..3600 => t!("pantalla-settings-hace-min", valor = seconds / 60),
        3600..86_400 => t!("pantalla-settings-hace-h", valor = seconds / 3600),
        _ => match seconds / 86_400 {
            1 => "ayer".to_owned(),
            days => t!("pantalla-settings-hace-days-dias", days = days),
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
    let state = &app.settings_page;
    let body: Element<'_, Message> = match &state.devices {
        Load::Idle | Load::Loading => ui::skeleton(
            t!("pantalla-settings-cargando-los-dispositivos-de-tu-cuenta"),
            2,
        ),
        Load::Failed(err) => ui::error_state(
            t!("pantalla-settings-no-se-pudieron-cargar-los-dispositivos"),
            err.as_str(),
            Some((t!("pantalla-settings-reintentar"), on(Msg::ReloadDevices))),
        ),
        Load::Loaded(list) => {
            let now = now_unix();
            let rows = list
                .devices
                .iter()
                .map(|device| {
                    let current = list.current == Some(device.id);
                    let short: String = device.fingerprint.chars().take(16).collect();
                    let mut trailing = Vec::new();
                    if current {
                        trailing.push(ui::status_badge(
                            Tone::Info,
                            t!("pantalla-settings-este-equipo"),
                        ));
                    } else {
                        trailing.push(
                            ui::button_with(
                                Kind::DestructiveFlat,
                                t!("pantalla-settings-cerrar-sesion"),
                                Some(Icon::LogOut),
                                Some(on(Msg::Revoke {
                                    id: device.id,
                                    name: device.name.clone(),
                                })),
                            )
                            .into(),
                        );
                    }
                    ui::list_row(
                        Some(icon_with(Icon::Laptop, 22.0, SvgClass::Secondary)),
                        device.name.as_str(),
                        Some(t!(
                            "pantalla-settings-huella-short-usado",
                            short = short,
                            valor = ago(device.last_seen_at, now)
                        )),
                        trailing,
                    )
                })
                .collect();
            ui::boxed_list(rows)
        }
    };
    column![
        ui::section(
            t!("pantalla-settings-dispositivos-de-tu-cuenta"),
            Some(t!("pantalla-settings-cada-equipo-con-el-que-inicias").to_owned()),
            vec![]
        ),
        body
    ]
    .spacing(space::XS)
    .into()
}

fn appearance(app: &App) -> Element<'_, Message> {
    let current = &app.settings.appearance;
    let theme = ui::segmented(vec![
        (
            t!("pantalla-settings-sistema").to_owned(),
            on(Msg::Theme(ThemeChoice::System)),
            current.theme == ThemeChoice::System,
        ),
        (
            t!("pantalla-settings-claro").to_owned(),
            on(Msg::Theme(ThemeChoice::Light)),
            current.theme == ThemeChoice::Light,
        ),
        (
            t!("pantalla-settings-oscuro").to_owned(),
            on(Msg::Theme(ThemeChoice::Dark)),
            current.theme == ThemeChoice::Dark,
        ),
    ]);
    let system_accent = app.system.accent.unwrap_or_default();
    let mut swatches = Row::new().spacing(space::XS).align_y(Alignment::Center);
    let swatch = |accent: Accent, key: String, selected: bool, tip: String| {
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
            tip,
        )
    };
    swatches = swatches.push(ui::normal(
        t!("pantalla-settings-del-sistema"),
        Some(on(Msg::Accent("system".to_owned()))),
    ));
    let _ = system_accent;
    for accent in Accent::NAMED {
        let key = accent.key();
        let selected = current.accent == key;
        swatches = swatches.push(swatch(accent, key, selected, accent.label()));
    }
    let accent_subtitle = if current.accent == "system" {
        t!(
            "pantalla-settings-del-sistema-2",
            valor = app.accent().label(),
            source = app.system.source
        )
    } else {
        app.accent().label()
    };
    let from_desktop = app.system.font.as_deref().is_some_and(|font| {
        coroom_ui::font::family_of(font).eq_ignore_ascii_case(app.fonts.family)
    });
    let font = if from_desktop {
        t!(
            "pantalla-settings-la-del-escritorio",
            family = app.fonts.family
        )
    } else if app.fonts.is_embedded() {
        t!(
            "pantalla-settings-incluida-con-coroom",
            family = app.fonts.family
        )
    } else {
        t!(
            "pantalla-settings-instalada-en-el-sistema",
            family = app.fonts.family
        )
    };
    column![
        ui::section(
            t!("pantalla-settings-estilo"),
            None,
            vec![
                ui::pref_row(
                    t!("pantalla-settings-tema"),
                    Some(match current.theme {
                        ThemeChoice::System => t!(
                            "pantalla-settings-sigue-al-sistema",
                            valor = match app.system.mode {
                                Some(coroom_ui::theme::Mode::Dark) =>
                                    t!("pantalla-settings-ahora-oscuro"),
                                Some(coroom_ui::theme::Mode::Light) =>
                                    t!("pantalla-settings-ahora-claro"),
                                None => t!("pantalla-settings-sin-preferencia-claro"),
                            }
                        ),
                        ThemeChoice::Light => t!("pantalla-settings-siempre-claro").to_owned(),
                        ThemeChoice::Dark => t!("pantalla-settings-siempre-oscuro").to_owned(),
                    }),
                    theme
                ),
                container(
                    column![
                        ui::body(t!("pantalla-settings-color-de-acento")),
                        ui::secondary(accent_subtitle),
                        swatches
                    ]
                    .spacing(space::XS)
                )
                .padding(Padding::from([10.0, 16.0]))
                .into(),
                ui::pref_row(t!("pantalla-settings-fuente"), Some(font), ui::gap(0.0)),
            ]
        ),
        ui::section(
            t!("pantalla-settings-atajos-de-la-aplicacion"),
            None,
            vec![
                ui::pref_row(
                    t!("pantalla-settings-salir"),
                    None,
                    ui::shortcut(&["Ctrl", "Q"])
                ),
                ui::pref_row(
                    t!("pantalla-settings-ajustes"),
                    None,
                    ui::shortcut(&["Ctrl", ","])
                ),
                ui::pref_row(
                    t!("pantalla-settings-pantalla-completa"),
                    None,
                    ui::shortcut(&["F11"])
                ),
                ui::pref_row(
                    t!("pantalla-settings-cerrar-dialogos"),
                    None,
                    ui::shortcut(&["Esc"])
                ),
                ui::pref_row(
                    t!("pantalla-settings-siguiente-campo"),
                    None,
                    ui::shortcut(&["Tab"])
                ),
            ]
        ),
    ]
    .spacing(space::L)
    .into()
}

fn updates(app: &App) -> Vec<Element<'_, Message>> {
    let state = &app.update;
    let updates = &app.settings.updates;
    let disabled = crate::update::disabled_by_env();
    let status = if disabled {
        t!(
            "pantalla-settings-desactivado-con-este-paquete-se-actualiza",
            valor = crate::update::DISABLE_ENV
        )
    } else if state.checking {
        t!("pantalla-settings-comprobando-en-las-publicaciones-del-proyecto").to_owned()
    } else if let Some(update) = &state.available {
        t!(
            "pantalla-settings-hay-una-version-nueva-tienes-la",
            version = update.version,
            version2 = app.version
        )
    } else if let Some(err) = &state.error {
        t!(
            "pantalla-settings-la-ultima-comprobacion-fallo-err",
            err = err
        )
    } else if updates.last_check.is_some() {
        t!("pantalla-settings-tienes-la-ultima-version").to_owned()
    } else {
        t!("pantalla-settings-pregunta-a-las-publicaciones-del-proyecto").to_owned()
    };
    let mut action = row![].spacing(space::S).align_y(Alignment::Center);
    if let Some(update) = &state.available {
        action = action.push(ui::primary(
            t!("pantalla-settings-ver-la-version"),
            Some(Message::OpenUrl(update.url.clone())),
        ));
    }
    action = action.push(ui::normal(
        if state.checking {
            t!("pantalla-settings-comprobando")
        } else {
            t!("pantalla-settings-comprobar-ahora")
        },
        (!state.checking && !disabled).then_some(on(Msg::CheckUpdates)),
    ));
    let intervals: Vec<Choice<u32>> = crate::update::INTERVAL_CHOICES
        .iter()
        .map(|hours| {
            choice(
                *hours,
                if *hours == 24 {
                    t!("pantalla-settings-una-vez-al-dia")
                } else {
                    t!("pantalla-settings-una-vez-a-la-semana")
                },
            )
        })
        .collect();
    let interval = intervals
        .iter()
        .find(|c| c.value == updates.interval_hours)
        .cloned();
    vec![
        ui::pref_row(t!("pantalla-settings-version-nueva"), Some(status), action),
        ui::pref_row(
            t!("pantalla-settings-comprobar-sola"),
            Some(t!("pantalla-settings-avisa-en-inicio-cuando-hay-una").to_owned()),
            ui::switch(
                updates.check && !disabled,
                (!disabled).then_some(|on_off| on(Msg::UpdateChecks(on_off))),
            ),
        ),
        ui::pref_row(
            t!("pantalla-settings-frecuencia"),
            None,
            ui::select(intervals, interval, |c| on(Msg::UpdateInterval(c))),
        ),
    ]
}

fn license_row<'a>(name: &'a str, license: &'a str) -> Element<'a, Message> {
    ui::pref_row(name, Some(license.to_owned()), ui::gap(0.0))
}

fn about(app: &App) -> Element<'_, Message> {
    let fingerprint = app
        .account
        .as_ref()
        .map(|account| account.fingerprint.as_str())
        .unwrap_or("");
    let header = ui::card(
        row![
            icon_with(Icon::RadioTower, 44.0, SvgClass::Accent),
            column![
                ui::title("coroom"),
                ui::secondary(t!(
                    "pantalla-settings-version-protocolo-protocol-version",
                    version = app.version,
                    PROTOCOL_VERSION = PROTOCOL_VERSION
                )),
                ui::caption(t!("pantalla-settings-juego-remoto-con-amigos-cifrado-de"))
            ]
            .spacing(4)
        ]
        .spacing(space::M)
        .align_y(Alignment::Center),
    );
    let peers: Vec<Element<'_, Message>> = if app.settings.peers.is_empty() {
        vec![
            container(ui::caption(t!(
                "pantalla-settings-aparecen-al-entrar-en-una-sala"
            )))
            .padding(Padding::from([10.0, 16.0]))
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
                    Some(format!("{preview}…")),
                    vec![
                        ui::normal(t!("pantalla-settings-ver"), Some(on(Msg::Peer(index)))).into(),
                    ],
                )
            })
            .collect()
    };
    let identity: Element<'_, Message> = if fingerprint.is_empty() {
        ui::caption(t!("pantalla-settings-inicia-sesion-para-ver-la-identidad")).into()
    } else {
        ui::copy_field(fingerprint, Message::Copy(fingerprint.to_owned()))
    };
    column![
        header,
        ui::section(t!("pantalla-settings-actualizaciones"), None, updates(app)),
        ui::section(
            t!("pantalla-settings-identidad-de-este-equipo"),
            Some(t!("pantalla-settings-tu-clave-ed25519-tus-amigos-ven").to_owned()),
            vec![
                container(identity)
                    .padding(Padding::from([10.0, 16.0]))
                    .into()
            ]
        ),
        ui::section(t!("pantalla-settings-numeros-de-seguridad"), None, peers),
        ui::section(
            t!("pantalla-settings-licencias"),
            None,
            vec![
                license_row(
                    t!("pantalla-settings-dependencias-de-rust"),
                    t!("pantalla-settings-mit-apache-2-0-bsd-isc")
                ),
                license_row(
                    t!("pantalla-settings-ffmpeg"),
                    t!("pantalla-settings-lgpl-2-1-o-posterior-enlazado")
                ),
                license_row(
                    t!("pantalla-settings-interfaz"),
                    t!("pantalla-settings-iced-mit-con-wgpu-mit-o")
                ),
                license_row(
                    t!("pantalla-settings-fuente-inter"),
                    t!("pantalla-settings-sil-open-font-license-1-1")
                ),
                license_row(t!("pantalla-settings-iconos-lucide"), "ISC"),
                license_row("coroom", t!("pantalla-settings-licencia-apache-2-0")),
            ]
        ),
    ]
    .spacing(space::L)
    .into()
}

fn diagnostics(app: &App) -> Element<'_, Message> {
    let state = &app.settings_page;
    let body: Element<'_, Message> = match &state.doctor {
        Load::Idle | Load::Loading => ui::card(ui::progress(
            t!("pantalla-settings-comprobando-el-sistema-captura-adaptador-grafico"),
            None,
            app.now.elapsed().as_secs_f32() / 1.4,
        )),
        Load::Failed(err) => ui::banner(Tone::Error, err.as_str(), None),
        Load::Loaded(lines) => container(
            Column::with_children(lines.iter().map(|line| {
                ui::mono(line.as_str(), type_size::CAPTION)
                    .wrapping(Wrapping::WordOrGlyph)
                    .into()
            }))
            .spacing(2),
        )
        .padding(space::M)
        .width(Length::Fill)
        .class(ContainerClass::Code)
        .into(),
    };
    let ready = matches!(state.doctor, Load::Loaded(_));
    column![
        row![
            column![
                ui::heading(t!("pantalla-settings-diagnostico")),
                ui::secondary(t!("pantalla-settings-lo-mismo-que-coroom-doctor-en"))
            ]
            .spacing(2)
            .width(Length::Fill),
            ui::button_with(
                Kind::Normal,
                t!("pantalla-settings-copiar"),
                Some(Icon::Copy),
                ready.then(|| on(Msg::CopyDoctor))
            ),
            ui::button_with(
                Kind::Normal,
                t!("pantalla-settings-volver-a-comprobar"),
                Some(Icon::Refresh),
                (!state.doctor.is_loading()).then(|| on(Msg::RunDoctor))
            )
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center),
        body
    ]
    .spacing(space::S)
    .width(Length::Fill)
    .max_width(size::CONTENT)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::tests::app;

    #[test]
    fn unlimited_bitrate_is_named_as_such() {
        let unlimited = coroom_session::remote::UNLIMITED_KBPS;
        assert_eq!(bitrate_label(unlimited), "Sin tope");
        assert_eq!(bitrate_phrase(unlimited), "Auto sin tope");
        assert_eq!(
            bitrate_phrase(80_000),
            format!("Auto hasta {}", stats::kbps(80_000.0))
        );
        assert_eq!(bitrate_label(80_000), stats::kbps(80_000.0));
        assert_eq!(
            bitrate_choices().last().map(|c| c.value),
            Some(unlimited),
            "«Sin tope» es la última opción"
        );
    }

    /// Ajustes explica si 4:4:4 y HDR están disponibles con el códec elegido.
    #[test]
    fn chroma_and_hdr_explain_what_this_machine_does() {
        use coroom_session::codec::Codec;
        let mut app = app();
        assert!(chroma_hint(&app).contains("Comprobando"));
        app.update(Message::Event(Event::VideoSupport(
            crate::ui::app::VideoSupport {
                yuv444: Vec::new(),
                hdr10: vec![(Codec::Av1, "AV1 (SVT-AV1)".to_owned())],
                decode_yuv444: vec![Codec::Av1, Codec::Hevc],
                decode_hdr10: vec![Codec::Av1],
                hdr_display: Some(false),
            },
        )));
        let chroma = chroma_hint(&app);
        assert!(chroma.contains("No disponible para compartir"), "{chroma}");
        assert!(chroma.contains("AV1 y HEVC"), "{chroma}");
        assert!(hdr_hint(&app).contains("tone mapping"));
        assert!(hdr_hint(&app).contains("AV1 (SVT-AV1)"));
        app.settings.video.codec = CodecChoice::H264;
        assert!(
            hdr_hint(&app).contains("No disponible con este códec (H.264)"),
            "{}",
            hdr_hint(&app)
        );
        if let Some(support) = &mut app.video_support {
            support.yuv444 = vec![(Codec::Hevc, "HEVC (NVENC)".to_owned())];
            support.hdr_display = Some(true);
        }
        assert!(chroma_hint(&app).contains("No disponible con este códec (H.264); sí con HEVC"));
        app.settings.video.codec = CodecChoice::Auto;
        assert!(chroma_hint(&app).contains("Disponible con HEVC (NVENC)"));
        assert!(hdr_hint(&app).contains("muestra HDR"));
        // Los interruptores se guardan.
        assert_eq!(
            app.update(Message::Settings(Msg::Hdr(true))),
            [Effect::SaveSettings]
        );
        assert!(app.settings.video.hdr);
    }

    #[test]
    fn every_change_is_saved() {
        let mut app = app();
        let saves =
            |app: &mut App, msg: Msg| app.update(Message::Settings(msg)) == [Effect::SaveSettings];
        assert!(saves(&mut app, Msg::Fps(120)));
        assert_eq!(app.settings.video.fps, 120);
        assert!(saves(&mut app, Msg::ForceRelay(true)));
        assert!(
            app.settings.network.port_mapping,
            "abrir el puerto viene activado"
        );
        assert!(saves(&mut app, Msg::PortMapping(false)));
        assert!(!app.settings.network.port_mapping);
        assert!(saves(&mut app, Msg::Theme(ThemeChoice::Dark)));
        assert_eq!(
            app.theme.mode(),
            coroom_ui::theme::Mode::Dark,
            "el tema cambia al momento"
        );
        assert!(saves(&mut app, Msg::Accent("purple".into())));
        assert_eq!(app.theme.palette.accent, Accent::Purple);
        assert!(saves(&mut app, Msg::Port("47000".into())));
        assert_eq!(app.settings.network.local_port, 47_000);
        assert!(
            app.update(Message::Settings(Msg::Port("80".into())))
                .is_empty()
        );
        assert!(
            app.settings_page.port_error.is_some(),
            "puertos privilegiados no"
        );
        assert!(saves(&mut app, Msg::Port(String::new())));
        assert_eq!(app.settings.network.local_port, 0);
    }

    #[test]
    fn shortcuts_are_captured_and_must_not_collide() {
        let mut app = app();
        app.update(Message::Settings(Msg::Capture(Capture::Shortcut(0))));
        assert_eq!(
            app.update(Message::Settings(Msg::KeyCaptured("k".into()))),
            [Effect::SaveSettings]
        );
        assert_eq!(app.settings.input.shortcuts.immersive, 'K');
        app.update(Message::Settings(Msg::Capture(Capture::Shortcut(1))));
        assert!(
            app.update(Message::Settings(Msg::KeyCaptured("K".into())))
                .is_empty()
        );
        assert!(
            app.toasts
                .iter()
                .any(|t| t.text.contains("ya es otro atajo"))
        );
        app.update(Message::Settings(Msg::Capture(Capture::PushToTalk)));
        app.update(Message::Settings(Msg::KeyCaptured("CapsLock".into())));
        assert_eq!(app.settings.audio.push_to_talk_key, "CapsLock");
        assert_eq!(app.settings_page.capturing, None);
    }

    #[test]
    fn pages_load_what_they_need() {
        let mut app = app();
        assert_eq!(
            app.update(Message::Settings(Msg::Page(SettingsPage::Diagnostics))),
            [Effect::RunDoctor]
        );
        assert!(app.settings_page.doctor.is_loading());
        app.update(Message::Event(Event::Doctor(vec!["coroom 0.1.0".into()])));
        assert!(
            app.update(Message::Settings(Msg::Page(SettingsPage::Diagnostics)))
                .is_empty()
        );
        assert_eq!(
            app.update(Message::Settings(Msg::Page(SettingsPage::Audio))),
            [Effect::LoadAudioDevices]
        );
        assert!(
            app.update(Message::Settings(Msg::Page(SettingsPage::Devices)))
                .is_empty(),
            "sin cuenta no hay dispositivos que pedir"
        );
        assert_eq!(ago(100, 100), "ahora mismo");
        assert_eq!(ago(0, 7200), "hace 2 h");
        assert_eq!(ago(0, 86_400 * 3), "hace 3 días");
    }
}
