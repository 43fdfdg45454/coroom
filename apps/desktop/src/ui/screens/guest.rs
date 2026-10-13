//! Pantalla 5: invitado.
//!
//! Al entrar, "Conectando" con sus pasos (señalización, claves, conexión directa o por relay,
//! calidad, códec) y, antes del vídeo, la **estimación de calidad**: el RTT medido con el anfitrión
//! y su nivel (`docs/ARQUITECTURA.md` §2.1) con para qué juegos va bien. Después el vídeo a toda la ventana
//! con una barra superior que se oculta sola (nivel, RTT, ruta, códec, bitrate, micrófono, audio
//! del juego, inmersivo, estadísticas, pantalla completa, salir) y el panel lateral de
//! estadísticas (el antiguo overlay). Los cambios de permisos llegan como avisos que no bloquean;
//! la reconexión se ve en una franja con el intento; el final, en un diálogo claro.

use std::sync::Arc;
use std::time::{Duration, Instant};

use coroom_session::input::capture::Allowed;
use coroom_session::latency::LatencySummary;
use coroom_session::remote::{QualityLevel, SessionEvent};
use coroom_transport::Path;
use coroom_ui::Element;
use coroom_ui::icon::{Icon, icon_with};
use coroom_ui::style::{ButtonClass, ContainerClass, SvgClass, TextClass};
use coroom_ui::theme::Tone;
use coroom_ui::tokens::{motion, size, space, text as type_size};
use coroom_ui::widget::{self as ui};
use iced_core::alignment::{Horizontal, Vertical};
use iced_core::text::Wrapping;
use iced_core::{Alignment, Length, Padding};
use iced_widget::{Column, Space, button, center, column, container, row, scrollable, stack, text};

use crate::ui::app::{App, Dialog, Effect, Event, Invitation, Message, Page, Screen, quality_tone};
use crate::ui::settings::KnownPeer;
use crate::ui::stats;
use crate::ui::video::{VideoFeed, video_view};

/// Lo que se ve de la estimación antes de pasar al vídeo, como poco.
pub const ESTIMATE_DWELL: Duration = Duration::from_millis(1500);

/// Estadísticas del invitado (las manda el backend cada 250 ms).
#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    /// Resumen de latencia con el enlace.
    pub summary: LatencySummary,
    /// Permisos.
    pub allowed: Allowed,
    /// Micrófono silenciado.
    pub mic_muted: bool,
    /// Juego silenciado.
    pub game_muted: bool,
    /// Voz activa (micrófono abierto y con permiso).
    pub voice: bool,
    /// Estado del audio en palabras.
    pub audio_status: String,
    /// Copias de CPU del vídeo en la ventana.
    pub window_copies: u32,
}

/// Fase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Conectando.
    Connecting,
    /// Vídeo.
    Video,
}

/// Estimación de calidad.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Estimate {
    /// RTT en ms.
    pub rtt_ms: f64,
    /// Nivel.
    pub level: QualityLevel,
    /// Cuándo llegó.
    pub at: Instant,
}

/// Reconexión en marcha.
#[derive(Debug, Clone, PartialEq)]
pub struct Reconnecting {
    /// Intento.
    pub attempt: u32,
    /// De cuántos.
    pub of: u32,
    /// Cuándo se intenta.
    pub at: Instant,
    /// Por qué.
    pub reason: String,
}

/// Estado del invitado.
#[derive(Debug, Clone)]
pub struct State {
    /// Código de la sala.
    pub code: String,
    /// Vídeo.
    pub feed: Arc<VideoFeed>,
    /// Fase.
    pub phase: Phase,
    /// Nombre del anfitrión.
    pub host_name: Option<String>,
    /// Huella del anfitrión.
    pub host_fingerprint: Option<String>,
    /// Número de seguridad.
    pub safety_number: Option<String>,
    /// Claves de la sala listas.
    pub keys: bool,
    /// Ruta.
    pub path: Option<Path>,
    /// Estimación.
    pub estimate: Option<Estimate>,
    /// Códec.
    pub codec: Option<String>,
    /// Estadísticas.
    pub stats: Option<Stats>,
    /// Panel de estadísticas abierto.
    pub stats_open: bool,
    /// Hasta cuándo se ve la barra.
    pub bar_until: Instant,
    /// Modo inmersivo.
    pub immersive: bool,
    /// Reconectando.
    pub reconnecting: Option<Reconnecting>,
    /// Saliendo.
    pub leaving: bool,
    /// Prueba local (sin red).
    pub probe: bool,
    /// Desde cuándo.
    pub started: Instant,
    /// Ventana de errores de decodificación: desde cuándo y cuántos había al empezarla.
    pub decode_window: Option<(Instant, u64)>,
    /// Último aviso de vídeo dañado.
    pub corrupt_notice_at: Option<Instant>,
}

/// Ventana en la que se cuentan los errores de decodificación.
const DECODE_WINDOW: Duration = Duration::from_secs(5);
/// Errores en [`DECODE_WINDOW`] a partir de los que el vídeo se da por dañado.
const DECODE_ERRORS_NOTICE: u64 = 10;
/// Intervalo mínimo entre dos avisos de vídeo dañado.
const CORRUPT_NOTICE_PERIOD: Duration = Duration::from_secs(60);

