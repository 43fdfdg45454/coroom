//! Pantalla 4: anfitrión.
//!
//! Paso 1, qué compartir: la fuente (pantalla o ventana por el portal en Linux, el monitor en
//! Windows, o la fuente de prueba), el audio del juego (sistema, una aplicación, un proceso o
//! ninguno) y la voz. Paso 2, la sala abierta: el código grande con copiar, la vista previa de lo
//! que se comparte, los invitados con sus permisos como interruptores (teclado y ratón, mando con
//! ranura, voz), sus estadísticas (ruta, RTT, bitrate, pérdida, latencia de entrada), pausar a
//! todos, expulsar, invitar a amigos conectados y "Dejar de compartir".

use std::sync::Arc;

use coroom_session::audio::GameCapture;
use coroom_session::remote::{GuestStatus, SessionEvent, permissions_label};
use coroom_session::{GuestId, Permissions};
use coroom_ui::Element;
use coroom_ui::icon::{Icon, icon_with};
use coroom_ui::style::{ButtonClass, ContainerClass, SvgClass};
use coroom_ui::theme::Tone;
use coroom_ui::tokens::{size, space, text as type_size};
use coroom_ui::widget::{self as ui, Kind};
use iced_core::text::Wrapping;
use iced_core::{Alignment, Length, Padding};
use iced_widget::{Column, button, column, container, row};

use crate::ui::app::{App, Dialog, Effect, Event, HostRequest, Load, Message, Page};
use crate::ui::settings::{CodecChoice, KnownPeer};
use crate::ui::stats;
use crate::ui::video::{VideoFeed, video_view};

/// Fuente.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceChoice {
    /// La pantalla o una ventana (portal en Linux, monitor principal en Windows).
    #[default]
    Screen,
    /// Patrón sintético.
    Synthetic,
}

/// Audio del juego.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioChoice {
    /// Lo que suena.
    #[default]
    System,
    /// Una aplicación (PipeWire).
    App,
    /// Un proceso (Windows).
    Process,
    /// Nada.
    None,
}

/// Ranura de mando en el selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PadSlot(pub Option<u8>);

impl std::fmt::Display for PadSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            None => f.write_str(t!("pantalla-host-sin-mando")),
            Some(slot) => write!(f, "{}", t!("pantalla-host-ranura-slot", slot = slot)),
        }
    }
}

/// Sala abierta.
#[derive(Debug, Clone)]
pub struct Room {
    /// Código (cuando el servidor la crea).
    pub code: Option<String>,
    /// Huella del anfitrión.
    pub fingerprint: String,
    /// Códec en uso.
    pub codec: Option<String>,
    /// Invitados.
    pub guests: Vec<GuestStatus>,
    /// Entrada en pausa.
    pub paused: bool,
    /// Vista previa.
    pub preview: Arc<VideoFeed>,
    /// Sin vista previa (la captura va por la GPU).
    pub preview_unavailable: bool,
    /// Sin relay (cuota o servidor).
    pub relay: Option<String>,
}

/// Estado.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// Fuente.
    pub source: SourceChoice,
    /// Audio del juego.
    pub audio: AudioChoice,
    /// Aplicación (con [`AudioChoice::App`]).
    pub app_name: String,
    /// PID (con [`AudioChoice::Process`]).
    pub pid: String,
    /// Voz.
    pub voice: bool,
    /// Abriendo.
    pub starting: bool,
    /// Cerrando.
    pub stopping: bool,
    /// Error al abrir.
    pub error: Option<String>,
    /// Error de los campos.
    pub field_error: Option<String>,
    /// Sala abierta.
    pub room: Option<Room>,
}

impl State {
    /// Si hay una sala abierta (o abriéndose).
    pub fn is_open(&self) -> bool {
        self.room.is_some() || self.starting
    }

    /// Vista previa, si hay sala.
    pub fn preview(&self) -> Option<Arc<VideoFeed>> {
        self.room.as_ref().map(|room| Arc::clone(&room.preview))
    }
}

