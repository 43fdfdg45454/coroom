//! Pantalla 3 («Dentro de la sala»), vista del invitado.
//!
//! Al entrar, la sala de espera con su cabecera: los pasos (señalización, claves, conexión
//! directa o por relay, calidad, códec) y, antes del vídeo, la **estimación de calidad** (el RTT
//! medido con el anfitrión y su nivel, `docs/ARQUITECTURA.md` §2.1). El chat ya funciona aquí, en
//! cuanto llegan las claves. Después, el vídeo a toda la ventana con:
//!
//! - una **barra superior** flotante que aparece al mover el ratón y se oculta a los 2,5 s
//!   (nivel y latencia añadida, sala, ruta, códec, resolución y bitrate; micrófono, audio,
//!   control, chat con sus no leídos, conexión, pantalla completa y salir);
//! - la **píldora** de latencia abajo a la izquierda («48 ms · Directa», «82 ms · Relay · CGNAT»,
//!   «Reconectando · 3 s»): fija en ventana; a pantalla completa se oculta con la barra;
//! - el **panel lateral** de 340 px con las pestañas Chat · Conexión · Sala (no se oculta solo);
//! - las **burbujas** de los mensajes del chat con el panel cerrado.
//!
//! La latencia añadida es la medida, nunca estimada: la mediana de extremo a extremo del vídeo
//! (`LatencySummary::total`), la de la entrada y la del audio del juego; sin muestras, «midiendo».
//! Los cambios de permisos llegan como avisos que no bloquean; el final, en un diálogo claro.

use std::sync::Arc;
use std::time::{Duration, Instant};

use coroom_session::input::capture::Allowed;
use coroom_session::input::gamepad::{ConnectedPad, PadChoice, PadKey, PadKind};
use coroom_session::latency::{AudioTrack, LatencySummary};
use coroom_session::remote::{QualityLevel, RoomMember, SessionEvent, talker_label};
use coroom_transport::Path;
use coroom_ui::Element;
use coroom_ui::icon::{Icon, icon_with};
use coroom_ui::style::{ButtonClass, ContainerClass, SliderClass, SvgClass, TextClass};
use coroom_ui::theme::{Palette, Tone};
use coroom_ui::tokens::{motion, size, space, text as type_size};
use coroom_ui::widget::{self as ui};
use iced_core::alignment::{Horizontal, Vertical};
use iced_core::text::Wrapping;
use iced_core::{Alignment, Length, Padding};
use iced_widget::{Column, Space, button, center, column, container, row, scrollable, stack, text};

use super::chat::{self, Surface};
use super::host::Takeover;
use super::{diagnostics, settings};
use crate::ui::app::{
    App, AudioDevice, Dialog, Effect, EndKind, Event, HEADERBAR_HEIGHT, Invitation, Message, Page,
    Screen, quality_tone,
};
use crate::ui::settings::{
    DEFAULT_VOICE_VOLUME, DecoderChoice, KnownPeer, MAX_VOICE_VOLUME, VoiceMode, voice_gain,
};
use crate::ui::stats;
use crate::ui::video::{VideoFeed, video_view};

/// Lo que se ve de la estimación antes de pasar al vídeo, como poco.
pub const ESTIMATE_DWELL: Duration = Duration::from_millis(1500);

/// Lo que tarda la barra en aparecer.
const BAR_IN: Duration = motion::PANEL;

/// Lo que se ve la píldora a pantalla completa (sin la barra) cuando cambia el nivel de latencia.
pub const PILL_FLASH: Duration = Duration::from_millis(2500);

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
    /// Ahora se envía la voz propia (el anfitrión no la devuelve en `VoiceState`).
    pub speaking: bool,
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

/// Pestaña del panel lateral de la sala.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// Chat de la sala (`Ctrl+Alt+C`).
    Chat,
    /// Conexión y calidad (`Ctrl+Alt+S`).
    Connection,
    /// Integrantes de la sala.
    Room,
    /// Ajustes de este equipo en la sala: vídeo, audio, dispositivos y mando (`Ctrl+Alt+J`).
    Settings,
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
    /// Dirección del anfitrión (o del relay) por la que va la conexión.
    pub remote: Option<std::net::SocketAddr>,
    /// Por qué se va por el relay (la frase entera; la píldora lleva el motivo corto).
    pub relay_reason: Option<String>,
    /// Estimación.
    pub estimate: Option<Estimate>,
    /// Códec.
    pub codec: Option<String>,
    /// Estadísticas.
    pub stats: Option<Stats>,
    /// Pestaña abierta del panel lateral (`None`: cerrado).
    pub panel: Option<Tab>,
    /// Quién está en la sala (con el anfitrión), según el último `Members`.
    pub members: Vec<RoomMember>,
    /// Hojas MLS de las voces que este equipo no quiere oír.
    pub muted: std::collections::BTreeSet<u32>,
    /// Volumen de cada voz en este equipo, por hoja MLS (porcentaje; sin entrada, 100).
    pub volumes: std::collections::BTreeMap<u32, u8>,
    /// Volumen del audio del juego en este equipo (porcentaje), aparte de silenciarlo.
    pub game_volume: u8,
    /// Mandos conectados a este equipo (`None`: aún no se sabe).
    pub pads: Option<Vec<ConnectedPad>>,
    /// Si el mando que se envía va tal cual.
    pub pads_raw: bool,
    /// Hasta cuándo se ve la barra.
    pub bar_until: Instant,
    /// Cuándo empezó a verse la barra (para su entrada).
    pub bar_shown_at: Instant,
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
    /// El último `Tick`.
    pub now: Instant,
    /// Ventana de errores de decodificación: desde cuándo y cuántos había al empezarla.
    pub decode_window: Option<(Instant, u64)>,
    /// Último aviso de vídeo dañado.
    pub corrupt_notice_at: Option<Instant>,
    /// Cuándo cambió por última vez el nivel de la latencia (a pantalla completa, la píldora se
    /// enseña un momento).
    pub level_changed_at: Option<Instant>,
    /// Si el anfitrión comparte algo (`None`: aún no lo ha dicho; un anfitrión anterior a la
    /// versión 14 del protocolo nunca lo dice y siempre comparte).
    pub host_sharing: Option<bool>,
    /// La sala pasó a otro anfitrión: al terminar esta sesión, se entra en la de este código.
    pub moving_to: Option<String>,
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
    /// Volumen de la voz de la hoja `leaf` en este equipo (porcentaje).
    pub fn volume(&self, leaf: u32) -> u8 {
        self.volumes
            .get(&leaf)
            .copied()
            .unwrap_or(DEFAULT_VOICE_VOLUME)
    }

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
            remote: None,
            relay_reason: None,
            estimate: None,
            codec: None,
            stats: None,
            panel: None,
            members: Vec::new(),
            muted: std::collections::BTreeSet::new(),
            volumes: std::collections::BTreeMap::new(),
            game_volume: DEFAULT_VOICE_VOLUME,
            pads: None,
            pads_raw: false,
            bar_until: now,
            bar_shown_at: now,
            immersive: false,
            reconnecting: None,
            leaving: false,
            probe: false,
            started: now,
            now,
            decode_window: None,
            corrupt_notice_at: None,
            level_changed_at: None,
            host_sharing: None,
            moving_to: None,
        }
    }

    /// Si el anfitrión dijo que no comparte nada (la sala es solo de voz y chat).
    pub fn nothing_shared(&self) -> bool {
        self.host_sharing == Some(false)
    }

    /// Si hay algo animándose a cada refresco: solo la entrada de la barra. Los pasos de la
    /// conexión y la cuenta atrás de la reconexión cambian a plazos fijos ([`State::next_tick`]),
    /// no hace falta dibujar a 60 Hz mientras tanto.
    pub fn animating(&self) -> bool {
        self.bar_visible(self.now) && self.now.duration_since(self.bar_shown_at) < BAR_IN
    }

    /// Próximo momento en que cambia algo sin eventos: la barra se oculta, la píldora deja de
    /// verse, acaba la espera de la estimación (pasa al vídeo) o la cuenta atrás de la reconexión
    /// baja un segundo.
    pub fn next_tick(&self) -> Option<Instant> {
        let now = self.now;
        let mut next: Option<Instant> = None;
        let mut consider = |at: Instant| {
            if at > now {
                next = Some(next.map_or(at, |next| next.min(at)));
            }
        };
        if self.phase == Phase::Video {
            consider(self.bar_until);
            if let Some(at) = self.level_changed_at {
                consider(at + PILL_FLASH);
            }
        } else if self.feed.has_frame()
            && let Some(estimate) = self.estimate
        {
            consider(estimate.at + ESTIMATE_DWELL);
        }
        if let Some(reconnecting) = &self.reconnecting {
            let wait = reconnecting.at.saturating_duration_since(now);
            if !wait.is_zero() {
                // El texto enseña los segundos redondeados hacia arriba: cambia en cada entero.
                consider(reconnecting.at - Duration::from_secs(wait.as_secs()));
            }
        }
        next
    }

    /// Si la barra se ve.
    pub fn bar_visible(&self, now: Instant) -> bool {
        self.phase == Phase::Video
            && (now < self.bar_until || self.panel.is_some() || self.reconnecting.is_some())
    }

    /// Si la píldora se ve: siempre en ventana; a pantalla completa, con la barra o un momento
    /// ([`PILL_FLASH`]) tras cambiar el nivel de latencia.
    pub fn pill_visible(&self, now: Instant, fullscreen: bool) -> bool {
        self.phase == Phase::Video
            && (!fullscreen
                || self.bar_visible(now)
                || self
                    .level_changed_at
                    .is_some_and(|at| now.duration_since(at) < PILL_FLASH))
    }

    /// Enseña la barra durante [`motion::BAR_HIDE`].
    fn reveal_bar(&mut self, now: Instant) {
        if !self.bar_visible(now) {
            self.bar_shown_at = now;
        }
        self.bar_until = now + motion::BAR_HIDE;
    }

    /// El ratón se movió.
    pub fn pointer_moved(&mut self, now: Instant) {
        if !self.immersive {
            self.reveal_bar(now);
        }
    }

    /// Pasa el tiempo.
    pub fn tick(&mut self, now: Instant) {
        self.now = now;
        // Sin nada compartido no llegará vídeo: se entra en cuanto el anfitrión lo dice.
        if self.phase == Phase::Connecting && self.nothing_shared() {
            self.phase = Phase::Video;
            self.reveal_bar(now);
        }
        if self.phase == Phase::Connecting
            && self.feed.has_frame()
            && self
                .estimate
                .is_none_or(|estimate| now.duration_since(estimate.at) >= ESTIMATE_DWELL)
        {
            self.phase = Phase::Video;
            self.reveal_bar(now);
        }
    }

    /// Latencia añadida medida (mediana de extremo a extremo del vídeo), en ms.
    pub fn added_ms(&self) -> Option<f64> {
        let total = &self.stats.as_ref()?.summary.total;
        (total.samples > 0).then(|| total.p50_us as f64 / 1000.0)
    }

    /// Nivel de la latencia añadida medida.
    pub fn level(&self) -> Option<QualityLevel> {
        self.added_ms().map(QualityLevel::from_overhead_ms)
    }
}

/// Lo que dice la píldora de latencia.
#[derive(Debug, Clone, PartialEq)]
pub struct Pill {
    /// Tono del punto (`None`: gris, sin datos).
    pub tone: Option<Tone>,
    /// Reconectando (lleva el icono de recargar).
    pub reconnecting: bool,
    /// Primer tramo: «48 ms», «Conectando…», «Reconectando».
    pub lead: String,
    /// Ruta: «Directa» o «Relay».
    pub route: Option<String>,
    /// Si va por el relay.
    pub relayed: bool,
    /// Motivo corto («CGNAT», «pérdida 4 %») o la espera de la reconexión («3 s»).
    pub reason: Option<String>,
}

impl Pill {
    /// Los tramos unidos con « · » (para las pruebas y la accesibilidad).
    pub fn label(&self) -> String {
        let mut parts = vec![self.lead.clone()];
        parts.extend(self.route.clone());
        parts.extend(self.reason.clone());
        parts.join(" · ")
    }
}

/// Ruta en una palabra.
fn route_label(path: Path) -> &'static str {
    match path {
        Path::Direct => t!("room-directa"),
        Path::Relayed => t!("room-relay"),
    }
}

/// Motivo corto del relay a partir de la frase de la conexión (`DirectAttempt::relay_reason`).
pub fn short_relay_reason(reason: &str) -> &'static str {
    if reason.contains("CGNAT") {
        t!("room-relay-cgnat")
    } else if reason.contains("red local") {
        t!("room-relay-red-local")
    } else if reason.contains("no anunció") {
        t!("room-relay-sin-direcciones")
    } else if reason.contains("se pidió") {
        t!("room-relay-forzado")
    } else {
        t!("room-relay-directa-fallo")
    }
}

/// Lo que más duele del enlace ahora: la pérdida si llega al 1 %, si no el RTT.
fn worst(summary: &LatencySummary) -> Option<String> {
    let link = summary.link.as_ref()?;
    Some(if link.loss_percent >= 1.0 {
        t!(
            "room-perdida-valor",
            value = stats::number(link.loss_percent, 0)
        )
    } else {
        t!("room-rtt-valor", value = stats::number(link.rtt_ms, 0))
    })
}

/// La píldora para el estado de ahora.
pub fn pill(state: &State, now: Instant) -> Pill {
    if let Some(reconnecting) = &state.reconnecting {
        let wait = reconnecting.at.saturating_duration_since(now);
        return Pill {
            tone: None,
            reconnecting: true,
            lead: t!("room-reconectando").to_owned(),
            route: None,
            relayed: false,
            reason: (!wait.is_zero()).then(|| {
                t!(
                    "room-segundos",
                    secs = format!("{:.0}", wait.as_secs_f64().ceil())
                )
            }),
        };
    }
    let route = state.path.map(|path| route_label(path).to_owned());
    let relayed = state.path == Some(Path::Relayed);
    let relay_reason = || {
        relayed.then(|| {
            state
                .relay_reason
                .as_deref()
                .map_or(t!("room-relay-directa-fallo"), short_relay_reason)
                .to_owned()
        })
    };
    let (Some(added), Some(stats)) = (state.added_ms(), state.stats.as_ref()) else {
        return Pill {
            tone: None,
            reconnecting: false,
            lead: if route.is_some() {
                t!("room-midiendo-corto").to_owned()
            } else {
                t!("room-conectando").to_owned()
            },
            route,
            relayed,
            reason: relay_reason(),
        };
    };
    let level = QualityLevel::from_overhead_ms(added);
    let reason = if level == QualityLevel::Poor {
        worst(&stats.summary).or_else(relay_reason)
    } else {
        relay_reason()
    };
    Pill {
        tone: Some(quality_tone(level)),
        reconnecting: false,
        lead: format!("{} ms", stats::number(added, 0)),
        route,
        relayed,
        reason,
    }
}