/// Si toca avisar de que el vídeo llega dañado, con `errors` rechazados por el decodificador
/// desde el principio. Cuenta por ventanas de [`DECODE_WINDOW`] y avisa como mucho una vez por
/// [`CORRUPT_NOTICE_PERIOD`].
pub fn corrupt_video_notice(
    window: &mut Option<(Instant, u64)>,
    last_notice: &mut Option<Instant>,
    errors: u64,
    now: Instant,
) -> bool {
    let (since, at_start) = *window.get_or_insert((now, errors));
    if now.duration_since(since) < DECODE_WINDOW {
        return false;
    }
    *window = Some((now, errors));
    if errors.saturating_sub(at_start) < DECODE_ERRORS_NOTICE {
        return false;
    }
    if last_notice.is_some_and(|at| now.duration_since(at) < CORRUPT_NOTICE_PERIOD) {
        return false;
    }
    *last_notice = Some(now);
    true
}

impl State {
    /// Sesión nueva en la sala `code` con `feed`.
    pub fn new(code: String, feed: Arc<VideoFeed>, now: Instant) -> Self {
        Self {
            code,
            feed,
            phase: Phase::Connecting,
            host_name: None,
            host_fingerprint: None,
            safety_number: None,
            keys: false,
            path: None,
            estimate: None,
            codec: None,
            stats: None,
            stats_open: false,
            bar_until: now,
            immersive: false,
            reconnecting: None,
            leaving: false,
            probe: false,
            started: now,
            decode_window: None,
            corrupt_notice_at: None,
        }
    }

    /// Si hay algo animándose.
    pub fn animating(&self) -> bool {
        self.phase == Phase::Connecting || self.reconnecting.is_some()
    }

    /// Próximo momento en que cambia algo sin eventos (la barra se oculta).
    pub fn next_tick(&self) -> Option<Instant> {
        (self.phase == Phase::Video).then_some(self.bar_until)
    }

    /// Si la barra se ve.
    pub fn bar_visible(&self, now: Instant) -> bool {
        self.phase == Phase::Video
            && (now < self.bar_until || self.stats_open || self.reconnecting.is_some())
    }

    /// El ratón se movió.
    pub fn pointer_moved(&mut self, now: Instant) {
        if !self.immersive {
            self.bar_until = now + motion::BAR_HIDE;
        }
    }

    /// Pasa el tiempo.
    pub fn tick(&mut self, now: Instant) {
        if self.phase == Phase::Connecting
            && self.feed.has_frame()
            && self
                .estimate
                .is_none_or(|estimate| now.duration_since(estimate.at) >= ESTIMATE_DWELL)
        {
            self.phase = Phase::Video;
            self.bar_until = now + motion::BAR_HIDE;
        }
    }
}

/// Mensajes.
#[derive(Debug, Clone)]
pub enum Msg {
    /// Panel de estadísticas.
    ToggleStats,
    /// Inmersivo.
    ToggleImmersive,
    /// Micrófono.
    ToggleMic,
    /// Audio del juego.
    ToggleGame,
    /// Pantalla completa.
    ToggleFullscreen,
    /// Salir (pide confirmación).
    Leave,
    /// Cancelar mientras conecta.
    Cancel,
    /// Reintentar la conexión ya.
    Reconnect,
    /// Número de seguridad.
    Safety,
    /// Ir al vídeo sin esperar.
    Continue,
    /// La ventana cambió el modo del ratón (atajo `Ctrl+Alt+G` o foco perdido).
    ImmersiveChanged(bool),
}

/// Entra en la sala `code`.
pub fn start(app: &mut App, code: String) -> Vec<Effect> {
    app.guest = Some(State::new(code.clone(), VideoFeed::detached(), app.now));
    app.screen = Screen::Guest;
    app.account_menu = false;
    vec![Effect::StopLobby, Effect::Join(code)]
}

/// Sale de la sala.
pub fn leave(app: &mut App) -> Vec<Effect> {
    match app.guest.as_mut() {
        Some(state) if !state.leaving => {
            state.leaving = true;
            vec![Effect::SetImmersive(false), Effect::Leave]
        }
        _ => Vec::new(),
    }
}

/// Vuelve a la pantalla de inicio cuando la sesión terminó.
pub fn close(app: &mut App) -> Vec<Effect> {
    if let Some(state) = app.guest.take() {
        state.feed.close();
    }
    app.dialog = None;
    let mut effects = vec![Effect::SetImmersive(false)];
    if app.fullscreen {
        effects.push(Effect::SetFullscreen(false));
    }
    if app.probe {
        effects.push(Effect::Quit);
        return effects;
    }
    if app.account.is_some() {
        app.screen = Screen::Main;
        app.page = Page::Home;
        effects.push(Effect::StartLobby);
    } else {
        app.screen = Screen::Login;
    }
    effects
}

/// Reduce un mensaje.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    let now = app.now;
    let fullscreen = app.fullscreen;
    let Some(state) = app.guest.as_mut() else {
        return Vec::new();
    };
    match msg {
        Msg::ToggleStats => state.stats_open = !state.stats_open,
        Msg::ToggleImmersive => {
            state.immersive = !state.immersive;
            if state.immersive {
                state.bar_until = now;
            }
            return vec![Effect::SetImmersive(state.immersive)];
        }
        Msg::ImmersiveChanged(immersive) => {
            state.immersive = immersive;
            if !immersive {
                state.bar_until = now + motion::BAR_HIDE;
            }
        }
        Msg::ToggleMic => return vec![Effect::ToggleMic],
        Msg::ToggleGame => return vec![Effect::ToggleGameAudio],
        Msg::ToggleFullscreen => return vec![Effect::SetFullscreen(!fullscreen)],
        Msg::Leave => {
            if state.probe {
                return vec![Effect::Quit];
            }
            app.dialog = Some(Dialog::Leave);
        }
        Msg::Cancel => return leave(app),
        Msg::Reconnect => return vec![Effect::Reconnect],
        Msg::Safety => {
            if let (Some(name), Some(number)) = (&state.host_name, &state.safety_number) {
                app.dialog = Some(Dialog::SafetyNumber {
                    name: name.clone(),
                    fingerprint: state.host_fingerprint.clone().unwrap_or_default(),
                    number: number.clone(),
                });
            }
        }
        Msg::Continue => {
            if state.feed.has_frame() {
                state.phase = Phase::Video;
                state.bar_until = now + motion::BAR_HIDE;
            }
        }
    }
    Vec::new()
}