/// Mensajes.
#[derive(Debug, Clone)]
pub enum Msg {
    /// Fuente.
    Source(SourceChoice),
    /// Audio.
    Audio(AudioChoice),
    /// Aplicación.
    AppName(String),
    /// PID.
    Pid(String),
    /// Voz.
    Voice(bool),
    /// Abrir la sala.
    Start,
    /// Copiar el código.
    CopyCode,
    /// Dejar de compartir (pide confirmación).
    Stop,
    /// Pausar o reanudar la entrada de todos.
    Pause(bool),
    /// Teclado y ratón de un invitado.
    Keyboard(GuestId, bool),
    /// Mando de un invitado.
    Pad(GuestId, PadSlot),
    /// Voz de un invitado.
    GuestVoice(GuestId, bool),
    /// Expulsar (pide confirmación).
    Kick(GuestId),
    /// Invitar a un amigo conectado.
    Invite(u64),
    /// Número de seguridad de un invitado.
    Safety(GuestId),
    /// Ir a Ajustes → Vídeo.
    VideoSettings,
}

fn guest(app: &App, id: GuestId) -> Option<&GuestStatus> {
    app.host
        .room
        .as_ref()
        .and_then(|room| room.guests.iter().find(|status| status.guest == id))
}

fn set_permissions(
    app: &mut App,
    id: GuestId,
    change: impl FnOnce(&mut Permissions),
) -> Vec<Effect> {
    let Some(room) = app.host.room.as_mut() else {
        return Vec::new();
    };
    let Some(status) = room.guests.iter_mut().find(|status| status.guest == id) else {
        return Vec::new();
    };
    change(&mut status.permissions);
    vec![Effect::SetPermissions(id, status.permissions)]
}

/// Reduce un mensaje.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Source(source) => app.host.source = source,
        Msg::Audio(audio) => {
            app.host.audio = audio;
            app.host.field_error = None;
        }
        Msg::AppName(name) => app.host.app_name = name,
        Msg::Pid(pid) => app.host.pid = pid.chars().filter(char::is_ascii_digit).collect(),
        Msg::Voice(voice) => app.host.voice = voice,
        Msg::Start => {
            if app.host.is_open() {
                return Vec::new();
            }
            let audio = match app.host.audio {
                AudioChoice::System => GameCapture::System,
                AudioChoice::None => GameCapture::None,
                AudioChoice::App => {
                    let name = app.host.app_name.trim();
                    if name.is_empty() {
                        app.host.field_error =
                            Some(t!("pantalla-host-escribe-el-nombre-de-la-aplicacion").to_owned());
                        return Vec::new();
                    }
                    GameCapture::App(name.to_owned())
                }
                AudioChoice::Process => match app.host.pid.parse::<u32>() {
                    Ok(pid) if pid > 0 => GameCapture::Process(pid),
                    _ => {
                        app.host.field_error =
                            Some(t!("pantalla-host-escribe-el-numero-de-proceso-pid").to_owned());
                        return Vec::new();
                    }
                },
            };
            app.host.starting = true;
            app.host.error = None;
            app.host.field_error = None;
            return vec![
                Effect::StopLobby,
                Effect::StartHost(HostRequest {
                    source: app.host.source,
                    audio,
                    voice: app.host.voice,
                }),
            ];
        }
        Msg::CopyCode => {
            if let Some(code) = app.host.room.as_ref().and_then(|room| room.code.clone()) {
                return app.update(Message::Copy(code));
            }
        }
        Msg::Stop => app.dialog = Some(Dialog::StopSharing),
        Msg::Pause(paused) => {
            if let Some(room) = app.host.room.as_mut() {
                room.paused = paused;
                return vec![Effect::PauseInput(paused)];
            }
        }
        Msg::Keyboard(id, on) => return set_permissions(app, id, |p| p.keyboard_mouse = on),
        Msg::Pad(id, slot) => {
            let taken = slot.0.is_some_and(|wanted| {
                app.host.room.as_ref().is_some_and(|room| {
                    room.guests
                        .iter()
                        .any(|g| g.guest != id && g.permissions.gamepad_slot == Some(wanted))
                })
            });
            if taken {
                app.toast(
                    Tone::Warning,
                    t!("pantalla-host-la-slot-ya-es-de-otro", slot = slot),
                );
                return Vec::new();
            }
            return set_permissions(app, id, |p| p.gamepad_slot = slot.0);
        }
        Msg::GuestVoice(id, on) => return set_permissions(app, id, |p| p.voice = on),
        Msg::Kick(id) => {
            if let Some(status) = guest(app, id) {
                app.dialog = Some(Dialog::Kick {
                    guest: id,
                    name: status.name.clone(),
                });
            }
        }
        Msg::Invite(user) => return app.update(Message::Home(super::home::Msg::Invite(user))),
        Msg::Safety(id) => {
            if let Some(status) = guest(app, id) {
                app.dialog = Some(Dialog::SafetyNumber {
                    name: status.name.clone(),
                    fingerprint: status.fingerprint.clone(),
                    number: status.safety_number.to_string(),
                });
            }
        }
        Msg::VideoSettings => {
            app.settings_page.page = super::settings::SettingsPage::Video;
            return app.update(Message::Navigate(Page::Settings));
        }
    }
    Vec::new()
}

