//! Pantalla 3 («Dentro de la sala»), vista del invitado: el destino «Sala» del riel.
//!
//! Al entrar, la sala de espera con su cabecera: los pasos (señalización, claves, conexión
//! directa o por relay —con un anillo que gira mientras se prueban caminos—, calidad, códec) y,
//! antes del vídeo, la **estimación de calidad** (el RTT medido con el anfitrión y su nivel,
//! `docs/ARQUITECTURA.md` §2.1). El chat ya funciona aquí, en cuanto llegan las claves. Después,
//! el vídeo (aparece con un fundido de 260 ms), con el riel a la izquierda en ventana, con:
//!
//! - la **latencia**, una sola y siempre a la vista arriba a la izquierda («14 ms · Directa»,
//!   «61 ms · Relay · CGNAT», «Reconectando · 3 s»): el RTT del enlace con la escala del
//!   anfitrión y de Diagnóstico; a pantalla completa, con la barra;
//! - una **barra superior** que baja deslizándose al mover el ratón y se va sola a los 2,5 s, con
//!   la sala y tres grupos de botones (voz: micrófono y sonido del juego, con su volumen en un
//!   menú; control: modo inmersivo; vista: Chat, Personas, Conexión y pantalla completa) y salir;
//! - **quién habla**: una fila por persona con su avatar, que entra desde la izquierda;
//! - el **panel lateral** con las pestañas Chat · Personas · Conexión, que entra desde la derecha
//!   encima del vídeo (no se oculta solo);
//! - el **aviso** de cómo salir al tomar el teclado y el ratón o entrar en modo inmersivo, abajo
//!   al centro, que se va del todo a los 3 s (sin animación);
//! - las **burbujas** de los mensajes del chat con el panel cerrado.
//!
//! Con el teclado y el ratón tomados o en modo inmersivo no se anima nada (se leen en el mismo
//! hilo que dibuja). Desde el riel se va a otras páginas sin salir de la sala
//! ([`crate::ui::app::App::away_from_room`]): la voz y el vídeo siguen, la entrada no se envía.
//! Los cambios de permisos llegan como avisos que no bloquean; el final, en un diálogo claro.

use std::sync::Arc;
use std::time::{Duration, Instant};

use coroom_session::input::capture::Allowed;
use coroom_session::input::gamepad::{ConnectedPad, PadChoice, PadKey};
use coroom_session::latency::{AudioTrack, LatencySummary};
use coroom_session::remote::{QualityLevel, RoomMember, SessionEvent, talker_label};
use coroom_transport::Path;
use coroom_ui::Element;
use coroom_ui::animate::Shift;
use coroom_ui::icon::{Icon, icon_with};
use coroom_ui::motion::{EXIT_EASING, Motion, Token};
use coroom_ui::sound::{Cue, cue};
use coroom_ui::style::{ButtonClass, ContainerClass, SliderClass, SvgClass, TextClass};
use coroom_ui::theme::{Palette, Tone};
use coroom_ui::tokens::{motion, size, space, text as type_size};
use coroom_ui::widget::{self as ui};
use iced_core::alignment::{Horizontal, Vertical};
use iced_core::text::Wrapping;
use iced_core::{Alignment, Length, Padding, Vector};
use iced_widget::{Column, button, center, column, container, row, scrollable, stack, text};

use super::chat::{self, Surface};
use super::host::Takeover;
use super::{diagnostics, settings};
use crate::ui::app::{
    App, Dialog, Effect, EndKind, Event, HEADERBAR_HEIGHT, Invitation, Message, Page, RAIL_MIC_ID,
    Screen, quality_tone,
};
use crate::ui::avatars;
use crate::ui::settings::{DEFAULT_VOICE_VOLUME, KnownPeer, MAX_VOICE_VOLUME, voice_gain};
use crate::ui::stats;
use crate::ui::video::{VideoFeed, video_view};

/// Lo que se ve de la estimación antes de pasar al vídeo, como poco.
pub const ESTIMATE_DWELL: Duration = Duration::from_millis(1500);

/// Lo que se ve el aviso del modo inmersivo, de tomar o de soltar el teclado y el ratón.
pub const NOTICE: Duration = Duration::from_secs(3);

/// Una vuelta del anillo de «Conexión con el anfitrión» mientras se prueban caminos.
const SPIN: Duration = Duration::from_millis(900);

/// Ancho del menú del sonido del juego.
const SOUND_MENU: f32 = 300.0;

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

/// Pestaña del panel lateral de la sala, en el orden de sus botones en la barra.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// Chat de la sala (`Ctrl+Alt+C`).
    Chat,
    /// Quién está, quién habla y cómo se le oye (`Ctrl+Alt+J`).
    People,
    /// Latencia, ruta, pérdida y calidad (`Ctrl+Alt+S`).
    Connection,
}

impl Tab {
    /// Todas, en el orden del panel y de la barra.
    pub const ALL: [Tab; 3] = [Tab::Chat, Tab::People, Tab::Connection];
}