/// Texto del cambio de permisos, si cambió algo que contar.
pub fn permissions_notice(before: Allowed, after: Allowed) -> Option<(Tone, String)> {
    if before == after {
        return None;
    }
    let gained = |b: bool, a: bool| !b && a;
    let text = if gained(before.keyboard_mouse, after.keyboard_mouse) {
        (
            Tone::Success,
            t!("pantalla-guest-el-anfitrion-te-ha-dado-teclado").to_owned(),
        )
    } else if before.keyboard_mouse && !after.keyboard_mouse {
        (
            Tone::Warning,
            t!("pantalla-guest-el-anfitrion-te-ha-quitado-el").to_owned(),
        )
    } else if let (None, Some(slot)) = (before.gamepad_slot, after.gamepad_slot) {
        (
            Tone::Success,
            t!("pantalla-guest-el-anfitrion-te-ha-dado-el", slot = slot),
        )
    } else if after.gamepad_slot.is_none() {
        (
            Tone::Warning,
            t!("pantalla-guest-el-anfitrion-te-ha-quitado-el-2").to_owned(),
        )
    } else {
        (
            Tone::Info,
            t!("pantalla-guest-ahora-puedes", label = after.label()),
        )
    };
    Some(text)
}

/// Resultados de la sesión del invitado.
pub fn on_event(app: &mut App, event: Event) -> Vec<Effect> {
    let now = app.now;
    match event {
        Event::GuestStarted(Ok(feed)) => {
            if let Some(state) = app.guest.as_mut() {
                state.feed = feed;
            }
        }
        Event::GuestStarted(Err(err)) => {
            app.dialog = Some(Dialog::SessionEnded {
                title: t!("pantalla-guest-no-se-pudo-entrar-en-la").to_owned(),
                text: err,
            });
        }
        Event::GuestStats(stats) => {
            let mut notice = None;
            let mut corrupt = false;
            if let Some(state) = app.guest.as_mut() {
                let before = state.stats.as_ref().map(|old| old.allowed);
                if let Some(before) = before {
                    notice = permissions_notice(before, stats.allowed);
                }
                corrupt = corrupt_video_notice(
                    &mut state.decode_window,
                    &mut state.corrupt_notice_at,
                    stats.summary.decode_errors,
                    now,
                );
                state.stats = Some(*stats);
                state.tick(now);
            }
            if let Some((tone, text)) = notice {
                app.toast(tone, text);
            }
            if corrupt {
                app.toast(
                    Tone::Warning,
                    t!("pantalla-guest-el-video-llega-danado").to_owned(),
                );
            }
        }
        Event::GuestEnded(reason) => {
            let Some(state) = app.guest.as_ref() else {
                return Vec::new();
            };
            match reason {
                None if state.leaving || state.probe => return close(app),
                None => {
                    app.dialog = Some(Dialog::SessionEnded {
                        title: t!("pantalla-guest-la-sesion-ha-terminado").to_owned(),
                        text: t!("pantalla-guest-el-anfitrion-ha-cerrado-la-sala").to_owned(),
                    });
                }
                Some(reason) => {
                    let lower = reason.to_lowercase();
                    let title = if lower.contains("expulsado") {
                        t!("pantalla-guest-el-anfitrion-te-ha-expulsado-de")
                    } else if lower.contains("cerrado la sala") {
                        t!("pantalla-guest-el-anfitrion-ha-cerrado-la-sala-2")
                    } else if lower.contains("red") || lower.contains("conexión") {
                        t!("pantalla-guest-se-perdio-la-conexion-con-la")
                    } else {
                        t!("pantalla-guest-la-sesion-ha-terminado")
                    };
                    app.dialog = Some(Dialog::SessionEnded {
                        title: title.to_owned(),
                        text: reason,
                    });
                }
            }
            return vec![Effect::SetImmersive(false)];
        }
        Event::Guest(event) => return session_event(app, event),
        _ => {}
    }
    Vec::new()
}