/// Mensajes.
#[derive(Debug, Clone)]
pub enum Msg {
    /// `Ctrl+Alt+S`: abre o cierra el panel en «Conexión».
    ToggleStats,
    /// `Ctrl+Alt+J`: abre o cierra el panel en «Sala» (mandos y personas).
    TogglePads,
    /// Abre el panel en esa pestaña o, si ya estaba en ella, lo cierra.
    TogglePanel(Tab),
    /// Cambia de pestaña.
    ShowTab(Tab),
    /// Cierra el panel.
    ClosePanel,
    /// Oír o no a alguien (su hoja MLS) en este equipo.
    Hear(u32, bool),
    /// Volumen de la voz de un miembro (por hoja MLS) en este equipo, mientras se arrastra.
    Volume(u32, u8),
    /// Al soltar el deslizador: recordar el volumen por el nombre del miembro.
    VolumeDone(u32),
    /// Volumen del audio del juego en este equipo, mientras se arrastra.
    GameVolume(u8),
    /// Al soltar el deslizador del juego: recordarlo en los ajustes.
    GameVolumeDone,
    /// Mandos conectados y si el que se envía va tal cual.
    Pads(Vec<ConnectedPad>, bool),
    /// Elegir qué mando enviar.
    ChoosePad(PadChoice),
    /// Inmersivo.
    ToggleImmersive,
    /// Micrófono (silenciarte para toda la sala).
    ToggleMic,
    /// Volumen de la voz propia que se envía (porcentaje), mientras se arrastra.
    MicVolume(u8),
    /// Al soltar el deslizador de la voz propia: recordarlo en los ajustes.
    MicVolumeDone,
    /// Audio del juego.
    ToggleGame,
    /// VSync de la ventana (ajuste de este equipo, al momento).
    SetVsync(bool),
    /// Decodificar por software (al momento).
    SoftwareDecoder(bool),
    /// Pantalla completa.
    ToggleFullscreen,
    /// Salir (pide confirmación).
    Leave,
    /// No aceptar la sala que pasa el anfitrión.
    DeclineHandoff,
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

/// Con el vídeo delante, si a la interfaz le importa que el cursor esté en `position` (lógica,
/// en una ventana de `size`): sobre la franja de la barra (y el aviso de reconexión), el panel
/// lateral, la píldora y las burbujas del chat abajo, o con toasts a la vista. Fuera de ahí solo
/// hay vídeo: moverse no cambia nada que dibujar, y la ventana se ahorra reconstruir la interfaz.
pub fn pointer_over_ui(app: &App, position: iced_core::Point, size: iced_core::Size) -> bool {
    let Some(state) = app.guest.as_ref() else {
        return true;
    };
    if state.phase != Phase::Video || !app.toasts.is_empty() || state.reconnecting.is_some() {
        return true;
    }
    let now = app.now;
    let top = space::S + size::VIDEO_BAR + space::S;
    if state.bar_visible(now) && position.y <= top {
        return true;
    }
    if state.panel.is_some() && position.x >= size.width - (size::STATS_PANEL + 2.0 * space::S) {
        return true;
    }
    // Abajo: la píldora (izquierda) y las burbujas del chat (derecha, hasta tres).
    let bottom = if app.chat.bubbles.is_empty() {
        size::CONTROL + 2.0 * space::S
    } else {
        4.0 * (size::CONTROL + space::S) + 2.0 * space::S
    };
    (state.pill_visible(now, app.fullscreen) || !app.chat.bubbles.is_empty())
        && position.y >= size.height - bottom
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
    app.chat.open = false;
    app.chat.composing = false;
    let mut effects = vec![Effect::SetImmersive(false)];
    if app.fullscreen {
        effects.push(Effect::SetFullscreen(false));
    }
    if app.probe {
        effects.push(Effect::Quit);
        return effects;
    }
    if app.host.room.is_some() {
        // Se salió para quedarse con la sala propia (pasó el anfitrión): a Compartir, sin la sala
        // de espera (la sala lleva su señalización).
        app.screen = Screen::Main;
        app.page = Page::Host;
    } else if app.account.is_some() {
        app.screen = Screen::Main;
        app.page = Page::Home;
        effects.push(Effect::StartLobby);
    } else {
        app.screen = Screen::Login;
    }
    effects
}

/// Abre el panel en `tab` (`None`: lo cierra); el chat se entera (solo se ve en su pestaña).
fn set_panel(app: &mut App, tab: Option<Tab>) -> Vec<Effect> {
    if let Some(state) = app.guest.as_mut() {
        state.panel = tab;
        if tab.is_some() {
            let now = app.now;
            state.reveal_bar(now);
        }
    }
    let mut effects = chat::tab_changed(app, tab);
    // «Ajustes» enseña los micrófonos y salidas: se pide la lista al abrirla.
    if tab == Some(Tab::Settings) {
        effects.extend(settings::request_audio(app));
    }
    effects
}

/// Abre `tab` o, si ya está abierta, cierra el panel.
fn toggle_panel(app: &mut App, tab: Tab) -> Vec<Effect> {
    let open = app.guest.as_ref().and_then(|state| state.panel);
    set_panel(app, (open != Some(tab)).then_some(tab))
}

/// Reduce un mensaje.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    let now = app.now;
    let fullscreen = app.fullscreen;
    let Some(state) = app.guest.as_mut() else {
        return Vec::new();
    };
    match msg {
        Msg::ToggleStats | Msg::TogglePads if state.phase == Phase::Connecting => {}
        Msg::ToggleStats => return toggle_panel(app, Tab::Connection),
        Msg::TogglePads => return toggle_panel(app, Tab::Settings),
        Msg::TogglePanel(tab) => return toggle_panel(app, tab),
        Msg::ShowTab(tab) => return set_panel(app, Some(tab)),
        Msg::ClosePanel => return set_panel(app, None),
        Msg::Hear(leaf, hear) => {
            if hear {
                state.muted.remove(&leaf);
            } else {
                state.muted.insert(leaf);
            }
            let mut effects = vec![Effect::SetVoiceMuted {
                host: false,
                source: leaf,
                muted: !hear,
            }];
            // Se recuerda por su nombre, como el volumen: vale al volver o en otra sala.
            if let Some(member) = state
                .members
                .iter()
                .find(|member| member.leaf == leaf && !member.you)
            {
                let name = member.name.clone();
                app.settings.audio.set_voice_muted(&name, !hear);
                effects.extend(app.settings_changed());
            }
            return effects;
        }
        Msg::Volume(leaf, percent) => {
            if state
                .members
                .iter()
                .any(|member| member.leaf == leaf && member.you)
            {
                // La propia voz no se oye aquí: nada que ajustar.
                return Vec::new();
            }
            let percent = percent.min(MAX_VOICE_VOLUME);
            state.volumes.insert(leaf, percent);
            return vec![Effect::SetVoiceGain {
                host: false,
                source: leaf,
                gain: voice_gain(percent),
            }];
        }
        Msg::VolumeDone(leaf) => {
            if let Some(member) = state
                .members
                .iter()
                .find(|member| member.leaf == leaf && !member.you)
            {
                let name = member.name.clone();
                let percent = state.volume(leaf);
                app.settings.audio.set_voice_volume(&name, percent);
                return app.settings_changed();
            }
        }
        Msg::GameVolume(percent) => {
            let percent = percent.min(MAX_VOICE_VOLUME);
            state.game_volume = percent;
            return vec![Effect::SetGameGain {
                gain: voice_gain(percent),
            }];
        }
        Msg::GameVolumeDone => {
            app.settings.audio.game_volume = state.game_volume;
            return app.settings_changed();
        }
        Msg::Pads(pads, raw) => {
            let slot = state.stats.as_ref().and_then(|s| s.allowed.gamepad_slot);
            let notice = state
                .pads
                .as_ref()
                .and_then(|before| pads_notice(before, &pads, slot.is_some()));
            state.pads = Some(pads);
            state.pads_raw = raw;
            if let Some(notice) = notice {
                app.toast(Tone::Info, notice);
            }
        }
        Msg::ChoosePad(choice) => {
            app.settings.input.pad = choice.clone();
            return vec![Effect::SetPadChoice(choice), Effect::SaveSettings];
        }
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
                state.reveal_bar(now);
            }
        }
        Msg::ToggleMic => return vec![Effect::ToggleMic],
        Msg::MicVolume(percent) => {
            app.settings.audio.mic_volume = percent.min(MAX_VOICE_VOLUME);
            return vec![Effect::SetMicGain {
                host: false,
                gain: voice_gain(app.settings.audio.mic_volume),
            }];
        }
        Msg::MicVolumeDone => return app.settings_changed(),
        Msg::ToggleGame => return vec![Effect::ToggleGameAudio],
        Msg::SetVsync(vsync) => {
            app.settings.video.vsync = vsync;
            return app.settings_changed();
        }
        Msg::SoftwareDecoder(software) => {
            app.settings.video.decoder = if software {
                DecoderChoice::Software
            } else {
                DecoderChoice::Auto
            };
            let mut effects = app.settings_changed();
            effects.push(Effect::SetSoftwareDecoder(software));
            return effects;
        }
        Msg::ToggleFullscreen => return vec![Effect::SetFullscreen(!fullscreen)],
        Msg::Leave => {
            if state.probe {
                return vec![Effect::Quit];
            }
            app.dialog = Some(Dialog::Leave);
        }
        Msg::Cancel => return leave(app),
        Msg::DeclineHandoff => {
            app.dialog = None;
            return vec![Effect::ReplyHandoff(None)];
        }
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
                state.reveal_bar(now);
            }
        }
    }
    Vec::new()
}