/// El aviso de abajo al centro sobre el vídeo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// Se entró en modo inmersivo: cómo volver al escritorio y cómo soltar.
    Immersive,
    /// El teclado y el ratón van a la sala: cómo soltarlos.
    Taken,
    /// Se soltaron.
    Released,
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
    /// Cuándo se abrió el panel (entra desde la derecha).
    pub panel_at: Instant,
    /// El panel que se cerró y cuándo, mientras sale.
    pub closing: Option<(Tab, Instant)>,
    /// El menú del sonido del juego (botón de sonido de la barra) abierto.
    pub sound_menu: bool,
    /// Quién está en la sala (con el anfitrión), según el último `Members`.
    pub members: Vec<RoomMember>,
    /// Los demás que hablan ahora (su `guest`), en el orden en que empezaron a hablar.
    pub talk_order: Vec<u32>,
    /// Cuándo empezó a hablar cada uno de [`State::talk_order`] (su fila entra desde la izquierda).
    pub talk_since: std::collections::BTreeMap<u32, Instant>,
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
    /// El aviso de abajo al centro y desde cuándo (se va a los [`NOTICE`]).
    pub notice: Option<(Notice, Instant)>,
    /// Cuándo empezó el vídeo (la imagen aparece con un fundido).
    pub video_at: Option<Instant>,
    /// Si la ventana reenvía ahora el teclado y el ratón (desde el clic en la imagen hasta
    /// soltarlos con el atajo o perder el foco).
    pub forwarding: bool,
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
            panel_at: now,
            closing: None,
            sound_menu: false,
            members: Vec::new(),
            muted: std::collections::BTreeSet::new(),
            volumes: std::collections::BTreeMap::new(),
            game_volume: DEFAULT_VOICE_VOLUME,
            pads: None,
            pads_raw: false,
            bar_until: now,
            bar_shown_at: now,
            immersive: false,
            notice: None,
            video_at: None,
            forwarding: false,
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
            talk_order: Vec::new(),
            talk_since: std::collections::BTreeMap::new(),
        }
    }

    /// Si el anfitrión dijo que no comparte nada (la sala es solo de voz y chat).
    pub fn nothing_shared(&self) -> bool {
        self.host_sharing == Some(false)
    }

    /// Si el teclado y el ratón van a la sala o se está en modo inmersivo: la entrada se lee en
    /// el mismo hilo que dibuja la interfaz, así que no se anima nada.
    pub fn input_taken(&self) -> bool {
        self.forwarding || self.immersive
    }

    /// El movimiento de la sala con el de la aplicación: ninguno con la entrada tomada.
    pub fn motion(&self, app: Motion) -> Motion {
        if self.input_taken() { Motion::OFF } else { app }
    }

    /// Si se están probando caminos hacia el anfitrión (el anillo gira): solo mientras conecta.
    pub fn trying_paths(&self) -> bool {
        self.phase == Phase::Connecting && marks(self)[2] == Mark::Active
    }

    /// Si hay algo animándose a cada refresco: el anillo mientras se prueban caminos y, con el
    /// vídeo, la entrada y la salida de la barra y del panel, las filas de quién habla que entran
    /// y el fundido de la imagen. Con la entrada tomada, nada. Los pasos de la conexión y la
    /// cuenta atrás de la reconexión cambian a plazos fijos ([`State::next_tick`]).
    pub fn animating(&self, motion: Motion) -> bool {
        let now = self.now;
        if self.phase == Phase::Connecting {
            return self.trying_paths() && !motion.is_off();
        }
        let motion = self.motion(motion);
        if motion.is_off() {
            return false;
        }
        let bar = self
            .bar_hidden(now, motion)
            .is_some_and(|hidden| hidden > 0.0 || !self.bar_visible(now));
        let panel = self
            .panel_drawn(now, motion)
            .is_some_and(|(_, hidden)| hidden > 0.0 || self.panel.is_none());
        let rows = self
            .talk_since
            .values()
            .any(|at| entered(motion, Token::Panel, *at, now) < 1.0);
        bar || panel || rows || self.cover(now, motion).is_some()
    }

    /// Próximo momento en que cambia algo sin eventos: la barra se oculta, la píldora deja de
    /// verse, se va el aviso, acaba la espera de la estimación (pasa al vídeo) o la cuenta atrás
    /// de la reconexión baja un segundo.
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
            if let Some((_, at)) = self.notice {
                consider(at + NOTICE);
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

    /// Si la barra se ve (con el panel o el menú del sonido abiertos, o reconectando, siempre; en
    /// modo inmersivo, nunca).
    pub fn bar_visible(&self, now: Instant) -> bool {
        self.phase == Phase::Video
            && !self.immersive
            && (now < self.bar_until
                || self.panel.is_some()
                || self.sound_menu
                || self.reconnecting.is_some())
    }

    /// Cuánto le falta a la barra para estar en su sitio (0: en su sitio, 1: fuera por arriba)
    /// mientras se dibuja: al aparecer baja deslizándose y al irse sube (más rápido); `None`
    /// cuando no se dibuja.
    pub fn bar_hidden(&self, now: Instant, motion: Motion) -> Option<f32> {
        if self.bar_visible(now) {
            return Some(1.0 - entered(motion, Token::Panel, self.bar_shown_at, now));
        }
        if self.phase != Phase::Video || self.immersive || self.bar_until <= self.bar_shown_at {
            return None;
        }
        let gone = gone(motion, Token::Panel, self.bar_until, now);
        (gone < 1.0).then_some(gone)
    }

    /// La pestaña del panel que se dibuja y cuánto le falta para estar en su sitio (0 a 1): entra
    /// desde la derecha y sale hacia ella.
    pub fn panel_drawn(&self, now: Instant, motion: Motion) -> Option<(Tab, f32)> {
        if let Some(tab) = self.panel {
            return Some((tab, 1.0 - entered(motion, Token::Panel, self.panel_at, now)));
        }
        let (tab, since) = self.closing?;
        let gone = gone(motion, Token::Panel, since, now);
        (gone < 1.0).then_some((tab, gone))
    }

    /// Lo que le queda al fundido con que aparece la imagen (1: tapada, 0: se ve), mientras dura.
    pub fn cover(&self, now: Instant, motion: Motion) -> Option<f32> {
        let shown = entered(motion, Token::Screen, self.video_at?, now);
        (shown < 1.0).then_some(1.0 - shown)
    }

    /// El aviso de abajo al centro, si se ve en `now` (los [`NOTICE`] desde que apareció).
    pub fn notice(&self, now: Instant) -> Option<Notice> {
        self.notice
            .filter(|(_, at)| now.saturating_duration_since(*at) < NOTICE)
            .map(|(notice, _)| notice)
    }

    /// Si la píldora de la latencia se ve: siempre en ventana; a pantalla completa, con la barra
    /// o un momento ([`PILL_FLASH`]) tras cambiar el nivel; en modo inmersivo, nunca.
    pub fn pill_visible(&self, now: Instant, fullscreen: bool) -> bool {
        self.phase == Phase::Video
            && !self.immersive
            && (!fullscreen || self.bar_visible(now) || self.pill_flash(now))
    }

    /// Si el nivel de la latencia acaba de cambiar (a pantalla completa, la píldora se enseña
    /// [`PILL_FLASH`] aunque no esté la barra).
    pub fn pill_flash(&self, now: Instant) -> bool {
        self.level_changed_at
            .is_some_and(|at| now.saturating_duration_since(at) < PILL_FLASH)
    }

    /// Enseña la barra durante [`motion::BAR_HIDE`].
    fn reveal_bar(&mut self, now: Instant) {
        if !self.bar_visible(now) && self.bar_hidden(now, Motion::FULL).is_none() {
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

    /// Pasa al vídeo: la barra se enseña y la imagen aparece con su fundido.
    fn enter_video(&mut self, now: Instant) {
        self.phase = Phase::Video;
        self.video_at = Some(now);
        self.reveal_bar(now);
    }

    /// Pasa el tiempo.
    pub fn tick(&mut self, now: Instant) {
        self.now = now;
        // Sin nada compartido no llegará vídeo: se entra en cuanto el anfitrión lo dice.
        if self.phase == Phase::Connecting && self.nothing_shared() {
            self.enter_video(now);
        }
        if self.phase == Phase::Connecting
            && self.feed.has_frame()
            && self
                .estimate
                .is_none_or(|estimate| now.duration_since(estimate.at) >= ESTIMATE_DWELL)
        {
            self.enter_video(now);
        }
    }

    /// Modo inmersivo (o escritorio): con el aviso de cómo salir al entrar y, al salir, el de si
    /// el teclado y el ratón siguen en la sala.
    fn set_immersive(&mut self, immersive: bool, now: Instant) {
        if self.immersive == immersive {
            return;
        }
        self.immersive = immersive;
        if immersive {
            self.bar_until = now;
            self.sound_menu = false;
            self.notice = Some((Notice::Immersive, now));
        } else {
            let notice = if self.forwarding {
                Notice::Taken
            } else {
                Notice::Released
            };
            self.notice = Some((notice, now));
            self.reveal_bar(now);
        }
    }

    /// Empieza o deja de reenviar el teclado y el ratón: el aviso lo cuenta (en modo inmersivo
    /// manda el suyo).
    fn set_forwarding(&mut self, forwarding: bool, now: Instant) {
        if self.forwarding == forwarding {
            return;
        }
        self.forwarding = forwarding;
        if forwarding {
            self.sound_menu = false;
            if !self.immersive {
                self.notice = Some((Notice::Taken, now));
            }
        } else {
            self.notice = Some((Notice::Released, now));
        }
    }

    /// Latencia añadida medida (mediana de extremo a extremo del vídeo), en ms.
    pub fn added_ms(&self) -> Option<f64> {
        let total = &self.stats.as_ref()?.summary.total;
        (total.samples > 0).then(|| total.p50_us as f64 / 1000.0)
    }

    /// RTT del enlace con el anfitrión, en ms: la latencia que se enseña, la misma medida que ve
    /// el anfitrión de cada invitado y la de Diagnóstico.
    pub fn rtt_ms(&self) -> Option<f64> {
        let link = self.stats.as_ref()?.summary.link.as_ref()?;
        (link.rtt_ms > 0.0).then_some(link.rtt_ms)
    }

    /// Nivel de la latencia, con la escala de toda la aplicación ([`stats::latency`]: la misma
    /// que ve el anfitrión en tu fila y Diagnóstico).
    pub fn level(&self) -> Option<QualityLevel> {
        self.rtt_ms().map(|rtt| stats::latency(rtt).1)
    }
}

/// Lo que lleva su entrada lo que empezó en `since` (0 a 1, con la curva de `token`).
fn entered(motion: Motion, token: Token, since: Instant, now: Instant) -> f32 {
    let duration = motion.duration(token);
    if duration.is_zero() {
        return 1.0;
    }
    let t = (now.saturating_duration_since(since).as_secs_f32() / duration.as_secs_f32())
        .clamp(0.0, 1.0);
    if t >= 1.0 {
        1.0
    } else {
        token.easing().value(t)
    }
}

/// Lo que lleva su salida lo que empezó a irse en `since` (0 a 1): el 70 % de la entrada.
fn gone(motion: Motion, token: Token, since: Instant, now: Instant) -> f32 {
    let duration = motion.exit_duration(token);
    if duration.is_zero() {
        return 1.0;
    }
    let t = (now.saturating_duration_since(since).as_secs_f32() / duration.as_secs_f32())
        .clamp(0.0, 1.0);
    if t >= 1.0 { 1.0 } else { EXIT_EASING.value(t) }
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

/// La pérdida del enlace si llega al 1 % (lo que más duele cuando la latencia ya es mala).
fn loss(summary: &LatencySummary) -> Option<String> {
    let link = summary.link.as_ref()?;
    (link.loss_percent >= 1.0).then(|| {
        t!(
            "room-perdida-valor",
            value = stats::number(link.loss_percent, 0)
        )
    })
}

/// La píldora para el estado de ahora: el RTT con el anfitrión, su ruta y, por el relay, el
/// motivo (o la pérdida, si la latencia es mala).
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
    let (Some(rtt), Some(stats)) = (state.rtt_ms(), state.stats.as_ref()) else {
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
    let (lead, level) = stats::latency(rtt);
    let reason = if level == QualityLevel::Poor {
        loss(&stats.summary).or_else(relay_reason)
    } else {
        relay_reason()
    };
    Pill {
        tone: Some(quality_tone(level)),
        reconnecting: false,
        lead,
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
    /// `Ctrl+Alt+J`: abre o cierra el panel en «Personas».
    TogglePeople,
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
    /// Elegir qué mando enviar (Ajustes → Mandos).
    ChoosePad(PadChoice),
    /// Abre o cierra el menú del sonido del juego (botón de sonido de la barra).
    ToggleSoundMenu,
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
    /// La ventana empezó o dejó de reenviar el teclado y el ratón (clic en la imagen, atajo de
    /// soltar, foco perdido o permiso quitado).
    ForwardingChanged(bool),
}

/// Entra en la sala `code`.
pub fn start(app: &mut App, code: String) -> Vec<Effect> {
    app.guest = Some(State::new(code.clone(), VideoFeed::detached(), app.now));
    app.screen = Screen::Guest;
    // «Sala» en el riel es esta sala.
    app.page = Page::Host;
    app.account_menu = false;
    vec![Effect::StopLobby, Effect::Join(code)]
}

/// La ranura del mando que se envía a la sala: la que dio el anfitrión, salvo desde otra página
/// del riel (no se envía nada; el anfitrión lo ve desconectarse y volver al regresar).
pub fn sent_pad_slot(app: &App, allowed: Option<u8>) -> Option<u8> {
    if app.away_from_room() { None } else { allowed }
}

/// Si el riel se ve junto a la sala: con la ventana normal (a pantalla completa se oculta, como
/// la barra), con cuenta y fuera de la prueba local.
pub fn rail_shown(app: &App) -> bool {
    app.screen == Screen::Guest && !app.fullscreen && !app.probe && app.account.is_some()
}

/// Dónde empieza el vídeo por la izquierda (lógico): a la derecha del riel y su línea, si se ve.
pub fn video_left(app: &App) -> f32 {
    if !rail_shown(app) {
        return 0.0;
    }
    let rail = if ui::rail_is_wide(app.window.width) {
        ui::RAIL_WIDE
    } else {
        ui::RAIL_NARROW
    };
    rail + 1.0
}

/// Con el vídeo delante, si a la interfaz le importa que el cursor esté en `position` (lógica,
/// en una ventana de `size`): sobre el riel, la franja de arriba (la latencia, la barra, su menú
/// y el aviso de reconexión), el panel lateral, el aviso y las burbujas del chat abajo, o con
/// toasts a la vista. Fuera de ahí solo hay vídeo: moverse no cambia nada que dibujar, y la
/// ventana se ahorra reconstruir la interfaz.
pub fn pointer_over_ui(app: &App, position: iced_core::Point, size: iced_core::Size) -> bool {
    let Some(state) = app.guest.as_ref() else {
        return true;
    };
    if state.phase != Phase::Video || !app.toasts.is_empty() || state.reconnecting.is_some() {
        return true;
    }
    if position.x <= video_left(app) {
        return true;
    }
    let now = app.now;
    let mut top = space::S + size::VIDEO_BAR + space::S;
    if state.sound_menu {
        top += SOUND_MENU_HEIGHT;
    }
    if (state.bar_visible(now) || state.pill_visible(now, app.fullscreen)) && position.y <= top {
        return true;
    }
    if state.panel.is_some() && position.x >= size.width - (size::STATS_PANEL + 2.0 * space::S) {
        return true;
    }
    // Abajo: el aviso o la indicación (al centro) y las burbujas del chat (derecha, hasta tres).
    let bottom = if app.chat.bubbles.is_empty() {
        NOTICE_HEIGHT + 2.0 * space::S
    } else {
        4.0 * (size::CONTROL + space::S) + 2.0 * space::S
    };
    let notice = state.notice(now).is_some() || state.pill_visible(now, app.fullscreen);
    (notice || !app.chat.bubbles.is_empty()) && position.y >= size.height - bottom
}

/// Alto del menú del sonido del juego bajo la barra (para saber si el cursor está encima).
const SOUND_MENU_HEIGHT: f32 = 120.0;

/// Alto del aviso de abajo al centro.
const NOTICE_HEIGHT: f32 = 52.0;

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

/// Pone el panel en `tab` (`None`: lo cierra): al abrirse entra desde la derecha y al cerrarse
/// sale hacia ella; cambiar de pestaña no lo mueve. `by_user`: lo pidió el usuario (suena).
pub fn place_panel(state: &mut State, tab: Option<Tab>, now: Instant, by_user: bool) {
    match (state.panel, tab) {
        (None, Some(_)) => {
            state.panel_at = now;
            state.closing = None;
            state.sound_menu = false;
            if by_user {
                cue(Cue::Open);
            }
        }
        (Some(old), None) => {
            state.closing = Some((old, now));
            if by_user {
                cue(Cue::Close);
            }
        }
        _ => {}
    }
    state.panel = tab;
    if tab.is_some() {
        state.reveal_bar(now);
    }
}

/// Abre el panel en `tab` (`None`: lo cierra); el chat se entera (solo se ve en su pestaña).
fn set_panel(app: &mut App, tab: Option<Tab>) -> Vec<Effect> {
    let now = app.now;
    if let Some(state) = app.guest.as_mut() {
        place_panel(state, tab, now, true);
    }
    chat::tab_changed(app, tab)
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
    if app.guest.is_none() {
        return Vec::new();
    }
    // Un atajo del panel desde otra página vuelve a la sala con el panel en esa pestaña.
    let panel_tab = match msg {
        Msg::ToggleStats => Some(Tab::Connection),
        Msg::TogglePeople => Some(Tab::People),
        Msg::TogglePanel(tab) | Msg::ShowTab(tab) => Some(tab),
        _ => None,
    };
    if let Some(tab) = panel_tab
        && app.away_from_room()
    {
        let mut effects = app.update(Message::Navigate(Page::Host));
        if app
            .guest
            .as_ref()
            .is_some_and(|g| g.phase == Phase::Video || tab == Tab::Chat)
        {
            effects.extend(set_panel(app, Some(tab)));
        }
        return effects;
    }
    let Some(state) = app.guest.as_mut() else {
        return Vec::new();
    };
    match msg {
        Msg::ToggleStats | Msg::TogglePeople if state.phase == Phase::Connecting => {}
        Msg::ToggleStats => return toggle_panel(app, Tab::Connection),
        Msg::TogglePeople => return toggle_panel(app, Tab::People),
        Msg::TogglePanel(tab) => return toggle_panel(app, tab),
        Msg::ShowTab(tab) => return set_panel(app, Some(tab)),
        Msg::ClosePanel => return set_panel(app, None),
        Msg::ToggleSoundMenu => {
            state.sound_menu = !state.sound_menu;
            state.reveal_bar(now);
        }
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
            let immersive = !state.immersive;
            state.set_immersive(immersive, now);
            return vec![Effect::SetImmersive(immersive)];
        }
        Msg::ForwardingChanged(forwarding) => state.set_forwarding(forwarding, now),
        Msg::ImmersiveChanged(immersive) => state.set_immersive(immersive, now),
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
                state.enter_video(now);
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
                let order = talk_order(&state.talk_order, &talking_now(state));
                state.talk_since.retain(|guest, _| order.contains(guest));
                for guest in &order {
                    state.talk_since.entry(*guest).or_insert(now);
                }
                state.talk_order = order;
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

/// Los pasos de la sala de espera: señalización, claves, conexión, calidad y vídeo. Cada uno está
/// hecho, en marcha (el primero que falta) o pendiente.
fn marks(state: &State) -> [Mark; 5] {
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
    [signal, keys, link, quality, video]
}

/// El anillo que gira mientras se prueban caminos: una vuelta cada [`SPIN`], en función del
/// tiempo (con las animaciones quitadas, quieto).
fn spinner<'a>(app: &App, state: &State) -> Element<'a, Message> {
    let turn = if app.motion().is_off() {
        0.0
    } else {
        let elapsed = app
            .now
            .saturating_duration_since(state.started)
            .as_secs_f32();
        (elapsed / SPIN.as_secs_f32()).fract() * std::f32::consts::TAU
    };
    iced_widget::svg::Svg::<'a, coroom_ui::Theme>::new(Icon::LoaderCircle.handle())
        .width(Length::Fixed(20.0))
        .height(Length::Fixed(20.0))
        .rotation(iced_core::Rotation::Floating(iced_core::Radians(turn)))
        .class(SvgClass::Accent)
        .into()
}

/// Un paso; `spin`, el anillo que gira en lugar del reloj de arena del paso en marcha.
fn step<'a>(
    mark: Mark,
    title: &'a str,
    detail: Option<String>,
    spin: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let glyph: Element<'a, Message> = match (mark, spin) {
        (Mark::Done, _) => icon_with(Icon::CircleCheck, 20.0, SvgClass::Tone(Tone::Success)),
        (Mark::Active, Some(spin)) => spin,
        (Mark::Active, None) => icon_with(Icon::Hourglass, 20.0, SvgClass::Accent),
        (Mark::Pending, _) => icon_with(Icon::CircleDashed, 20.0, SvgClass::Secondary),
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
pub fn room_title(state: &State) -> String {
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
    let [signal, keys, link, quality, video] = marks(state);
    let mut steps = Column::new().spacing(space::S);
    steps = steps.push(step(
        signal,
        t!("room-senalizacion"),
        state
            .host_name
            .as_ref()
            .map(|name| t!("room-dentro-de-la-sala-de-name", name = name)),
        None,
    ));
    steps = steps.push(step(
        keys,
        t!("room-claves-de-la-sala"),
        Some(t!("room-cifrado-de-extremo-a-extremo-con").to_owned()),
        None,
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
        state.trying_paths().then(|| spinner(app, state)),
    ));
    steps = steps.push(step(
        quality,
        t!("room-calidad-de-la-conexion"),
        state
            .estimate
            .map(|e| format!("{} · {}", stats::ms(e.rtt_ms), e.level.label())),
        None,
    ));
    steps = steps.push(step(video, t!("room-video"), state.codec.clone(), None));
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
    // La tarjeta de la calidad tiene su sitio aunque falte (con el espacio de encima dentro, para
    // que el hueco no lo sume): los botones de después no cambian de posición entre los hijos.
    let above = |content: Element<'a, Message>| -> Element<'a, Message> {
        container(content)
            .padding(Padding {
                top: space::M,
                ..Padding::ZERO
            })
            .into()
    };
    let mut actions = row![ui::flat(t!("room-cancelar"), Some(on(Msg::Cancel)))].spacing(space::XS);
    if state.feed.has_frame() {
        actions = actions
            .push(ui::fill())
            .push(ui::primary(t!("room-ver-ya"), Some(on(Msg::Continue))));
    }
    let cards = Column::new()
        .max_width(size::NARROW + 80.0)
        .push(ui::card(content))
        .push(ui::slot(
            state
                .estimate
                .as_ref()
                .map(|estimate| above(quality_card(estimate))),
        ))
        .push(above(actions.into()));
    let body: Element<'a, Message> = scrollable(
        container(cards)
            .padding(Padding::from([space::XL, space::L]))
            .center_x(Length::Fill),
    )
    .height(Length::Fill)
    .width(Length::Fill)
    .into();
    let panel = (state.panel == Some(Tab::Chat)).then(|| {
        container(chat::panel(app, Surface::Window))
            .width(Length::Fixed(size::STATS_PANEL))
            .height(Length::Fill)
            .padding(Padding {
                top: space::S,
                right: space::S,
                bottom: space::S,
                left: 0.0,
            })
            .into()
    });
    let main = row![body, ui::slot(panel)].height(Length::Fill);
    let page = column![waiting_header(app, state), main].height(Length::Fill);
    // La capa de las burbujas está siempre (vacía con el chat abierto o sin mensajes): si la pila
    // apareciera solo con un mensaje, los pasos y el chat volverían arriba.
    let bubbles = chat::bubbles(app).map(|bubbles| {
        container(bubbles)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Horizontal::Right)
            .align_y(Vertical::Bottom)
            .padding(space::S)
            .into()
    });
    stack![page, ui::slot(bubbles)].into()
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

/// Los niveles de calidad, para medir las ranuras de sus cifras.
const LEVELS: [QualityLevel; 4] = [
    QualityLevel::Excellent,
    QualityLevel::Good,
    QualityLevel::Acceptable,
    QualityLevel::Poor,
];

/// Alto de la píldora de la latencia y de la indicación del teclado y el ratón.
const PILL_HEIGHT: f32 = 36.0;

/// Margen del fondo de un grupo de botones de la barra.
const GROUP_PADDING: f32 = 2.0;

/// Lado de un grupo de un solo botón (el de control).
const SINGLE_GROUP: f32 = size::CONTROL + 2.0 * GROUP_PADDING;

/// Lo que se desliza una fila de quién habla al entrar desde la izquierda.
const SPEAKER_SLIDE: f32 = 240.0;

/// Alto de cada persona en «Personas» (su fila y su volumen): las filas no cambian de alto
/// cuando alguien habla, se silencia o llega su número de seguridad.
const PERSON_HEIGHT: f32 = 84.0;

/// Caracteres del nombre de una persona en «Personas».
const PERSON_NAME_MAX: usize = 26;

/// Distancia del borde de arriba a lo que va debajo de la barra (quién habla, el panel).
const BELOW_BAR: f32 = space::S + size::VIDEO_BAR + space::S;

/// «AV1 · 1440p60 · 18,4 Mbit/s»: lo que llega (la ruta va con la latencia).
fn bar_facts(state: &State) -> String {
    let mut facts = Vec::new();
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

/// Un grupo de botones de la barra sobre su fondo.
fn bar_group<'a>(buttons: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    container(
        iced_widget::Row::with_children(buttons)
            .spacing(2)
            .align_y(Alignment::Center),
    )
    .padding(GROUP_PADDING)
    .class(ContainerClass::OverlaySegmented)
    .into()
}

/// El grupo de voz: el micrófono (si el anfitrión te dio la voz; su sitio queda aunque no) y el
/// sonido del juego, que abre el menú con su volumen.
fn voice_group<'a>(app: &'a App, state: &'a State) -> Element<'a, Message> {
    let stats_now = state.stats.as_ref();
    let mic_muted = stats_now.is_some_and(|s| s.mic_muted);
    let voice = stats_now.is_some_and(|s| s.voice || s.mic_muted);
    let game_muted = stats_now.is_some_and(|s| s.game_muted);
    let keys = &app.settings.input.shortcuts;
    let mic = voice.then(|| {
        bar_button(
            if mic_muted { Icon::MicOff } else { Icon::Mic },
            if mic_muted {
                t!("room-activar-el-microfono", key = keys.mic)
            } else {
                t!("room-silenciar-el-microfono", key = keys.mic)
            },
            mic_muted,
            on(Msg::ToggleMic),
        )
    });
    let sound = bar_button(
        if game_muted {
            Icon::VolumeOff
        } else {
            Icon::Volume
        },
        t!("room-sonido-del-juego-tip", key = keys.game),
        state.sound_menu,
        on(Msg::ToggleSoundMenu),
    );
    bar_group(vec![ui::reserved(size::CONTROL, size::CONTROL, mic), sound])
}

/// Lo que va a la derecha del grupo de voz: el grupo de control (el modo inmersivo, si el
/// anfitrión te dio el teclado y el ratón; su sitio queda aunque no), el de vista (una pestaña del
/// panel por botón, en su orden, y la pantalla completa) y salir. El menú del sonido lo mide para
/// quedar bajo su botón.
fn bar_tail<'a>(app: &'a App, state: &'a State) -> Element<'a, Message> {
    let keys = &app.settings.input.shortcuts;
    let allowed = state.stats.as_ref().map(|s| s.allowed).unwrap_or_default();
    let control = allowed.keyboard_mouse.then(|| {
        bar_group(vec![bar_button(
            Icon::Pointer,
            t!("room-modo-inmersivo", key = keys.immersive),
            state.immersive,
            on(Msg::ToggleImmersive),
        )])
    });
    let tab_button = |this: Tab| -> Element<'a, Message> {
        let active = state.panel == Some(this);
        let message = on(Msg::TogglePanel(this));
        match this {
            Tab::Chat => chat::with_unread(
                bar_button(
                    Icon::Chat,
                    t!("room-chat", key = keys.chat),
                    active,
                    message,
                ),
                app.chat.unread,
                size::CONTROL,
            ),
            Tab::People => bar_button(
                Icon::Users,
                t!("room-personas", key = keys.pads),
                active,
                message,
            ),
            Tab::Connection => bar_button(
                Icon::Activity,
                t!("room-conexion", key = keys.stats),
                active,
                message,
            ),
        }
    };
    let mut view: Vec<Element<'a, Message>> = Tab::ALL.into_iter().map(tab_button).collect();
    view.push(bar_button(
        if app.fullscreen {
            Icon::Minimize
        } else {
            Icon::Maximize
        },
        t!("room-pantalla-completa-f11"),
        app.fullscreen,
        on(Msg::ToggleFullscreen),
    ));
    let leave = button(
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
    .on_press(on(Msg::Leave));
    row![
        ui::reserved(SINGLE_GROUP, SINGLE_GROUP, control),
        bar_group(view),
        leave,
    ]
    .spacing(space::S)
    .align_y(Alignment::Center)
    .into()
}

/// La barra: el sitio de la latencia (que va en su propia capa y no se mueve con la barra), la
/// sala con lo que llega y los tres grupos de botones con salir.
fn top_bar<'a>(app: &'a App, state: &'a State) -> Element<'a, Message> {
    let title = column![
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
    .width(Length::Fill);
    container(
        row![
            ui::reserve([pill_view(app, state)], None),
            container(title).width(Length::Fill).clip(true),
            voice_group(app, state),
            bar_tail(app, state),
        ]
        .spacing(space::S)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([0.0, space::S]))
    .height(Length::Fixed(size::VIDEO_BAR))
    .center_y(Length::Fixed(size::VIDEO_BAR))
    .width(Length::Fill)
    .class(ContainerClass::Overlay)
    .into()
}

/// El menú del botón de sonido: el volumen del juego en este equipo (de 0 a 200 % de 5 en 5;
/// Ctrl+clic, 100 %), con el botón de silenciarlo delante (el mismo que `Ctrl+Alt+V`; va aparte
/// del volumen y manda). Mientras se arrastra ya suena así; al soltar se recuerda en los ajustes.
fn sound_menu<'a>(state: &State) -> Element<'a, Message> {
    let percent = state.game_volume;
    let muted = state.stats.as_ref().is_some_and(|s| s.game_muted);
    container(
        column![
            section_title(t!("room-volumen-del-juego"), None),
            ui::slider_with(
                0..=MAX_VOICE_VOLUME,
                percent,
                5,
                DEFAULT_VOICE_VOLUME,
                Some(mute_button(muted, on(Msg::ToggleGame))),
                t!("room-volumen-porcentaje", percent = percent),
                SliderClass::Overlay.muted(muted),
                |value| on(Msg::GameVolume(value)),
                Some(on(Msg::GameVolumeDone)),
            ),
            overlay_text(t!("room-sonido-del-juego-nota"))
                .size(type_size::CAPTION)
                .wrapping(Wrapping::WordOrGlyph),
        ]
        .spacing(space::XS),
    )
    .padding(Padding::from([space::S, space::M]))
    .width(Length::Fixed(SOUND_MENU))
    .class(ContainerClass::Overlay)
    .into()
}

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

/// La indicación del teclado y el ratón abajo al centro (como la píldora, a pantalla completa
/// solo con la barra), solo si el anfitrión le dio ese permiso a este invitado: cómo soltarlos
/// mientras se reenvían o, sueltos, cómo volver a usarlos. Sin nada compartido no hay imagen en
/// la que hacer clic.
pub fn input_hint(app: &App, state: &State) -> Option<String> {
    let allowed = state
        .stats
        .as_ref()
        .is_some_and(|stats| stats.allowed.keyboard_mouse);
    if !allowed || state.nothing_shared() {
        return None;
    }
    Some(if state.forwarding {
        t!(
            "room-soltar-teclado-y-raton",
            key = app.settings.input.shortcuts.release
        )
    } else {
        t!("room-clic-para-usar-teclado-y-raton").to_owned()
    })
}

fn input_hint_view<'a>(app: &App, state: &State) -> Option<Element<'a, Message>> {
    let hint = input_hint(app, state)?;
    let glyph = if state.forwarding {
        Icon::Keyboard
    } else {
        Icon::Mouse
    };
    let content = row![
        icon_with(glyph, 16.0, SvgClass::Overlay),
        text(hint)
            .size(type_size::SECONDARY)
            .class(TextClass::Overlay)
            .wrapping(Wrapping::None),
    ]
    .spacing(space::XS)
    .align_y(Alignment::Center);
    Some(
        container(content)
            .padding(Padding::from([0.0, 12.0]))
            .height(Length::Fixed(PILL_HEIGHT))
            .center_y(Length::Fixed(PILL_HEIGHT))
            .class(ContainerClass::OverlayPill)
            .into(),
    )
}

/// Lo que dice el aviso de abajo al centro: el título y, debajo, las teclas (las de Atajos).
pub fn notice_text(app: &App, notice: Notice) -> (String, Option<String>) {
    let keys = &app.settings.input.shortcuts;
    match notice {
        Notice::Immersive => (
            t!("room-aviso-inmersivo").to_owned(),
            Some(t!(
                "room-aviso-inmersivo-teclas",
                desktop = keys.immersive,
                release = keys.release
            )),
        ),
        Notice::Taken => (
            t!("room-aviso-tomados").to_owned(),
            Some(t!("room-aviso-tomados-teclas", release = keys.release)),
        ),
        Notice::Released => (t!("room-aviso-soltaste").to_owned(), None),
    }
}

/// El aviso de abajo al centro, grande: aparece y se va de golpe (con la entrada tomada no se
/// anima nada).
fn notice_view<'a>(app: &App, notice: Notice) -> Element<'a, Message> {
    let glyph = match notice {
        Notice::Immersive => Icon::Pointer,
        Notice::Taken => Icon::Keyboard,
        Notice::Released => Icon::Mouse,
    };
    let (title, keys) = notice_text(app, notice);
    let mut texts = column![
        text(title)
            .size(type_size::HEADING)
            .font(coroom_ui::font::SEMIBOLD)
            .class(TextClass::Overlay)
            .wrapping(Wrapping::None)
    ]
    .spacing(2);
    if let Some(keys) = keys {
        texts = texts.push(
            text(keys)
                .size(type_size::BODY)
                .class(TextClass::OverlaySecondary)
                .wrapping(Wrapping::None),
        );
    }
    container(
        row![icon_with(glyph, 24.0, SvgClass::Overlay), texts]
            .spacing(space::S)
            .align_y(Alignment::Center),
    )
    .padding(Padding::from([0.0, space::L]))
    .height(Length::Fixed(NOTICE_HEIGHT))
    .center_y(Length::Fixed(NOTICE_HEIGHT))
    .class(ContainerClass::Overlay)
    .into()
}

/// Los demás que hablan ahora (su `guest`), en el orden de «Personas» (uno mismo no: ya lo
/// sabe).
fn talking_now(state: &State) -> Vec<u32> {
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
        .map(|member| member.guest)
        .collect()
}

/// El orden del indicador de quién habla con `now` hablando: quien ya hablaba conserva su sitio,
/// quien calla sale y quien empieza va al final (así las filas no saltan mientras se habla).
pub fn talk_order(before: &[u32], now: &[u32]) -> Vec<u32> {
    let mut order: Vec<u32> = before
        .iter()
        .copied()
        .filter(|guest| now.contains(guest))
        .collect();
    for guest in now {
        if !order.contains(guest) {
            order.push(*guest);
        }
    }
    order
}

/// Los demás que hablan ahora, en el orden en que empezaron (quien empezó después del último
/// `GuestStats` va al final).
pub fn talking_members(state: &State) -> Vec<&RoomMember> {
    talk_order(&state.talk_order, &talking_now(state))
        .iter()
        .filter_map(|guest| {
            state
                .members
                .iter()
                .find(|member| !member.you && member.guest == *guest)
        })
        .collect()
}

/// Los demás que hablan ahora, por sus nombres y en el orden en que empezaron.
pub fn talking_names(state: &State) -> Vec<&str> {
    talking_members(state)
        .into_iter()
        .map(|member| member.name.as_str())
        .collect()
}

/// Quién habla, debajo de la barra: una fila por persona con su avatar, cada una en su píldora.
/// La de quien empieza entra desde la izquierda (solo se dibuja desplazada: no mueve a los demás).
fn talking_view<'a>(
    app: &'a App,
    state: &'a State,
    motion: Motion,
) -> Option<Element<'a, Message>> {
    use coroom_ui::avatar::{SPEAKER_AVATAR, SPEAKER_NAME_MAX, SPEAKERS_MAX_ROWS, ringed};
    let people = talking_members(state);
    if people.is_empty() {
        return None;
    }
    let (shown, rest) = coroom_ui::avatar::speaker_rows(people.len(), SPEAKERS_MAX_ROWS);
    let line = |leading: Element<'a, Message>, label: String| -> Element<'a, Message> {
        container(
            row![
                leading,
                text(label)
                    .size(type_size::SECONDARY)
                    .font(coroom_ui::font::SEMIBOLD)
                    .class(TextClass::Overlay)
                    .wrapping(Wrapping::None),
            ]
            .spacing(space::XS)
            .align_y(Alignment::Center),
        )
        .padding(Padding {
            top: 3.0,
            right: 12.0,
            bottom: 3.0,
            left: 3.0,
        })
        .class(ContainerClass::OverlayPill)
        .into()
    };
    let mut column = Column::new().spacing(space::XS);
    for member in &people[..shown] {
        let ring = ringed(
            &app.theme,
            &member.name,
            avatars::of(app, member.avatar),
            SPEAKER_AVATAR,
            true,
        );
        let entered = state
            .talk_since
            .get(&member.guest)
            .map_or(1.0, |at| entered(motion, Token::Panel, *at, app.now));
        column = column.push(Shift::new(
            line(
                ring,
                ui::ellipsize(&member.name, SPEAKER_NAME_MAX).into_owned(),
            ),
            Vector::new(-SPEAKER_SLIDE * (1.0 - entered), 0.0),
        ));
    }
    if rest > 0 {
        let count = container(
            text(format!("+{rest}"))
                .size(type_size::CAPTION)
                .font(coroom_ui::font::SEMIBOLD)
                .class(TextClass::Overlay),
        )
        .center_x(Length::Fixed(
            SPEAKER_AVATAR + 2.0 * coroom_ui::avatar::RING_PADDING,
        ))
        .center_y(Length::Fixed(
            SPEAKER_AVATAR + 2.0 * coroom_ui::avatar::RING_PADDING,
        ));
        column = column.push(line(count.into(), t!("room-hablan-mas", n = rest)));
    }
    Some(column.into())
}

/// La latencia: el RTT con el anfitrión y su ruta («14 ms · Directa»), con su nombre al pasar el
/// ratón; un clic abre el panel en «Conexión» y, por el relay, el tramo «Relay · motivo» abre
/// el diagnóstico.
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
    // Las cifras («48 ms», «5 s») en ranuras de ancho fijo: la píldora no cambia de ancho a cada
    // medida. Un texto más largo que la muestra («Midiendo», un motivo del relay) ensancha la
    // ranura, pero esos cambian de tarde en tarde. Sin cuenta animada sobre el vídeo: cada cuadro
    // de la cuenta volvería a medir la interfaz y competiría con la entrega de los fotogramas.
    lead = lead.push(ui::figure(
        strong,
        pill.lead.clone(),
        &[stats::LATENCY_WIDEST],
    ));
    let reason_samples = [
        t!("room-perdida-valor", value = "88"),
        t!("room-segundos", secs = "88"),
    ];
    let reason = pill.reason.clone().map(|reason| {
        let samples: Vec<&str> = reason_samples.iter().map(String::as_str).collect();
        ui::figure_after(
            |content: String| {
                text(content)
                    .size(type_size::SECONDARY)
                    .class(TextClass::OverlaySecondary)
            },
            reason,
            &samples,
        )
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
            ui::with_tooltip(segment(lead.into(), connection), t!("room-latencia-tip")),
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
        ui::with_tooltip(segment(all.into(), connection), t!("room-latencia-tip"))
    };
    container(content)
        .padding(2)
        .height(Length::Fixed(PILL_HEIGHT))
        .center_y(Length::Fixed(PILL_HEIGHT))
        .class(ContainerClass::OverlayPill)
        .into()
}

/// Fila de un dato del panel: el valor, a la derecha, en una ranura del ancho de la más ancha de
/// `widest` (no cambia de ancho con cada medida).
fn stat_row<'a>(
    label: impl text::IntoFragment<'a>,
    value: String,
    widest: &[&str],
) -> Element<'a, Message> {
    row![
        overlay_text(label).width(Length::Fill),
        ui::figure(
            |content: String| {
                text(content)
                    .size(type_size::SECONDARY)
                    .font(coroom_ui::font::SEMIBOLD)
                    .class(TextClass::Overlay)
            },
            value,
            widest,
        )
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

/// Si `member` habla ahora («Hablando» en «Personas»). El anfitrión no devuelve a cada uno su
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

/// Orden de «Personas»: tú, el anfitrión y los demás por orden de llegada.
fn member_order(member: &RoomMember) -> u8 {
    if member.you {
        0
    } else if member.guest == 0 {
        1
    } else {
        2
    }
}

/// Lado del avatar en «Personas».
const MEMBER_AVATAR: f32 = 28.0;

/// Una persona de «Personas», en su caja de alto fijo: su avatar (con el anillo si habla), su
/// nombre, su papel con «Hablando» en una ranura reservada, el número de seguridad (del
/// anfitrión, con quien va el cifrado; su sitio queda en todas las filas) y su volumen debajo.
fn person_view<'a>(app: &'a App, state: &'a State, member: &'a RoomMember) -> Element<'a, Message> {
    let talks = member_talks(state, member);
    let role = if member.guest == 0 {
        t!("room-anfitrion")
    } else {
        t!("room-invitado")
    };
    let talking = || {
        overlay_text(format!(" · {}", t!("room-hablando")))
            .size(type_size::CAPTION)
            .wrapping(Wrapping::None)
    };
    let facts = row![
        overlay_text(role)
            .size(type_size::CAPTION)
            .wrapping(Wrapping::None),
        ui::reserve([talking().into()], talks.then(|| talking().into())),
    ];
    let name = if member.you {
        t!("room-tu-nombre", name = member.name.as_str())
    } else {
        member.name.clone()
    };
    let safety = (member.guest == 0 && !member.you && state.safety_number.is_some()).then(|| {
        bar_button(
            Icon::ShieldCheck,
            t!("room-comprobar-numero", name = member.name.as_str()),
            false,
            on(Msg::Safety),
        )
    });
    let head = row![
        avatars::ringed_view(
            app,
            &member.name,
            avatars::of_member(app, member.guest, member.you).or(member.avatar),
            MEMBER_AVATAR,
            talks,
        ),
        column![
            text(ui::ellipsize(&name, PERSON_NAME_MAX).into_owned())
                .size(type_size::BODY)
                .class(TextClass::Overlay)
                .wrapping(Wrapping::None),
            facts,
        ]
        .spacing(2)
        .width(Length::Fill),
        ui::reserved(size::CONTROL, size::CONTROL, safety),
    ]
    .spacing(space::S)
    .align_y(Alignment::Center);
    let volume = if member.you {
        own_volume_row(app, state)
    } else {
        volume_row(state, member)
    };
    container(column![head, volume].spacing(space::XS))
        .width(Length::Fill)
        .height(Length::Fixed(PERSON_HEIGHT))
        .clip(true)
        .into()
}