/// Ranuras que se ofrecen, la preferida primero.
pub fn pad_choices(preferred: u8, max: u8) -> Vec<PadSlot> {
    let mut slots: Vec<PadSlot> = (1..=max).map(|slot| PadSlot(Some(slot))).collect();
    if (1..=max).contains(&preferred) {
        slots.sort_by_key(|slot| slot.0 != Some(preferred));
    }
    slots.insert(0, PadSlot(None));
    slots
}

/// Resultados de la sesión del anfitrión.
pub fn on_event(app: &mut App, event: Event) -> Vec<Effect> {
    match event {
        Event::HostStarted(Ok(preview)) => {
            app.host.starting = false;
            app.host.room = Some(Room {
                code: None,
                fingerprint: String::new(),
                codec: None,
                guests: Vec::new(),
                paused: false,
                preview,
                preview_unavailable: false,
                relay: None,
            });
            app.page = Page::Host;
        }
        Event::HostStarted(Err(err)) => {
            app.host.starting = false;
            app.host.error = Some(err);
            return vec![Effect::StartLobby];
        }
        Event::HostGuests(guests) => {
            if let Some(room) = app.host.room.as_mut() {
                room.guests = guests;
            }
        }
        Event::PreviewUnavailable => {
            if let Some(room) = app.host.room.as_mut() {
                room.preview_unavailable = true;
            }
        }
        Event::HostEnded(reason) => {
            let was_open = app.host.room.take().is_some();
            app.host.starting = false;
            app.host.stopping = false;
            if let Some(reason) = reason {
                app.toast(
                    Tone::Error,
                    t!("pantalla-host-la-sala-se-cerro-reason", reason = reason),
                );
            } else if was_open {
                app.toast(Tone::Info, t!("pantalla-host-has-dejado-de-compartir"));
            }
            if app.page == Page::Host && !was_open {
                app.page = Page::Home;
            }
            return if app.account.is_some() {
                vec![Effect::StartLobby]
            } else {
                Vec::new()
            };
        }
        Event::Host(event) => return session_event(app, event),
        _ => {}
    }
    Vec::new()
}

fn session_event(app: &mut App, event: SessionEvent) -> Vec<Effect> {
    let mut effects = Vec::new();
    let Some(room) = app.host.room.as_mut() else {
        return effects;
    };
    match event {
        SessionEvent::RoomCreated { code, fingerprint } => {
            room.code = Some(code);
            room.fingerprint = fingerprint;
        }
        SessionEvent::GuestJoined {
            name,
            fingerprint,
            safety_number,
            permissions,
            ..
        } => {
            app.settings.remember_peer(KnownPeer {
                name: name.clone(),
                fingerprint,
                safety_number: safety_number.to_string(),
            });
            effects.push(Effect::SaveSettings);
            app.toast(
                Tone::Info,
                t!(
                    "pantalla-host-name-ha-entrado-en-la-sala",
                    name = name,
                    valor = permissions_label(&permissions)
                ),
            );
        }
        SessionEvent::GuestLeft { guest, kicked } => {
            let name = room
                .guests
                .iter()
                .find(|status| status.guest == guest)
                .map_or_else(
                    || t!("pantalla-host-el-invitado", valor = guest.0),
                    |s| s.name.clone(),
                );
            room.guests.retain(|status| status.guest != guest);
            app.toast(
                Tone::Neutral,
                if kicked {
                    t!(
                        "pantalla-host-name-ha-sido-expulsado-las-claves",
                        name = name
                    )
                } else {
                    t!("pantalla-host-name-ha-salido-de-la-sala", name = name)
                },
            );
        }
        SessionEvent::VideoCodec { codec, detail } => {
            room.codec = Some(format!(
                "{codec} ({})",
                detail.trim_start_matches(t!("pantalla-host-codifica"))
            ));
        }
        SessionEvent::RelayUnavailable(message) => room.relay = Some(message),
        SessionEvent::Warning(message) => app.toast(Tone::Warning, message),
        _ => {}
    }
    effects
}