fn session_event(app: &mut App, event: SessionEvent) -> Vec<Effect> {
    let now = app.now;
    let Some(state) = app.guest.as_mut() else {
        return Vec::new();
    };
    match event {
        SessionEvent::Joined {
            host_name,
            host_fingerprint,
            safety_number,
        } => {
            state.host_name = Some(host_name.clone());
            state.host_fingerprint = Some(host_fingerprint.clone());
            state.safety_number = Some(safety_number.to_string());
            app.settings.remember_peer(KnownPeer {
                name: host_name,
                fingerprint: host_fingerprint,
                safety_number: safety_number.to_string(),
            });
            return vec![Effect::SaveSettings];
        }
        SessionEvent::EpochChanged { .. } => state.keys = true,
        SessionEvent::Connected { path } => state.path = Some(path),
        SessionEvent::QualityEstimate { rtt_ms, level } => {
            state.estimate = Some(Estimate {
                rtt_ms,
                level,
                at: now,
            });
        }
        SessionEvent::VideoCodec { codec, detail } => {
            state.codec = Some(format!(
                "{codec} ({})",
                detail.trim_start_matches(t!("pantalla-guest-decodifica"))
            ));
        }
        SessionEvent::Reconnecting {
            attempt,
            of,
            retry_in,
            reason,
        } => {
            state.reconnecting = Some(Reconnecting {
                attempt,
                of,
                at: now + retry_in,
                reason,
            });
        }
        SessionEvent::Reconnected { path } => {
            state.reconnecting = None;
            state.path = Some(path);
            app.toast(Tone::Success, t!("pantalla-guest-conexion-recuperada"));
        }
        SessionEvent::RelayUnavailable(message) => {
            app.toast(
                Tone::Warning,
                t!("pantalla-guest-sin-relay-message", message = message),
            );
        }
        SessionEvent::Warning(message) => app.toast(Tone::Warning, message),
        SessionEvent::Invited {
            from_user,
            from_name,
            code,
        } => {
            return app.update(Message::Event(Event::Invited(Invitation {
                from_user,
                from_name,
                code,
            })));
        }
        _ => {}
    }
    Vec::new()
}

fn on(msg: Msg) -> Message {
    Message::Guest(msg)
}

/// Dibuja la pantalla.
pub fn view<'a>(app: &'a App, state: &'a State) -> Element<'a, Message> {
    match state.phase {
        Phase::Connecting => connecting(app, state),
        Phase::Video => video(app, state),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Done,
    Active,
    Pending,
}

fn step<'a>(mark: Mark, title: &'a str, detail: Option<String>) -> Element<'a, Message> {
    let glyph: Element<'a, Message> = match mark {
        Mark::Done => icon_with(Icon::CircleCheck, 20.0, SvgClass::Tone(Tone::Success)),
        Mark::Active => icon_with(Icon::Hourglass, 20.0, SvgClass::Accent),
        Mark::Pending => icon_with(Icon::CircleDashed, 20.0, SvgClass::Secondary),
    };
    let mut texts = column![match mark {
        Mark::Pending => text(title)
            .size(type_size::BODY)
            .class(TextClass::Secondary),
        _ => ui::strong(title),
    }]
    .spacing(2);
    if let Some(detail) = detail {
        texts = texts.push(ui::secondary(detail));
    }
    row![glyph, texts.width(Length::Fill)]
        .spacing(space::S)
        .align_y(Alignment::Center)
        .into()
}

fn quality_card<'a>(estimate: &Estimate) -> Element<'a, Message> {
    let tone = quality_tone(estimate.level);
    ui::card(
        column![
            ui::caption(t!("pantalla-guest-calidad-esperada-con-este-anfitrion")),
            row![
                text(format!("{:.0}", estimate.rtt_ms))
                    .size(type_size::DISPLAY)
                    .font(coroom_ui::font::SEMIBOLD),
                column![
                    ui::secondary(t!("pantalla-guest-ms-de-rtt")),
                    ui::status_badge(tone, estimate.level.label())
                ]
                .spacing(4)
            ]
            .spacing(space::S)
            .align_y(Alignment::Center),
            ui::body(capitalize(estimate.level.description())).wrapping(Wrapping::WordOrGlyph),
            ui::caption(t!(
                "pantalla-guest-sumando-el-pipeline-tipico-25-ms",
                valor = format!(
                    "{:.0}",
                    estimate.rtt_ms + coroom_session::remote::TYPICAL_PIPELINE_MS
                )
            ))
            .wrapping(Wrapping::WordOrGlyph)
        ]
        .spacing(space::XS),
    )
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect::<String>() + ".",
        None => String::new(),
    }
}

fn connecting<'a>(app: &'a App, state: &'a State) -> Element<'a, Message> {
    let signal = if state.host_name.is_some() {
        Mark::Done
    } else {
        Mark::Active
    };
    let next = |done: bool, previous: Mark| match (done, previous) {
        (true, _) => Mark::Done,
        (false, Mark::Done) => Mark::Active,
        _ => Mark::Pending,
    };
    let keys = next(state.keys, signal);
    let link = next(state.path.is_some(), keys);
    let quality = next(state.estimate.is_some(), link);
    let video = next(state.feed.has_frame(), quality);
    let mut steps = Column::new().spacing(space::S);
    steps = steps.push(step(
        signal,
        t!("pantalla-guest-senalizacion"),
        state
            .host_name
            .as_ref()
            .map(|name| t!("pantalla-guest-dentro-de-la-sala-de-name", name = name)),
    ));
    steps = steps.push(step(
        keys,
        t!("pantalla-guest-claves-de-la-sala"),
        Some(t!("pantalla-guest-cifrado-de-extremo-a-extremo-con").to_owned()),
    ));
    steps = steps.push(step(
        link,
        t!("pantalla-guest-conexion-con-el-anfitrion"),
        state.path.map(|path| match path {
            Path::Direct => t!("pantalla-guest-directa-entre-los-dos-equipos").to_owned(),
            Path::Relayed => t!("pantalla-guest-por-el-relay-del-servidor").to_owned(),
        }),
    ));
    steps = steps.push(step(
        quality,
        t!("pantalla-guest-calidad-de-la-conexion"),
        state
            .estimate
            .map(|e| format!("{} · {}", stats::ms(e.rtt_ms), e.level.label())),
    ));
    steps = steps.push(step(video, t!("pantalla-guest-video"), state.codec.clone()));
    let mut content = Column::new()
        .spacing(space::M)
        .push(ui::heading(t!(
            "pantalla-guest-entrando-en-la-sala",
            valor = super::host::spaced(&state.code)
        )))
        .push(steps);
    if let Some(reconnecting) = &state.reconnecting {
        content = content.push(reconnect_banner(app, reconnecting));
    }
    let mut cards = Column::new()
        .spacing(space::M)
        .max_width(size::NARROW + 80.0)
        .push(ui::card(content));
    if let Some(estimate) = &state.estimate {
        cards = cards.push(quality_card(estimate));
    }
    let mut actions = row![ui::flat(
        t!("pantalla-guest-cancelar"),
        Some(on(Msg::Cancel))
    )]
    .spacing(space::XS);
    if state.feed.has_frame() {
        actions = actions.push(ui::fill()).push(ui::primary(
            t!("pantalla-guest-ver-ya"),
            Some(on(Msg::Continue)),
        ));
    }
    cards = cards.push(actions);
    scrollable(
        container(cards)
            .padding(Padding::from([space::XL * 1.5, space::L]))
            .center_x(Length::Fill),
    )
    .height(Length::Fill)
    .into()
}