/// La voz propia: lo alto que te oyen los demás (de 0 a 200 % de 5 en 5; Ctrl+clic, 100 %), con
/// el micrófono que te silencia para todos delante. Sin voz (sin micrófono o sin permiso), por qué.
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
            if muted { Icon::MicOff } else { Icon::Mic },
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

/// Pestaña «Personas»: quién está en la sala, quién habla y cómo se le oye. En las filas de los
/// demás, el deslizador es lo alto que se le oye en este equipo y el altavoz deja de oírle aquí
/// (los demás le siguen oyendo); en la propia, lo alto que te oyen y el micrófono te silencia
/// para todos.
fn people_tab<'a>(app: &'a App, state: &'a State) -> Element<'a, Message> {
    let mut members: Vec<&RoomMember> = state.members.iter().collect();
    members.sort_by_key(|member| member_order(member));
    let rows = members
        .into_iter()
        .map(|member| person_view(app, state, member))
        .collect();
    let mut content = Column::new()
        .spacing(space::S)
        .padding(Padding::from([space::S, space::M]))
        .push(stat_section(t!("room-integrantes"), rows));
    if state.members.is_empty() {
        content = content.push(overlay_text(t!("room-esperando-la-lista")));
    }
    content = content.push(
        overlay_text(t!("room-solo-en-tu-equipo"))
            .size(type_size::CAPTION)
            .wrapping(Wrapping::WordOrGlyph),
    );
    scrollable(content).height(Length::Fill).into()
}