fn on(msg: Msg) -> Message {
    Message::Host(msg)
}

/// Dibuja la página.
pub fn view(app: &App) -> Element<'_, Message> {
    match &app.host.room {
        Some(room) => room_view(app, room),
        None => setup(app),
    }
}

fn choice<'a>(
    title: &'a str,
    subtitle: &'a str,
    selected: bool,
    message: Message,
) -> Element<'a, Message> {
    let mark = if selected {
        icon_with(Icon::CircleCheck, 20.0, SvgClass::Accent)
    } else {
        container(ui::gap(0.0))
            .width(Length::Fixed(20.0))
            .height(Length::Fixed(20.0))
            .into()
    };
    button(ui::pref_row(title, Some(subtitle.to_owned()), mark))
        .padding(0)
        .width(Length::Fill)
        .class(ButtonClass::Flat)
        .on_press(message)
        .into()
}

fn setup(app: &App) -> Element<'_, Message> {
    let state = &app.host;
    let video = &app.settings.video;
    let (width, height) = video.max_resolution.size();
    let codec = match video.codec {
        CodecChoice::Auto => t!("pantalla-host-el-mejor-comun-con-los-invitados"),
        CodecChoice::Av1 => "AV1",
        CodecChoice::Hevc => "HEVC",
        CodecChoice::H264 => "H.264",
    };
    let source = ui::section(
        t!("pantalla-host-que-compartir"),
        None,
        vec![
            choice(
                t!("pantalla-host-pantalla-o-ventana"),
                t!("pantalla-host-en-linux-lo-eliges-en-el"),
                state.source == SourceChoice::Screen,
                on(Msg::Source(SourceChoice::Screen)),
            ),
            choice(
                t!("pantalla-host-fuente-de-prueba"),
                t!("pantalla-host-un-patron-animado-sirve-para-probar"),
                state.source == SourceChoice::Synthetic,
                on(Msg::Source(SourceChoice::Synthetic)),
            ),
        ],
    );
    let mut audio_rows = vec![
        choice(
            t!("pantalla-host-todo-lo-que-suena"),
            t!("pantalla-host-el-audio-del-sistema-como-lo"),
            state.audio == AudioChoice::System,
            on(Msg::Audio(AudioChoice::System)),
        ),
        choice(
            t!("pantalla-host-solo-una-aplicacion"),
            t!("pantalla-host-linux-el-nombre-de-la-aplicacion"),
            state.audio == AudioChoice::App,
            on(Msg::Audio(AudioChoice::App)),
        ),
    ];
    if state.audio == AudioChoice::App {
        audio_rows.push(
            container(Element::from(
                ui::field(t!("pantalla-host-aplicacion"), &state.app_name)
                    .placeholder(t!("pantalla-host-juego"))
                    .on_input(|value| on(Msg::AppName(value)))
                    .error(state.field_error.as_deref())
                    .id("audio-app"),
            ))
            .padding(Padding::from([4.0, 16.0]))
            .into(),
        );
    }
    audio_rows.push(choice(
        t!("pantalla-host-solo-un-proceso"),
        t!("pantalla-host-windows-el-juego-y-sus-procesos"),
        state.audio == AudioChoice::Process,
        on(Msg::Audio(AudioChoice::Process)),
    ));
    if state.audio == AudioChoice::Process {
        audio_rows.push(
            container(Element::from(
                ui::field(t!("pantalla-host-pid-del-proceso"), &state.pid)
                    .placeholder("4242")
                    .on_input(|value| on(Msg::Pid(value)))
                    .error(state.field_error.as_deref())
                    .id("audio-pid"),
            ))
            .padding(Padding::from([4.0, 16.0]))
            .into(),
        );
    }
    audio_rows.push(choice(
        t!("pantalla-host-sin-audio"),
        t!("pantalla-host-solo-la-imagen"),
        state.audio == AudioChoice::None,
        on(Msg::Audio(AudioChoice::None)),
    ));
    let audio = ui::section(t!("pantalla-host-audio-del-juego"), None, audio_rows);
    let voice = ui::section(
        t!("pantalla-host-voz"),
        None,
        vec![ui::pref_row(
            t!("pantalla-host-hablar-por-la-sala-con-el"),
            Some(t!("pantalla-host-los-invitados-te-oyen-y-hablan").to_owned()),
            ui::switch(state.voice, Some(|on_off| on(Msg::Voice(on_off)))),
        )],
    );
    let quality = ui::section(
        t!("pantalla-host-calidad"),
        None,
        vec![ui::pref_row(
            t!("pantalla-host-video"),
            Some(t!(
                "pantalla-host-hasta-width-height-a-fps-y",
                width = width,
                height = height,
                fps = video.fps,
                valor = stats::kbps(f64::from(video.max_bitrate_kbps)),
                codec = codec
            )),
            ui::button_with(
                Kind::Link,
                t!("pantalla-host-cambiar"),
                None,
                Some(on(Msg::VideoSettings)),
            ),
        )],
    );
    let mut page = Column::new()
        .spacing(space::L)
        .push(ui::page_header(
            t!("pantalla-host-compartir-mi-pantalla"),
            Some(t!("pantalla-host-paso-1-de-2-elige-que").to_owned()),
            vec![],
        ))
        .push(source)
        .push(audio)
        .push(voice)
        .push(quality);
    if let Some(error) = &state.error {
        page = page.push(ui::banner(
            Tone::Error,
            t!("pantalla-host-no-se-pudo-abrir-la-sala", error = error),
            Some((t!("pantalla-host-reintentar"), on(Msg::Start))),
        ));
    }
    let action: Element<'_, Message> = if state.starting {
        container(ui::progress(
            t!("pantalla-host-abriendo-la-sala-en-linux-elige"),
            None,
            app.now.elapsed().as_secs_f32(),
        ))
        .width(Length::Fixed(420.0))
        .into()
    } else {
        ui::button_with(
            Kind::Primary,
            t!("pantalla-host-abrir-la-sala"),
            Some(Icon::ShareScreen),
            Some(on(Msg::Start)),
        )
        .into()
    };
    page = page.push(row![ui::fill(), action].align_y(Alignment::Center));
    ui::page(page)
}