fn reconnect_banner<'a>(app: &'a App, reconnecting: &'a Reconnecting) -> Element<'a, Message> {
    let wait = reconnecting.at.saturating_duration_since(app.now);
    let text = if wait.is_zero() {
        t!(
            "pantalla-guest-se-perdio-la-conexion-reconectando-intento",
            attempt = reconnecting.attempt,
            of = reconnecting.of
        )
    } else {
        t!(
            "pantalla-guest-se-perdio-la-conexion-reconectando-en",
            valor = format!("{:.0}", wait.as_secs_f64().ceil()),
            attempt = reconnecting.attempt,
            of = reconnecting.of
        )
    };
    ui::banner(
        Tone::Warning,
        text,
        Some((t!("pantalla-guest-reintentar-ya"), on(Msg::Reconnect))),
    )
}

fn bar_button<'a>(
    glyph: Icon,
    tip: impl text::IntoFragment<'a>,
    active: bool,
    message: Message,
) -> Element<'a, Message> {
    let svg = if active {
        SvgClass::OnAccent
    } else {
        SvgClass::Overlay
    };
    ui::with_tooltip(
        button(center(icon_with(glyph, 18.0, svg)))
            .width(Length::Fixed(size::CONTROL))
            .height(Length::Fixed(size::CONTROL))
            .padding(0)
            .class(ButtonClass::Overlay { active })
            .on_press(message),
        tip,
    )
}

fn overlay_text<'a>(
    content: impl text::IntoFragment<'a>,
) -> iced_widget::Text<'a, coroom_ui::Theme, coroom_ui::Renderer> {
    text(content)
        .size(type_size::SECONDARY)
        .class(TextClass::OverlaySecondary)
}

fn top_bar<'a>(app: &'a App, state: &'a State) -> Element<'a, Message> {
    let summary = state.stats.as_ref().map(|stats| &stats.summary);
    let rtt = summary
        .and_then(|summary| summary.link.as_ref())
        .map(|link| link.rtt_ms)
        .or(state.estimate.map(|estimate| estimate.rtt_ms));
    let level = rtt.map(QualityLevel::from_rtt_ms);
    let mut facts = Vec::new();
    if let Some(rtt) = rtt {
        facts.push(format!("RTT {}", stats::ms(rtt)));
    }
    if let Some(path) = state.path {
        facts.push(
            match path {
                Path::Direct => "directo",
                Path::Relayed => "relay",
            }
            .to_owned(),
        );
    }
    if let Some(codec) = state.codec.as_ref().and_then(|c| c.split(' ').next()) {
        facts.push(codec.to_owned());
    }
    if let Some(summary) = summary {
        facts.push(stats::kbps(summary.bitrate_kbps));
    }
    let mut left = row![].spacing(space::S).align_y(Alignment::Center);
    if let Some(level) = level {
        left = left.push(ui::status_badge(quality_tone(level), level.label()));
    }
    left = left.push(
        column![
            text(state.host_name.as_ref().map_or_else(
                || t!("pantalla-guest-sala").to_owned(),
                |name| t!("pantalla-guest-sala-de-name", name = name)
            ))
            .size(type_size::BODY)
            .font(coroom_ui::font::SEMIBOLD)
            .class(TextClass::Overlay),
            overlay_text(facts.join(" · "))
        ]
        .spacing(1),
    );
    let stats_now = state.stats.as_ref();
    let mic_muted = stats_now.is_some_and(|s| s.mic_muted);
    let voice = stats_now.is_some_and(|s| s.voice);
    let game_muted = stats_now.is_some_and(|s| s.game_muted);
    let allowed = stats_now.map(|s| s.allowed).unwrap_or_default();
    let keys = &app.settings.input.shortcuts;
    let mut buttons = row![].spacing(space::XXS).align_y(Alignment::Center);
    if voice {
        buttons = buttons.push(bar_button(
            if mic_muted { Icon::MicOff } else { Icon::Mic },
            if mic_muted {
                t!("pantalla-guest-activar-el-microfono", key = keys.mic)
            } else {
                t!("pantalla-guest-silenciar-el-microfono", key = keys.mic)
            },
            mic_muted,
            on(Msg::ToggleMic),
        ));
    }
    buttons = buttons.push(bar_button(
        if game_muted {
            Icon::VolumeOff
        } else {
            Icon::Volume
        },
        if game_muted {
            t!("pantalla-guest-activar-el-audio-del-juego", key = keys.game)
        } else {
            t!(
                "pantalla-guest-silenciar-el-audio-del-juego",
                key = keys.game
            )
        },
        game_muted,
        on(Msg::ToggleGame),
    ));
    if allowed.keyboard_mouse {
        buttons = buttons.push(bar_button(
            Icon::Pointer,
            t!("pantalla-guest-modo-inmersivo", key = keys.immersive),
            state.immersive,
            on(Msg::ToggleImmersive),
        ));
    }
    buttons = buttons.push(bar_button(
        Icon::Activity,
        t!("pantalla-guest-estadisticas", key = keys.stats),
        state.stats_open,
        on(Msg::ToggleStats),
    ));
    buttons = buttons.push(bar_button(
        if app.fullscreen {
            Icon::Minimize
        } else {
            Icon::Maximize
        },
        t!("pantalla-guest-pantalla-completa-f11"),
        app.fullscreen,
        on(Msg::ToggleFullscreen),
    ));
    buttons = buttons.push(ui::gap(space::XS));
    buttons = buttons.push(
        button(
            row![
                icon_with(Icon::LogOut, 16.0, SvgClass::OnDestructive),
                text(t!("pantalla-guest-salir"))
                    .size(type_size::BODY)
                    .font(coroom_ui::font::SEMIBOLD)
            ]
            .spacing(space::XS)
            .align_y(Alignment::Center),
        )
        .padding(Padding::from([8.0, 14.0]))
        .class(ButtonClass::Destructive)
        .on_press(on(Msg::Leave)),
    );
    container(
        row![left.width(Length::Fill), buttons]
            .spacing(space::M)
            .align_y(Alignment::Center),
    )
    .padding(Padding::from([8.0, 16.0]))
    .width(Length::Fill)
    .class(ContainerClass::Overlay)
    .into()
}