/// Mediana en ms de una latencia en µs, o «midiendo».
fn median(samples: usize, p50_us: u64) -> String {
    if samples > 0 {
        format!("{} ms", stats::number(p50_us as f64 / 1000.0, 0))
    } else {
        t!("room-midiendo").to_owned()
    }
}

/// Pestaña «Conexión»: la latencia con el anfitrión (la misma de la píldora, con su nivel), la
/// ruta y la pérdida; la latencia añadida por partes y la calidad.
fn connection_tab<'a>(app: &'a App, state: &'a State) -> Element<'a, Message> {
    let dark = dark(app);
    let summary = state.stats.as_ref().map(|stats| &stats.summary);
    let mut sections = Column::new()
        .spacing(space::M)
        .padding(Padding::from([space::S, space::M]));

    // La latencia con el anfitrión: el RTT con su nivel, como la píldora y Diagnóstico.
    let headline: Element<'a, Message> = match (state.rtt_ms(), state.level()) {
        (Some(rtt), Some(level)) => {
            let widest = LEVELS.map(|level| {
                t!(
                    "room-latencia-nivel",
                    value = stats::LATENCY_WIDEST,
                    level = level.label()
                )
            });
            let widest: Vec<&str> = widest.iter().map(String::as_str).collect();
            ui::with_tooltip(
                ui::figure(
                    |content: String| {
                        text(content)
                            .size(type_size::SECONDARY)
                            .font(coroom_ui::font::SEMIBOLD)
                            .class(TextClass::Custom(
                                dark.tone(quality_tone(level)).text.color(),
                            ))
                    },
                    t!(
                        "room-latencia-nivel",
                        value = stats::latency(rtt).0,
                        level = level.label()
                    ),
                    &widest,
                ),
                t!("room-latencia-tip-corto"),
            )
        }
        _ => overlay_text(t!("room-midiendo")).into(),
    };
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
                t!("room-perdida"),
                stats::percent(link.loss_percent),
                &["88,8 %"],
            ));
        }
        link_rows.push(stat_row(
            t!("room-fps-bitrate"),
            format!(
                "{} · {}",
                stats::number(summary.fps_recent, 0),
                stats::kbps(summary.bitrate_recent_kbps)
            ),
            &["888 · 888 kbit/s", "888 · 88,8 Mbit/s", "888 · 888 Mbit/s"],
        ));
    }
    let mut latency = column![section_title(
        t!("room-latencia-con-el-anfitrion"),
        Some(headline)
    )]
    .spacing(6);
    for row in link_rows {
        latency = latency.push(row);
    }
    sections = sections.push(latency);

    // Latencia añadida: de tu ratón al juego, del juego a tu pantalla y el audio del juego.
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
    sections = sections.push(stat_section(
        t!("room-latencia-anadida"),
        vec![
            stat_row(t!("room-entrada-raton-juego"), input, &["888 ms"]),
            stat_row(t!("room-video-captura-pantalla"), video, &["888 ms"]),
            stat_row(t!("room-audio"), audio, &["888 ms"]),
        ],
    ));

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
    sections = sections.push(
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
        .align_y(Alignment::Center),
    );

    scrollable(sections).height(Length::Fill).into()
}