fn code_card<'a>(room: &'a Room) -> Element<'a, Message> {
    let code: Element<'a, Message> = match &room.code {
        Some(code) => row![
            ui::mono(spaced(code), type_size::CODE).font(coroom_ui::font::MONO),
            ui::fill(),
            ui::button_with(
                Kind::Normal,
                t!("pantalla-host-copiar"),
                Some(Icon::Copy),
                Some(on(Msg::CopyCode))
            )
        ]
        .align_y(Alignment::Center)
        .into(),
        None => ui::progress(
            t!("pantalla-host-creando-la-sala-en-el-servidor"),
            None,
            0.5,
        ),
    };
    ui::card(
        column![
            ui::caption(t!("pantalla-host-codigo-de-la-sala")),
            code,
            ui::secondary(t!("pantalla-host-daselo-a-tus-invitados-lo-escriben"))
                .wrapping(Wrapping::WordOrGlyph)
        ]
        .spacing(space::XS),
    )
}

/// `K7Q2MZ` → `K7Q 2MZ` (más fácil de dictar).
pub fn spaced(code: &str) -> String {
    let (a, b) = code.split_at(code.len().min(3));
    if b.is_empty() {
        a.to_owned()
    } else {
        format!("{a} {b}")
    }
}

fn preview_card(room: &Room) -> Element<'_, Message> {
    let body: Element<'_, Message> = if room.preview_unavailable {
        container(
            column![
                icon_with(Icon::Monitor, 28.0, SvgClass::Overlay),
                iced_widget::text(t!("pantalla-host-sin-vista-previa-la-captura-va"))
                    .size(type_size::SECONDARY)
                    .class(coroom_ui::style::TextClass::Overlay)
                    .align_x(iced_core::alignment::Horizontal::Center)
                    .wrapping(Wrapping::WordOrGlyph)
            ]
            .spacing(space::XS)
            .align_x(Alignment::Center),
        )
        .padding(space::M)
        .center(Length::Fill)
        .into()
    } else {
        video_view(&room.preview)
    };
    container(body)
        .width(Length::Fixed(288.0))
        .height(Length::Fixed(162.0))
        .class(ContainerClass::Video)
        .into()
}