fn stat_row<'a>(label: impl text::IntoFragment<'a>, value: String) -> Element<'a, Message> {
    row![
        overlay_text(label).width(Length::Fill),
        text(value)
            .size(type_size::SECONDARY)
            .font(coroom_ui::font::SEMIBOLD)
            .class(TextClass::Overlay)
            .align_x(Horizontal::Right)
    ]
    .spacing(space::XS)
    .into()
}

fn stat_section<'a>(title: &'a str, rows: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    let mut content = column![
        text(title)
            .size(type_size::CAPTION)
            .font(coroom_ui::font::SEMIBOLD)
            .class(TextClass::OverlaySecondary)
    ]
    .spacing(4);
    for row in rows {
        content = content.push(row);
    }
    content.into()
}

fn stats_panel<'a>(app: &'a App, state: &'a State) -> Element<'a, Message> {
    let Some(current) = &state.stats else {
        return container(overlay_text(t!("pantalla-guest-midiendo")))
            .padding(space::M)
            .width(Length::Fixed(size::STATS_PANEL))
            .class(ContainerClass::Overlay)
            .into();
    };
    let summary = &current.summary;
    let mut sections = Column::new().spacing(space::M);
    let mut video = vec![
        stat_row(
            t!("pantalla-guest-fotogramas-por-segundo"),
            format!("{:.0}", summary.fps),
        ),
        stat_row(
            t!("pantalla-guest-bitrate-recibido"),
            stats::kbps(summary.bitrate_kbps),
        ),
    ];
    if let Some(codec) = &state.codec {
        video.push(stat_row(t!("pantalla-guest-codec"), codec.clone()));
    }
    if !summary.cpu_copies.is_empty() {
        video.push(stat_row(
            t!("pantalla-guest-copias-de-cpu-por-fotograma"),
            format!("{:.1}", summary.cpu_copies_per_frame),
        ));
    }
    video.push(stat_row(
        t!("pantalla-guest-descartados"),
        t!(
            "pantalla-guest-de",
            drops_total = summary.drops_total,
            frames_captured = summary.frames_captured
        ),
    ));
    sections = sections.push(stat_section(t!("pantalla-guest-video"), video));
    let mut latency: Vec<Element<'a, Message>> = summary
        .spans
        .iter()
        .filter(|span| span.samples > 0)
        .map(|span| {
            stat_row(
                format!("{} → {}", span.from.label(), span.to.label()),
                stats::p50_p95(span.p50_us, span.p95_us),
            )
        })
        .collect();
    if summary.total.samples > 0 {
        latency.push(stat_row(
            t!("pantalla-guest-total-de-extremo-a-extremo"),
            stats::p50_p95(summary.total.p50_us, summary.total.p95_us),
        ));
    }
    if !latency.is_empty() {
        sections = sections.push(stat_section(
            t!("pantalla-guest-latencia-por-etapa-p50-p95"),
            latency,
        ));
    }
    if let Some(link) = &summary.link {
        let mut rows = vec![
            stat_row(t!("pantalla-guest-ruta"), link.path.clone()),
            stat_row("RTT", stats::ms(link.rtt_ms)),
            stat_row(
                t!("pantalla-guest-bitrate-objetivo"),
                stats::kbps(f64::from(link.target_kbps)),
            ),
            stat_row(
                t!("pantalla-guest-perdida"),
                stats::percent(link.loss_percent),
            ),
            stat_row(
                t!("pantalla-guest-grupos-recuperados-perdidos"),
                format!("{} / {}", link.groups_recovered, link.groups_dropped),
            ),
            stat_row(t!("pantalla-guest-epoca-mls"), link.epoch.to_string()),
            stat_row(
                t!("pantalla-guest-cifrado-de-los-medios"),
                if link.sframe {
                    "SFrame".to_owned()
                } else {
                    t!("pantalla-guest-sin-sframe-desarrollo").to_owned()
                },
            ),
        ];
        rows.push(
            column![
                overlay_text(t!("pantalla-guest-numero-de-seguridad")),
                text(link.safety_number.clone())
                    .size(type_size::CAPTION)
                    .font(coroom_ui::font::MONO)
                    .class(TextClass::Overlay)
                    .wrapping(Wrapping::WordOrGlyph)
            ]
            .spacing(2)
            .into(),
        );
        sections = sections.push(stat_section(t!("pantalla-guest-enlace"), rows));
    }
    let mut input = vec![stat_row(
        t!("pantalla-guest-permisos"),
        current.allowed.label(),
    )];
    if let Some(latency) = summary.input.filter(|latency| latency.samples > 0) {
        input.push(stat_row(
            t!("pantalla-guest-latencia-de-entrada"),
            stats::p50_p95(latency.p50_us, latency.p95_us),
        ));
    }
    sections = sections.push(stat_section(t!("pantalla-guest-entrada"), input));
    if let Some(audio) = &summary.audio {
        let mut rows = vec![stat_row(
            t!("pantalla-guest-estado"),
            current.audio_status.clone(),
        )];
        if let Some(av) = audio.av.filter(|av| av.samples > 0) {
            rows.push(stat_row(
                t!("pantalla-guest-a-v"),
                format!("{} / {}", stats::ms(av.p50_ms), stats::ms(av.p95_ms)),
            ));
        }
        if !audio.speaking.is_empty() {
            rows.push(stat_row(
                t!("pantalla-guest-hablando"),
                audio.speaking.join(", "),
            ));
        }
        sections = sections.push(stat_section(t!("pantalla-guest-audio"), rows));
    }
    sections = sections.push(
        column![
            overlay_text(t!("pantalla-guest-atajos")),
            overlay_text(shortcuts_line(&app.settings.input.shortcuts))
                .size(type_size::CAPTION)
                .wrapping(Wrapping::WordOrGlyph)
        ]
        .spacing(2),
    );
    container(
        scrollable(sections)
            .spacing(space::XS)
            .height(Length::Shrink),
    )
    .padding(space::M)
    .width(Length::Fixed(size::STATS_PANEL))
    .class(ContainerClass::Overlay)
    .into()
}