/// El panel lateral: pestañas Chat · Personas · Conexión (las de los botones de la barra, en su
/// orden; el atajo de cada una en su ayuda) y cerrar.
fn side_panel<'a>(app: &'a App, state: &'a State, tab: Tab) -> Element<'a, Message> {
    let keys = &app.settings.input.shortcuts;
    let tab_button = |this: Tab| -> Element<'a, Message> {
        let (label, letter) = match this {
            Tab::Chat => (t!("room-tab-chat"), keys.chat),
            Tab::People => (t!("room-tab-personas"), keys.pads),
            Tab::Connection => (t!("room-tab-conexion"), keys.stats),
        };
        ui::with_tooltip(
            button(
                text(label)
                    .size(type_size::SECONDARY)
                    .font(coroom_ui::font::SEMIBOLD)
                    .wrapping(Wrapping::None),
            )
            .padding(Padding::from([5.0, 10.0]))
            .class(ButtonClass::Overlay {
                active: tab == this,
            })
            .on_press(on(Msg::ShowTab(this))),
            format!("Ctrl+Alt+{letter}"),
        )
    };
    let tabs =
        container(iced_widget::Row::with_children(Tab::ALL.into_iter().map(tab_button)).spacing(2))
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
        Tab::People => people_tab(app, state),
        Tab::Connection => connection_tab(app, state),
    };
    container(column![header, content].height(Length::Fill))
        .width(Length::Fixed(size::STATS_PANEL))
        .height(Length::Fill)
        .class(ContainerClass::Overlay)
        .into()
}

/// Una capa de toda la superficie del vídeo, vacía: el fundido de la imagen o el borde.
fn veil<'a>(class: ContainerClass) -> Element<'a, Message> {
    container(iced_widget::Space::new())
        .width(Length::Fill)
        .height(Length::Fill)
        .class(class)
        .into()
}

fn video<'a>(app: &'a App, state: &'a State) -> Element<'a, Message> {
    let now = app.now;
    // Con la entrada tomada no se anima nada: todo aparece y se va de golpe.
    let motion = state.motion(app.motion());
    let immersive = state.immersive;
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
    // Las capas están siempre, en este orden, vacías cuando no hacen falta: iced casa el estado
    // de cada una (el scroll del panel, el foco del chat, un deslizador que se arrastra) con su
    // posición en la pila, y una capa que aparecía delante (quién habla) dejaba el panel arriba,
    // el chat sin foco y cortaba el arrastre.

    // El fundido con que aparece la imagen al entrar.
    let cover = state
        .cover(now, motion)
        .map(|left| veil(ContainerClass::VideoCover(left)));
    // Con el teclado y el ratón en la sala y la ventana normal, el borde del vídeo en el acento.
    let focus = (state.forwarding && !immersive && !app.fullscreen)
        .then(|| veil(ContainerClass::VideoFocus));
    // La barra baja deslizándose y sube al irse (solo se dibuja desplazada); el aviso de
    // reconexión va con ella.
    let bar_hidden = state.bar_hidden(now, motion).filter(|_| !immersive);
    let lift = |hidden: f32| Vector::new(0.0, -(space::S + size::VIDEO_BAR) * hidden);
    let bar = bar_hidden.map(|hidden| {
        let reconnect = state.reconnecting.as_ref().map(|reconnecting| {
            container(
                container(reconnect_banner(app, reconnecting))
                    .center_x(Length::Fill)
                    .max_width(640),
            )
            .padding(Padding {
                top: space::S,
                ..Padding::ZERO
            })
            .into()
        });
        container(Shift::new(
            column![top_bar(app, state), ui::slot(reconnect)],
            lift(hidden),
        ))
        .width(Length::Fill)
        .padding(Padding {
            top: space::S,
            right: space::S,
            bottom: 0.0,
            left: space::S,
        })
        .into()
    });
    // La latencia, en el sitio que le guarda la barra: en ventana se queda aunque la barra se
    // vaya; a pantalla completa va y viene con ella (o se enseña quieta un momento si cambia el
    // nivel); en modo inmersivo, nunca.
    let pill_shift = if immersive {
        None
    } else if !app.fullscreen || state.pill_flash(now) {
        Some(0.0)
    } else {
        bar_hidden
    };
    let pill = pill_shift.map(|hidden| {
        container(Shift::new(pill_view(app, state), lift(hidden)))
            .padding(Padding {
                top: space::S + (size::VIDEO_BAR - PILL_HEIGHT) / 2.0,
                left: 2.0 * space::S,
                ..Padding::ZERO
            })
            .into()
    });
    // Quién habla, debajo del sitio de la barra (no sube ni baja con ella), recortado: la fila
    // que entra no se dibuja sobre el riel.
    let talking = (!immersive && app.settings.room.talking_overlay)
        .then(|| talking_view(app, state, motion))
        .flatten()
        .map(|talking| {
            container(talking)
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(Padding {
                    top: BELOW_BAR,
                    right: space::S,
                    bottom: space::S,
                    left: space::S,
                })
                .clip(true)
                .into()
        });
    // El panel entra desde la derecha encima del vídeo y sale hacia ella.
    let panel = (!immersive)
        .then(|| state.panel_drawn(now, motion))
        .flatten()
        .map(|(tab, hidden)| {
            container(Shift::new(
                side_panel(app, state, tab),
                Vector::new((size::STATS_PANEL + space::S) * hidden, 0.0),
            ))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Horizontal::Right)
            .padding(Padding {
                top: BELOW_BAR,
                right: space::S,
                bottom: space::S,
                left: space::S,
            })
            .into()
        });
    // Abajo al centro, el aviso (3 s) o la indicación del teclado y el ratón; con el panel
    // abierto, centrados en lo que queda a su izquierda.
    let bottom = match state.notice(now) {
        Some(notice) => Some(notice_view(app, notice)),
        None => state
            .pill_visible(now, app.fullscreen)
            .then(|| input_hint_view(app, state))
            .flatten(),
    }
    .map(|content| {
        let right = if state.panel.is_some() && !immersive {
            size::STATS_PANEL + 2.0 * space::S
        } else {
            space::S
        };
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Bottom)
            .padding(Padding {
                right,
                ..Padding::new(space::S)
            })
            .into()
    });
    let bubbles = (!immersive && state.panel.is_none())
        .then(|| chat::bubbles(app))
        .flatten()
        .map(|bubbles| {
            container(bubbles)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Horizontal::Right)
                .align_y(Vertical::Bottom)
                .padding(space::S)
                .into()
        });
    // El menú del sonido, bajo su botón: a su derecha, el hueco de lo que hay en la barra a la
    // derecha del grupo de voz.
    let menu = bar_hidden.filter(|_| state.sound_menu).map(|hidden| {
        container(Shift::new(
            row![
                ui::fill(),
                sound_menu(state),
                ui::reserve([bar_tail(app, state)], None),
            ]
            .spacing(space::S),
            lift(hidden),
        ))
        .width(Length::Fill)
        .padding(Padding {
            top: space::S + size::VIDEO_BAR + space::XS,
            right: 2.0 * space::S,
            bottom: 0.0,
            left: space::S,
        })
        .into()
    });
    stack![
        base,
        ui::slot(cover),
        ui::slot(focus),
        ui::slot(talking),
        ui::slot(bar),
        ui::slot(pill),
        ui::slot(panel),
        ui::slot(bottom),
        ui::slot(bubbles),
        ui::slot(menu),
    ]
    .into()
}