fn guest_card<'a>(app: &'a App, status: &'a GuestStatus) -> Element<'a, Message> {
    let id = status.guest;
    let connected = status.path.is_some();
    let mut facts = Vec::new();
    if let Some(path) = status.path {
        facts.push(match path {
            coroom_transport::Path::Direct => "directo".to_owned(),
            coroom_transport::Path::Relayed => t!("pantalla-host-por-el-relay").to_owned(),
        });
    }
    if let Some(rtt) = status.rtt_ms {
        facts.push(format!("RTT {}", stats::ms(rtt)));
    }
    if let Some(kbps) = status.target_kbps {
        facts.push(stats::kbps(f64::from(kbps)));
    }
    if status.input.samples > 0 {
        facts.push(t!(
            "pantalla-host-entrada",
            valor = stats::ms(status.input.p50_us as f64 / 1000.0)
        ));
    }
    if facts.is_empty() {
        facts.push(t!("pantalla-host-conectando").to_owned());
    }
    let header = row![
        ui::avatar(&app.theme, &status.name, size::AVATAR),
        column![
            ui::strong(status.name.as_str()),
            ui::secondary(facts.join(" · "))
        ]
        .spacing(2)
        .width(Length::Fill),
        if connected {
            ui::status_badge(Tone::Success, t!("pantalla-host-conectado"))
        } else {
            ui::status_badge(Tone::Warning, t!("pantalla-host-conectando-2"))
        },
        ui::icon_button(
            Icon::Fingerprint,
            t!("pantalla-host-numero-de-seguridad"),
            Some(on(Msg::Safety(id)))
        ),
        ui::button_with(
            Kind::DestructiveFlat,
            t!("pantalla-host-expulsar"),
            Some(Icon::UserX),
            Some(on(Msg::Kick(id)))
        )
    ]
    .spacing(space::S)
    .align_y(Alignment::Center);
    let max = coroom_session::input::max_gamepads();
    let slots = pad_choices(app.settings.input.preferred_slot, max);
    let current = PadSlot(status.permissions.gamepad_slot);
    let permissions = row![
        row![
            icon_with(Icon::Keyboard, 18.0, SvgClass::Secondary),
            ui::body(t!("pantalla-host-teclado-y-raton")),
            ui::switch(
                status.permissions.keyboard_mouse,
                Some(move |on_off| on(Msg::Keyboard(id, on_off)))
            )
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center),
        row![
            icon_with(Icon::Gamepad, 18.0, SvgClass::Secondary),
            ui::select(slots, Some(current), move |slot| on(Msg::Pad(id, slot)))
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center),
        row![
            icon_with(Icon::Mic, 18.0, SvgClass::Secondary),
            ui::body(t!("pantalla-host-voz")),
            ui::switch(
                status.permissions.voice,
                Some(move |on_off| on(Msg::GuestVoice(id, on_off)))
            )
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center),
    ]
    .spacing(space::L)
    .align_y(Alignment::Center);
    let mut card = column![header, permissions].spacing(space::S);
    if !status.input_key
        && (status.permissions.keyboard_mouse || status.permissions.gamepad_slot.is_some())
    {
        card = card.push(ui::caption(t!(
            "pantalla-host-esperando-la-clave-de-entrada-pareja"
        )));
    }
    ui::card(card)
}