/// Aviso de un mando que se conectó o se desconectó, con el que se envía ahora si hay ranura.
fn pads_notice(before: &[ConnectedPad], after: &[ConnectedPad], slot: bool) -> Option<String> {
    let has = |list: &[ConnectedPad], key: &PadKey| list.iter().any(|pad| &pad.key == key);
    let active = after
        .iter()
        .find(|pad| pad.active)
        .map(|pad| pad.key.name.as_str());
    let (mut text, connected) = if let Some(pad) = after.iter().find(|p| !has(before, &p.key)) {
        (
            t!("room-mando-conectado", name = pad.key.name.as_str()),
            Some(&pad.key),
        )
    } else {
        let pad = before.iter().find(|p| !has(after, &p.key))?;
        (
            t!("room-mando-desconectado", name = pad.key.name.as_str()),
            None,
        )
    };
    if slot {
        let more = match (active, connected) {
            (Some(name), Some(key)) if name != key.name => {
                t!("room-sigues-enviando", name = name)
            }
            (Some(name), None) => t!("room-ahora-envias", name = name),
            (None, None) => t!("room-no-envias-ningun-mando").to_owned(),
            _ => String::new(),
        };
        if !more.is_empty() {
            text.push(' ');
            text.push_str(&more);
        }
    }
    Some(text)
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
            t!("room-el-anfitrion-te-ha-dado-teclado").to_owned(),
        )
    } else if before.keyboard_mouse && !after.keyboard_mouse {
        (
            Tone::Warning,
            t!("room-el-anfitrion-te-ha-quitado-el").to_owned(),
        )
    } else if let (None, Some(slot)) = (before.gamepad_slot, after.gamepad_slot) {
        (
            Tone::Success,
            t!("room-el-anfitrion-te-ha-dado-el", slot = slot),
        )
    } else if after.gamepad_slot.is_none() {
        (
            Tone::Warning,
            t!("room-el-anfitrion-te-ha-quitado-el-2").to_owned(),
        )
    } else {
        (Tone::Info, t!("room-ahora-puedes", label = after.label()))
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
                // La sesión arranca con el juego y la voz propia al volumen normal: se les
                // aplica el recordado.
                state.game_volume = app.settings.audio.game_volume;
                let mut effects = Vec::new();
                if state.game_volume != DEFAULT_VOICE_VOLUME {
                    effects.push(Effect::SetGameGain {
                        gain: voice_gain(state.game_volume),
                    });
                }
                if app.settings.audio.mic_volume != DEFAULT_VOICE_VOLUME {
                    effects.push(Effect::SetMicGain {
                        host: false,
                        gain: voice_gain(app.settings.audio.mic_volume),
                    });
                }
                // Y con el micrófono abierto: quien llega de otra sala callado sigue callado.
                if app.carried_mute.take() == Some(true) {
                    effects.push(Effect::SetMicMuted {
                        host: false,
                        muted: true,
                    });
                }
                return effects;
            }
        }
        Event::GuestStarted(Err(err)) => {
            app.carried_mute = None;
            app.dialog = Some(Dialog::SessionEnded {
                title: t!("room-no-se-pudo-entrar-en-la").to_owned(),
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
                let level_before = state.level();
                state.stats = Some(*stats);
                if state.level() != level_before && level_before.is_some() {
                    state.level_changed_at = Some(now);
                }
                state.tick(now);
            }
            if let Some((tone, text)) = notice {
                app.toast(tone, text);
            }
            if corrupt {
                app.toast(Tone::Warning, t!("room-el-video-llega-danado").to_owned());
            }
        }
        Event::GuestEnded(reason) => {
            let Some(state) = app.guest.as_ref() else {
                return Vec::new();
            };
            match std::mem::take(&mut app.host.takeover) {
                // Ya se contestó con la sala propia: que esta termine es lo esperado.
                Takeover::Replied(_) => return close(app),
                // La retirada sigue en pie hasta que la sala propia abra (y se cierre).
                Takeover::Withdrawn => app.host.takeover = Takeover::Withdrawn,
                // Sin sala de la que venir, la que se abre queda como una más.
                Takeover::Opening | Takeover::None => {}
            }
            if let Some(code) = state.moving_to.clone() {
                // La sala pasó a otro anfitrión: de esta a la suya, sin pasar por Inicio.
                if let Some(old) = app.guest.take() {
                    old.feed.close();
                }
                return start(app, code);
            }
            match reason {
                None if state.leaving || state.probe => return close(app),
                None => {
                    app.dialog = Some(Dialog::SessionEnded {
                        title: t!("room-la-sesion-ha-terminado").to_owned(),
                        text: t!("room-el-anfitrion-ha-cerrado-la-sala").to_owned(),
                    });
                }
                Some(end) => {
                    let title = match end.kind {
                        EndKind::Removed => t!("room-el-anfitrion-te-ha-expulsado-de"),
                        EndKind::RoomClosed => t!("room-el-anfitrion-ha-cerrado-la-sala-2"),
                        EndKind::Network => t!("room-se-perdio-la-conexion-con-la"),
                        EndKind::Other => t!("room-la-sesion-ha-terminado"),
                    };
                    app.dialog = Some(Dialog::SessionEnded {
                        title: title.to_owned(),
                        text: end.text,
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
        SessionEvent::Connected { path, remote } => {
            state.path = Some(path);
            state.remote = Some(remote);
        }
        SessionEvent::Candidates { relay_reason, .. } => state.relay_reason = relay_reason,
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
                detail.trim_start_matches(t!("room-decodifica"))
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
            if path == Path::Direct {
                state.relay_reason = None;
            }
            app.toast(Tone::Success, t!("room-conexion-recuperada"));
        }
        SessionEvent::RelayUnavailable(message) => {
            app.toast(
                Tone::Warning,
                t!("room-sin-relay-message", message = message),
            );
        }
        SessionEvent::Warning(message) => app.toast(Tone::Warning, message),
        SessionEvent::Audio(issue) => app.toast(Tone::Warning, settings::audio_issue_text(&issue)),
        SessionEvent::HandoffOffered => {
            let host = state
                .host_name
                .clone()
                .unwrap_or_else(|| t!("room-el-anfitrion").to_owned());
            app.dialog = Some(Dialog::HandoffOffered { host });
        }
        SessionEvent::HandoffCancelled => {
            // El anfitrión retiró la oferta: fuera el diálogo y, si ya se abría la sala propia,
            // se cierra; se sigue de invitado en la suya.
            let mut effects = Vec::new();
            let mut changed = matches!(app.dialog, Some(Dialog::HandoffOffered { .. }));
            if changed {
                app.dialog = None;
            }
            let host = &mut app.host;
            match std::mem::take(&mut host.takeover) {
                Takeover::None => {}
                // Aún abriéndose: se cierra en cuanto abra.
                Takeover::Opening if host.room.is_none() => {
                    host.takeover = Takeover::Withdrawn;
                    changed = true;
                }
                Takeover::Withdrawn => host.takeover = Takeover::Withdrawn,
                Takeover::Opening | Takeover::Replied(_) => {
                    changed = true;
                    // Se cierra sin el aviso de «Has dejado de compartir» (`HostEnded`).
                    host.takeover = Takeover::Withdrawn;
                    if !host.stopping {
                        host.stopping = true;
                        effects.push(Effect::StopHost);
                    }
                }
            }
            if changed {
                app.toast(Tone::Info, t!("room-el-anfitrion-retiro-la-sala"));
            }
            return effects;
        }
        SessionEvent::MoveToRoom { code, host_name } => {
            if app.host.takeover == Takeover::Replied(code.clone()) {
                // Es la sala propia: el anfitrión confirma que recibió el código; ya se puede
                // salir de la suya (los demás vienen solos).
                app.host.takeover = Takeover::None;
                return leave(app);
            }
            if state.moving_to.is_none() && !state.leaving {
                // El silencio propio de esta sala sigue en la nueva.
                app.carried_mute = state.stats.as_ref().map(|stats| stats.mic_muted);
                state.moving_to = Some(code);
                app.toast(
                    Tone::Info,
                    t!("room-la-sala-pasa-a", name = host_name.as_str()),
                );
                return leave(app);
            }
        }
        SessionEvent::Sharing(active) => {
            let changed = state.host_sharing.is_some_and(|before| before != active);
            state.host_sharing = Some(active);
            let now = app.now;
            state.tick(now);
            if changed {
                app.toast(
                    Tone::Info,
                    if active {
                        t!("room-el-anfitrion-comparte")
                    } else {
                        t!("room-el-anfitrion-dejo-de-compartir")
                    },
                );
            }
        }
        SessionEvent::Members(members) => {
            // Quien ya no está deja de contar como silenciado ni conserva su volumen, también en
            // el audio (su hoja puede volver a usarla otra persona).
            let mut effects = Vec::new();
            let present = |leaf: u32| members.iter().any(|member| member.leaf == leaf);
            for leaf in state.muted.iter().copied().filter(|leaf| !present(*leaf)) {
                effects.push(Effect::SetVoiceMuted {
                    host: false,
                    source: leaf,
                    muted: false,
                });
            }
            for leaf in state.volumes.keys().copied().filter(|leaf| !present(*leaf)) {
                effects.push(Effect::SetVoiceGain {
                    host: false,
                    source: leaf,
                    gain: voice_gain(DEFAULT_VOICE_VOLUME),
                });
            }
            state.muted.retain(|leaf| present(*leaf));
            state.volumes.retain(|leaf, _| present(*leaf));
            // A quien está se le aplica lo recordado por su nombre: el silencio (que se guarda al
            // tocarlo) siempre, y el volumen al llegar (una vez: lo que se toque después manda).
            // Nunca a uno mismo.
            for member in members.iter().filter(|member| !member.you) {
                let muted = app.settings.audio.voice_muted(&member.name);
                if muted != state.muted.contains(&member.leaf) {
                    if muted {
                        state.muted.insert(member.leaf);
                    } else {
                        state.muted.remove(&member.leaf);
                    }
                    effects.push(Effect::SetVoiceMuted {
                        host: false,
                        source: member.leaf,
                        muted,
                    });
                }
            }
            for member in members.iter().filter(|member| !member.you) {
                if state.volumes.contains_key(&member.leaf) {
                    continue;
                }
                let percent = app.settings.audio.voice_volume(&member.name);
                if percent != DEFAULT_VOICE_VOLUME {
                    state.volumes.insert(member.leaf, percent);
                    effects.push(Effect::SetVoiceGain {
                        host: false,
                        source: member.leaf,
                        gain: voice_gain(percent),
                    });
                }
            }
            state.members = members;
            return effects;
        }
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

// --- Sala de espera -------------------------------------------------------------------------------

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
            ui::caption(t!("room-calidad-esperada-con-este-anfitrion")),
            row![
                text(format!("{:.0}", estimate.rtt_ms))
                    .size(type_size::DISPLAY)
                    .font(coroom_ui::font::SEMIBOLD),
                column![
                    ui::secondary(t!("room-ms-de-rtt")),
                    ui::status_badge(tone, estimate.level.label())
                ]
                .spacing(4)
            ]
            .spacing(space::S)
            .align_y(Alignment::Center),
            ui::body(capitalize(estimate.level.description())).wrapping(Wrapping::WordOrGlyph),
            ui::caption(t!(
                "room-sumando-el-pipeline-tipico-25-ms",
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

/// Título de la sala: «Sala de Bea» (o «Sala» hasta saber de quién es).
fn room_title(state: &State) -> String {
    state.host_name.as_ref().map_or_else(
        || t!("room-sala").to_owned(),
        |name| t!("room-sala-de-name", name = name),
    )
}

/// Cabecera de la sala de espera: volver (cancela la entrada), la sala y el chat.
fn waiting_header<'a>(app: &'a App, state: &'a State) -> Element<'a, Message> {
    let keys = &app.settings.input.shortcuts;
    let chat_open = state.panel == Some(Tab::Chat);
    let chat_button = ui::with_tooltip(
        button(center(icon_with(
            Icon::Chat,
            size::ICON,
            if chat_open {
                SvgClass::OnAccent
            } else {
                SvgClass::Text
            },
        )))
        .width(Length::Fixed(size::CONTROL))
        .height(Length::Fixed(size::CONTROL))
        .padding(0)
        .class(if chat_open {
            ButtonClass::Primary
        } else {
            ButtonClass::Flat
        })
        .on_press(on(Msg::TogglePanel(Tab::Chat))),
        if app.chat.ready {
            t!("room-chat", key = keys.chat)
        } else {
            t!("room-chat-con-las-claves").to_owned()
        },
    );
    container(
        row![
            ui::icon_button(Icon::ArrowLeft, t!("room-cancelar"), Some(on(Msg::Cancel))),
            ui::heading(room_title(state)),
            ui::fill(),
            chat::with_unread(chat_button, app.chat.unread, size::CONTROL),
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center),
    )
    .height(Length::Fixed(HEADERBAR_HEIGHT))
    .padding(Padding::from([0.0, space::S]))
    .center_y(Length::Fixed(HEADERBAR_HEIGHT))
    .class(ContainerClass::Headerbar)
    .into()
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
        t!("room-senalizacion"),
        state
            .host_name
            .as_ref()
            .map(|name| t!("room-dentro-de-la-sala-de-name", name = name)),
    ));
    steps = steps.push(step(
        keys,
        t!("room-claves-de-la-sala"),
        Some(t!("room-cifrado-de-extremo-a-extremo-con").to_owned()),
    ));
    steps = steps.push(step(
        link,
        t!("room-conexion-con-el-anfitrion"),
        state.path.map(|path| {
            let text = match path {
                Path::Direct => t!("room-directa-entre-los-dos-equipos"),
                Path::Relayed => t!("room-por-el-relay-del-servidor"),
            };
            match state.remote {
                Some(remote) => format!("{text} · {remote}"),
                None => text.to_owned(),
            }
        }),
    ));
    steps = steps.push(step(
        quality,
        t!("room-calidad-de-la-conexion"),
        state
            .estimate
            .map(|e| format!("{} · {}", stats::ms(e.rtt_ms), e.level.label())),
    ));
    steps = steps.push(step(video, t!("room-video"), state.codec.clone()));
    let mut content = Column::new()
        .spacing(space::M)
        .push(ui::heading(t!(
            "room-entrando-en-la-sala",
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
    let mut actions = row![ui::flat(t!("room-cancelar"), Some(on(Msg::Cancel)))].spacing(space::XS);
    if state.feed.has_frame() {
        actions = actions
            .push(ui::fill())
            .push(ui::primary(t!("room-ver-ya"), Some(on(Msg::Continue))));
    }
    cards = cards.push(actions);
    let body: Element<'a, Message> = scrollable(
        container(cards)
            .padding(Padding::from([space::XL, space::L]))
            .center_x(Length::Fill),
    )
    .height(Length::Fill)
    .width(Length::Fill)
    .into();
    let mut main = row![body].height(Length::Fill);
    if state.panel == Some(Tab::Chat) {
        main = main.push(
            container(chat::panel(app, Surface::Window))
                .width(Length::Fixed(size::STATS_PANEL))
                .height(Length::Fill)
                .padding(Padding {
                    top: space::S,
                    right: space::S,
                    bottom: space::S,
                    left: 0.0,
                }),
        );
    }
    let page = column![waiting_header(app, state), main].height(Length::Fill);
    match chat::bubbles(app) {
        Some(bubbles) => stack![
            page,
            container(bubbles)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Horizontal::Right)
                .align_y(Vertical::Bottom)
                .padding(space::S)
        ]
        .into(),
        None => page.into(),
    }
}

fn reconnect_banner<'a>(app: &'a App, reconnecting: &'a Reconnecting) -> Element<'a, Message> {
    let wait = reconnecting.at.saturating_duration_since(app.now);
    let text = if wait.is_zero() {
        t!(
            "room-se-perdio-la-conexion-reconectando-intento",
            attempt = reconnecting.attempt,
            of = reconnecting.of
        )
    } else {
        t!(
            "room-se-perdio-la-conexion-reconectando-en",
            valor = format!("{:.0}", wait.as_secs_f64().ceil()),
            attempt = reconnecting.attempt,
            of = reconnecting.of
        )
    };
    ui::banner(
        Tone::Warning,
        text,
        Some((t!("room-reintentar-ya"), on(Msg::Reconnect))),
    )
}

// --- Sobre el vídeo -------------------------------------------------------------------------------

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

/// Colores del modo oscuro (el panel y la barra flotan siempre oscuros sobre el vídeo); la
/// aplicación los tiene calculados con el tema.
fn dark(app: &App) -> &Palette {
    &app.dark
}

/// Insignia del nivel con la latencia añadida («Buena · 48 ms»), o «Midiendo».
fn level_badge<'a>(state: &State, dark: &Palette) -> Element<'a, Message> {
    let (tone, label) = match (state.level(), state.added_ms()) {
        _ if state.reconnecting.is_some() => (Tone::Neutral, t!("room-reconectando").to_owned()),
        (Some(level), Some(added)) => (
            quality_tone(level),
            t!(
                "room-nivel-ms",
                level = level.label(),
                value = stats::number(added, 0)
            ),
        ),
        _ => (Tone::Neutral, t!("room-midiendo-corto").to_owned()),
    };
    // Sobre la barra (siempre oscura), con los colores de la paleta oscura en los dos modos.
    container(
        row![
            ui::dot(tone),
            text(label)
                .size(type_size::CAPTION)
                .font(coroom_ui::font::SEMIBOLD)
                .class(TextClass::Custom(dark.tone(tone).text.color()))
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([3.0, 10.0]))
    .class(ContainerClass::OverlayBadge(tone))
    .into()
}

/// «Directa · AV1 · 1440p60 · 18,4 Mbit/s».
fn bar_facts(state: &State) -> String {
    let mut facts = Vec::new();
    if let Some(path) = state.path {
        facts.push(route_label(path).to_owned());
    }
    if let Some(codec) = state.codec.as_ref().and_then(|c| c.split(' ').next()) {
        facts.push(codec.to_owned());
    }
    let summary = state.stats.as_ref().map(|stats| &stats.summary);
    if let Some((_, height)) = state.feed.frame_size() {
        let fps = summary.map_or(0.0, |summary| summary.fps_recent);
        facts.push(if fps >= 1.0 {
            format!("{height}p{:.0}", fps)
        } else {
            format!("{height}p")
        });
    }
    if let Some(summary) = summary {
        facts.push(stats::kbps(summary.bitrate_recent_kbps));
    }
    facts.join(" · ")
}

fn top_bar<'a>(app: &'a App, state: &'a State) -> Element<'a, Message> {
    let left = row![
        level_badge(state, dark(app)),
        column![
            text(room_title(state))
                .size(type_size::BODY)
                .font(coroom_ui::font::SEMIBOLD)
                .class(TextClass::Overlay)
                .wrapping(Wrapping::None),
            overlay_text(bar_facts(state))
                .size(type_size::CAPTION)
                .wrapping(Wrapping::None)
        ]
        .spacing(1)
    ]
    .spacing(space::S)
    .align_y(Alignment::Center);
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
                t!("room-activar-el-microfono", key = keys.mic)
            } else {
                t!("room-silenciar-el-microfono", key = keys.mic)
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
            t!("room-activar-el-audio-del-juego", key = keys.game)
        } else {
            t!("room-silenciar-el-audio-del-juego", key = keys.game)
        },
        game_muted,
        on(Msg::ToggleGame),
    ));
    if allowed.keyboard_mouse {
        buttons = buttons.push(bar_button(
            Icon::Pointer,
            t!("room-modo-inmersivo", key = keys.immersive),
            state.immersive,
            on(Msg::ToggleImmersive),
        ));
    }
    buttons = buttons.push(chat::with_unread(
        bar_button(
            Icon::Chat,
            t!("room-chat", key = keys.chat),
            state.panel == Some(Tab::Chat),
            on(Msg::TogglePanel(Tab::Chat)),
        ),
        app.chat.unread,
        size::CONTROL,
    ));
    buttons = buttons.push(bar_button(
        Icon::Activity,
        t!("room-conexion", key = keys.stats),
        state.panel == Some(Tab::Connection),
        on(Msg::TogglePanel(Tab::Connection)),
    ));
    buttons = buttons.push(bar_button(
        if app.fullscreen {
            Icon::Minimize
        } else {
            Icon::Maximize
        },
        t!("room-pantalla-completa-f11"),
        app.fullscreen,
        on(Msg::ToggleFullscreen),
    ));
    buttons = buttons.push(ui::gap(space::XS));
    buttons = buttons.push(
        button(
            row![
                icon_with(Icon::LogOut, 16.0, SvgClass::OnDestructive),
                text(t!("room-salir"))
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
    .padding(Padding::from([0.0, space::S]))
    .height(Length::Fixed(size::VIDEO_BAR))
    .center_y(Length::Fixed(size::VIDEO_BAR))
    .width(Length::Fill)
    .class(ContainerClass::Overlay)
    .into()
}

/// La píldora de latencia: un clic abre el panel en «Conexión»; el tramo «Relay · motivo», el
/// diagnóstico.
/// Lo que se ve en lugar del vídeo cuando el anfitrión no comparte nada.
fn nothing_shared_view(state: &State) -> Element<'_, Message> {
    let host = state
        .host_name
        .as_deref()
        .map_or_else(|| t!("room-el-anfitrion").to_owned(), str::to_owned);
    center(
        column![
            icon_with(Icon::Monitor, 48.0, SvgClass::Overlay),
            text(t!("room-nadie-esta-compartiendo"))
                .size(type_size::TITLE)
                .font(coroom_ui::font::SEMIBOLD)
                .class(TextClass::Overlay),
            overlay_text(t!(
                "room-nadie-esta-compartiendo-detalle",
                host = host.as_str()
            ))
            .wrapping(Wrapping::WordOrGlyph)
            .align_x(Horizontal::Center),
        ]
        .spacing(space::S)
        .align_x(Alignment::Center)
        .max_width(420.0),
    )
    .into()
}

/// Los demás que hablan ahora, por sus nombres (uno mismo no: ya lo sabe).
pub fn talking_names(state: &State) -> Vec<&str> {
    let Some(audio) = state
        .stats
        .as_ref()
        .and_then(|stats| stats.summary.audio.as_ref())
    else {
        return Vec::new();
    };
    let mut members: Vec<&RoomMember> = state.members.iter().filter(|m| !m.you).collect();
    members.sort_by_key(|member| member_order(member));
    members
        .into_iter()
        .filter(|member| audio.speaking.contains(&talker_label(member.guest)))
        .map(|member| member.name.as_str())
        .collect()
}

/// Indicador de quién habla: el micrófono y los nombres, en una píldora opaca sobre el vídeo.
fn talking_view(state: &State) -> Option<Element<'_, Message>> {
    let names = talking_names(state);
    if names.is_empty() {
        return None;
    }
    let content = row![
        icon_with(Icon::Mic, 16.0, SvgClass::Overlay),
        text(names.join(", "))
            .size(type_size::SECONDARY)
            .font(coroom_ui::font::SEMIBOLD)
            .class(TextClass::Overlay)
            .wrapping(Wrapping::None),
    ]
    .spacing(space::XS)
    .align_y(Alignment::Center);
    Some(
        container(content)
            .padding(Padding::from([6.0, 12.0]))
            .class(ContainerClass::OverlayPill)
            .into(),
    )
}

fn pill_view<'a>(app: &'a App, state: &'a State) -> Element<'a, Message> {
    let pill = pill(state, app.now);
    let sep = || {
        text("·")
            .size(type_size::SECONDARY)
            .class(TextClass::OverlaySecondary)
    };
    let strong = |content: String| {
        text(content)
            .size(type_size::SECONDARY)
            .font(coroom_ui::font::SEMIBOLD)
            .class(TextClass::Overlay)
            .wrapping(Wrapping::None)
    };
    let dot: Element<'a, Message> = match pill.tone {
        Some(tone) => ui::dot(tone),
        None => ui::dot(Tone::Neutral),
    };
    let mut lead = row![dot].spacing(space::XS).align_y(Alignment::Center);
    if pill.reconnecting {
        lead = lead.push(icon_with(Icon::Refresh, 14.0, SvgClass::Overlay));
    }
    lead = lead.push(strong(pill.lead.clone()));
    let reason = pill.reason.clone().map(|reason| {
        text(reason)
            .size(type_size::SECONDARY)
            .class(TextClass::OverlaySecondary)
            .wrapping(Wrapping::None)
    });
    let segment = |content: Element<'a, Message>, message: Message| {
        button(content)
            .padding(Padding::from([4.0, 8.0]))
            .class(ButtonClass::Overlay { active: false })
            .on_press(message)
    };
    let connection = on(Msg::TogglePanel(Tab::Connection));
    let content: Element<'a, Message> = if pill.relayed && !pill.reconnecting {
        // «48 ms» abre el panel; «Relay · CGNAT», el diagnóstico.
        let mut relay = row![strong(pill.route.clone().unwrap_or_default())]
            .spacing(space::XS)
            .align_y(Alignment::Center);
        if let Some(reason) = reason {
            relay = relay.push(sep()).push(reason);
        }
        row![
            segment(lead.into(), connection),
            sep(),
            ui::with_tooltip(
                segment(relay.into(), Message::Diagnostics(diagnostics::Msg::Open)),
                t!("room-por-que-relay")
            ),
        ]
        .spacing(2)
        .align_y(Alignment::Center)
        .into()
    } else {
        let mut all = lead;
        if let Some(route) = pill.route.clone() {
            all = all.push(sep()).push(strong(route));
        }
        if let Some(reason) = reason {
            all = all.push(sep()).push(reason);
        }
        segment(all.into(), connection).into()
    };
    container(content)
        .padding(2)
        .height(Length::Fixed(36.0))
        .center_y(Length::Fixed(36.0))
        .class(ContainerClass::OverlayPill)
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
    .align_y(Alignment::Center)
    .into()
}

/// Título de sección del panel, en versalitas, con algo a la derecha si hace falta.
fn section_title<'a>(title: &str, right: Option<Element<'a, Message>>) -> Element<'a, Message> {
    let mut line = row![
        text(title.to_uppercase())
            .size(type_size::CAPTION - 1.0)
            .font(coroom_ui::font::SEMIBOLD)
            .class(TextClass::OverlaySecondary)
            .width(Length::Fill)
    ]
    .align_y(Alignment::Center);
    if let Some(right) = right {
        line = line.push(right);
    }
    line.into()
}