/// El pie del riel en la sala de otro: la sala, tu micrófono (silenciarte sin volver al vídeo) y,
/// desde otra página, «Volver a la sala». Plegado, solo los iconos. Todo en cajas de tamaño fijo:
/// un cambio del micrófono o salir del vídeo no mueve nada.
pub fn rail_footer<'a>(app: &'a App, state: &'a State, wide: bool) -> Element<'a, Message> {
    let stats = state.stats.as_ref();
    let voice = stats.is_some_and(|s| s.voice || s.mic_muted);
    let muted = stats.is_some_and(|s| s.mic_muted);
    let (mic_icon, mic_label, mic_tip) = if !voice {
        (Icon::MicOff, t!("app-sin-microfono"), t!("room-sin-voz"))
    } else if muted {
        (
            Icon::MicOff,
            t!("app-microfono-silenciado"),
            t!("app-activar-tu-microfono"),
        )
    } else {
        (
            Icon::Mic,
            t!("app-microfono-abierto"),
            t!("app-silenciarte"),
        )
    };
    let mic_tint = if voice && muted {
        SvgClass::Tone(Tone::Error)
    } else {
        SvgClass::Text
    };
    let on_mic = voice.then(|| on(Msg::ToggleMic));
    let away = app.away_from_room();
    let back = || Message::Navigate(Page::Host);
    let title = room_title(state);
    let line = |icon: Icon, tint: SvgClass, label: Element<'a, Message>| -> Element<'a, Message> {
        row![icon_with(icon, size::ICON, tint), label]
            .spacing(space::XS)
            .align_y(Alignment::Center)
            .into()
    };
    let (room, mic, back): (
        Element<'a, Message>,
        Element<'a, Message>,
        Option<Element<'a, Message>>,
    ) = if wide {
        let room = container(line(
            Icon::Monitor,
            SvgClass::Secondary,
            ui::one_line(title.clone(), ui::badge_text).into(),
        ))
        .width(Length::Fill)
        .height(Length::Fixed(size::CONTROL))
        .padding(Padding::from([0.0, space::S]))
        .center_y(Length::Fixed(size::CONTROL));
        let mic = coroom_ui::animate::button(line(
            mic_icon,
            mic_tint,
            ui::one_line(mic_label, ui::badge_text).into(),
        ))
        .width(Length::Fill)
        .height(Length::Fixed(size::CONTROL))
        .padding(Padding::from([0.0, space::S]))
        .class(ButtonClass::Flat)
        .on_press_maybe(on_mic);
        let back = away.then(|| {
            coroom_ui::animate::button(line(
                Icon::ArrowLeft,
                SvgClass::Accent,
                ui::one_line(t!("room-volver-a-la-sala"), ui::badge_text).into(),
            ))
            .width(Length::Fill)
            .height(Length::Fixed(size::CONTROL))
            .padding(Padding::from([0.0, space::S]))
            .class(ButtonClass::Flat)
            .on_press(back())
            .into()
        });
        (
            ui::with_tooltip(room, title.clone()),
            ui::with_tooltip(mic, mic_tip),
            back,
        )
    } else {
        let room = container(icon_with(Icon::Monitor, size::ICON, SvgClass::Secondary))
            .center_x(Length::Fixed(size::CONTROL))
            .center_y(Length::Fixed(size::CONTROL));
        let mic = coroom_ui::animate::button(center(icon_with(mic_icon, size::ICON, mic_tint)))
            .width(Length::Fixed(size::CONTROL))
            .height(Length::Fixed(size::CONTROL))
            .padding(0)
            .class(ButtonClass::Flat)
            .press_scale(true)
            .on_press_maybe(on_mic);
        let back = away.then(|| {
            ui::rail_tooltip(
                coroom_ui::animate::button(center(icon_with(
                    Icon::ArrowLeft,
                    size::ICON,
                    SvgClass::Accent,
                )))
                .width(Length::Fixed(size::CONTROL))
                .height(Length::Fixed(size::CONTROL))
                .padding(0)
                .class(ButtonClass::Flat)
                .press_scale(true)
                .on_press(back()),
                t!("room-volver-a-la-sala"),
            )
        });
        (
            ui::rail_tooltip(room, title.clone()),
            ui::rail_tooltip(mic, mic_tip),
            back,
        )
    };
    let width = if wide {
        ui::RAIL_WIDE - 2.0 * space::XS
    } else {
        ui::RAIL_ITEM
    };
    column![
        room,
        container(mic).id(iced_widget::Id::new(RAIL_MIC_ID)),
        ui::reserved(width, size::CONTROL, back),
        iced_widget::rule::horizontal(1),
    ]
    .spacing(2)
    .width(Length::Fixed(width))
    .align_x(Alignment::Center)
    .into()
}

/// Escenas del invitado para las capturas.
pub(crate) mod shots {
    use std::time::{Duration, Instant};

    use coroom_session::input::gamepad::{ConnectedPad, PadChoice, PadKey, PadKind};
    use coroom_session::latency::{AudioSummary, InputLatency, LatencyLedger, LinkSummary, Stage};
    use coroom_session::remote::{QualityLevel, RoomMember, SessionEvent};
    use coroom_transport::quic::Path;