/// Los atajos del vídeo, con las letras de Ajustes → Entrada.
fn shortcuts_line(shortcuts: &crate::ui::settings::Shortcuts) -> String {
    t!(
        "pantalla-guest-ctrl-alt-inmersivo-ctrl-alt-estadisticas",
        immersive = shortcuts.immersive,
        stats = shortcuts.stats,
        mic = shortcuts.mic,
        game = shortcuts.game,
        leave = shortcuts.leave
    )
}

fn video<'a>(app: &'a App, state: &'a State) -> Element<'a, Message> {
    let base = container(video_view(&state.feed))
        .width(Length::Fill)
        .height(Length::Fill)
        .class(ContainerClass::Video);
    let mut layers: Vec<Element<'a, Message>> = vec![base.into()];
    let bar_visible = state.bar_visible(app.now);
    let mut overlay = Column::new().spacing(space::S).padding(space::S);
    if bar_visible {
        overlay = overlay.push(top_bar(app, state));
    }
    if let Some(reconnecting) = &state.reconnecting {
        overlay = overlay.push(
            container(reconnect_banner(app, reconnecting))
                .center_x(Length::Fill)
                .max_width(640),
        );
    }
    if state.stats_open {
        overlay = overlay.push(
            container(stats_panel(app, state))
                .width(Length::Fill)
                .align_x(Horizontal::Right),
        );
    }
    layers.push(
        container(overlay)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_y(Vertical::Top)
            .into(),
    );
    if !bar_visible && !state.immersive {
        // Una franja invisible arriba: acercar el ratón hace volver la barra (mover el ratón en
        // cualquier sitio también).
        layers.push(
            Space::new()
                .width(Length::Fill)
                .height(Length::Fixed(4.0))
                .into(),
        );
    }
    stack(layers).into()
}

#[cfg(test)]
mod tests {
    #[test]
    fn corrupt_video_is_noticed_once_per_period() {
        let start = Instant::now();
        let (mut window, mut last) = (None, None);
        assert!(!corrupt_video_notice(&mut window, &mut last, 0, start));
        // Pocos errores en la ventana: no avisa.
        let t1 = start + DECODE_WINDOW;
        assert!(!corrupt_video_notice(&mut window, &mut last, 3, t1));
        // Muchos en la siguiente: avisa.
        let t2 = t1 + DECODE_WINDOW;
        assert!(corrupt_video_notice(&mut window, &mut last, 40, t2));
        // Siguen llegando, pero no se repite antes de tiempo.
        let t3 = t2 + DECODE_WINDOW;
        assert!(!corrupt_video_notice(&mut window, &mut last, 80, t3));
        let t4 = t2 + CORRUPT_NOTICE_PERIOD + DECODE_WINDOW;
        assert!(corrupt_video_notice(&mut window, &mut last, 200, t4));
    }