fn room_view<'a>(app: &'a App, room: &'a Room) -> Element<'a, Message> {
    let status = match (&room.codec, room.guests.is_empty()) {
        (Some(codec), _) => t!("pantalla-host-compartiendo-codec", codec = codec),
        (None, true) => t!("pantalla-host-esperando-al-primer-invitado-para-elegir").to_owned(),
        (None, false) => t!("pantalla-host-eligiendo-el-codec-con-tus-invitados").to_owned(),
    };
    let pause: Element<'a, Message> = if room.paused {
        ui::button_with(
            Kind::Normal,
            t!("pantalla-host-reanudar-la-entrada"),
            Some(Icon::Play),
            Some(on(Msg::Pause(false))),
        )
        .into()
    } else {
        ui::button_with(
            Kind::Normal,
            t!("pantalla-host-pausar-a-todos"),
            Some(Icon::Pause),
            Some(on(Msg::Pause(true))),
        )
        .into()
    };
    let stop: Element<'a, Message> = ui::button_with(
        Kind::Destructive,
        t!("pantalla-host-dejar-de-compartir"),
        Some(Icon::Close),
        (!app.host.stopping).then(|| on(Msg::Stop)),
    )
    .into();
    let mut page = Column::new().spacing(space::L).push(ui::page_header(
        t!("pantalla-host-tu-sala"),
        Some(status),
        vec![pause, stop],
    ));
    if room.paused {
        page = page.push(ui::banner(
            Tone::Warning,
            t!("pantalla-host-la-entrada-de-todos-los-invitados"),
            Some((t!("pantalla-host-reanudar"), on(Msg::Pause(false)))),
        ));
    }
    if let Some(relay) = &room.relay {
        page = page.push(ui::banner(
            Tone::Warning,
            t!(
                "pantalla-host-sin-relay-relay-solo-podran-entrar",
                relay = relay
            ),
            None,
        ));
    }
    page = page.push(
        row![
            container(code_card(room)).width(Length::Fill),
            preview_card(room)
        ]
        .spacing(space::M)
        .align_y(Alignment::Start),
    );
    let guests: Element<'a, Message> = if room.guests.is_empty() {
        ui::card(ui::empty_state(
            Icon::Users,
            t!("pantalla-host-todavia-no-ha-entrado-nadie"),
            t!("pantalla-host-comparte-el-codigo-de-la-sala"),
            None,
        ))
    } else {
        Column::with_children(room.guests.iter().map(|status| guest_card(app, status)))
            .spacing(space::S)
            .into()
    };
    page = page.push(
        column![
            ui::heading(t!("pantalla-host-invitados", valor = room.guests.len())),
            guests
        ]
        .spacing(space::S),
    );
    page = page.push(invite_friends(app, room));
    ui::page(page)
}