fn stat_section<'a>(title: &str, rows: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    let mut content = column![section_title(title, None)].spacing(6);
    for row in rows {
        content = content.push(row);
    }
    content.into()
}

/// Fila con un interruptor.
fn switch_row<'a>(
    label: impl text::IntoFragment<'a>,
    value: bool,
    on_toggle: Option<Message>,
) -> Element<'a, Message> {
    row![
        overlay_text(label)
            .wrapping(Wrapping::WordOrGlyph)
            .width(Length::Fill),
        ui::switch(value, on_toggle.map(|message| move |_| message.clone())),
    ]
    .spacing(space::S)
    .align_y(Alignment::Center)
    .into()
}

/// Una opción del panel de mandos.
fn pad_option<'a>(
    selected: bool,
    name: String,
    detail: Option<String>,
    badges: Vec<String>,
    choice: PadChoice,
) -> Element<'a, Message> {
    let mark: Element<'a, Message> = if selected {
        icon_with(Icon::Check, 16.0, SvgClass::Overlay)
    } else {
        Space::new().width(Length::Fixed(16.0)).into()
    };
    let mut texts = column![
        text(name)
            .size(type_size::BODY)
            .font(coroom_ui::font::SEMIBOLD)
            .class(TextClass::Overlay)
            .wrapping(Wrapping::WordOrGlyph)
    ]
    .spacing(1);
    if let Some(detail) = detail {
        texts = texts.push(overlay_text(detail));
    }
    if !badges.is_empty() {
        let mut chips = row![].spacing(space::XXS);
        for badge in badges {
            chips = chips.push(ui::status_badge(Tone::Neutral, badge));
        }
        texts = texts.push(chips);
    }
    button(
        row![mark, texts.width(Length::Fill)]
            .spacing(space::XS)
            .align_y(Alignment::Center),
    )
    .padding(Padding::from([6.0, 8.0]))
    .width(Length::Fill)
    .class(ButtonClass::Overlay { active: selected })
    .on_press(on(Msg::ChoosePad(choice)))
    .into()
}

/// Botón de silenciar lo que regula un deslizador, delante de su raíl: altavoz (suena) o altavoz
/// tachado (silenciado), con «Silenciar» / «Activar sonido».
fn mute_button<'a>(muted: bool, message: Message) -> Element<'a, Message> {
    bar_button(
        if muted { Icon::VolumeOff } else { Icon::Volume },
        if muted {
            t!("room-activar-sonido")
        } else {
            t!("room-silenciar")
        },
        muted,
        message,
    )
}

/// Si `member` habla ahora («Hablando» en «Integrantes»). El anfitrión no devuelve a cada uno su
/// propio `VoiceState`: la fila propia mira el audio de este equipo.
fn member_talks(state: &State, member: &RoomMember) -> bool {
    let Some(stats) = state.stats.as_ref() else {
        return false;
    };
    if member.you {
        return stats.speaking;
    }
    let label = talker_label(member.guest);
    stats
        .summary
        .audio
        .as_ref()
        .is_some_and(|audio| audio.speaking.contains(&label))
}

/// Orden de «Integrantes»: tú, el anfitrión y los demás por orden de llegada.
fn member_order(member: &RoomMember) -> u8 {
    if member.you {
        0
    } else if member.guest == 0 {
        1
    } else {
        2
    }
}

/// «Integrantes»: quién está en la sala, quién habla y cómo se le oye. En las filas de los demás,
/// el deslizador es lo alto que se le oye en este equipo y el altavoz deja de oírle aquí (los
/// demás le siguen oyendo); en la propia, lo alto que te oyen y el altavoz te silencia para todos.
fn people_section<'a>(app: &'a App, state: &'a State) -> Element<'a, Message> {
    let mut members: Vec<&RoomMember> = state.members.iter().collect();
    members.sort_by_key(|member| member_order(member));
    let mut rows: Vec<Element<'_, Message>> = Vec::new();
    for member in members {
        let talks = member_talks(state, member);
        let role = if member.guest == 0 {
            t!("room-anfitrion")
        } else {
            t!("room-invitado")
        };
        let mut facts = vec![role.to_owned()];
        if talks {
            facts.push(t!("room-hablando").to_owned());
        }
        rows.push(
            column![
                text(if member.you {
                    t!("room-tu-nombre", name = member.name.as_str())
                } else {
                    member.name.clone()
                })
                .size(type_size::BODY)
                .class(TextClass::Overlay),
                overlay_text(facts.join(" · ")).size(type_size::CAPTION),
            ]
            .spacing(2)
            .width(Length::Fill)
            .into(),
        );
        if member.you {
            rows.push(own_volume_row(app, state));
        } else {
            rows.push(volume_row(state, member));
        }
    }
    let mut content = Column::new()
        .spacing(space::S)
        .push(stat_section(t!("room-integrantes"), rows));
    if state.members.is_empty() {
        content = content.push(overlay_text(t!("room-esperando-la-lista")));
    }
    content = content.push(
        overlay_text(t!("room-solo-en-tu-equipo"))
            .size(type_size::CAPTION)
            .wrapping(Wrapping::WordOrGlyph),
    );
    content.into()
}

/// La voz propia: lo alto que te oyen los demás (de 0 a 200 % de 5 en 5; Ctrl+clic, 100 %), con
/// el altavoz que te silencia para todos delante. Sin voz (sin micrófono o sin permiso), por qué.
fn own_volume_row<'a>(app: &App, state: &State) -> Element<'a, Message> {
    let stats = state.stats.as_ref();
    let voice = stats.is_some_and(|s| s.voice || s.mic_muted);
    if !voice {
        return overlay_text(t!("room-sin-voz"))
            .size(type_size::CAPTION)
            .wrapping(Wrapping::WordOrGlyph)
            .into();
    }
    let muted = stats.is_some_and(|s| s.mic_muted);
    let percent = app.settings.audio.mic_volume;
    ui::slider_with(
        0..=MAX_VOICE_VOLUME,
        percent,
        5,
        DEFAULT_VOICE_VOLUME,
        Some(bar_button(
            if muted { Icon::VolumeOff } else { Icon::Volume },
            if muted {
                t!("room-activar-tu-microfono")
            } else {
                t!("room-silenciarte")
            },
            muted,
            on(Msg::ToggleMic),
        )),
        t!("room-volumen-porcentaje", percent = percent),
        SliderClass::Overlay.muted(muted),
        |value| on(Msg::MicVolume(value)),
        Some(on(Msg::MicVolumeDone)),
    )
}

/// Volumen de la voz de `member` en este equipo, bajo su nombre: de 0 a 200 % de 5 en 5
/// (Ctrl+clic, 100 %), con el botón de dejar de oírle delante (tachado y con la barra apagada si
/// no se le oye). Mientras se arrastra ya suena así; al soltar se recuerda por su nombre.
fn volume_row<'a>(state: &State, member: &RoomMember) -> Element<'a, Message> {
    let leaf = member.leaf;
    let percent = state.volume(leaf);
    let muted = state.muted.contains(&leaf);
    ui::slider_with(
        0..=MAX_VOICE_VOLUME,
        percent,
        5,
        DEFAULT_VOICE_VOLUME,
        Some(mute_button(muted, on(Msg::Hear(leaf, muted)))),
        t!("room-volumen-porcentaje", percent = percent),
        SliderClass::Overlay.muted(muted),
        move |value| on(Msg::Volume(leaf, value)),
        Some(on(Msg::VolumeDone(leaf))),
    )
}

/// Mandos: cuál se envía (automático o uno concreto) entre los conectados.
fn pads_section<'a>(app: &'a App, state: &'a State) -> Element<'a, Message> {
    let choice = &app.settings.input.pad;
    let slot = state.stats.as_ref().and_then(|s| s.allowed.gamepad_slot);
    let pads = state.pads.as_deref().unwrap_or_default();
    let mut options = vec![pad_option(
        *choice == PadChoice::Auto,
        t!("room-automatico").to_owned(),
        Some(t!("room-el-ultimo-que-conectes").to_owned()),
        Vec::new(),
        PadChoice::Auto,
    )];
    for pad in pads {
        let detail = if pad.active {
            match slot {
                Some(slot) => t!("room-enviando-ranura", slot = slot),
                None => t!("room-sin-ranura").to_owned(),
            }
        } else {
            t!("room-conectado").to_owned()
        };
        let transport = match &pad.kind {
            PadKind::Hid => "HID",
            PadKind::XInput => "XInput",
            PadKind::Converted(_) => t!("room-solo-convertido"),
        };
        let mut badges = vec![format!(
            "{:04x}:{:04x} · {transport}",
            pad.key.vendor, pad.key.product
        )];
        if pad.active && slot.is_some() {
            badges.push(if state.pads_raw {
                t!("room-tal-cual").to_owned()
            } else {
                t!("room-convertido").to_owned()
            });
        }
        options.push(pad_option(
            *choice == PadChoice::Pad(pad.key.clone()),
            pad.key.name.clone(),
            Some(detail),
            badges,
            PadChoice::Pad(pad.key.clone()),
        ));
    }
    // El elegido, aunque ahora no esté conectado.
    if let PadChoice::Pad(key) = choice
        && !pads.iter().any(|pad| &pad.key == key)
    {
        options.push(pad_option(
            true,
            key.name.clone(),
            Some(t!("room-desconectado").to_owned()),
            vec![format!("{:04x}:{:04x}", key.vendor, key.product)],
            choice.clone(),
        ));
    }
    let mut content = Column::new()
        .spacing(space::S)
        .push(stat_section(t!("room-mando-que-envias"), options));
    if pads.is_empty() {
        content = content.push(overlay_text(t!("room-no-hay-mandos-conectados")));
    }
    content =
        content.push(overlay_text(t!("room-se-recuerda-la-eleccion")).size(type_size::CAPTION));
    content.into()
}

/// Volumen del audio del juego en este equipo, encima de las personas: de 0 a 200 % de 5 en 5
/// (Ctrl+clic, 100 %), con el botón de silenciarlo delante (el mismo que el de la barra y
/// `Ctrl+Alt+V`; va aparte del volumen y manda). Mientras se arrastra ya suena así; al soltar se
/// recuerda en los ajustes.
fn game_volume_section(state: &State) -> Element<'_, Message> {
    let percent = state.game_volume;
    let muted = state.stats.as_ref().is_some_and(|s| s.game_muted);
    stat_section(
        t!("room-volumen-del-juego"),
        vec![ui::slider_with(
            0..=MAX_VOICE_VOLUME,
            percent,
            5,
            DEFAULT_VOICE_VOLUME,
            Some(mute_button(muted, on(Msg::ToggleGame))),
            t!("room-volumen-porcentaje", percent = percent),
            SliderClass::Overlay,
            |value| on(Msg::GameVolume(value)),
            Some(on(Msg::GameVolumeDone)),
        )],
    )
}