    use super::*;
    use crate::ui::app::tests::app;
    use coroom_render::{PresentFrame, PresentImage};
    use coroom_session::SessionError;

    fn frame() -> PresentFrame {
        PresentFrame {
            frame_id: 1,
            width: 2,
            height: 2,
            image: PresentImage::I420 {
                y: vec![16; 4],
                u: vec![128],
                v: vec![128],
                y_stride: 2,
                uv_stride: 1,
            },
            color: coroom_render::ColorInfo::SDR,
        }
    }

    #[test]
    fn connecting_shows_the_steps_and_waits_for_the_estimate() {
        let mut app = app();
        app.screen = crate::ui::app::Screen::Main;
        let effects = start(&mut app, "K7Q2MZ".into());
        assert_eq!(effects, [Effect::StopLobby, Effect::Join("K7Q2MZ".into())]);
        let feed = VideoFeed::detached();
        app.update(Message::Event(Event::GuestStarted(Ok(Arc::clone(&feed)))));
        let joined = SessionEvent::Joined {
            host_name: "Ana".into(),
            host_fingerprint: "ab".repeat(32),
            safety_number: coroom_transport::crypto::SafetyNumber::between(&[1; 32], &[2; 32]),
        };
        assert_eq!(
            app.update(Message::Event(Event::Guest(joined))),
            [Effect::SaveSettings]
        );
        app.update(Message::Event(Event::Guest(SessionEvent::Connected {
            path: Path::Direct,
        })));
        app.update(Message::Event(Event::Guest(
            SessionEvent::QualityEstimate {
                rtt_ms: 12.0,
                level: QualityLevel::from_rtt_ms(12.0),
            },
        )));
        let state = app.guest.as_ref().expect("invitado");
        assert_eq!(state.host_name.as_deref(), Some("Ana"));
        assert_eq!(
            state.estimate.map(|e| e.level),
            Some(QualityLevel::Excellent)
        );
        // Llega vídeo, pero la estimación se ve un momento antes.
        feed.push(frame(), None, None);
        app.update(Message::Tick);
        assert_eq!(app.guest.as_ref().map(|g| g.phase), Some(Phase::Connecting));
        app.now += ESTIMATE_DWELL;
        app.update(Message::Tick);
        let state = app.guest.as_ref().expect("invitado");
        assert_eq!(state.phase, Phase::Video);
        assert!(state.bar_visible(app.now), "la barra se ve al empezar");
        app.now += motion::BAR_HIDE;
        assert!(
            !app.guest.as_ref().is_some_and(|g| g.bar_visible(app.now)),
            "y se oculta sola"
        );
        app.update(Message::PointerMoved);
        assert!(app.guest.as_ref().is_some_and(|g| g.bar_visible(app.now)));
    }

    #[test]
    fn reconnection_and_endings_are_explained() {
        let mut app = app();
        start(&mut app, "K7Q2MZ".into());
        app.update(Message::Event(Event::Guest(SessionEvent::Reconnecting {
            attempt: 2,
            of: 5,
            retry_in: Duration::from_secs(1),
            reason: "error de red".into(),
        })));
        assert!(app.guest.as_ref().is_some_and(|g| g.reconnecting.is_some()));
        assert_eq!(
            app.update(Message::Guest(Msg::Reconnect)),
            [Effect::Reconnect]
        );
        app.update(Message::Event(Event::Guest(SessionEvent::Reconnected {
            path: Path::Relayed,
        })));
        assert!(app.guest.as_ref().is_some_and(|g| g.reconnecting.is_none()));
        assert!(app.toasts.iter().any(|t| t.text.contains("recuperada")));
        app.update(Message::Event(Event::GuestEnded(Some(
            SessionError::Removed.to_string(),
        ))));
        assert!(matches!(
            &app.dialog,
            Some(Dialog::SessionEnded { title, .. }) if title.contains("expulsado")
        ));
        app.update(Message::ConfirmDialog);
        assert!(app.guest.is_none());
        assert_eq!(app.screen, Screen::Login, "sin cuenta, al inicio de sesión");
    }

    #[test]
    fn leaving_asks_and_permission_changes_are_announced() {
        let mut app = app();
        start(&mut app, "K7Q2MZ".into());
        app.update(Message::Guest(Msg::Leave));
        assert_eq!(app.dialog, Some(Dialog::Leave));
        assert_eq!(
            app.update(Message::ConfirmDialog),
            [Effect::SetImmersive(false), Effect::Leave]
        );
        assert!(
            app.update(Message::Guest(Msg::Cancel)).is_empty(),
            "ya saliendo"
        );
        let view_only = Allowed::default();
        let kbm = Allowed {
            keyboard_mouse: true,
            gamepad_slot: None,
        };
        assert!(permissions_notice(view_only, kbm).is_some_and(|(_, t)| t.contains("teclado")));
        assert!(permissions_notice(kbm, view_only).is_some_and(|(tone, _)| tone == Tone::Warning));
        let pad = Allowed {
            keyboard_mouse: false,
            gamepad_slot: Some(2),
        };
        assert!(permissions_notice(view_only, pad).is_some_and(|(_, t)| t.contains("ranura 2")));
        assert_eq!(permissions_notice(kbm, kbm), None);
        app.update(Message::Event(Event::GuestEnded(None)));
        assert!(app.guest.is_none(), "salida normal: sin diálogo");
    }
}