fn invite_friends<'a>(app: &'a App, room: &'a Room) -> Element<'a, Message> {
    let body: Element<'a, Message> = match &app.friends {
        Load::Loaded(friends) => {
            let inside: Vec<u64> = room.guests.iter().map(|status| status.user).collect();
            let online: Vec<_> = friends
                .friends
                .iter()
                .filter(|friend| friend.online && !inside.contains(&friend.id))
                .collect();
            if online.is_empty() {
                ui::caption(t!("pantalla-host-ninguno-de-tus-amigos-conectados-esta")).into()
            } else {
                let rows = online
                    .into_iter()
                    .map(|friend| {
                        ui::list_row(
                            Some(ui::avatar(&app.theme, &friend.display_name, 28.0)),
                            friend.display_name.as_str(),
                            Some(format!("@{}", friend.username)),
                            vec![
                                ui::button_with(
                                    Kind::Normal,
                                    t!("pantalla-host-invitar"),
                                    Some(Icon::Mail),
                                    room.code.as_ref().map(|_| on(Msg::Invite(friend.id))),
                                )
                                .into(),
                            ],
                        )
                    })
                    .collect();
                ui::boxed_list(rows)
            }
        }
        Load::Failed(err) => ui::banner(Tone::Error, err.as_str(), None),
        Load::Idle | Load::Loading => ui::skeleton(t!("pantalla-host-cargando-tus-amigos"), 1),
    };
    column![ui::heading(t!("pantalla-host-invitar-amigos")), body]
        .spacing(space::S)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::tests::app;

    fn status(id: u32, name: &str) -> GuestStatus {
        GuestStatus {
            guest: GuestId(id),
            user: u64::from(id) + 10,
            name: name.into(),
            public_key: [0; 32],
            fingerprint: "ab".repeat(32),
            safety_number: coroom_transport::crypto::SafetyNumber::between(&[1; 32], &[2; 32]),
            permissions: Permissions::VIEW_ONLY,
            leaf: id,
            path: None,
            rtt_ms: None,
            target_kbps: None,
            input_key: true,
            input: Default::default(),
        }
    }

    fn open_room(app: &mut App) {
        app.update(Message::Host(Msg::Start));
        app.update(Message::Event(Event::HostStarted(
            Ok(VideoFeed::detached()),
        )));
        app.update(Message::Event(Event::Host(SessionEvent::RoomCreated {
            code: "K7Q2MZ".into(),
            fingerprint: "cd".repeat(32),
        })));
        app.update(Message::Event(Event::HostGuests(vec![
            status(1, "Bea"),
            status(2, "Carla"),
        ])));
    }

    #[test]
    fn opening_a_room_asks_for_the_audio_details() {
        let mut app = app();
        app.update(Message::Host(Msg::Audio(AudioChoice::App)));
        assert!(app.update(Message::Host(Msg::Start)).is_empty());
        assert!(app.host.field_error.is_some());
        app.update(Message::Host(Msg::AppName("Juego".into())));
        app.update(Message::Host(Msg::Voice(true)));
        let effects = app.update(Message::Host(Msg::Start));
        assert_eq!(
            effects,
            [
                Effect::StopLobby,
                Effect::StartHost(HostRequest {
                    source: SourceChoice::Screen,
                    audio: GameCapture::App("Juego".into()),
                    voice: true,
                })
            ]
        );
        assert!(app.host.starting && app.host.is_open());
        assert!(
            app.update(Message::Host(Msg::Start)).is_empty(),
            "una sola sala"
        );
        app.update(Message::Event(Event::HostStarted(Err(
            "no hay pantalla".into()
        ))));
        assert!(!app.host.is_open());
        assert_eq!(app.host.error.as_deref(), Some("no hay pantalla"));
        app.update(Message::Host(Msg::Audio(AudioChoice::Process)));
        app.update(Message::Host(Msg::Pid("4a2".into())));
        assert_eq!(app.host.pid, "42", "solo cifras");
    }

    #[test]
    fn permissions_are_switches_and_slots_are_exclusive() {
        let mut app = app();
        open_room(&mut app);
        let effects = app.update(Message::Host(Msg::Keyboard(GuestId(1), true)));
        assert!(matches!(
            &effects[..],
            [Effect::SetPermissions(GuestId(1), p)] if p.keyboard_mouse && p.gamepad_slot.is_none()
        ));
        let effects = app.update(Message::Host(Msg::Pad(GuestId(1), PadSlot(Some(2)))));
        assert!(
            matches!(&effects[..], [Effect::SetPermissions(_, p)] if p.gamepad_slot == Some(2) && p.keyboard_mouse)
        );
        // La ranura 2 ya es de Bea.
        assert!(
            app.update(Message::Host(Msg::Pad(GuestId(2), PadSlot(Some(2)))))
                .is_empty()
        );
        assert!(app.toasts.iter().any(|t| t.text.contains("Ranura 2")));
        app.update(Message::Host(Msg::Kick(GuestId(2))));
        assert!(matches!(&app.dialog, Some(Dialog::Kick { name, .. }) if name == "Carla"));
        assert_eq!(
            app.update(Message::ConfirmDialog),
            [Effect::Kick(GuestId(2))]
        );
        assert_eq!(
            app.update(Message::Host(Msg::Pause(true))),
            [Effect::PauseInput(true)]
        );
    }

    #[test]
    fn guests_joining_and_leaving_are_announced() {
        let mut app = app();
        open_room(&mut app);
        let effects = app.update(Message::Event(Event::Host(SessionEvent::GuestJoined {
            guest: GuestId(3),
            user: 13,
            name: "Dani".into(),
            fingerprint: "ef".repeat(32),
            safety_number: coroom_transport::crypto::SafetyNumber::between(&[1; 32], &[3; 32]),
            permissions: Permissions::VIEW_ONLY,
        })));
        assert_eq!(
            effects,
            [Effect::SaveSettings],
            "se recuerda su número de seguridad"
        );
        assert_eq!(app.settings.peers[0].name, "Dani");
        app.update(Message::Event(Event::Host(SessionEvent::GuestLeft {
            guest: GuestId(1),
            kicked: true,
        })));
        assert!(
            app.toasts
                .iter()
                .any(|t| t.text.contains("Bea ha sido expulsado"))
        );
        assert_eq!(app.host.room.as_ref().map(|r| r.guests.len()), Some(1));
        let effects = app.update(Message::Event(Event::HostEnded(None)));
        assert!(app.host.room.is_none());
        assert!(effects.is_empty(), "sin cuenta no hay sala de espera");
    }

    #[test]
    fn pad_slots_offer_the_preferred_first() {
        let names: Vec<String> = pad_choices(3, 4).iter().map(ToString::to_string).collect();
        assert_eq!(
            names,
            ["Sin mando", "Ranura 3", "Ranura 1", "Ranura 2", "Ranura 4"]
        );
        assert_eq!(pad_choices(0, 2).len(), 3);
        assert_eq!(spaced("K7Q2MZ"), "K7Q 2MZ");
    }
}