/// Pestaña «Sala»: los integrantes (a quién se oye y cómo).
fn room_tab<'a>(app: &'a App, state: &'a State) -> Element<'a, Message> {
    scrollable(
        column![people_section(app, state)]
            .spacing(space::L)
            .padding(Padding::from([space::S, space::M])),
    )
    .height(Length::Fill)
    .into()
}

/// Mediana en ms de una latencia en µs, o «midiendo».
fn median(samples: usize, p50_us: u64) -> String {
    if samples > 0 {
        format!("{} ms", stats::number(p50_us as f64 / 1000.0, 0))
    } else {
        t!("room-midiendo").to_owned()
    }
}

/// Pestaña «Conexión»: latencia añadida por partes, enlace, calidad y lo de este equipo.
fn connection_tab<'a>(app: &'a App, state: &'a State) -> Element<'a, Message> {
    let dark = dark(app);
    let summary = state.stats.as_ref().map(|stats| &stats.summary);
    let mut sections = Column::new()
        .spacing(space::M)
        .padding(Padding::from([space::S, space::M]));

    // Latencia añadida: la del vídeo de extremo a extremo, con su nivel; y por partes.
    let headline: Element<'a, Message> = match (state.added_ms(), state.level()) {
        (Some(added), Some(level)) => text(t!(
            "room-ms-nivel",
            value = stats::number(added, 0),
            level = level.label()
        ))
        .size(type_size::SECONDARY)
        .font(coroom_ui::font::MONO)
        .class(TextClass::Overlay)
        .into(),
        _ => overlay_text(t!("room-midiendo")).into(),
    };
    let input = summary.and_then(|summary| summary.input).map_or_else(
        || t!("room-midiendo").to_owned(),
        |l| median(l.samples, l.p50_us),
    );
    let video = summary.map_or_else(
        || t!("room-midiendo").to_owned(),
        |summary| median(summary.total.samples, summary.total.p50_us),
    );
    // El audio del juego de captura a que suena, si la sesión lo mide; si no, «—».
    let audio = summary
        .and_then(|summary| summary.audio.as_ref())
        .and_then(|audio| audio.track(AudioTrack::Game))
        .filter(|track| track.total.samples > 0)
        .map_or_else(
            || "—".to_owned(),
            |track| median(track.total.samples, track.total.p50_us),
        );
    sections = sections.push(
        column![
            section_title(t!("room-latencia-anadida"), Some(headline)),
            stat_row(t!("room-entrada-raton-juego"), input),
            stat_row(t!("room-video-captura-pantalla"), video),
            stat_row(t!("room-audio"), audio),
        ]
        .spacing(6),
    );

    // Enlace: la ruta con su motivo, RTT y pérdida, fps y bitrate.
    let mut link_rows = Vec::new();
    if let Some(path) = state.path {
        let (label, tone) = match path {
            Path::Direct => (route_label(path).to_owned(), Tone::Success),
            Path::Relayed => (
                match &state.relay_reason {
                    Some(reason) => {
                        format!("{} · {}", route_label(path), short_relay_reason(reason))
                    }
                    None => route_label(path).to_owned(),
                },
                Tone::Warning,
            ),
        };
        link_rows.push(
            row![
                overlay_text(t!("room-ruta")).width(Length::Fill),
                text(label)
                    .size(type_size::SECONDARY)
                    .font(coroom_ui::font::SEMIBOLD)
                    .class(TextClass::Custom(dark.tone(tone).text.color()))
            ]
            .align_y(Alignment::Center)
            .into(),
        );
        if path == Path::Relayed
            && let Some(reason) = &state.relay_reason
        {
            link_rows.push(
                overlay_text(capitalize(reason))
                    .size(type_size::CAPTION)
                    .wrapping(Wrapping::WordOrGlyph)
                    .into(),
            );
        }
    }
    if let Some(summary) = summary {
        if let Some(link) = &summary.link {
            link_rows.push(stat_row(
                t!("room-rtt-perdida"),
                format!(
                    "{} ms · {}",
                    stats::number(link.rtt_ms, 0),
                    stats::percent(link.loss_percent)
                ),
            ));
        }
        link_rows.push(stat_row(
            t!("room-fps-bitrate"),
            format!(
                "{} · {}",
                stats::number(summary.fps_recent, 0),
                stats::kbps(summary.bitrate_recent_kbps)
            ),
        ));
    } else {
        link_rows.push(overlay_text(t!("room-midiendo")).into());
    }
    if state.safety_number.is_some() {
        link_rows.push(
            row![
                overlay_text(t!("room-numero-de-seguridad")).width(Length::Fill),
                button(
                    text(t!("room-comprobar"))
                        .size(type_size::SECONDARY)
                        .font(coroom_ui::font::SEMIBOLD)
                        .class(TextClass::Custom(dark.accent_text.color()))
                )
                .padding(Padding::from([2.0, 6.0]))
                .class(ButtonClass::Overlay { active: false })
                .on_press(on(Msg::Safety)),
            ]
            .align_y(Alignment::Center)
            .into(),
        );
    }
    link_rows.push(
        row![
            overlay_text(t!("room-mas-detalle-en")).size(type_size::CAPTION),
            button(
                text(t!("room-diagnostico"))
                    .size(type_size::CAPTION)
                    .font(coroom_ui::font::SEMIBOLD)
                    .class(TextClass::Custom(dark.accent_text.color()))
            )
            .padding(Padding::from([2.0, 4.0]))
            .class(ButtonClass::Overlay { active: false })
            .on_press(Message::Diagnostics(diagnostics::Msg::Open)),
        ]
        .spacing(2)
        .align_y(Alignment::Center)
        .into(),
    );
    sections = sections.push(stat_section(t!("room-enlace"), link_rows));

    // Calidad: la elige el anfitrión (el invitado la ve, no la cambia).
    let segment = |label: &'static str| {
        container(
            text(label)
                .size(type_size::CAPTION)
                .font(coroom_ui::font::SEMIBOLD)
                .class(TextClass::OverlaySecondary)
                .wrapping(Wrapping::None),
        )
        .padding(Padding::from([5.0, 10.0]))
    };
    let target = summary
        .and_then(|summary| summary.link.as_ref())
        .map(|link| link.target_kbps)
        .filter(|kbps| *kbps > 0);
    sections = sections.push(stat_section(
        t!("room-calidad"),
        vec![
            container(
                row![
                    segment(t!("room-min-latencia")),
                    segment(t!("room-equilibrado")),
                    segment(t!("room-max-calidad")),
                ]
                .spacing(2),
            )
            .padding(2)
            .class(ContainerClass::OverlaySegmented)
            .into(),
            overlay_text(match target {
                Some(kbps) => t!(
                    "room-la-elige-el-anfitrion-objetivo",
                    value = stats::kbps(f64::from(kbps))
                ),
                None => t!("room-la-elige-el-anfitrion").to_owned(),
            })
            .size(type_size::CAPTION)
            .wrapping(Wrapping::WordOrGlyph)
            .into(),
        ],
    ));

    scrollable(sections).height(Length::Fill).into()
}

/// Pestaña «Ajustes»: lo de este equipo en la sala (vídeo, decodificador, audio con sus
/// dispositivos y volumen del juego, mando y atajos). Lo que cambia se guarda en los ajustes.
fn settings_tab<'a>(app: &'a App, state: &'a State) -> Element<'a, Message> {
    let mut sections = Column::new()
        .spacing(space::M)
        .padding(Padding::from([space::S, space::M]));
    // Tu vídeo: escalado (solo «Ajustar» por ahora) y VSync.
    sections = sections.push(stat_section(
        t!("room-tu-video"),
        vec![
            row![
                overlay_text(t!("room-escalado")).width(Length::Fill),
                container(
                    row![
                        text(t!("room-ajustar"))
                            .size(type_size::SECONDARY)
                            .class(TextClass::OverlaySecondary),
                        icon_with(Icon::ChevronDown, 12.0, SvgClass::Overlay),
                    ]
                    .spacing(space::XS)
                    .align_y(Alignment::Center),
                )
                .padding(Padding::from([5.0, 10.0]))
                .class(ContainerClass::OverlaySegmented),
            ]
            .align_y(Alignment::Center)
            .into(),
            overlay_text(t!("room-escalado-nota"))
                .size(type_size::CAPTION)
                .wrapping(Wrapping::WordOrGlyph)
                .into(),
            switch_row(
                t!("room-vsync"),
                app.settings.video.vsync,
                Some(on(Msg::SetVsync(!app.settings.video.vsync))),
            ),
            overlay_text(t!("room-vsync-nota"))
                .size(type_size::CAPTION)
                .wrapping(Wrapping::WordOrGlyph)
                .into(),
            switch_row(
                t!("room-decodificar-por-software"),
                app.settings.video.decoder == DecoderChoice::Software,
                Some(on(Msg::SoftwareDecoder(
                    app.settings.video.decoder != DecoderChoice::Software,
                ))),
            ),
            overlay_text(t!("room-decodificar-nota"))
                .size(type_size::CAPTION)
                .wrapping(Wrapping::WordOrGlyph)
                .into(),
        ],
    ));

    // Tu audio: micrófono (con su tecla si es pulsar para hablar) y audio del juego.
    let stats_now = state.stats.as_ref();
    let voice = stats_now.is_some_and(|s| s.voice);
    let mic_label = if app.settings.audio.voice_mode == VoiceMode::PushToTalk {
        t!(
            "room-microfono-ptt",
            key = app.settings.audio.push_to_talk_key.as_str()
        )
    } else {
        t!("room-microfono").to_owned()
    };
    let mut audio_rows = vec![switch_row(
        mic_label,
        voice && !stats_now.is_some_and(|s| s.mic_muted),
        voice.then(|| on(Msg::ToggleMic)),
    )];
    if !voice && let Some(stats_now) = stats_now {
        audio_rows.push(
            overlay_text(capitalize(&stats_now.audio_status))
                .size(type_size::CAPTION)
                .wrapping(Wrapping::WordOrGlyph)
                .into(),
        );
    }
    audio_rows.push(switch_row(
        t!("room-audio-del-juego"),
        !stats_now.is_some_and(|s| s.game_muted),
        Some(on(Msg::ToggleGame)),
    ));
    // El micrófono y la salida, que cambian al momento (los mismos de Ajustes → Audio y voz).
    let devices: Vec<AudioDevice> = app
        .settings_page
        .audio_devices
        .loaded()
        .cloned()
        .unwrap_or_default();
    // Los nombres largos, recortados con «…» para que quepan en el selector del panel.
    let fit = |choices: Vec<settings::Choice<Option<String>>>| -> Vec<_> {
        choices
            .into_iter()
            .map(|mut choice| {
                choice.label = ui::ellipsize(&choice.label, DEVICE_LABEL).into_owned();
                choice
            })
            .collect()
    };
    let mics = fit(settings::device_choices(
        &devices,
        true,
        &app.settings.audio.microphone,
    ));
    let mic = mics
        .iter()
        .find(|c| c.value == app.settings.audio.microphone)
        .cloned();
    let speakers = fit(settings::device_choices(
        &devices,
        false,
        &app.settings.audio.speaker,
    ));
    let speaker = speakers
        .iter()
        .find(|c| c.value == app.settings.audio.speaker)
        .cloned();
    audio_rows.push(device_row(
        t!("room-dispositivo-microfono"),
        ui::select_fixed(
            mics,
            mic,
            |c| Message::Settings(settings::Msg::Microphone(c)),
            DEVICE_SELECT,
        ),
    ));
    audio_rows.push(device_row(
        t!("room-dispositivo-salida"),
        ui::select_fixed(
            speakers,
            speaker,
            |c| Message::Settings(settings::Msg::Speaker(c)),
            DEVICE_SELECT,
        ),
    ));
    audio_rows.push(
        overlay_text(t!("room-dispositivos-nota"))
            .size(type_size::CAPTION)
            .wrapping(Wrapping::WordOrGlyph)
            .into(),
    );
    sections = sections.push(stat_section(t!("room-tu-audio"), audio_rows));
    sections = sections.push(game_volume_section(state));
    sections = sections.push(pads_section(app, state));
    sections = sections.push(
        overlay_text(shortcuts_line(&app.settings.input.shortcuts))
            .size(type_size::CAPTION)
            .wrapping(Wrapping::WordOrGlyph),
    );
    scrollable(sections).height(Length::Fill).into()
}

/// Caracteres que caben en un selector de dispositivo del panel.
const DEVICE_LABEL: usize = 38;

/// Ancho de los selectores de dispositivo del panel: todo el panel menos sus márgenes.
const DEVICE_SELECT: f32 = size::STATS_PANEL - 2.0 * space::M;

/// Selector de dispositivo del panel, bajo su nombre (los nombres de los dispositivos son
/// largos).
fn device_row<'a>(label: &'a str, select: Element<'a, Message>) -> Element<'a, Message> {
    column![overlay_text(label), select]
        .spacing(space::XS)
        .into()
}

/// Los atajos del vídeo, con las letras de Ajustes → Entrada.
fn shortcuts_line(shortcuts: &crate::ui::settings::Shortcuts) -> String {
    t!(
        "room-atajos-linea",
        immersive = shortcuts.immersive,
        stats = shortcuts.stats,
        chat = shortcuts.chat,
        mic = shortcuts.mic,
        game = shortcuts.game,
        pads = shortcuts.pads,
        leave = shortcuts.leave
    )
}

/// El panel lateral: pestañas Chat · Conexión · Sala, el atajo de la pestaña y cerrar.
fn side_panel<'a>(app: &'a App, state: &'a State, tab: Tab) -> Element<'a, Message> {
    let keys = &app.settings.input.shortcuts;
    // Cuatro pestañas no dejan sitio al atajo en la cabecera: va en la ayuda de cada una.
    let shortcut = |this: Tab| match this {
        Tab::Chat => Some(keys.chat),
        Tab::Connection => Some(keys.stats),
        Tab::Room => None,
        Tab::Settings => Some(keys.pads),
    };
    let tab_button = |label: &'static str, this: Tab| -> Element<'a, Message> {
        let control = button(
            text(label)
                .size(type_size::SECONDARY)
                .font(coroom_ui::font::SEMIBOLD)
                .wrapping(Wrapping::None),
        )
        .padding(Padding::from([5.0, 10.0]))
        .class(ButtonClass::Overlay {
            active: tab == this,
        })
        .on_press(on(Msg::ShowTab(this)));
        match shortcut(this) {
            Some(letter) => ui::with_tooltip(control, format!("Ctrl+Alt+{letter}")),
            None => control.into(),
        }
    };
    let tabs = container(
        row![
            tab_button(t!("room-tab-chat"), Tab::Chat),
            tab_button(t!("room-tab-conexion"), Tab::Connection),
            tab_button(t!("room-tab-sala"), Tab::Room),
            tab_button(t!("room-tab-ajustes"), Tab::Settings),
        ]
        .spacing(2),
    )
    .padding(2)
    .class(ContainerClass::OverlaySegmented);
    let header = container(
        row![
            tabs,
            ui::fill(),
            ui::with_tooltip(
                button(center(icon_with(Icon::Close, 16.0, SvgClass::Overlay)))
                    .width(Length::Fixed(28.0))
                    .height(Length::Fixed(28.0))
                    .padding(0)
                    .class(ButtonClass::Overlay { active: false })
                    .on_press(on(Msg::ClosePanel)),
                t!("room-cerrar-el-panel"),
            ),
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center),
    )
    .padding(Padding {
        top: space::XS,
        right: space::XS,
        bottom: space::XS,
        left: space::S,
    });
    let content: Element<'a, Message> = match tab {
        Tab::Chat => chat::panel(app, Surface::Overlay),
        Tab::Connection => connection_tab(app, state),
        Tab::Room => room_tab(app, state),
        Tab::Settings => settings_tab(app, state),
    };
    container(column![header, content].height(Length::Fill))
        .width(Length::Fixed(size::STATS_PANEL))
        .height(Length::Fill)
        .class(ContainerClass::Overlay)
        .into()
}