    use super::{Msg, NOTICE, Phase, Stats, Tab};
    use crate::ui::app::{App, Event, Message, Page};
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
            // La barra ya entró y la imagen ya apareció.
            state.bar_shown_at = app.now - Duration::from_secs(1);
            state.video_at = Some(app.now - Duration::from_secs(1));
        }
    }

    /// Lo que entra (el panel, las filas de quién habla, la barra, la imagen) ya entró, y lo que
    /// salía ya salió.
    fn settled(app: &mut App) {
        let second = Duration::from_secs(1);
        if let Some(state) = app.guest.as_mut() {
            state.panel_at -= second;
            state.bar_shown_at -= second;
            state.closing = None;
            if let Some(at) = state.video_at.as_mut() {
                *at -= second;
            }
            for at in state.talk_since.values_mut() {
                *at -= second;
            }
        }
    }

    /// Pasan `elapsed` sin eventos (los avisos y las entradas cuentan desde antes).
    fn later(app: &mut App, elapsed: Duration) {
        if let Some(state) = app.guest.as_mut() {
            let back = |at: &mut std::time::Instant| *at -= elapsed;
            back(&mut state.bar_until);
            back(&mut state.bar_shown_at);
            back(&mut state.panel_at);
            if let Some((_, at)) = state.notice.as_mut() {
                back(at);
            }
            for at in state.talk_since.values_mut() {
                back(at);
            }
        }
    }

    /// El ratón lleva quieto más de 2,5 s (y la barra ya terminó de irse).
    fn still(app: &mut App) {
        if let Some(state) = app.guest.as_mut() {
            state.bar_until = app.now - Duration::from_secs(1);
        }
    }

    pub(crate) fn guest_video(app: &mut App) {
        video(app);
    }

    /// Recién tomados el teclado y el ratón: el aviso grande dice cómo soltarlos y el borde del
    /// vídeo va en el acento.
    pub(crate) fn guest_video_forwarding(app: &mut App) {
        video(app);
        app.update(Message::Guest(Msg::ForwardingChanged(true)));
        still(app);
    }

    /// Con el teclado y el ratón tomados pasados los 3 s: la indicación de siempre y el borde.
    pub(crate) fn guest_video_forwarding_later(app: &mut App) {
        guest_video_forwarding(app);
        later(app, NOTICE);
    }

    /// Recién soltados.
    pub(crate) fn guest_video_released(app: &mut App) {
        guest_video_forwarding(app);
        later(app, NOTICE);
        app.update(Message::Guest(Msg::ForwardingChanged(false)));
    }

    /// Recién entrado en modo inmersivo: solo el aviso de cómo salir.
    pub(crate) fn guest_immersive_notice(app: &mut App) {
        video(app);
        app.update(Message::Guest(Msg::ForwardingChanged(true)));
        app.update(Message::Guest(Msg::ToggleImmersive));
        app.update(Message::FullscreenChanged(true));
    }

    /// En modo inmersivo pasados los 3 s: nada de interfaz sobre el juego.
    pub(crate) fn guest_immersive(app: &mut App) {
        guest_immersive_notice(app);
        later(app, NOTICE);
    }

    /// El menú del botón de sonido, con el volumen del juego.
    pub(crate) fn guest_sound_menu(app: &mut App) {
        video(app);
        app.update(Message::Guest(Msg::GameVolume(70)));
        app.update(Message::Guest(Msg::ToggleSoundMenu));
    }

    /// Desde Inicio sin salir de la sala: el pie del riel con la sala, el micrófono y «Volver a la
    /// sala».
    pub(crate) fn guest_away_home(app: &mut App) {
        video(app);
        app.update(Message::Navigate(Page::Home));
        app.page_change = None;
    }

    /// Desde Ajustes → Mandos: los mandos conectados de la sala para elegir cuál se envía.
    pub(crate) fn guest_away_pads(app: &mut App) {
        guest_people(app);
        app.update(Message::Navigate(Page::Settings));
        app.settings_page.page = crate::ui::screens::settings::SettingsPage::Gamepads;
        app.page_change = None;
        app.toasts.clear();
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
        settled(app);
    }

    /// Quién está en la sala de las escenas del chat (sin pasar por `Members`, que escribiría en
    /// el chat quién entra): así cada mensaje lleva el avatar de su autor.
    fn chat_members(app: &mut App) {
        if let Some(state) = app.guest.as_mut() {
            state.members = vec![
                member(0, 0, "Bea", false),
                member(1, 1, "Ana", true),
                member(2, 2, "Carla", false),
            ];
        }
    }

    pub(crate) fn guest_chat(app: &mut App) {
        video(app);
        chat_members(app);
        chat::shots::conversation(app);
        app.update(Message::Guest(Msg::TogglePanel(Tab::Chat)));
        app.chat.composing = false;
        settled(app);
    }

    pub(crate) fn guest_bubbles(app: &mut App) {
        video(app);
        chat_members(app);
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

    /// Pestaña «Personas»: quién está (a una no se la oye), quién habla, el volumen de cada uno y
    /// el número de seguridad del anfitrión; con dos mandos conectados (para Ajustes → Mandos).
    pub(crate) fn guest_people(app: &mut App) {
        video(app);
        if let Some(stats) = app.guest.as_mut().and_then(|state| state.stats.as_mut()) {
            stats.allowed.gamepad_slot = Some(2);
        }
        app.update(Message::Event(Event::Guest(SessionEvent::Members(vec![
            member(0, 0, "Bea", false),
            member(1, 1, "Ana", true),
            member(2, 2, "Carla", false),
            member(3, 3, "Dani", false),
        ]))));
        app.update(Message::Guest(Msg::Hear(2, false)));
        app.update(Message::Guest(Msg::Volume(0, 150)));
        let first = sample_pad(0x1234, 0x5678, "Mando de ejemplo", PadKind::Hid, true);
        let second = sample_pad(0x1234, 0x9abc, "Mando de ejemplo 2", PadKind::XInput, false);
        app.settings.input.pad = PadChoice::Pad(first.key.clone());
        app.update(Message::Guest(Msg::Pads(vec![first.clone()], true)));
        app.update(Message::Guest(Msg::Pads(vec![first, second], true)));
        speaking(app, &["anfitrión"]);
        if let Some(stats) = app.guest.as_mut().and_then(|state| state.stats.as_mut()) {
            stats.voice = true;
        }
        app.update(Message::Guest(Msg::MicVolume(120)));
        app.update(Message::Guest(Msg::ShowTab(Tab::People)));
        app.toasts.clear();
        settled(app);
    }

    /// Integrante de ejemplo (con el usuario y el avatar de la lista de amigos de ejemplo).
    fn member(guest: u32, leaf: u32, name: &str, you: bool) -> RoomMember {
        RoomMember {
            guest,
            leaf,
            name: name.to_owned(),
            you,
            user: match name {
                "Ana" => 1,
                "Bea" => 2,
                "Carla" => 3,
                _ => 4,
            },
            avatar: crate::ui::shots::sample_avatar(name),
        }
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

    /// El indicador de quién habla sobre el vídeo (con la barra escondida): tres personas, una
    /// fila cada una con su avatar (Carla sin imagen: sus iniciales).
    pub(crate) fn guest_talking(app: &mut App) {
        guest_people(app);
        app.update(Message::Event(Event::Guest(SessionEvent::Members(vec![
            member(0, 0, "Bea", false),
            member(1, 1, "Ana", true),
            member(2, 2, "Carla", false),
            member(3, 3, "Dani", false),
        ]))));
        speaking(app, &["invitado 3"]);
        let stats = app.guest.as_ref().and_then(|state| state.stats.clone());
        if let Some(stats) = stats {
            app.update(Message::Event(Event::GuestStats(Box::new(stats))));
        }
        speaking(app, &["anfitrión", "invitado 2", "invitado 3"]);
        app.update(Message::Guest(Msg::ClosePanel));
        still(app);
        settled(app);
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
        ("13-video-estrecho", |app| {
            guest_people(app);
            app.update(Message::Guest(Msg::ToggleSoundMenu));
            app.window = iced_core::Size::new(
                crate::ui::shots::NARROW.0 as f32,
                crate::ui::shots::NARROW.1 as f32,
            );
            settled(app);
        }),
        ("13-video-ancho", |app| {
            video(app);
            app.window = iced_core::Size::new(
                crate::ui::shots::WIDE.0 as f32,
                crate::ui::shots::WIDE.1 as f32,
            );
        }),
        ("13-video-reenviando", guest_video_forwarding),
        ("13-video-reenviando-despues", guest_video_forwarding_later),
        ("13-video-soltaste", guest_video_released),
        ("13-video-sonido", guest_sound_menu),
        ("13-inmersivo-aviso", guest_immersive_notice),
        ("13-inmersivo", guest_immersive),
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
        ("14-personas", guest_people),
        ("16-fuera-de-la-sala", guest_away_home),
        ("16-fuera-de-la-sala-mandos", guest_away_pads),
        ("15-reconectando", guest_reconnecting),
        ("15-relay-cgnat", guest_relay_cgnat),
    ];
}

#[cfg(test)]
mod tests {
    /// Con la transmisión delante el escritorio no se bloquea; en una sala solo de voz, sí.
    #[test]
    fn the_screen_does_not_lock_while_watching_a_stream() {
        let mut app = app();
        shots::joined(&mut app, false, true);
        app.update(Message::Event(Event::Guest(SessionEvent::Sharing(true))));
        app.now += ESTIMATE_DWELL;
        app.update(Message::Tick);
        assert!(app.video_in_front());
        assert!(app.wants_idle_inhibit());
        app.update(Message::Event(Event::Guest(SessionEvent::Sharing(false))));
        assert!(
            !app.wants_idle_inhibit(),
            "sin nada compartido no hay qué mirar"
        );
    }

    /// Cerrar la ventana (o `Ctrl+Q`) dentro de una sala pregunta antes, como `Esc`; al
    /// confirmar, sale de la sala y cierra.
    #[test]
    fn quitting_inside_a_room_asks_first() {
        let mut app = app();
        shots::joined(&mut app, false, true);
        assert!(app.in_room_as_guest());
        let effects = app.update(Message::Shortcut(crate::ui::app::Shortcut::Quit));
        assert!(effects.is_empty(), "{effects:?}");
        assert!(matches!(
            app.dialog,
            Some(crate::ui::app::Dialog::QuitWhileGuest)
        ));
        let effects = app.update(Message::ConfirmDialog);
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Leave)),
            "{effects:?}"
        );
        assert!(matches!(effects.last(), Some(Effect::Quit)), "{effects:?}");
    }

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
        assert!(state.animating(Motion::FULL), "y entra con su animación");
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
        // El RTT con el anfitrión, con la escala de la sala (13,8 ms es «Excelente», como la ve
        // el anfitrión en tu fila), no la latencia añadida del vídeo (48 ms).
        assert_eq!(
            pill_label(&app),
            ("14 ms · Directa".to_owned(), Some(Tone::Success))
        );
        assert_eq!(
            app.guest.as_ref().and_then(State::level),
            Some(stats::latency(13.8).1)
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
        set_added(&mut app, 82, 0.2, 61.0);
        assert_eq!(
            pill_label(&app),
            ("61 ms · Relay · CGNAT".to_owned(), Some(Tone::Warning))
        );
        // Mala: el motivo pasa a ser lo que más duele.
        set_added(&mut app, 131, 4.0, 95.0);
        assert_eq!(
            pill_label(&app),
            ("95 ms · Relay · pérdida 4 %".to_owned(), Some(Tone::Error))
        );
        set_added(&mut app, 131, 0.2, 95.0);
        assert_eq!(pill_label(&app).0, "95 ms · Relay · CGNAT");
        // Reconectando: gris, con la espera.
        app.update(Message::Event(Event::Guest(SessionEvent::Reconnecting {
            attempt: 1,
            of: 5,
            retry_in: Duration::from_millis(2_400),
            reason: "red".into(),
        })));
        assert_eq!(pill_label(&app), ("Reconectando · 3 s".to_owned(), None));
        // Sin medida del enlace: «Midiendo», nunca vacía.
        let mut app = watching();
        let mut stats = shots::sample_stats();
        stats.summary.link = None;
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

    /// El aviso del teclado y el ratón: solo con ese permiso, dice cómo soltarlos mientras se
    /// reenvían y cómo volver a usarlos sueltos; a pantalla completa se va con la píldora.
    #[test]
    fn the_input_hint_follows_the_permission_and_the_capture() {
        let mut app = watching();
        let hint = |app: &App| input_hint(app, app.guest.as_ref().expect("invitado"));
        assert_eq!(
            hint(&app).as_deref(),
            Some("Haz clic en la imagen para usar el teclado y el ratón"),
            "con el permiso recién dado, aún no se reenvía"
        );
        app.update(Message::Guest(Msg::ForwardingChanged(true)));
        assert_eq!(
            hint(&app).as_deref(),
            Some("Ctrl+Alt+R suelta el teclado y el ratón")
        );
        app.settings.input.shortcuts.release = 'T';
        assert!(hint(&app).is_some_and(|text| text.starts_with("Ctrl+Alt+T ")));
        // Sin el permiso (solo mirar o solo mando), nada.
        let mut stats = shots::sample_stats();
        stats.allowed.keyboard_mouse = false;
        stats.allowed.gamepad_slot = Some(1);
        app.update(Message::Event(Event::GuestStats(Box::new(stats))));
        assert_eq!(hint(&app), None);
        // A pantalla completa, como la píldora: solo con la barra.
        let mut app = watching();
        app.now += motion::BAR_HIDE + Duration::from_millis(1);
        let state = app.guest.as_ref().expect("invitado");
        assert!(input_hint(&app, state).is_some());
        assert!(!state.pill_visible(app.now, true));
        assert!(state.pill_visible(app.now, false));
        // Sin nada compartido no hay imagen en la que hacer clic.
        app.update(Message::Event(Event::Guest(SessionEvent::Sharing(false))));
        assert_eq!(hint(&app), None);
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
        // Otra pestaña: el teclado vuelve al juego. «Personas» (`Ctrl+Alt+J`).
        assert_eq!(
            app.update(Message::Guest(Msg::TogglePeople)),
            [Effect::FocusChat(false)]
        );
        assert_eq!(app.guest.as_ref().and_then(|g| g.panel), Some(Tab::People));
        assert!(!app.chat.open && !app.chat.composing);
        app.update(Message::Guest(Msg::TogglePeople));
        assert_eq!(app.guest.as_ref().and_then(|g| g.panel), None);
        // El panel abierto mantiene la barra.
        app.update(Message::Guest(Msg::TogglePanel(Tab::Connection)));
        app.now += motion::BAR_HIDE * 2;
        assert!(app.guest.as_ref().is_some_and(|g| g.bar_visible(app.now)));
        app.update(Message::Guest(Msg::ClosePanel));
        assert_eq!(app.guest.as_ref().and_then(|g| g.panel), None);
        // Un botón por pestaña en la barra, en el orden del panel.
        assert_eq!(Tab::ALL, [Tab::Chat, Tab::People, Tab::Connection]);
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
            user: u64::from(guest) + 1,
            avatar: None,
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
        // Sin los avisos de quien entra y sale: aquí solo cuentan los efectos de la voz.
        app.settings.notifications.volume = 0;
        shots::video(&mut app);
        let member = |leaf: u32, name: &str, you: bool| RoomMember {
            guest: leaf,
            leaf,
            name: name.to_owned(),
            you,
            user: u64::from(leaf) + 1,
            avatar: None,
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
                user: 1,
                avatar: None,
            },
            RoomMember {
                guest: 1,
                leaf: 1,
                name: "Ana".to_owned(),
                you: true,
                user: 2,
                avatar: None,
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
        assert!(
            !state.animating(Motion::FULL),
            "los pasos no se animan a cada refresco"
        );
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
        assert!(!state.animating(Motion::FULL));
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

    #[test]
    fn whoever_starts_talking_goes_last_and_the_rest_keep_their_place() {
        assert_eq!(talk_order(&[], &[]), Vec::<u32>::new());
        assert_eq!(
            talk_order(&[], &[0, 2]),
            [0, 2],
            "a la vez, en el orden de la sala"
        );
        assert_eq!(
            talk_order(&[2], &[0, 2]),
            [2, 0],
            "quien ya hablaba sigue delante"
        );
        assert_eq!(talk_order(&[2, 0], &[0, 3]), [0, 3], "quien calla sale");
        assert_eq!(talk_order(&[3, 0], &[0, 2, 3]), [3, 0, 2]);
    }

    /// Tres hablan, uno a uno: una fila cada uno en el orden en que empezaron, sin uno mismo; con
    /// más de los que caben, la última fila dice cuántos más.
    #[test]
    fn the_talking_indicator_has_a_row_per_person_in_the_order_they_started() {
        let mut app = app();
        shots::video(&mut app);
        let member = |guest: u32, name: &str, you: bool| RoomMember {
            guest,
            leaf: guest,
            name: name.to_owned(),
            you,
            user: u64::from(guest) + 10,
            avatar: None,
        };
        app.update(Message::Event(Event::Guest(SessionEvent::Members(vec![
            member(0, "Bea", false),
            member(1, "Ana", true),
            member(2, "Carla", false),
            member(3, "Dani", false),
            member(4, "Eli", false),
            member(5, "Fran", false),
        ]))));
        let talk = |app: &mut App, who: &[&str]| {
            let mut stats = app
                .guest
                .as_ref()
                .and_then(|state| state.stats.clone())
                .expect("estadísticas");
            stats.summary.audio = Some(coroom_session::latency::AudioSummary {
                tracks: Vec::new(),
                av: None,
                speaking: who.iter().map(|name| (*name).to_owned()).collect(),
                status: String::new(),
            });
            stats.speaking = true;
            app.update(Message::Event(Event::GuestStats(Box::new(stats))));
            talking_names(app.guest.as_ref().expect("invitado"))
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert!(talk(&mut app, &[]).is_empty());
        assert_eq!(talk(&mut app, &["invitado 3"]), ["Dani"]);
        assert_eq!(
            talk(&mut app, &["anfitrión", "invitado 3"]),
            ["Dani", "Bea"]
        );
        // Uno mismo habla (y el anfitrión le cuenta): no sale, ya lo sabe.
        assert_eq!(
            talk(
                &mut app,
                &["anfitrión", "invitado 1", "invitado 2", "invitado 3"]
            ),
            ["Dani", "Bea", "Carla"]
        );
        assert_eq!(
            talk(&mut app, &["anfitrión", "invitado 2"]),
            ["Bea", "Carla"]
        );
        let state = app.guest.as_ref().expect("invitado");
        assert!(
            talking_view(&app, state, Motion::FULL).is_some(),
            "con gente hablando, se ve"
        );
        let five = talk(
            &mut app,
            &[
                "anfitrión",
                "invitado 2",
                "invitado 3",
                "invitado 4",
                "invitado 5",
            ],
        );
        assert_eq!(five, ["Bea", "Carla", "Dani", "Eli", "Fran"]);
        assert_eq!(
            coroom_ui::avatar::speaker_rows(five.len(), coroom_ui::avatar::SPEAKERS_MAX_ROWS),
            (3, 2),
            "tres filas y «y 2 más»"
        );
        assert_eq!(t!("room-hablan-mas", n = 2), "y 2 más");
        assert!(talk(&mut app, &[]).is_empty());
        let state = app.guest.as_ref().expect("invitado");
        assert!(
            talking_view(&app, state, Motion::FULL).is_none(),
            "nadie habla: no se ve"
        );
    }

    /// Desde el riel se va a otras páginas sin salir de la sala: la sesión, el vídeo y el chat
    /// siguen; el teclado, el ratón y los mandos dejan de enviarse; al volver, todo como estaba.
    #[test]
    fn the_rail_leaves_the_video_without_leaving_the_room() {
        let mut app = app();
        shots::video(&mut app);
        assert_eq!((app.screen, app.page), (Screen::Guest, Page::Host));
        assert!(app.video_in_front() && !app.away_from_room());
        assert_eq!(
            sent_pad_slot(&app, Some(2)),
            Some(2),
            "en la sala, el mando va"
        );
        let feed = Arc::clone(&app.guest.as_ref().expect("invitado").feed);
        for page in [Page::Home, Page::Friends, Page::Settings, Page::Account] {
            let effects = app.update(Message::Navigate(page));
            assert!(
                !effects
                    .iter()
                    .any(|e| matches!(e, Effect::Leave | Effect::SetImmersive(true))),
                "{page:?}: no sale de la sala ({effects:?})"
            );
            assert_eq!((app.screen, app.page), (Screen::Main, page));
            assert!(app.in_room_as_guest() && app.away_from_room(), "{page:?}");
            // La entrada no se envía (la ventana suelta lo pulsado al dejar de estar delante).
            assert!(!app.video_in_front(), "{page:?}: el vídeo no está delante");
            assert_eq!(sent_pad_slot(&app, Some(2)), None, "{page:?}: ni el mando");
        }
        // La sala sigue: llegan estadísticas y mensajes, el vídeo se sigue recibiendo.
        set_added(&mut app, 48, 0.1, 22.0);
        assert_eq!(app.guest.as_ref().and_then(State::rtt_ms), Some(22.0));
        feed.push(frame(), None, None);
        assert!(feed.has_frame(), "el vídeo sigue llegando");
        app.update(Message::Event(Event::Guest(SessionEvent::ChatMessage(
            chat::shots::line(0, "Bea", "¿seguís ahí?", 1, false),
        ))));
        assert_eq!(
            app.chat.unread, 1,
            "el chat sigue, sin leer desde otra página"
        );
        // Volver: por «Sala» del riel, `Ctrl+3` o atrás.
        app.update(Message::Navigate(Page::Host));
        assert_eq!((app.screen, app.page), (Screen::Guest, Page::Host));
        assert!(app.video_in_front());
        assert_eq!(sent_pad_slot(&app, Some(2)), Some(2));
        assert_eq!(
            app.guest.as_ref().map(|g| g.phase),
            Some(Phase::Video),
            "al volver, la imagen sin esperar"
        );
        app.update(Message::Shortcut(crate::ui::app::Shortcut::Go(
            Page::Friends,
        )));
        assert_eq!((app.screen, app.page), (Screen::Main, Page::Friends));
        app.update(Message::Shortcut(crate::ui::app::Shortcut::Back));
        assert_eq!(
            (app.screen, app.page),
            (Screen::Guest, Page::Host),
            "atrás vuelve a la sala"
        );
        // Salir sigue siendo solo con «Salir».
        assert!(app.guest.is_some());
    }

    /// Un atajo del panel desde otra página vuelve a la sala con el panel abierto; con el chat
    /// abierto, lo que llegó mientras tanto queda leído al volver.
    #[test]
    fn panel_shortcuts_from_another_page_return_to_the_room() {
        let mut app = watching();
        app.update(Message::Guest(Msg::TogglePanel(Tab::Chat)));
        app.update(Message::Navigate(Page::Home));
        assert!(
            !chat::visible(&app),
            "desde otra página el chat no está a la vista"
        );
        app.update(Message::Event(Event::Guest(SessionEvent::ChatMessage(
            chat::shots::line(0, "Bea", "hola", 1, false),
        ))));
        assert_eq!(app.chat.unread, 1);
        app.update(Message::Navigate(Page::Host));
        assert_eq!(
            app.chat.unread, 0,
            "con el chat abierto, al volver se da por leído"
        );
        app.update(Message::Navigate(Page::Settings));
        app.update(Message::Guest(Msg::ToggleStats));
        assert_eq!(app.screen, Screen::Guest);
        assert_eq!(
            app.guest.as_ref().and_then(|g| g.panel),
            Some(Tab::Connection)
        );
    }

    /// Dejar el vídeo en modo inmersivo devuelve el ratón; con el campo del chat escribiendo, el
    /// teclado vuelve a la interfaz.
    #[test]
    fn leaving_the_video_gives_the_mouse_and_keyboard_back() {
        let mut app = watching();
        app.update(Message::Guest(Msg::ToggleImmersive));
        app.chat.composing = true;
        let effects = app.update(Message::Navigate(Page::Home));
        assert!(
            effects.contains(&Effect::SetImmersive(false)),
            "{effects:?}"
        );
        assert!(effects.contains(&Effect::FocusChat(false)), "{effects:?}");
        assert!(!app.chat.composing);
    }

    /// El aviso de abajo: al tomar el teclado y el ratón, cómo soltarlos; en modo inmersivo, cómo
    /// salir; al soltar, que se soltaron. Se va del todo a los 3 s, sin animación.
    #[test]
    fn notices_say_how_to_get_out_and_go_away_after_three_seconds() {
        let mut app = watching();
        let notice = |app: &App| app.guest.as_ref().and_then(|g| g.notice(app.now));
        assert_eq!(notice(&app), None);
        app.update(Message::Guest(Msg::ForwardingChanged(true)));
        assert_eq!(notice(&app), Some(Notice::Taken));
        let (title, keys) = notice_text(&app, Notice::Taken);
        assert_eq!(title, "Teclado y ratón en la sala");
        assert_eq!(keys.as_deref(), Some("Ctrl+Alt+R para soltarlos"));
        let shown_at = app.guest.as_ref().and_then(|g| g.notice).expect("aviso").1;
        app.now += motion::BAR_HIDE;
        app.update(Message::Tick);
        assert_eq!(
            app.guest.as_ref().and_then(State::next_tick),
            Some(shown_at + NOTICE),
            "despierta para quitarlo"
        );
        app.now = shown_at + NOTICE;
        assert_eq!(notice(&app), None, "a los 3 s, fuera");
        // Modo inmersivo: el aviso con las teclas configuradas y nada más.
        app.settings.input.shortcuts.immersive = 'K';
        app.update(Message::Guest(Msg::ToggleImmersive));
        assert_eq!(notice(&app), Some(Notice::Immersive));
        assert_eq!(
            notice_text(&app, Notice::Immersive).1.as_deref(),
            Some("Ctrl+Alt+K vuelve al modo escritorio · Ctrl+Alt+R suelta el teclado y el ratón")
        );
        let state = app.guest.as_ref().expect("invitado");
        assert!(!state.bar_visible(app.now), "sin barra");
        assert!(!state.pill_visible(app.now, false), "ni latencia");
        app.update(Message::PointerMoved);
        assert!(
            !app.guest.as_ref().expect("invitado").bar_visible(app.now),
            "mover el ratón no la trae"
        );
        app.now += NOTICE;
        app.update(Message::Tick);
        assert_eq!(notice(&app), None, "a los 3 s no queda nada");
        app.update(Message::Guest(Msg::ToggleImmersive));
        assert_eq!(
            notice(&app),
            Some(Notice::Taken),
            "sigue con el teclado y el ratón"
        );
        app.update(Message::Guest(Msg::ForwardingChanged(false)));
        assert_eq!(notice(&app), Some(Notice::Released));
        assert_eq!(
            notice_text(&app, Notice::Released).0,
            "Soltaste el teclado y el ratón"
        );
    }

    /// Con el teclado y el ratón tomados no se anima nada: la barra, el panel y el fundido
    /// aparecen y se van de golpe (con todas las animaciones encendidas, sí se animan).
    #[test]
    fn nothing_animates_with_the_input_taken() {
        let mut app = watching();
        let state = app.guest.as_mut().expect("invitado");
        let now = state.now;
        place_panel(state, Some(Tab::People), now, false);
        assert!(
            state.animating(Motion::FULL),
            "suelto, el panel entra animado"
        );
        assert_eq!(
            state.panel_drawn(now, Motion::FULL),
            Some((Tab::People, 1.0)),
            "empieza fuera, a la derecha"
        );
        app.update(Message::Guest(Msg::ForwardingChanged(true)));
        let state = app.guest.as_ref().expect("invitado");
        assert!(!state.animating(Motion::FULL), "tomados, nada se anima");
        let motion = state.motion(Motion::FULL);
        assert_eq!(state.panel_drawn(app.now, motion), Some((Tab::People, 0.0)));
        assert_eq!(state.bar_hidden(app.now, motion), Some(0.0));
        assert_eq!(state.cover(app.now, motion), None, "ni el fundido");
        // Al cerrarlo, ya no está.
        let mut app = app;
        app.update(Message::Guest(Msg::ClosePanel));
        let state = app.guest.as_ref().expect("invitado");
        assert_eq!(state.panel_drawn(app.now, state.motion(Motion::FULL)), None);
    }

    /// El panel entra desde la derecha en 200 ms y sale hacia ella; la imagen aparece con un
    /// fundido de 260 ms al empezar el vídeo.
    #[test]
    fn the_panel_slides_and_the_picture_fades_in() {
        let mut app = watching();
        let start = app.now;
        let state = app.guest.as_ref().expect("invitado");
        assert_eq!(
            state.cover(start, Motion::FULL),
            Some(1.0),
            "tapada al empezar"
        );
        assert_eq!(
            state.cover(start + motion::SCREEN, Motion::FULL),
            None,
            "a los 260 ms, entera"
        );
        app.update(Message::Guest(Msg::TogglePanel(Tab::Connection)));
        let state = app.guest.as_ref().expect("invitado");
        let shown = |at| {
            state
                .panel_drawn(at, Motion::FULL)
                .map(|(_, hidden)| hidden)
        };
        assert_eq!(shown(start), Some(1.0));
        assert!(shown(start + motion::PANEL / 2).is_some_and(|h| h > 0.0 && h < 1.0));
        assert_eq!(shown(start + motion::PANEL), Some(0.0));
        app.update(Message::Guest(Msg::ClosePanel));
        let state = app.guest.as_ref().expect("invitado");
        assert!(state.panel_drawn(start, Motion::FULL).is_some(), "sale");
        assert_eq!(
            state.panel_drawn(start + motion::PANEL, Motion::FULL),
            None,
            "y se va"
        );
    }

    /// El anillo de «Conexión con el anfitrión» gira solo mientras se prueban caminos.
    #[test]
    fn the_ring_spins_only_while_trying_paths() {
        let mut app = app();
        start(&mut app, "K7Q2MZ".into());
        app.update(Message::Event(Event::Guest(SessionEvent::Joined {
            host_name: "Bea".into(),
            host_fingerprint: "b7".repeat(32),
            safety_number: crate::ui::shots::safety(),
        })));
        let state = app.guest.as_ref().expect("invitado");
        assert!(!state.trying_paths(), "aún sin claves");
        app.update(Message::Event(Event::Guest(SessionEvent::EpochChanged {
            epoch: 1,
        })));
        let state = app.guest.as_ref().expect("invitado");
        assert!(state.trying_paths() && state.animating(Motion::FULL));
        assert!(!state.animating(Motion::OFF), "sin animaciones, quieto");
        app.update(Message::Event(Event::Guest(SessionEvent::Connected {
            path: Path::Relayed,
            remote: "192.0.2.10:4433".parse().expect("dirección"),
        })));
        let state = app.guest.as_ref().expect("invitado");
        assert!(!state.trying_paths() && !state.animating(Motion::FULL));
    }

    /// El menú del sonido mantiene la barra; `Esc` lo cierra antes que nada (sin salir).
    #[test]
    fn the_sound_menu_keeps_the_bar_and_escape_closes_it() {
        let mut app = watching();
        app.update(Message::Guest(Msg::ToggleSoundMenu));
        app.now += motion::BAR_HIDE * 2;
        let state = app.guest.as_ref().expect("invitado");
        assert!(state.sound_menu && state.bar_visible(app.now));
        app.update(Message::Shortcut(crate::ui::app::Shortcut::Escape));
        assert!(!app.guest.as_ref().expect("invitado").sound_menu);
        assert!(app.dialog.is_none(), "el primer Esc no pregunta si salir");
        // El volumen del juego se ajusta en el menú y se recuerda.
        app.update(Message::Guest(Msg::GameVolume(70)));
        app.update(Message::Guest(Msg::GameVolumeDone));
        assert_eq!(app.settings.audio.game_volume, 70);
    }

    /// El riel queda a la izquierda del vídeo en ventana y se oculta a pantalla completa; la
    /// ventana mide el vídeo desde su borde.
    #[test]
    fn the_rail_sits_left_of_the_video_in_a_window() {
        let mut app = app();
        shots::video(&mut app);
        app.window = iced_core::Size::new(1180.0, 780.0);
        assert!(rail_shown(&app));
        assert_eq!(video_left(&app), ui::RAIL_NARROW + 1.0);
        let size = app.window;
        assert!(pointer_over_ui(
            &app,
            iced_core::Point::new(10.0, 400.0),
            size
        ));
        app.window = iced_core::Size::new(1400.0, 780.0);
        assert_eq!(video_left(&app), ui::RAIL_WIDE + 1.0);
        app.update(Message::FullscreenChanged(true));
        assert!(!rail_shown(&app));
        assert_eq!(video_left(&app), 0.0);
    }

    /// Dentro de la sala de otro no se entra en otra ni se abre la propia desde Inicio.
    #[test]
    fn home_does_not_join_another_room_from_inside_one() {
        let mut app = app();
        shots::video(&mut app);
        app.update(Message::Navigate(Page::Home));
        app.update(Message::Home(crate::ui::screens::home::Msg::Code(
            "ABCDEF".into(),
        )));
        let effects = app.update(Message::Home(crate::ui::screens::home::Msg::Join));
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(app.guest.as_ref().map(|g| g.code.as_str()), Some("K7Q2MZ"));
    }
}