fn video<'a>(app: &'a App, state: &'a State) -> Element<'a, Message> {
    // Con compositor el vídeo va debajo de la interfaz: la base queda transparente; sin él (las
    // capturas) el fondo negro de las bandas lo pone el contenedor.
    let base = if state.nothing_shared() {
        // Ni el último fotograma: el anfitrión dejó de compartir.
        container(nothing_shared_view(state))
            .width(Length::Fill)
            .height(Length::Fill)
            .class(ContainerClass::Video)
    } else {
        container(video_view(&state.feed))
            .width(Length::Fill)
            .height(Length::Fill)
            .class(if state.feed.is_composited() {
                ContainerClass::Transparent
            } else {
                ContainerClass::Video
            })
    };
    let mut layers: Vec<Element<'a, Message>> = vec![base.into()];
    let now = app.now;
    let bar_visible = state.bar_visible(now);
    // La barra entra bajando un poco (200 ms, `ease-out`).
    let entered = (now.duration_since(state.bar_shown_at).as_secs_f32() / BAR_IN.as_secs_f32())
        .clamp(0.0, 1.0);
    let slide = (1.0 - (1.0 - entered) * (1.0 - entered)) * space::S;
    let mut top = Column::new().spacing(space::S).padding(Padding {
        top: if bar_visible { slide } else { space::S },
        right: space::S,
        bottom: 0.0,
        left: space::S,
    });
    if bar_visible {
        top = top.push(top_bar(app, state));
    }
    if let Some(reconnecting) = &state.reconnecting {
        top = top.push(
            container(reconnect_banner(app, reconnecting))
                .center_x(Length::Fill)
                .max_width(640),
        );
    }
    layers.push(
        container(top)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_y(Vertical::Top)
            .into(),
    );
    // Quién habla, arriba a la izquierda (debajo de la barra si se ve).
    if app.settings.room.talking_overlay
        && let Some(talking) = talking_view(state)
    {
        let below_bar = if bar_visible {
            space::S + size::VIDEO_BAR + space::S
        } else {
            space::S
        };
        layers.push(
            container(talking)
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(Padding {
                    top: below_bar,
                    right: space::S,
                    bottom: space::S,
                    left: space::S,
                })
                .into(),
        );
    }
    if let Some(tab) = state.panel {
        let below_bar = space::S + size::VIDEO_BAR + space::S;
        layers.push(
            container(side_panel(app, state, tab))
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Horizontal::Right)
                .padding(Padding {
                    top: below_bar,
                    right: space::S,
                    bottom: space::S,
                    left: space::S,
                })
                .into(),
        );
    }
    if state.pill_visible(now, app.fullscreen) {
        layers.push(
            container(pill_view(app, state))
                .width(Length::Fill)
                .height(Length::Fill)
                .align_y(Vertical::Bottom)
                .padding(space::S)
                .into(),
        );
    }
    if state.panel.is_none()
        && let Some(bubbles) = chat::bubbles(app)
    {
        layers.push(
            container(bubbles)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Horizontal::Right)
                .align_y(Vertical::Bottom)
                .padding(space::S)
                .into(),
        );
    }
    stack(layers).into()
}

/// Escenas del invitado para las capturas.
pub(crate) mod shots {
    use std::time::{Duration, Instant};

    use coroom_session::input::gamepad::{ConnectedPad, PadChoice, PadKey, PadKind};
    use coroom_session::latency::{AudioSummary, InputLatency, LatencyLedger, LinkSummary, Stage};
    use coroom_session::remote::{QualityLevel, RoomMember, SessionEvent};
    use coroom_transport::quic::Path;

    use super::{Msg, Phase, Stats, Tab};
    use crate::ui::app::{App, AudioDevice, Event, Message, Page};
    use crate::ui::screens::chat;
    use crate::ui::shots::{Builder, cursor, cursor_image, game_frame, safety, signed_in};
    use crate::ui::video::VideoFeed;

    /// Entra en la sala de Bea (con la estimación y el primer fotograma si se piden).
    pub(crate) fn joined(app: &mut App, estimate: bool, frame: bool) {
        signed_in(app, Page::Home);
        app.update(Message::Home(crate::ui::screens::home::Msg::Code(
            "K7Q2MZ".to_owned(),
        )));
        app.update(Message::Home(crate::ui::screens::home::Msg::Join));
        let feed = VideoFeed::detached();
        if frame {
            feed.push(
                game_frame(2, 1280, 720),
                Some(cursor()),
                Some(cursor_image()),
            );
        }
        app.update(Message::Event(Event::GuestStarted(Ok(feed))));
        app.update(Message::Event(Event::Guest(SessionEvent::Joined {
            host_name: "Bea".to_owned(),
            host_fingerprint: "b7".repeat(32),
            safety_number: safety(),
        })));
        app.update(Message::Event(Event::Guest(SessionEvent::EpochChanged {
            epoch: 1,
        })));
        app.update(Message::Event(Event::Guest(SessionEvent::Connected {
            path: Path::Direct,
            remote: "192.0.2.10:4433".parse().expect("dirección"),
        })));
        if estimate {
            app.update(Message::Event(Event::Guest(
                SessionEvent::QualityEstimate {
                    rtt_ms: 18.4,
                    level: QualityLevel::from_rtt_ms(18.4),
                },
            )));
        }
        app.toasts.clear();
    }

    /// Esperando: las claves ya están (el chat funciona), aún sin conexión con el anfitrión, y el
    /// chat abierto con la conversación de ejemplo.
    pub(crate) fn guest_connecting(app: &mut App) {
        joined(app, false, false);
        if let Some(state) = app.guest.as_mut() {
            state.path = None;
        }
        chat::shots::conversation(app);
        app.update(Message::Guest(Msg::TogglePanel(Tab::Chat)));
        app.chat.composing = false;
    }

    pub(crate) fn guest_estimate(app: &mut App) {
        joined(app, true, false);
    }

    /// Hasta el vídeo, con la barra visible.
    pub(crate) fn video(app: &mut App) {
        joined(app, true, true);
        app.update(Message::Guest(Msg::Continue));
        app.update(Message::Event(Event::Guest(SessionEvent::VideoCodec {
            codec: coroom_session::codec::Codec::Av1,
            detail: "decodifica Vulkan Video".to_owned(),
        })));
        app.update(Message::Event(Event::GuestStats(Box::new(sample_stats()))));
        app.update(Message::PointerMoved);
        app.toasts.clear();
        if let Some(state) = app.guest.as_mut() {
            state.phase = Phase::Video;
            state.bar_until = app.now + Duration::from_secs(3);
            // La barra ya entró.
            state.bar_shown_at = app.now - Duration::from_secs(1);
        }
    }

    /// El ratón lleva quieto más de 2,5 s.
    fn still(app: &mut App) {
        if let Some(state) = app.guest.as_mut() {
            state.bar_until = app.now - Duration::from_millis(1);
        }
    }

    pub(crate) fn guest_video(app: &mut App) {
        video(app);
    }

    pub(crate) fn guest_video_still(app: &mut App) {
        video(app);
        still(app);
    }

    pub(crate) fn guest_fullscreen_still(app: &mut App) {
        video(app);
        app.update(Message::FullscreenChanged(true));
        still(app);
    }

    pub(crate) fn guest_connection(app: &mut App) {
        video(app);
        app.update(Message::Guest(Msg::ToggleStats));
    }

    pub(crate) fn guest_chat(app: &mut App) {
        video(app);
        chat::shots::conversation(app);
        app.update(Message::Guest(Msg::TogglePanel(Tab::Chat)));
        app.chat.composing = false;
    }

    pub(crate) fn guest_bubbles(app: &mut App) {
        video(app);
        chat::shots::conversation(app);
        let now = app.now;
        for (guest, name, text) in [
            (2, "Carla", "yo sin micro hoy, os escribo por aquí 🙃"),
            (
                0,
                "Bea",
                "Listo, entrad. Guía de la build: coroom.example.com/guia",
            ),
        ] {
            app.update(Message::Event(Event::Guest(SessionEvent::ChatMessage(
                chat::shots::line(guest, name, text, 4, false),
            ))));
        }
        for bubble in &mut app.chat.bubbles {
            bubble.at = now;
        }
    }

    pub(crate) fn sample_pad(
        vendor: u16,
        product: u16,
        name: &str,
        kind: PadKind,
        active: bool,
    ) -> ConnectedPad {
        ConnectedPad {
            key: PadKey {
                vendor,
                product,
                name: name.to_owned(),
            },
            kind,
            active,
        }
    }

    /// Pestaña «Sala»: personas (a una no se la oye) y mandos.
    pub(crate) fn guest_room(app: &mut App) {
        video(app);
        if let Some(stats) = app.guest.as_mut().and_then(|state| state.stats.as_mut()) {
            stats.allowed.gamepad_slot = Some(2);
        }
        let member = |guest: u32, leaf: u32, name: &str, you: bool| RoomMember {
            guest,
            leaf,
            name: name.to_owned(),
            you,
        };
        app.update(Message::Event(Event::Guest(SessionEvent::Members(vec![
            member(0, 0, "Bea", false),
            member(1, 1, "Ana", true),
            member(2, 2, "Carla", false),
        ]))));
        app.update(Message::Guest(Msg::Hear(2, false)));
        app.update(Message::Guest(Msg::Volume(0, 150)));
        app.update(Message::Guest(Msg::GameVolume(70)));
        let eightbitdo = sample_pad(
            0x2dc8,
            0x6012,
            "8BitDo Ultimate 2 Wireless Controller",
            PadKind::Hid,
            true,
        );
        let xbox = sample_pad(
            0x045e,
            0x0b12,
            "Xbox Wireless Controller",
            PadKind::XInput,
            false,
        );
        app.settings.input.pad = PadChoice::Pad(eightbitdo.key.clone());
        app.update(Message::Guest(Msg::Pads(vec![eightbitdo.clone()], true)));
        app.update(Message::Guest(Msg::Pads(vec![eightbitdo, xbox], true)));
        speaking(app, &["anfitrión"]);
        if let Some(stats) = app.guest.as_mut().and_then(|state| state.stats.as_mut()) {
            stats.voice = true;
        }
        app.update(Message::Guest(Msg::MicVolume(120)));
        app.update(Message::Guest(Msg::ShowTab(Tab::Room)));
        app.toasts.clear();
    }

    /// Quién habla según el resumen del audio («anfitrión», «invitado 2»…).
    fn speaking(app: &mut App, who: &[&str]) {
        if let Some(stats) = app.guest.as_mut().and_then(|state| state.stats.as_mut()) {
            let audio = stats.summary.audio.get_or_insert_with(|| AudioSummary {
                tracks: Vec::new(),
                av: None,
                speaking: Vec::new(),
                status: String::new(),
            });
            audio.speaking = who.iter().map(|name| (*name).to_owned()).collect();
        }
    }

    /// El indicador de quién habla sobre el vídeo (con la barra escondida).
    pub(crate) fn guest_talking(app: &mut App) {
        guest_room(app);
        speaking(app, &["anfitrión", "invitado 2"]);
        app.update(Message::Guest(Msg::ClosePanel));
        still(app);
    }

    /// La pestaña «Ajustes» del panel: vídeo, decodificador, audio con sus dispositivos, volumen
    /// del juego y mando.
    pub(crate) fn guest_settings(app: &mut App) {
        guest_room(app);
        app.update(Message::Event(Event::AudioDevices(Ok(vec![
            AudioDevice {
                name: "usb-mic".to_owned(),
                description: "Micrófono USB".to_owned(),
                input: true,
                default: true,
            },
            AudioDevice {
                name: "headset".to_owned(),
                description: "Auriculares".to_owned(),
                input: false,
                default: true,
            },
        ]))));
        app.update(Message::Guest(Msg::ShowTab(Tab::Settings)));
        app.toasts.clear();
    }

    pub(crate) fn guest_reconnecting(app: &mut App) {
        video(app);
        app.update(Message::Event(Event::Guest(SessionEvent::Reconnecting {
            attempt: 2,
            of: 5,
            retry_in: Duration::from_secs(3),
            reason: "la red dejó de responder".to_owned(),
        })));
        app.toasts.clear();
    }

    /// Por el relay detrás de CGNAT, Aceptable: la píldora en ámbar con el motivo.
    pub(crate) fn guest_relay_cgnat(app: &mut App) {
        video(app);
        app.update(Message::Event(Event::Guest(SessionEvent::Candidates {
            guest: None,
            candidates: Vec::new(),
            relay_reason: Some(
                "ninguna dirección respondió: la NAT de uno de los dos no abre (CGNAT)".to_owned(),
            ),
        })));
        app.update(Message::Event(Event::Guest(SessionEvent::Reconnected {
            path: Path::Relayed,
        })));
        let mut stats = sample_stats();
        stats.summary.total.p50_us = 82_000;
        if let Some(link) = stats.summary.link.as_mut() {
            link.path = "relay".to_owned();
            link.rtt_ms = 61.0;
        }
        app.update(Message::Event(Event::GuestStats(Box::new(stats))));
        still(app);
        app.toasts.clear();
    }

    /// Estadísticas de ejemplo: un registro de latencia con 120 fotogramas plausibles y el enlace.
    pub(crate) fn sample_stats() -> Stats {
        let ledger = LatencyLedger::new();
        ledger.set_video("AV1 1920×1080 a 60 fps (Vulkan Video)");
        let start = Instant::now();
        // Microsegundos por etapa (captura → codificado → … → presentado), con algo de variación.
        let steps = [2_600u64, 180, 40, 6_900, 60, 1_700, 3_100];
        for frame in 1..=120u64 {
            let jitter = (frame * 37) % 900;
            let mut at = start + Duration::from_micros(frame * 16_667);
            ledger.mark(frame, Stage::Captured, at);
            for (stage, step) in Stage::ALL[1..].iter().zip(steps) {
                let extra = if *stage == Stage::Received { jitter } else { 0 };
                at += Duration::from_micros(step + extra);
                ledger.mark(frame, *stage, at);
            }
            ledger.record_encoded_bytes_at(42_000, at);
        }
        let mut summary = ledger.summary();
        summary.fps = 60.0;
        summary.bitrate_kbps = 18_400.0;
        summary.fps_recent = 60.0;
        summary.bitrate_recent_kbps = 18_400.0;
        // Lo que se ve en el boceto: 48 ms añadidos (Buena).
        summary.total.p50_us = 48_000;
        summary.link = Some(LinkSummary {
            path: "directo".to_owned(),
            remote: Some("192.0.2.10:4433".to_owned()),
            rtt_ms: 13.8,
            target_kbps: 20_000,
            loss_percent: 0.1,
            groups_dropped: 2,
            groups_recovered: 41,
            epoch: 1,
            safety_number: safety().to_string(),
            peer_fingerprint: "b7".repeat(32),
            peer_name: "Bea".to_owned(),
            sframe: true,
        });
        summary.input = Some(InputLatency {
            samples: 840,
            p50_us: 9_200,
            p95_us: 14_100,
            p99_us: 18_000,
            max_us: 22_500,
            applied: 840,
            dropped: 0,
            queue_p50_us: 300,
            network_p50_us: 8_100,
            inject_p50_us: 800,
            repaired: 2,
        });
        Stats {
            summary,
            allowed: coroom_session::input::capture::Allowed {
                keyboard_mouse: true,
                gamepad_slot: None,
            },
            mic_muted: false,
            game_muted: false,
            voice: true,
            speaking: false,
            audio_status: "micrófono activo".to_owned(),
            window_copies: 0,
        }
    }

    /// Escenas de esta pantalla.
    pub(crate) const BUILDERS: &[(&str, Builder)] = &[
        ("11-conectando", guest_connecting),
        ("12-estimacion", guest_estimate),
        ("13-video", guest_video),
        ("13-video-quieto", guest_video_still),
        ("13-video-pantalla-completa-quieto", guest_fullscreen_still),
        ("14-conexion", guest_connection),
        ("14-chat", guest_chat),
        ("14-burbujas", guest_bubbles),
        ("13-video-hablando", guest_talking),
        ("13-nadie-comparte", |app| {
            joined(app, true, false);
            app.update(Message::Event(Event::Guest(SessionEvent::Sharing(false))));
            app.update(Message::Tick);
        }),
        ("14-sala", guest_room),
        ("14-ajustes", guest_settings),
        ("15-reconectando", guest_reconnecting),
        ("15-relay-cgnat", guest_relay_cgnat),
    ];
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
    use crate::ui::app::GuestEnd;
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

    /// Un invitado ya con vídeo y estadísticas de ejemplo.
    fn watching() -> App {
        let mut app = app();
        start(&mut app, "K7Q2MZ".into());
        let feed = VideoFeed::detached();
        feed.push(frame(), None, None);
        app.update(Message::Event(Event::GuestStarted(Ok(feed))));
        app.update(Message::Event(Event::Guest(SessionEvent::Connected {
            path: Path::Direct,
            remote: "192.0.2.10:4433".parse().expect("dirección"),
        })));
        app.update(Message::Guest(Msg::Continue));
        app.update(Message::Event(Event::GuestStats(Box::new(
            shots::sample_stats(),
        ))));
        app
    }

    fn set_added(app: &mut App, added_ms: u64, loss: f64, rtt: f64) {
        let mut stats = shots::sample_stats();
        stats.summary.total.p50_us = added_ms * 1000;
        if let Some(link) = stats.summary.link.as_mut() {
            link.loss_percent = loss;
            link.rtt_ms = rtt;
        }
        app.update(Message::Event(Event::GuestStats(Box::new(stats))));
    }

    fn pill_label(app: &App) -> (String, Option<Tone>) {
        let state = app.guest.as_ref().expect("invitado");
        let pill = pill(state, app.now);
        (pill.label(), pill.tone)
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
            remote: "192.0.2.10:4433".parse().expect("dirección"),
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
        assert!(state.animating(), "y entra con su animación");
        app.now += motion::BAR_HIDE;
        app.update(Message::Tick);
        assert!(
            !app.guest.as_ref().is_some_and(|g| g.bar_visible(app.now)),
            "y se oculta sola"
        );
        assert_eq!(
            app.guest.as_ref().and_then(State::next_tick),
            None,
            "oculta, no hace falta despertar (nada de bucles)"
        );
        app.update(Message::PointerMoved);
        assert!(app.guest.as_ref().is_some_and(|g| g.bar_visible(app.now)));
    }

    #[test]
    fn the_pill_follows_route_level_and_reconnection() {
        let mut app = app();
        start(&mut app, "K7Q2MZ".into());
        assert_eq!(pill_label(&app), ("Conectando…".to_owned(), None));
        let mut app = watching();
        assert_eq!(
            pill_label(&app),
            ("48 ms · Directa".to_owned(), Some(Tone::Success))
        );
        // Por el relay detrás de CGNAT, Aceptable.
        app.update(Message::Event(Event::Guest(SessionEvent::Candidates {
            guest: None,
            candidates: Vec::new(),
            relay_reason: Some(
                "ninguna dirección respondió: la NAT de uno de los dos no abre (CGNAT)".into(),
            ),
        })));
        app.update(Message::Event(Event::Guest(SessionEvent::Reconnected {
            path: Path::Relayed,
        })));
        set_added(&mut app, 82, 0.2, 40.0);
        assert_eq!(
            pill_label(&app),
            ("82 ms · Relay · CGNAT".to_owned(), Some(Tone::Warning))
        );
        // Mala: el motivo pasa a ser lo que más duele.
        set_added(&mut app, 131, 4.0, 60.0);
        assert_eq!(
            pill_label(&app),
            ("131 ms · Relay · pérdida 4 %".to_owned(), Some(Tone::Error))
        );
        set_added(&mut app, 131, 0.2, 95.0);
        assert_eq!(pill_label(&app).0, "131 ms · Relay · RTT 95 ms");
        // Reconectando: gris, con la espera.
        app.update(Message::Event(Event::Guest(SessionEvent::Reconnecting {
            attempt: 1,
            of: 5,
            retry_in: Duration::from_millis(2_400),
            reason: "red".into(),
        })));
        assert_eq!(pill_label(&app), ("Reconectando · 3 s".to_owned(), None));
        // Sin muestras de latencia: «Midiendo», nunca vacía.
        let mut app = watching();
        let mut stats = shots::sample_stats();
        stats.summary.total.samples = 0;
        app.update(Message::Event(Event::GuestStats(Box::new(stats))));
        assert_eq!(pill_label(&app).0, "Midiendo · Directa");
    }

    #[test]
    fn the_pill_hides_with_the_bar_only_in_fullscreen() {
        let mut app = watching();
        app.now += motion::BAR_HIDE + Duration::from_millis(1);
        let state = app.guest.as_ref().expect("invitado");
        assert!(!state.bar_visible(app.now));
        assert!(state.pill_visible(app.now, false), "en ventana, fija");
        assert!(
            !state.pill_visible(app.now, true),
            "a pantalla completa, se va"
        );
        app.update(Message::PointerMoved);
        let state = app.guest.as_ref().expect("invitado");
        assert!(
            state.pill_visible(app.now, true),
            "y vuelve al mover el ratón"
        );
    }

    /// El anfitrión pasa la sala a este invitado: acepta, abre la suya solo con voz y, con el
    /// código, contesta; sale de la del anfitrión hacia Compartir cuando este lo confirma.
    #[test]
    fn accepting_the_room_opens_a_voice_room_and_answers_with_its_code() {
        let mut app = watching();
        app.account = Some(crate::ui::app::tests::account());
        app.update(Message::Event(Event::Guest(SessionEvent::Joined {
            host_name: "Bea".to_owned(),
            host_fingerprint: "b7".repeat(32),
            safety_number: crate::ui::shots::safety(),
        })));
        app.update(Message::Event(Event::Guest(SessionEvent::HandoffOffered)));
        assert!(matches!(&app.dialog, Some(Dialog::HandoffOffered { host }) if host == "Bea"));
        let effects = app.update(Message::ConfirmDialog);
        let [Effect::StartHost(request)] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert_eq!(
            request.source,
            crate::ui::screens::host::SourceChoice::Nothing
        );
        assert_eq!(request.audio, coroom_session::audio::GameCapture::None);
        app.update(Message::Event(Event::HostStarted(
            Ok(VideoFeed::detached()),
        )));
        let effects = app.update(Message::Event(Event::Host(SessionEvent::RoomCreated {
            code: "NEWRM2".into(),
            fingerprint: "ab".repeat(32),
        })));
        assert!(
            effects.contains(&Effect::ReplyHandoff(Some("NEWRM2".into()))),
            "{effects:?}"
        );
        // Sin salir aún: si se cortase ahora, la respuesta podría no llegar.
        assert!(!effects.contains(&Effect::Leave), "{effects:?}");
        // El anfitrión manda a todos a la sala nueva, a este también: es la confirmación.
        let effects = app.update(Message::Event(Event::Guest(SessionEvent::MoveToRoom {
            code: "NEWRM2".into(),
            host_name: "Ana".into(),
        })));
        assert!(effects.contains(&Effect::Leave), "{effects:?}");
        assert!(!effects.iter().any(|e| matches!(e, Effect::Join(_))));
        let effects = app.update(Message::Event(Event::GuestEnded(None)));
        assert!(!effects.contains(&Effect::StartLobby), "{effects:?}");
        assert!(!effects.iter().any(|e| matches!(e, Effect::Join(_))));
        assert_eq!((app.screen, app.page), (Screen::Main, Page::Host));
        assert!(app.dialog.is_none());
    }

    /// Ya contestado, si la sala del anfitrión se cierra antes de confirmar (se fue sin más),
    /// no hay diálogo de «ha cerrado la sala»: se queda en la propia.
    #[test]
    fn after_answering_the_old_room_closing_is_expected() {
        let mut app = watching();
        app.account = Some(crate::ui::app::tests::account());
        app.update(Message::Event(Event::Guest(SessionEvent::HandoffOffered)));
        app.update(Message::ConfirmDialog);
        app.update(Message::Event(Event::HostStarted(
            Ok(VideoFeed::detached()),
        )));
        app.update(Message::Event(Event::Host(SessionEvent::RoomCreated {
            code: "NEWRM2".into(),
            fingerprint: "ab".repeat(32),
        })));
        app.update(Message::Event(Event::GuestEnded(None)));
        assert!(app.dialog.is_none(), "{:?}", app.dialog);
        assert_eq!((app.screen, app.page), (Screen::Main, Page::Host));
        assert!(app.host.room.is_some());
    }

    /// Cerrar la oferta sin elegir (Esc, clic fuera) es no aceptarla: el anfitrión lo sabe.
    #[test]
    fn closing_the_offer_declines_it() {
        let mut app = watching();
        app.update(Message::Event(Event::Guest(SessionEvent::HandoffOffered)));
        assert_eq!(
            app.update(Message::CloseDialog),
            [Effect::ReplyHandoff(None)]
        );
        assert!(app.dialog.is_none());
        // Cerrar otro diálogo no contesta nada.
        app.update(Message::Guest(Msg::Leave));
        assert!(app.update(Message::CloseDialog).is_empty());
    }

    /// El anfitrión retira la oferta: el diálogo se va; si ya se abría la sala propia, se cierra
    /// (al momento si está abierta, al abrirse si aún no) y se sigue de invitado.
    #[test]
    fn a_withdrawn_offer_closes_the_dialog_and_the_new_room() {
        let mut app = watching();
        app.account = Some(crate::ui::app::tests::account());
        app.update(Message::Event(Event::Guest(SessionEvent::HandoffOffered)));
        let effects = app.update(Message::Event(Event::Guest(SessionEvent::HandoffCancelled)));
        assert!(effects.is_empty(), "{effects:?}");
        assert!(app.dialog.is_none());
        assert!(app.toasts.iter().any(|t| t.text.contains("ya no te pasa")));

        // Aceptada y abriéndose: se cierra en cuanto abra.
        app.update(Message::Event(Event::Guest(SessionEvent::HandoffOffered)));
        app.update(Message::ConfirmDialog);
        assert!(
            app.update(Message::Event(Event::Guest(SessionEvent::HandoffCancelled)))
                .is_empty()
        );
        let effects = app.update(Message::Event(Event::HostStarted(
            Ok(VideoFeed::detached()),
        )));
        assert!(effects.contains(&Effect::StopHost), "{effects:?}");
        assert!(
            !app.update(Message::Event(Event::Host(SessionEvent::RoomCreated {
                code: "NEWRM2".into(),
                fingerprint: "ab".repeat(32),
            })))
            .iter()
            .any(|e| matches!(e, Effect::ReplyHandoff(_)))
        );
        app.update(Message::Event(Event::HostEnded(None)));
        assert!(app.guest.is_some(), "sigue de invitado");

        // Ya contestado: se cierra la sala propia y no se sale de la del anfitrión.
        app.update(Message::Event(Event::Guest(SessionEvent::HandoffOffered)));
        app.update(Message::ConfirmDialog);
        app.update(Message::Event(Event::HostStarted(
            Ok(VideoFeed::detached()),
        )));
        app.update(Message::Event(Event::Host(SessionEvent::RoomCreated {
            code: "NEWRM3".into(),
            fingerprint: "ab".repeat(32),
        })));
        let effects = app.update(Message::Event(Event::Guest(SessionEvent::HandoffCancelled)));
        assert_eq!(effects, [Effect::StopHost]);
        assert!(app.guest.as_ref().is_some_and(|guest| !guest.leaving));
        // Al cerrarse, ni «Has dejado de compartir» ni sala de espera: se sigue de invitado.
        app.toasts.clear();
        let effects = app.update(Message::Event(Event::HostEnded(None)));
        assert!(effects.is_empty(), "{effects:?}");
        assert!(app.toasts.is_empty(), "{:?}", app.toasts);
        assert_eq!(app.host.takeover, Takeover::None);
        assert!(app.guest.is_some());
    }

    /// Quien estaba callado sigue callado en la sala nueva (cada sesión empieza con el micrófono
    /// abierto).
    #[test]
    fn a_muted_guest_stays_muted_in_the_new_room() {
        let mut app = watching();
        let mut stats = shots::sample_stats();
        stats.mic_muted = true;
        app.update(Message::Event(Event::GuestStats(Box::new(stats))));
        app.update(Message::Event(Event::Guest(SessionEvent::MoveToRoom {
            code: "NEWRM2".into(),
            host_name: "Carla".into(),
        })));
        app.update(Message::Event(Event::GuestEnded(None)));
        let effects = app.update(Message::Event(Event::GuestStarted(Ok(
            VideoFeed::detached(),
        ))));
        assert!(
            effects.contains(&Effect::SetMicMuted {
                host: false,
                muted: true
            }),
            "{effects:?}"
        );
        // Solo una vez: entrar después en otra sala empieza como siempre.
        app.update(Message::Guest(Msg::Leave));
        app.update(Message::ConfirmDialog);
        app.update(Message::Event(Event::GuestEnded(None)));
        start(&mut app, "OTRA22".into());
        let effects = app.update(Message::Event(Event::GuestStarted(Ok(
            VideoFeed::detached(),
        ))));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::SetMicMuted { .. })),
            "{effects:?}"
        );
    }

    /// No aceptar contesta que no y cierra el diálogo.
    #[test]
    fn declining_the_room_answers_no() {
        let mut app = watching();
        app.update(Message::Event(Event::Guest(SessionEvent::HandoffOffered)));
        assert_eq!(
            app.update(Message::Guest(Msg::DeclineHandoff)),
            [Effect::ReplyHandoff(None)]
        );
        assert!(app.dialog.is_none());
    }

    /// La sala pasa a otro: se sale de esta y se entra en la suya sin pasar por Inicio.
    #[test]
    fn moving_to_the_new_host_joins_its_room() {
        let mut app = watching();
        let effects = app.update(Message::Event(Event::Guest(SessionEvent::MoveToRoom {
            code: "NEWRM2".into(),
            host_name: "Carla".into(),
        })));
        assert!(effects.contains(&Effect::Leave), "{effects:?}");
        let effects = app.update(Message::Event(Event::GuestEnded(None)));
        assert_eq!(effects, [Effect::StopLobby, Effect::Join("NEWRM2".into())]);
        assert_eq!(app.guest.as_ref().map(|g| g.code.as_str()), Some("NEWRM2"));
        assert_eq!(app.screen, Screen::Guest);
    }

    #[test]
    fn panel_tabs() {
        let mut app = watching();
        assert!(app.update(Message::Guest(Msg::ToggleStats)).is_empty());
        assert_eq!(
            app.guest.as_ref().and_then(|g| g.panel),
            Some(Tab::Connection)
        );
        assert!(!app.chat.open);
        // La pestaña del chat: lo abre y pone el cursor en el campo cuando hay claves.
        app.update(Message::Event(Event::Guest(SessionEvent::EpochChanged {
            epoch: 1,
        })));
        assert_eq!(
            app.update(Message::Guest(Msg::ShowTab(Tab::Chat))),
            [Effect::FocusChat(true)]
        );
        assert!(app.chat.open && app.chat.composing);
        // Otra pestaña: el teclado vuelve al juego. «Ajustes» (`Ctrl+Alt+J`, la del mando)
        // pide la lista de micrófonos y salidas.
        assert_eq!(
            app.update(Message::Guest(Msg::TogglePads)),
            [Effect::FocusChat(false), Effect::LoadAudioDevices]
        );
        assert_eq!(
            app.guest.as_ref().and_then(|g| g.panel),
            Some(Tab::Settings)
        );
        assert!(!app.chat.open && !app.chat.composing);
        app.update(Message::Guest(Msg::TogglePads));
        assert_eq!(app.guest.as_ref().and_then(|g| g.panel), None);
        // El panel abierto mantiene la barra.
        app.update(Message::Guest(Msg::TogglePanel(Tab::Connection)));
        app.now += motion::BAR_HIDE * 2;
        assert!(app.guest.as_ref().is_some_and(|g| g.bar_visible(app.now)));
        app.update(Message::Guest(Msg::ClosePanel));
        assert_eq!(app.guest.as_ref().and_then(|g| g.panel), None);
        // VSync es un ajuste de este equipo (se guarda).
        app.update(Message::Guest(Msg::SetVsync(true)));
        assert!(app.settings.video.vsync);
        assert!(app.next_tick().is_some(), "se guarda al poco");
    }

    #[test]
    fn short_relay_reasons() {
        assert_eq!(
            short_relay_reason("solo había direcciones de red local y ninguna respondió"),
            "solo red local"
        );
        assert_eq!(short_relay_reason("se pidió usar el relay"), "forzado");
        assert_eq!(
            short_relay_reason("3 de 4 direcciones fallaron"),
            "la directa falló"
        );
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
        app.update(Message::Event(Event::GuestEnded(Some(GuestEnd::from(
            SessionError::Removed,
        )))));
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

    #[test]
    fn every_other_voice_has_a_volume_remembered_by_name_but_not_yours() {
        let mut app = app();
        app.settings.audio.set_voice_volume("Carla", 60);
        app.settings.audio.set_voice_volume("Ana", 40);
        shots::video(&mut app);
        let member = |guest: u32, leaf: u32, name: &str, you: bool| RoomMember {
            guest,
            leaf,
            name: name.to_owned(),
            you,
        };
        let members = vec![
            member(0, 0, "Bea", false),
            member(1, 1, "Ana", true),
            member(2, 2, "Carla", false),
        ];
        let effects = app.update(Message::Event(Event::Guest(SessionEvent::Members(
            members.clone(),
        ))));
        assert_eq!(
            effects,
            [Effect::SetVoiceGain {
                host: false,
                source: 2,
                gain: 0.6,
            }],
            "el recordado de Carla, nunca el de uno mismo"
        );
        let state = app.guest.as_ref().expect("invitado");
        assert_eq!(
            (state.volume(0), state.volume(1), state.volume(2)),
            (100, 100, 60)
        );
        // «Integrantes» empieza por uno mismo, sigue con el anfitrión y después los demás.
        let mut order: Vec<&RoomMember> = members.iter().collect();
        order.sort_by_key(|member| member_order(member));
        let names: Vec<&str> = order.iter().map(|member| member.name.as_str()).collect();
        assert_eq!(names, ["Ana", "Bea", "Carla"]);
        // El deslizador propio es lo alto que te oyen: se aplica en caliente y se recuerda.
        assert_eq!(
            app.update(Message::Guest(Msg::MicVolume(150))),
            [Effect::SetMicGain {
                host: false,
                gain: 1.5,
            }]
        );
        assert_eq!(app.settings.audio.mic_volume, 150);
        app.flush_settings();
        app.update(Message::Guest(Msg::MicVolumeDone));
        assert_eq!(app.flush_settings(), Some(Effect::SaveSettings));
        // Arrastrar cambia el volumen en caliente; soltar lo recuerda por el nombre.
        let effects = app.update(Message::Guest(Msg::Volume(0, 150)));
        assert_eq!(
            effects,
            [Effect::SetVoiceGain {
                host: false,
                source: 0,
                gain: 1.5,
            }]
        );
        assert_eq!(
            app.settings.audio.voice_volume("Bea"),
            100,
            "aún no se soltó"
        );
        assert!(app.update(Message::Guest(Msg::VolumeDone(0))).is_empty());
        assert_eq!(app.settings.audio.voice_volume("Bea"), 150);
        assert_eq!(app.flush_settings(), Some(Effect::SaveSettings));
        // La propia hoja se ignora aunque llegue el mensaje.
        assert!(app.update(Message::Guest(Msg::Volume(1, 20))).is_empty());
        assert_eq!(app.guest.as_ref().expect("invitado").volume(1), 100);
        // Quien se va no deja entrada; quien sigue conserva lo que se tocó.
        app.update(Message::Event(Event::Guest(SessionEvent::Members(vec![
            member(0, 0, "Bea", false),
            member(1, 1, "Ana", true),
        ]))));
        let state = app.guest.as_ref().expect("invitado");
        assert!(!state.volumes.contains_key(&2));
        assert_eq!(state.volume(0), 150);
    }

    #[test]
    fn local_mutes_are_remembered_by_name_across_reconnections() {
        let mut app = app();
        shots::video(&mut app);
        let member = |leaf: u32, name: &str, you: bool| RoomMember {
            guest: leaf,
            leaf,
            name: name.to_owned(),
            you,
        };
        app.update(Message::Event(Event::Guest(SessionEvent::Members(vec![
            member(0, "Bea", false),
            member(1, "Ana", true),
            member(2, "Carla", false),
        ]))));
        // Dejar de oír a Carla se aplica y se recuerda por su nombre.
        let effects = app.update(Message::Guest(Msg::Hear(2, false)));
        assert_eq!(
            effects,
            [Effect::SetVoiceMuted {
                host: false,
                source: 2,
                muted: true,
            }]
        );
        assert!(app.settings.audio.voice_muted("Carla"));
        assert_eq!(app.flush_settings(), Some(Effect::SaveSettings));
        // Se va: su hoja queda libre también en el audio.
        let effects = app.update(Message::Event(Event::Guest(SessionEvent::Members(vec![
            member(0, "Bea", false),
            member(1, "Ana", true),
        ]))));
        assert_eq!(
            effects,
            [Effect::SetVoiceMuted {
                host: false,
                source: 2,
                muted: false,
            }]
        );
        // Vuelve con otra hoja (o en otra sala): sigue sin oírse.
        let effects = app.update(Message::Event(Event::Guest(SessionEvent::Members(vec![
            member(0, "Bea", false),
            member(1, "Ana", true),
            member(4, "Carla", false),
        ]))));
        assert_eq!(
            effects,
            [Effect::SetVoiceMuted {
                host: false,
                source: 4,
                muted: true,
            }]
        );
        assert!(app.guest.as_ref().expect("invitado").muted.contains(&4));
        // Volver a oírla lo olvida.
        app.update(Message::Guest(Msg::Hear(4, true)));
        assert!(!app.settings.audio.voice_muted("Carla"));
        assert!(
            app.update(Message::Event(Event::Guest(SessionEvent::Members(vec![
                member(0, "Bea", false),
                member(1, "Ana", true),
                member(4, "Carla", false),
            ]))))
            .is_empty()
        );
    }

    #[test]
    fn your_own_row_says_you_are_talking_from_the_local_audio() {
        let mut app = app();
        shots::video(&mut app);
        let members = vec![
            RoomMember {
                guest: 0,
                leaf: 0,
                name: "Bea".to_owned(),
                you: false,
            },
            RoomMember {
                guest: 1,
                leaf: 1,
                name: "Ana".to_owned(),
                you: true,
            },
        ];
        app.update(Message::Event(Event::Guest(SessionEvent::Members(
            members.clone(),
        ))));
        let state = app.guest.as_mut().expect("invitado");
        let stats = state.stats.as_mut().expect("estadísticas");
        // El anfitrión dice que habla «invitado 1» (no lo hace, pero da igual): la fila propia
        // no le hace caso; manda el audio de aquí.
        stats.summary.audio = Some(coroom_session::latency::AudioSummary {
            tracks: Vec::new(),
            av: None,
            speaking: vec!["anfitrión".to_owned(), "invitado 1".to_owned()],
            status: String::new(),
        });
        stats.speaking = false;
        let state = app.guest.as_ref().expect("invitado");
        assert!(
            member_talks(state, &members[0]),
            "el anfitrión, por VoiceState"
        );
        assert!(!member_talks(state, &members[1]));
        app.guest
            .as_mut()
            .and_then(|state| state.stats.as_mut())
            .expect("estadísticas")
            .speaking = true;
        let state = app.guest.as_ref().expect("invitado");
        assert!(member_talks(state, &members[1]), "uno mismo, por su audio");
    }

    #[test]
    fn the_game_volume_is_applied_on_entering_and_remembered_on_release() {
        let mut app = app();
        app.settings.audio.game_volume = 60;
        start(&mut app, "K7Q2MZ".into());
        assert_eq!(
            app.guest.as_ref().expect("invitado").game_volume,
            DEFAULT_VOICE_VOLUME,
            "hasta que arranca la sesión, el normal"
        );
        // Al arrancar la sesión se le aplica el recordado (la sesión nueva está al normal).
        let effects = app.update(Message::Event(Event::GuestStarted(Ok(
            VideoFeed::detached(),
        ))));
        assert_eq!(effects, [Effect::SetGameGain { gain: 0.6 }]);
        assert_eq!(app.guest.as_ref().expect("invitado").game_volume, 60);
        // Arrastrar cambia el volumen en caliente; soltar lo recuerda en los ajustes.
        let effects = app.update(Message::Guest(Msg::GameVolume(150)));
        assert_eq!(effects, [Effect::SetGameGain { gain: 1.5 }]);
        assert_eq!(app.settings.audio.game_volume, 60, "aún no se soltó");
        assert!(app.update(Message::Guest(Msg::GameVolumeDone)).is_empty());
        assert_eq!(app.settings.audio.game_volume, 150);
        assert_eq!(app.flush_settings(), Some(Effect::SaveSettings));
        // Acotado al doble; el volumen normal no manda ningún efecto al arrancar.
        let effects = app.update(Message::Guest(Msg::GameVolume(250)));
        assert_eq!(effects, [Effect::SetGameGain { gain: 2.0 }]);
        assert_eq!(app.guest.as_ref().expect("invitado").game_volume, 200);
        app.settings.audio.game_volume = DEFAULT_VOICE_VOLUME;
        assert!(
            app.update(Message::Event(Event::GuestStarted(Ok(
                VideoFeed::detached()
            ))))
            .is_empty()
        );
        assert_eq!(
            app.guest.as_ref().expect("invitado").game_volume,
            DEFAULT_VOICE_VOLUME
        );
        // Las voces no se tocan.
        assert!(app.guest.as_ref().expect("invitado").volumes.is_empty());
    }

    #[test]
    fn colors_are_from_the_palette() {
        // Los colores del panel salen de la paleta oscura (nada fijo en la pantalla).
        let app = app();
        let dark = dark(&app);
        assert_ne!(dark.accent_text.color(), iced_core::Color::TRANSPARENT);
    }

    #[test]
    fn nothing_animates_while_connecting_and_the_next_tick_is_a_deadline() {
        let mut app = app();
        shots::joined(&mut app, true, false);
        let state = app.guest.as_ref().expect("invitado");
        assert_eq!(state.phase, Phase::Connecting);
        assert!(!state.animating(), "los pasos no se animan a cada refresco");
        assert_eq!(
            state.next_tick(),
            None,
            "sin vídeo no hay plazo que esperar"
        );
        // Con vídeo, el plazo es el final de la espera de la estimación.
        let feed = Arc::clone(&state.feed);
        feed.push(frame(), None, None);
        let estimate_at = state.estimate.expect("estimación").at;
        assert_eq!(state.next_tick(), Some(estimate_at + ESTIMATE_DWELL));
        // Reconectando: la cuenta atrás cambia en cada segundo entero, no a 60 Hz.
        app.update(Message::Event(Event::Guest(SessionEvent::Reconnecting {
            attempt: 1,
            of: 3,
            retry_in: Duration::from_millis(2400),
            reason: "red".into(),
        })));
        let state = app.guest.as_ref().expect("invitado");
        assert!(!state.animating());
        let next = state.next_tick().expect("plazo");
        assert_eq!(next, app.now + Duration::from_millis(400));
    }

    #[test]
    fn the_pointer_only_matters_to_the_interface_over_its_pieces() {
        use iced_core::{Point, Size};
        let mut app = app();
        shots::video(&mut app);
        let size = Size::new(1280.0, 800.0);
        let state = app.guest.as_ref().expect("invitado");
        assert!(state.bar_visible(app.now));
        // Sobre la barra (arriba) y la píldora (abajo), sí; en medio del vídeo, no.
        assert!(pointer_over_ui(&app, Point::new(600.0, 20.0), size));
        assert!(pointer_over_ui(&app, Point::new(40.0, 790.0), size));
        assert!(!pointer_over_ui(&app, Point::new(600.0, 400.0), size));
        // Con el panel abierto, la franja derecha también.
        app.update(Message::Guest(Msg::TogglePanel(Tab::Connection)));
        assert!(pointer_over_ui(&app, Point::new(1200.0, 400.0), size));
        assert!(!pointer_over_ui(&app, Point::new(600.0, 400.0), size));
        // A pantalla completa, con la barra escondida, la píldora se enseña un momento cuando
        // cambia el nivel y luego se va.
        app.update(Message::Guest(Msg::ClosePanel));
        app.fullscreen = true;
        let now = app.now;
        let state = app.guest.as_mut().expect("invitado");
        state.bar_until = now - Duration::from_millis(1);
        assert!(!state.bar_visible(now));
        assert!(!state.pill_visible(now, true));
        state.level_changed_at = Some(now);
        assert!(state.pill_visible(now, true));
        assert_eq!(state.next_tick(), Some(now + PILL_FLASH));
        assert!(!state.pill_visible(now + PILL_FLASH, true));
    }
}
