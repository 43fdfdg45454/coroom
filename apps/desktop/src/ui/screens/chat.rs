//! Chat de la sala (anfitrión e invitado): mensajes, avisos del sistema, campo de escritura,
//! burbujas, contador de no leídos, notificación del escritorio y sonido.
//!
//! - Los mensajes llegan de la sesión ([`SessionEvent::ChatMessage`], cifrados de extremo a
//!   extremo con la clave de la época MLS) en el orden que fija el anfitrión; el propio no se
//!   pinta al enviarlo, sino cuando vuelve con `own`. Al entrar llega el historial
//!   ([`SessionEvent::ChatHistory`], los últimos 50), que sustituye lo que hubiese.
//! - Los mensajes del sistema (entradas, salidas, permisos, expulsiones, vaciado) son **locales**:
//!   los genera este equipo con lo que ya sabe; no viajan ni se pueden falsificar.
//! - Con el panel cerrado, cada mensaje nuevo sube el contador y sale en una **burbuja** de 5 s
//!   (hasta tres apiladas y «y N más»); con la ventana sin foco, además, una notificación del
//!   escritorio. El sonido suena con el panel cerrado o la ventana sin foco, nunca por los
//!   mensajes del sistema.
//! - Mientras el campo tiene el foco ([`State::composing`]) las teclas van al chat y no al juego
//!   (lo desvía `runner.rs`); `Esc` devuelve el teclado al juego sin cerrar el panel.

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use coroom_session::Permissions;
use coroom_session::input::capture::Allowed;
use coroom_session::remote::chat::validate_text;
use coroom_session::remote::{ChatLine, RoomMember, SessionEvent};
use coroom_transport::proto::control::CHAT_TEXT_MAX_CHARS;
use coroom_ui::editor::EditorClass;
use coroom_ui::icon::{Icon, icon_with};
use coroom_ui::style::{ButtonClass, ContainerClass, SvgClass, TextClass};
use coroom_ui::theme::{Mode, Palette, Tone};
use coroom_ui::tokens::{size, space, text as type_size};
use coroom_ui::widget::{self as ui, Kind};
use coroom_ui::{Element, Renderer};
use iced_core::text::Wrapping;
use iced_core::{Alignment, Color, Font, Length, Padding, mouse};
use iced_widget::text::Span;
use iced_widget::text_editor::{self, Binding, KeyPress};
use iced_widget::{
    Column, button, center, column, container, mouse_area, rich_text, row, scrollable, span, stack,
    text,
};

use crate::ui::app::{App, Effect, Event, Message, Page, Screen};
use crate::ui::screens::guest::Tab;

/// Identificador del campo de escritura (para darle el foco y saber si lo tiene).
pub const INPUT_ID: &str = "chat-input";
/// Lo que dura una burbuja.
pub const BUBBLE_LIFE: Duration = Duration::from_secs(5);
/// Burbujas apiladas como mucho (las demás, «y N más»).
pub const BUBBLES_SHOWN: usize = 3;
/// Mensajes y avisos que se guardan como mucho (los más antiguos se van).
pub const ENTRIES_MAX: usize = 500;
/// Los que se pintan de entrada (los últimos); los anteriores, con «Ver los N anteriores».
pub const ENTRIES_SHOWN: usize = 150;
/// Caracteres a partir de los que se ve el contador del campo.
pub const COUNTER_FROM: usize = CHAT_TEXT_MAX_CHARS - 200;
/// Tiempo mínimo entre dos sonidos de mensaje (una ráfaga suena una vez).
const SOUND_GAP: Duration = Duration::from_secs(1);
/// Cada cuánto se vuelve a leer la zona horaria (cambios de horario).
const CLOCK_REFRESH: Duration = Duration::from_secs(3600);

/// Un trozo del texto de un mensaje: su tramo en el texto y adónde lleva si es un enlace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    /// Tramo de bytes en el texto.
    pub range: std::ops::Range<usize>,
    /// URL si es un enlace.
    pub link: Option<String>,
}

/// Los trozos de `text` ([`segments`]) como tramos, para calcularlos una vez al llegar el mensaje.
pub fn pieces(text: &str) -> Vec<Piece> {
    let base = text.as_ptr() as usize;
    segments(text)
        .into_iter()
        .map(|(piece, link)| {
            let start = piece.as_ptr() as usize - base;
            Piece {
                range: start..start + piece.len(),
                link,
            }
        })
        .collect()
}

/// Un mensaje del chat o un aviso del sistema, con su hora local ya calculada. Cada entrada
/// lleva una clave propia y estable (`seq`), con la que la lista conserva lo que `iced` tiene
/// medido de cada fila aunque las más antiguas se vayan.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    /// Mensaje de alguien de la sala.
    Message {
        /// Clave estable (la pone [`State::push`]).
        seq: u64,
        /// El mensaje.
        line: ChatLine,
        /// Sus trozos (texto y enlaces), calculados al llegar.
        pieces: Vec<Piece>,
        /// `HH:MM`.
        clock: String,
    },
    /// Aviso local (en gris y centrado): trozos de texto y si van en negrita.
    System {
        /// Clave estable (la pone [`State::push`]).
        seq: u64,
        /// Trozos.
        spans: Vec<(String, bool)>,
        /// Hora Unix en ms (para ordenarlo con el historial).
        at_unix_ms: u64,
        /// `HH:MM`.
        clock: String,
    },
}

impl Entry {
    fn at_unix_ms(&self) -> u64 {
        match self {
            Entry::Message { line, .. } => line.at_unix_ms,
            Entry::System { at_unix_ms, .. } => *at_unix_ms,
        }
    }

    fn seq(&self) -> u64 {
        match self {
            Entry::Message { seq, .. } | Entry::System { seq, .. } => *seq,
        }
    }

    fn set_seq(&mut self, value: u64) {
        match self {
            Entry::Message { seq, .. } | Entry::System { seq, .. } => *seq = value,
        }
    }
}

/// Una burbuja de un mensaje recién llegado.
#[derive(Debug, Clone, PartialEq)]
pub struct Bubble {
    /// El mensaje.
    pub line: ChatLine,
    /// Cuándo llegó.
    pub at: Instant,
}

/// Estado del chat.
#[derive(Debug)]
pub struct State {
    /// Panel del chat a la vista (invitado: pestaña «Chat» del panel; anfitrión: además, la sala
    /// en pantalla, ver [`visible`]).
    pub open: bool,
    /// Hay claves: se puede escribir.
    pub ready: bool,
    /// Mensajes y avisos, del más antiguo al más nuevo.
    pub entries: Vec<Entry>,
    /// Siguiente clave de entrada.
    next_seq: u64,
    /// Se pintan todas las entradas, no solo las últimas [`ENTRIES_SHOWN`].
    pub show_all: bool,
    /// Lo que se está escribiendo.
    pub draft: text_editor::Content<Renderer>,
    /// El campo tiene el foco: las teclas van al chat y no al juego.
    pub composing: bool,
    /// Mensajes sin leer.
    pub unread: usize,
    /// Burbujas vivas, de la más antigua a la más nueva.
    pub bubbles: Vec<Bubble>,
    /// Por qué no salió el último mensaje («Vas muy rápido…»).
    pub error: Option<String>,
    /// Lo último que se envió y aún no ha vuelto (vuelve al campo si falla).
    pub pending: Option<String>,
    /// Pidiendo confirmación para vaciar el chat (anfitrión).
    pub confirm_clear: bool,
    /// La ventana tiene el foco del escritorio.
    pub window_focused: bool,
    /// Nombre de la sala para las notificaciones («Sala de Bea»).
    pub room: Option<String>,
    /// Nombre del anfitrión (invitado).
    pub host_name: Option<String>,
    /// Quién estaba en la sala (invitado), para contar entradas y salidas.
    members: Vec<RoomMember>,
    /// Si ya se avisó de «Entraste a la sala».
    joined: bool,
    /// Permisos de este equipo (invitado), para contar cambios.
    allowed: Option<Allowed>,
    /// Invitados de la sala propia: nombre y permisos (anfitrión).
    guests: BTreeMap<u32, (String, Permissions)>,
    /// Último sonido.
    last_sound: Option<Instant>,
    /// Desfase con UTC y cuándo se leyó.
    utc_offset: Option<(i64, Instant)>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            ready: false,
            entries: Vec::new(),
            next_seq: 0,
            show_all: false,
            draft: text_editor::Content::new(),
            composing: false,
            unread: 0,
            bubbles: Vec::new(),
            error: None,
            pending: None,
            confirm_clear: false,
            window_focused: true,
            room: None,
            host_name: None,
            members: Vec::new(),
            joined: false,
            allowed: None,
            guests: BTreeMap::new(),
            last_sound: None,
            utc_offset: None,
        }
    }
}

/// Mensajes del chat.
#[derive(Debug, Clone)]
pub enum Msg {
    /// `Ctrl+Alt+C`: abre el panel en «Chat» con el cursor en el campo; si ya se está escribiendo,
    /// lo cierra.
    Toggle,
    /// Abrir el panel en «Chat» con el cursor en el campo (botón de la barra, burbuja).
    Open,
    /// Cerrar el panel.
    Close,
    /// `Esc` con el campo enfocado: el teclado vuelve al juego (el panel sigue abierto).
    Escape,
    /// Edición en el campo.
    Edit(text_editor::Action),
    /// Enviar lo escrito (`Intro`).
    Send,
    /// El campo ganó o perdió el foco (lo mira la ventana tras cada evento).
    Composing(bool),
    /// La ventana ganó o perdió el foco del escritorio.
    WindowFocused(bool),
    /// Anfitrión: «Vaciar el chat» (pide confirmación).
    AskClear,
    /// Anfitrión: confirma vaciar el chat para todos.
    ConfirmClear,
    /// Anfitrión: no vaciar.
    CancelClear,
    /// Pintar también los mensajes anteriores a los últimos [`ENTRIES_SHOWN`].
    ShowOlder,
}

impl State {
    /// Si `Esc` lo tiene que recibir el chat (el campo tiene el foco).
    pub fn takes_escape(&self) -> bool {
        self.composing
    }

    /// Cuándo hace falta el próximo `Tick` (la burbuja más antigua caduca).
    pub fn next_tick(&self, _now: Instant) -> Option<Instant> {
        self.bubbles.first().map(|bubble| bubble.at + BUBBLE_LIFE)
    }

    /// Pasa el tiempo: las burbujas caducan.
    pub fn tick(&mut self, now: Instant) {
        self.bubbles
            .retain(|bubble| now.duration_since(bubble.at) < BUBBLE_LIFE);
    }

    /// Sala nueva o cerrada: todo de cero (salvo el foco de la ventana y la zona horaria).
    fn reset(&mut self) {
        let window_focused = self.window_focused;
        let utc_offset = self.utc_offset;
        *self = Self {
            window_focused,
            utc_offset,
            ..Self::default()
        };
    }

    /// `HH:MM` local de `at_unix_ms`. La zona se lee como mucho una vez por hora.
    fn clock(&mut self, now: Instant, at_unix_ms: u64) -> String {
        let unix_s = i64::try_from(at_unix_ms / 1000).unwrap_or(i64::MAX / 2);
        let offset = match self.utc_offset {
            Some((offset, at)) if now.duration_since(at) < CLOCK_REFRESH => offset,
            _ => {
                let offset = crate::localtime::utc_offset(unix_s);
                self.utc_offset = Some((offset, now));
                offset
            }
        };
        crate::localtime::hhmm(unix_s, offset)
    }

    /// Fija el desfase con UTC (escenas y pruebas: hora fija sin mirar el sistema).
    pub fn set_utc_offset(&mut self, offset: i64, now: Instant) {
        self.utc_offset = Some((offset, now));
    }

    fn push(&mut self, mut entry: Entry) {
        entry.set_seq(self.next_seq);
        self.next_seq += 1;
        self.entries.push(entry);
        if self.entries.len() > ENTRIES_MAX {
            let extra = self.entries.len() - ENTRIES_MAX;
            self.entries.drain(..extra);
        }
    }

    /// Añade un aviso del sistema con la hora de ahora.
    fn system(&mut self, now: Instant, spans: Vec<(String, bool)>) {
        let at_unix_ms = unix_ms_now();
        let clock = self.clock(now, at_unix_ms);
        self.push(Entry::System {
            seq: 0,
            spans,
            at_unix_ms,
            clock,
        });
    }

    /// Añade un mensaje.
    fn message(&mut self, now: Instant, line: ChatLine) {
        let clock = self.clock(now, line.at_unix_ms);
        let pieces = pieces(&line.text);
        self.push(Entry::Message {
            seq: 0,
            line,
            pieces,
            clock,
        });
    }
}

/// Hora Unix de ahora en ms.
fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// `text` en trozos, con en negrita la primera aparición de cada uno de `bold` (los valores de las
/// variables del mensaje: el nombre, el permiso).
pub fn emphasize(text: &str, bold: &[&str]) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut rest = text;
    for value in bold.iter().filter(|value| !value.is_empty()) {
        if let Some(at) = rest.find(value) {
            if at > 0 {
                out.push((rest[..at].to_owned(), false));
            }
            out.push(((*value).to_owned(), true));
            rest = &rest[at + value.len()..];
        }
    }
    if !rest.is_empty() {
        out.push((rest.to_owned(), false));
    }
    out
}

/// Si hay un chat en marcha (sala propia abierta o dentro de una).
pub fn active(app: &App) -> bool {
    app.guest.is_some() || app.host.is_open()
}

/// Si los mensajes están a la vista: el invitado con el panel en «Chat»; el anfitrión, además,
/// con la sala en pantalla (el panel del chat va fijo a su derecha).
pub fn visible(app: &App) -> bool {
    if app.guest.is_some() {
        return app.chat.open;
    }
    app.chat.open || (app.host.is_open() && app.screen == Screen::Main && app.page == Page::Host)
}

/// Abre el panel en «Chat» y pone el cursor en el campo (si ya se puede escribir).
fn open(app: &mut App) -> Vec<Effect> {
    let mut effects = Vec::new();
    if let Some(guest) = app.guest.as_mut() {
        guest.panel = Some(Tab::Chat);
        if guest.immersive {
            // El ratón vuelve para poder escribir y pulsar (la ventana confirma el cambio).
            effects.push(Effect::SetImmersive(false));
        }
    } else if app.host.is_open() && app.screen == Screen::Main && app.page != Page::Host {
        effects.extend(app.update(Message::Navigate(Page::Host)));
    }
    app.chat.open = true;
    app.chat.unread = 0;
    app.chat.bubbles.clear();
    if app.chat.ready {
        app.chat.composing = true;
        effects.push(Effect::FocusChat(true));
    }
    effects
}

/// Cierra el panel (el teclado vuelve al juego).
fn close(app: &mut App) -> Vec<Effect> {
    if let Some(guest) = app.guest.as_mut()
        && guest.panel == Some(Tab::Chat)
    {
        guest.panel = None;
    }
    app.chat.open = false;
    app.chat.confirm_clear = false;
    blur(app)
}

/// El campo pierde el foco.
fn blur(app: &mut App) -> Vec<Effect> {
    if std::mem::take(&mut app.chat.composing) {
        vec![Effect::FocusChat(false)]
    } else {
        Vec::new()
    }
}

/// El invitado cambió de pestaña en el panel (o lo cerró): el chat se ve solo en la suya.
pub fn tab_changed(app: &mut App, tab: Option<Tab>) -> Vec<Effect> {
    if tab == Some(Tab::Chat) {
        return open(app);
    }
    app.chat.open = false;
    blur(app)
}

/// Reduce un mensaje del chat.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Toggle => {
            if !active(app) {
                return Vec::new();
            }
            if app.chat.open && app.chat.composing {
                if app.guest.is_some() {
                    close(app)
                } else {
                    blur(app)
                }
            } else {
                open(app)
            }
        }
        Msg::Open => {
            if active(app) {
                open(app)
            } else {
                Vec::new()
            }
        }
        Msg::Close => close(app),
        Msg::Escape => blur(app),
        Msg::Edit(action) => {
            // Un clic o una tecla en el campo: tiene el foco (rodar la rueda encima, no).
            if !matches!(action, text_editor::Action::Scroll { .. }) {
                app.chat.composing = true;
            }
            if action.is_edit() {
                app.chat.error = None;
            }
            app.chat.draft.perform(action);
            Vec::new()
        }
        Msg::Send => send(app),
        Msg::Composing(composing) => {
            app.chat.composing = composing;
            Vec::new()
        }
        Msg::WindowFocused(focused) => {
            app.chat.window_focused = focused;
            Vec::new()
        }
        Msg::AskClear => {
            app.chat.confirm_clear = app.host.is_open() && app.guest.is_none();
            Vec::new()
        }
        Msg::ConfirmClear => {
            if std::mem::take(&mut app.chat.confirm_clear) && app.host.is_open() {
                vec![Effect::ClearChat]
            } else {
                Vec::new()
            }
        }
        Msg::CancelClear => {
            app.chat.confirm_clear = false;
            Vec::new()
        }
        Msg::ShowOlder => {
            app.chat.show_all = true;
            Vec::new()
        }
    }
}

/// Envía lo escrito: el campo se vacía y el mensaje se pinta cuando vuelve del anfitrión.
fn send(app: &mut App) -> Vec<Effect> {
    let draft = app.chat.draft.text();
    if draft.trim().is_empty() {
        return Vec::new();
    }
    if !app.chat.ready || !active(app) {
        app.chat.error = Some(t!("chat-aun-no-disponible").to_owned());
        return Vec::new();
    }
    match validate_text(&draft) {
        Ok(text) => {
            app.chat.draft = text_editor::Content::new();
            app.chat.error = None;
            app.chat.pending = Some(draft);
            vec![Effect::SendChat(text)]
        }
        Err(reason) => {
            app.chat.error = Some(reason);
            Vec::new()
        }
    }
}

/// Mira los eventos de la sesión (mensajes, historial, vaciado, quién entra y sale, permisos)
/// antes que la pantalla.
pub fn on_event(app: &mut App, event: &Event) -> Vec<Effect> {
    let now = app.now;
    match event {
        Event::GuestStarted(Ok(_)) | Event::HostStarted(Ok(_)) => {
            app.chat.reset();
            if matches!(event, Event::HostStarted(_)) {
                app.chat.room = Some(t!("chat-tu-sala").to_owned());
            }
        }
        Event::GuestEnded(_) | Event::HostEnded(_) => app.chat.reset(),
        Event::Guest(event) => return session_event(app, event, false),
        Event::Host(event) => return session_event(app, event, true),
        Event::GuestStats(stats) => guest_permissions(app, stats.allowed),
        Event::HostGuests(guests) => {
            for status in guests {
                let id = status.guest.0;
                let before = app
                    .chat
                    .guests
                    .insert(id, (status.name.clone(), status.permissions));
                if let Some((_, before)) = before {
                    for (gave, what) in permission_changes(before, status.permissions) {
                        let key_text = if gave {
                            t!("chat-diste-what-a-name", what = what, name = status.name)
                        } else {
                            t!("chat-quitaste-what-a-name", what = what, name = status.name)
                        };
                        let spans = emphasize(&key_text, &[&what]);
                        app.chat.system(now, spans);
                    }
                }
            }
        }
        Event::ChatFailed(reason) => {
            app.chat.error = Some(reason.clone());
            // Lo que no salió vuelve al campo si no se ha escrito otra cosa.
            if let Some(pending) = app.chat.pending.take()
                && app.chat.draft.text().trim().is_empty()
            {
                app.chat.draft = text_editor::Content::with_text(pending.trim_end());
            }
        }
        _ => {}
    }
    Vec::new()
}

/// Lo que cambió de `before` a `after` para los avisos: `(dado, qué)`.
fn permission_changes(before: Permissions, after: Permissions) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    if before.keyboard_mouse != after.keyboard_mouse {
        out.push((
            after.keyboard_mouse,
            if after.keyboard_mouse {
                t!("chat-teclado-y-raton").to_owned()
            } else {
                t!("chat-el-teclado-y-el-raton").to_owned()
            },
        ));
    }
    match (before.gamepad_slot, after.gamepad_slot) {
        (None, Some(slot)) => out.push((true, t!("chat-el-mando-ranura", slot = slot))),
        (Some(_), None) => out.push((false, t!("chat-el-mando").to_owned())),
        (Some(a), Some(b)) if a != b => out.push((true, t!("chat-el-mando-ranura", slot = b))),
        _ => {}
    }
    if before.voice != after.voice {
        out.push((after.voice, t!("chat-la-voz").to_owned()));
    }
    out
}

/// Invitado: el anfitrión te dio o te quitó algo (avisos en el chat, además del toast).
fn guest_permissions(app: &mut App, allowed: Allowed) {
    let now = app.now;
    let before = app.chat.allowed.replace(allowed);
    let Some(before) = before else {
        return;
    };
    let host = app
        .chat
        .host_name
        .clone()
        .unwrap_or_else(|| t!("chat-el-anfitrion").to_owned());
    let to_permissions = |allowed: Allowed| Permissions {
        keyboard_mouse: allowed.keyboard_mouse,
        gamepad_slot: allowed.gamepad_slot,
        ..Permissions::VIEW_ONLY
    };
    for (gave, what) in permission_changes(to_permissions(before), to_permissions(allowed)) {
        let line = if gave {
            t!("chat-name-te-dio-what", name = host, what = what)
        } else {
            t!("chat-name-te-quito-what", name = host, what = what)
        };
        let spans = emphasize(&line, &[&what]);
        app.chat.system(now, spans);
    }
}

fn session_event(app: &mut App, event: &SessionEvent, host: bool) -> Vec<Effect> {
    let now = app.now;
    let chat = &mut app.chat;
    match event {
        SessionEvent::Joined { host_name, .. } => {
            chat.host_name = Some(host_name.clone());
            chat.room = Some(t!("chat-sala-de-name", name = host_name));
        }
        SessionEvent::RoomCreated { .. } | SessionEvent::EpochChanged { .. } => chat.ready = true,
        SessionEvent::ChatHistory { messages } => {
            chat.ready = true;
            // Sustituye los mensajes; los avisos locales se quedan, en su sitio por la hora.
            chat.entries
                .retain(|entry| matches!(entry, Entry::System { .. }));
            for line in messages {
                chat.message(now, line.clone());
            }
            chat.entries.sort_by_key(Entry::at_unix_ms);
        }
        SessionEvent::ChatMessage(line) => {
            chat.ready = true;
            chat.message(now, line.clone());
            if line.own {
                chat.pending = None;
                return Vec::new();
            }
            return arrived(app, line);
        }
        SessionEvent::ChatCleared => {
            chat.entries.clear();
            chat.bubbles.clear();
            chat.unread = 0;
            let text = if host {
                t!("chat-vaciaste-el-chat").to_owned()
            } else {
                t!("chat-el-anfitrion-vacio-el-chat").to_owned()
            };
            chat.system(now, vec![(text, false)]);
        }
        SessionEvent::ChatRejected { reason } => {
            chat.system(now, vec![(reason.clone(), false)]);
        }
        SessionEvent::Members(members) => {
            if !chat.joined {
                chat.joined = true;
                chat.system(now, vec![(t!("chat-entraste-a-la-sala").to_owned(), false)]);
            } else {
                let had = |list: &[RoomMember], member: &RoomMember| {
                    list.iter().any(|other| other.guest == member.guest)
                };
                let before = std::mem::take(&mut chat.members);
                for member in members.iter().filter(|m| !m.you && !had(&before, m)) {
                    let line = t!("chat-name-entro-a-la-sala", name = member.name);
                    chat.system(now, emphasize(&line, &[&member.name]));
                }
                for member in before.iter().filter(|m| !m.you && !had(members, m)) {
                    let line = t!("chat-name-salio-de-la-sala", name = member.name);
                    chat.system(now, emphasize(&line, &[&member.name]));
                }
            }
            chat.members = members.clone();
        }
        SessionEvent::GuestJoined {
            guest,
            name,
            permissions,
            ..
        } => {
            chat.guests.insert(guest.0, (name.clone(), *permissions));
            let line = t!("chat-name-entro-a-la-sala", name = name);
            chat.system(now, emphasize(&line, &[name]));
        }
        SessionEvent::GuestLeft { guest, kicked } => {
            if let Some((name, _)) = chat.guests.remove(&guest.0) {
                let line = if *kicked {
                    t!("chat-expulsaste-a-name", name = name)
                } else {
                    t!("chat-name-salio-de-la-sala", name = name)
                };
                chat.system(now, emphasize(&line, &[&name]));
            }
        }
        _ => {}
    }
    Vec::new()
}

/// Llegó un mensaje de otro: contador, burbuja, notificación y sonido según lo que se vea.
fn arrived(app: &mut App, line: &ChatLine) -> Vec<Effect> {
    let now = app.now;
    let seen = visible(app);
    let room = app.settings.room.clone();
    let chat = &mut app.chat;
    let mut effects = Vec::new();
    if !seen {
        chat.unread += 1;
        if room.chat_bubbles {
            chat.bubbles.push(Bubble {
                line: line.clone(),
                at: now,
            });
        }
    }
    if !chat.window_focused && room.chat_notifications {
        let name = author_name(line, None);
        let title = match &chat.room {
            Some(room) => t!("chat-notificacion-titulo", name = name, room = room),
            None => name,
        };
        effects.push(Effect::ChatNotify {
            title,
            body: ui::ellipsize(&line.text, 200).into_owned(),
        });
    }
    if room.chat_sound
        && (!seen || !chat.window_focused)
        && chat
            .last_sound
            .is_none_or(|at| now.duration_since(at) >= SOUND_GAP)
    {
        chat.last_sound = Some(now);
        effects.push(Effect::ChatSound);
    }
    effects
}

/// Nombre que se enseña del autor: «Tú» si es propio (con `you`), su nombre, o «Anfitrión».
fn author_name(line: &ChatLine, you: Option<&str>) -> String {
    if line.own
        && let Some(you) = you
    {
        return you.to_owned();
    }
    if line.author.name.is_empty() {
        t!("chat-anfitrion").to_owned()
    } else {
        line.author.name.clone()
    }
}

// --- Enlaces ------------------------------------------------------------------------------------

/// Dominios de primer nivel con los que `dominio.tld` (sin ruta) ya cuenta como enlace.
const COMMON_TLDS: &[&str] = &[
    "com", "org", "net", "io", "dev", "app", "gg", "tv", "me", "co", "es", "ar", "mx", "cl", "uy",
    "pe", "info", "edu", "gov", "eu", "de", "fr", "uk", "it", "pt", "br", "example",
];

/// Adónde lleva `word` si es un enlace: `https://…` y `http://…` tal cual, y los dominios con
/// ruta, con tres partes o con un dominio de primer nivel conocido (`coroom.example.com/guia`)
/// con `https://` delante.
pub fn link_target(word: &str) -> Option<String> {
    let lower = word.to_ascii_lowercase();
    for scheme in ["https://", "http://"] {
        if let Some(rest) = lower.strip_prefix(scheme) {
            return rest
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '[')
                .then(|| word.to_owned());
        }
    }
    if word.contains('@') || word.contains("://") {
        return None;
    }
    let (host, has_path) = match word.find(['/', '?', '#']) {
        Some(at) => (&word[..at], true),
        None => (word, false),
    };
    let host = host.split(':').next().unwrap_or(host);
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2
        || labels.iter().any(|label| {
            label.is_empty()
                || label.starts_with('-')
                || !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
    {
        return None;
    }
    let tld = labels[labels.len() - 1].to_ascii_lowercase();
    if !(2..=24).contains(&tld.len()) || !tld.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    (has_path || labels.len() >= 3 || COMMON_TLDS.contains(&tld.as_str()))
        .then(|| format!("https://{word}"))
}

/// El texto en trozos: `(trozo, adónde lleva si es un enlace)`. El enlace se ve tal cual (sin
/// acortar); la puntuación de alrededor (`.`, `,`, `)`, `»`…) no forma parte de él.
pub fn segments(text: &str) -> Vec<(&str, Option<String>)> {
    const TRAILING: &[char] = &['.', ',', ';', ':', '!', '?', ')', ']', '}', '»', '"', '\''];
    const LEADING: &[char] = &['(', '[', '{', '«', '"', '\''];
    let mut out = Vec::new();
    let mut plain = 0;
    let mut word_start: Option<usize> = None;
    let mut words = Vec::new();
    for (at, c) in text.char_indices() {
        match (c.is_whitespace(), word_start) {
            (true, Some(start)) => {
                words.push((start, at));
                word_start = None;
            }
            (false, None) => word_start = Some(at),
            _ => {}
        }
    }
    if let Some(start) = word_start {
        words.push((start, text.len()));
    }
    for (start, end) in words {
        let word = &text[start..end];
        let trimmed = word.trim_end_matches(TRAILING);
        let core = trimmed.trim_start_matches(LEADING);
        let core_start = start + (trimmed.len() - core.len());
        if core.is_empty() {
            continue;
        }
        if let Some(url) = link_target(core) {
            if core_start > plain {
                out.push((&text[plain..core_start], None));
            }
            out.push((core, Some(url)));
            plain = core_start + core.len();
        }
    }
    if plain < text.len() {
        out.push((&text[plain..], None));
    }
    out
}

// --- Vista ----------------------------------------------------------------------------------------

/// Dónde se dibuja el panel: en la ventana (anfitrión y sala de espera, con el tema de la
/// ventana) o flotando sobre el vídeo (invitado, oscuro).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// Panel fijo con el tema de la ventana.
    Window,
    /// Panel flotante sobre el vídeo.
    Overlay,
}

/// Colores de una superficie.
#[derive(Clone, Copy)]
struct Look {
    surface: Surface,
    /// Enlaces y el nombre propio.
    accent: Color,
}

impl Look {
    fn new(app: &App, surface: Surface) -> Self {
        let accent = match surface {
            // Sobre el vídeo (siempre oscuro), el acento del modo oscuro.
            Surface::Overlay => Palette::new(Mode::Dark, app.accent()).accent_text.color(),
            Surface::Window => app.theme.palette.accent_text.color(),
        };
        Self { surface, accent }
    }

    fn primary(self) -> TextClass {
        match self.surface {
            Surface::Overlay => TextClass::Overlay,
            Surface::Window => TextClass::Primary,
        }
    }

    fn secondary(self) -> TextClass {
        match self.surface {
            Surface::Overlay => TextClass::OverlaySecondary,
            Surface::Window => TextClass::Secondary,
        }
    }
}

/// Teclas del campo: `Intro` envía, `Mayús+Intro` salta de línea; el resto, como siempre.
fn key_binding(press: KeyPress) -> Option<Binding<Message>> {
    use iced_core::keyboard::{Key, key::Named};
    if matches!(press.status, text_editor::Status::Focused { .. })
        && matches!(press.key.as_ref(), Key::Named(Named::Enter))
    {
        return Some(if press.modifiers.shift() {
            Binding::Enter
        } else {
            Binding::Custom(Message::Chat(Msg::Send))
        });
    }
    Binding::from_key_press(press)
}

/// Panel del chat (mensajes y campo) para `surface`. Sobre el vídeo es el contenido de la
/// pestaña «Chat» del panel de la sala; en la ventana lleva su cabecera («Chat de la sala» y, en
/// la sala propia, «Vaciar el chat»).
pub fn panel(app: &App, surface: Surface) -> Element<'_, Message> {
    let look = Look::new(app, surface);
    let body = column![messages(app, look), composer(app, look)].height(Length::Fill);
    match surface {
        Surface::Overlay => body.into(),
        Surface::Window => container(column![header(app), body].height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .class(ContainerClass::Card)
            .into(),
    }
}

/// Panel del chat con el tema de la ventana.
pub fn view(app: &App) -> Element<'_, Message> {
    panel(app, Surface::Window)
}

fn header(app: &App) -> Element<'_, Message> {
    let host = app.guest.is_none() && app.host.is_open();
    let content: Element<'_, Message> = if app.chat.confirm_clear {
        row![
            ui::strong(t!("chat-vaciar-para-todos")).width(Length::Fill),
            ui::flat(t!("chat-cancelar"), Some(Message::Chat(Msg::CancelClear))),
            ui::destructive(t!("chat-vaciar"), Some(Message::Chat(Msg::ConfirmClear))),
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center)
        .into()
    } else {
        let mut line = row![
            icon_with(Icon::Chat, size::ICON, SvgClass::Text),
            ui::strong(t!("chat-titulo")).width(Length::Fill),
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center);
        if host {
            line = line.push(ui::icon_button(
                Icon::Eraser,
                t!("chat-vaciar-el-chat"),
                (!app.chat.entries.is_empty()).then_some(Message::Chat(Msg::AskClear)),
            ));
        }
        line.into()
    };
    container(content)
        .padding(Padding::from([space::XS, space::S]))
        .height(Length::Fixed(size::CONTROL + space::S))
        .center_y(Length::Fixed(size::CONTROL + space::S))
        .into()
}

fn messages<'a>(app: &'a App, look: Look) -> Element<'a, Message> {
    if app.chat.entries.is_empty() {
        let detail = if app.chat.ready {
            t!("chat-aun-sin-mensajes-detalle")
        } else {
            t!("chat-esperando-las-claves")
        };
        return center(
            column![
                icon_with(
                    Icon::ChatText,
                    size::ICON_LARGE,
                    match look.surface {
                        Surface::Overlay => SvgClass::Overlay,
                        Surface::Window => SvgClass::Secondary,
                    }
                ),
                text(t!("chat-aun-sin-mensajes"))
                    .size(type_size::BODY)
                    .font(coroom_ui::font::SEMIBOLD)
                    .class(look.primary()),
                text(detail)
                    .size(type_size::SECONDARY)
                    .class(look.secondary())
                    .align_x(iced_core::alignment::Horizontal::Center)
                    .wrapping(Wrapping::WordOrGlyph),
            ]
            .spacing(space::XS)
            .align_x(Alignment::Center)
            .max_width(260),
        )
        .padding(space::M)
        .into();
    }
    let you = app
        .account
        .as_ref()
        .map(|account| account.display_name.as_str());
    // Filas con clave estable: lo que `iced` tiene medido de cada una sobrevive a que las más
    // antiguas se vayan. De entrada solo las últimas; las demás, a petición.
    let entries = &app.chat.entries;
    let hidden = if app.chat.show_all {
        0
    } else {
        entries.len().saturating_sub(ENTRIES_SHOWN)
    };
    let mut list = iced_widget::keyed::Column::with_capacity(entries.len() - hidden + 1)
        .spacing(space::XS)
        .padding(Padding::from([space::S, space::M]));
    if hidden > 0 {
        list = list.push(
            u64::MAX,
            container(
                ui::button_with(
                    Kind::Flat,
                    t!("chat-ver-los-n-anteriores", count = hidden),
                    None,
                    Some(Message::Chat(Msg::ShowOlder)),
                )
                .width(Length::Fill),
            )
            .width(Length::Fill),
        );
    }
    for entry in &entries[hidden..] {
        list = list.push(
            entry.seq(),
            match entry {
                Entry::Message {
                    line,
                    pieces,
                    clock,
                    ..
                } => message_row(app, look, line, pieces, clock, you),
                Entry::System { spans, clock, .. } => system_row(look, spans, clock),
            },
        );
    }
    scrollable(list)
        .anchor_bottom()
        .height(Length::Fill)
        .width(Length::Fill)
        .into()
}

fn message_row<'a>(
    app: &'a App,
    look: Look,
    line: &'a ChatLine,
    pieces: &'a [Piece],
    clock: &'a str,
    you: Option<&'a str>,
) -> Element<'a, Message> {
    let shown = author_name(line, None);
    let seed = if line.own {
        you.map_or_else(|| shown.clone(), str::to_owned)
    } else {
        shown.clone()
    };
    let name = if line.own {
        text(t!("chat-tu"))
            .size(type_size::SECONDARY)
            .font(coroom_ui::font::SEMIBOLD)
            .class(TextClass::Custom(look.accent))
    } else {
        text(shown)
            .size(type_size::SECONDARY)
            .font(coroom_ui::font::SEMIBOLD)
            .class(look.primary())
    };
    let mut who = row![name].spacing(space::XS).align_y(Alignment::Center);
    if line.author.guest == 0 {
        who = who.push(coroom_ui::tag::live(t!("chat-anfitrion")));
    }
    who = who.push(
        text(clock)
            .size(type_size::CAPTION)
            .font(coroom_ui::font::MONO)
            .class(look.secondary()),
    );
    row![
        ui::avatar(&app.theme, &seed, 28.0),
        column![who, message_text(look, &line.text, pieces)]
            .spacing(2)
            .width(Length::Fill)
    ]
    .spacing(space::XS)
    .into()
}

/// Texto plano con los enlaces subrayados y clicables (`pieces`: sus trozos, ya calculados).
fn message_text<'a>(look: Look, content: &'a str, pieces: &'a [Piece]) -> Element<'a, Message> {
    let spans: Vec<Span<'a, String, Font>> = pieces
        .iter()
        .map(|piece| {
            let text = content.get(piece.range.clone()).unwrap_or_default();
            match &piece.link {
                Some(url) => span(text)
                    .link(url.clone())
                    .underline(true)
                    .color(look.accent),
                None => span(text),
            }
        })
        .collect();
    rich_text(spans)
        .on_link_click(Message::OpenUrl)
        .size(type_size::BODY)
        .class(look.primary())
        .wrapping(Wrapping::WordOrGlyph)
        .width(Length::Fill)
        .into()
}

fn system_row<'a>(look: Look, spans: &'a [(String, bool)], clock: &'a str) -> Element<'a, Message> {
    let mut pieces: Vec<Span<'a, String, Font>> = spans
        .iter()
        .map(|(piece, bold)| {
            let piece = span(piece.as_str());
            if *bold {
                piece.font(coroom_ui::font::SEMIBOLD)
            } else {
                piece
            }
        })
        .collect();
    pieces.push(span(" · "));
    pieces.push(span(clock));
    container(
        rich_text(pieces)
            .on_link_click(Message::OpenUrl)
            .size(type_size::CAPTION)
            .class(look.secondary())
            .align_x(Alignment::Center)
            .wrapping(Wrapping::WordOrGlyph)
            .width(Length::Fill),
    )
    .padding(Padding::from([2.0, 0.0]))
    .width(Length::Fill)
    .into()
}

/// Relleno vertical de la caja de texto del chat.
const EDITOR_PADDING_Y: f32 = 8.0;

fn composer(app: &App, look: Look) -> Element<'_, Message> {
    let chat = &app.chat;
    let ready = chat.ready;
    let mut editor = iced_widget::text_editor(&chat.draft)
        .id(INPUT_ID)
        .placeholder(if ready {
            t!("chat-escribe-un-mensaje")
        } else {
            t!("chat-esperando-las-claves-corto")
        })
        .key_binding(key_binding)
        .height(Length::Shrink)
        // `min_height` acota el texto, sin el relleno: con una línea la caja mide lo mismo que el
        // botón de enviar (`CONTROL`), y crece a partir de ahí.
        .min_height(size::CONTROL - 2.0 * EDITOR_PADDING_Y)
        .max_height(120.0)
        .padding(Padding::from([EDITOR_PADDING_Y, 10.0]))
        .size(type_size::BODY)
        .wrapping(Wrapping::WordOrGlyph)
        .class(match look.surface {
            Surface::Overlay => EditorClass::Overlay,
            Surface::Window => EditorClass::Normal,
        });
    if ready {
        editor = editor.on_action(|action| Message::Chat(Msg::Edit(action)));
    }
    let draft = chat.draft.text();
    let empty = draft.trim().is_empty();
    let send = ui::with_tooltip(
        button(center(icon_with(
            Icon::Send,
            18.0,
            if ready && !empty {
                SvgClass::OnAccent
            } else {
                SvgClass::Secondary
            },
        )))
        .width(Length::Fixed(size::CONTROL))
        .height(Length::Fixed(size::CONTROL))
        .padding(0)
        .class(ButtonClass::Primary)
        .on_press_maybe((ready && !empty).then_some(Message::Chat(Msg::Send))),
        t!("chat-enviar"),
    );
    let mut content = column![
        row![editor, send]
            .spacing(space::XS)
            .align_y(Alignment::End)
    ]
    .spacing(space::XXS);
    let chars = draft.trim_end_matches('\n').chars().count();
    let mut notes = row![].spacing(space::XS).align_y(Alignment::Center);
    let mut any = false;
    if let Some(error) = &chat.error {
        any = true;
        notes = notes.push(
            row![
                icon_with(Icon::CircleAlert, 14.0, SvgClass::Tone(Tone::Error)),
                text(error.as_str())
                    .size(type_size::CAPTION)
                    .class(match look.surface {
                        Surface::Overlay => TextClass::Overlay,
                        Surface::Window => TextClass::Tone(Tone::Error),
                    })
                    .wrapping(Wrapping::WordOrGlyph)
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .width(Length::Fill),
        );
    } else if chat.composing && look.surface == Surface::Overlay {
        any = true;
        notes = notes.push(
            text(t!("chat-teclas-del-campo"))
                .size(type_size::CAPTION)
                .class(look.secondary())
                .width(Length::Fill),
        );
    } else {
        notes = notes.push(ui::fill());
    }
    if chars >= COUNTER_FROM {
        any = true;
        let over = chars > CHAT_TEXT_MAX_CHARS;
        notes = notes.push(
            text(t!(
                "chat-contador",
                count = chars,
                max = CHAT_TEXT_MAX_CHARS
            ))
            .size(type_size::CAPTION)
            .font(coroom_ui::font::MONO)
            .class(if over {
                TextClass::Tone(Tone::Error)
            } else {
                look.secondary()
            }),
        );
    }
    if any {
        content = content.push(notes);
    }
    container(content)
        .padding(Padding::from([space::S, space::S]))
        .width(Length::Fill)
        .into()
}

/// Contador de no leídos encima de un botón de `side` px (sin mensajes, el botón tal cual).
pub fn with_unread<'a>(
    control: impl Into<Element<'a, Message>>,
    unread: usize,
    side: f32,
) -> Element<'a, Message> {
    let control = control.into();
    if unread == 0 {
        return control;
    }
    let label = if unread > 99 {
        "99+".to_owned()
    } else {
        unread.to_string()
    };
    // Como en el boceto: 16 px de alto, redondo con una cifra, en la esquina del botón (sin
    // moverlo: el botón mide lo mismo con y sin mensajes).
    let single = label.chars().count() == 1;
    let badge = container(
        text(label)
            .size(10.5)
            .font(coroom_ui::font::SEMIBOLD)
            .wrapping(Wrapping::None),
    )
    .padding(Padding::from([0.0, if single { 0.0 } else { 4.0 }]))
    .center_x(if single {
        Length::Fixed(16.0)
    } else {
        Length::Shrink
    })
    .center_y(Length::Fixed(16.0))
    .class(ContainerClass::Counter);
    stack![
        control,
        container(badge)
            .width(Length::Fixed(side))
            .align_x(iced_core::alignment::Horizontal::Right),
    ]
    .into()
}

/// Burbujas de los mensajes recién llegados (abajo a la derecha, con el panel cerrado): las tres
/// más nuevas y «y N más». Un clic abre el chat.
pub fn bubbles(app: &App) -> Option<Element<'_, Message>> {
    let chat = &app.chat;
    if chat.bubbles.is_empty() || visible(app) {
        return None;
    }
    let skip = chat.bubbles.len().saturating_sub(BUBBLES_SHOWN);
    let mut list = Column::new()
        .spacing(space::XS)
        .align_x(iced_core::alignment::Horizontal::Right);
    if skip > 0 {
        list = list.push(
            container(
                text(t!("chat-y-n-mas", count = skip))
                    .size(type_size::CAPTION)
                    .font(coroom_ui::font::SEMIBOLD)
                    .class(TextClass::Overlay),
            )
            .padding(Padding::from([4.0, 12.0]))
            .class(ContainerClass::OverlayPill),
        );
    }
    for bubble in &chat.bubbles[skip..] {
        let name = author_name(&bubble.line, None);
        let mut who = row![
            text(name.clone())
                .size(type_size::SECONDARY)
                .font(coroom_ui::font::SEMIBOLD)
                .class(TextClass::Overlay)
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center);
        if bubble.line.author.guest == 0 {
            who = who.push(coroom_ui::tag::live(t!("chat-anfitrion")));
        }
        let first_line = bubble.line.text.lines().next().unwrap_or_default();
        // Una línea que quepa en la burbuja; si una letra ancha la pasa, salta en vez de salirse.
        let body = text(ui::ellipsize(first_line, 44).into_owned())
            .size(type_size::SECONDARY)
            .class(TextClass::OverlaySecondary)
            .wrapping(Wrapping::WordOrGlyph);
        list = list.push(
            mouse_area(
                container(
                    row![
                        ui::avatar(&app.theme, &name, 28.0),
                        column![who, body].spacing(2)
                    ]
                    .spacing(space::XS)
                    .align_y(Alignment::Center),
                )
                .padding(Padding {
                    top: 8.0,
                    right: 12.0,
                    bottom: 8.0,
                    left: 10.0,
                })
                .max_width(360)
                .class(ContainerClass::Overlay),
            )
            .interaction(mouse::Interaction::Pointer)
            .on_press(Message::Chat(Msg::Open)),
        );
    }
    Some(list.into())
}

/// Escenas del chat (las monta la pantalla del invitado: `guest::shots`).
pub(crate) mod shots {
    use std::time::Instant;

    use coroom_session::remote::{ChatAuthor, ChatLine, SessionEvent};

    use super::Entry;
    use crate::ui::app::{App, Event, Message};

    /// 21:02 del 15 de octubre de 2026 en UTC (las escenas van con desfase 0).
    pub(crate) const BASE_MS: u64 = 1_792_098_120_000;

    /// Un mensaje de ejemplo `minutes` después de [`BASE_MS`].
    pub(crate) fn line(guest: u32, name: &str, text: &str, minutes: u64, own: bool) -> ChatLine {
        ChatLine {
            author: ChatAuthor {
                guest,
                leaf: guest,
                name: name.to_owned(),
            },
            text: text.to_owned(),
            at_unix_ms: BASE_MS + minutes * 60_000,
            own,
        }
    }

    /// Un aviso del sistema con hora fija.
    pub(crate) fn system(app: &mut App, spans: &[(&str, bool)], minutes: u64) {
        let at_unix_ms = BASE_MS + minutes * 60_000;
        let clock = crate::localtime::hhmm((at_unix_ms / 1000) as i64, 0);
        app.chat.push(Entry::System {
            seq: 0,
            spans: spans
                .iter()
                .map(|(text, bold)| ((*text).to_owned(), *bold))
                .collect(),
            at_unix_ms,
            clock,
        });
    }

    /// Una conversación de ejemplo (invitado «Ana» en la sala de «Bea»).
    pub(crate) fn conversation(app: &mut App) {
        app.chat.set_utc_offset(0, Instant::now());
        app.chat.entries.clear();
        system(app, &[("Entraste a la sala", false)], 0);
        let guest = |app: &mut App, line: ChatLine| {
            app.update(Message::Event(Event::Guest(SessionEvent::ChatMessage(
                line,
            ))));
        };
        guest(
            app,
            line(
                0,
                "Bea",
                "Buenas, arranco en 2 min, estoy actualizando el juego",
                0,
                false,
            ),
        );
        system(app, &[("Carla", true), (" entró a la sala", false)], 1);
        guest(
            app,
            line(
                2,
                "Carla",
                "yo sin micro hoy, os escribo por aquí 🙃",
                1,
                false,
            ),
        );
        system(app, &[("Bea te dio ", false), ("teclado y ratón", true)], 2);
        guest(
            app,
            line(1, "Ana", "vale, el código del lobby es 7KQ2-MA", 3, true),
        );
        guest(
            app,
            line(
                0,
                "Bea",
                "Listo, entrad. Guía de la build: coroom.example.com/guia",
                3,
                false,
            ),
        );
        app.chat.unread = 0;
        app.chat.bubbles.clear();
    }
}

#[cfg(test)]
mod tests {

    use coroom_session::remote::{ChatAuthor, RoomMember};
    use coroom_session::{GuestId, SessionError};

    use super::*;
    use crate::ui::app::tests::app;
    use crate::ui::screens::guest;
    use crate::ui::video::VideoFeed;

    fn line(guest: u32, name: &str, text: &str, own: bool) -> ChatLine {
        ChatLine {
            author: ChatAuthor {
                guest,
                leaf: guest,
                name: name.into(),
            },
            text: text.into(),
            at_unix_ms: 1_792_098_120_000,
            own,
        }
    }

    /// Un invitado dentro de la sala de Bea, con claves.
    fn guest_app() -> App {
        let mut app = app();
        app.chat.set_utc_offset(0, app.now);
        guest::start(&mut app, "K7Q2MZ".into());
        app.update(Message::Event(Event::GuestStarted(Ok(
            VideoFeed::detached(),
        ))));
        app.chat.set_utc_offset(0, app.now);
        app.update(Message::Event(Event::Guest(SessionEvent::Joined {
            host_name: "Bea".into(),
            host_fingerprint: "b7".repeat(32),
            safety_number: coroom_transport::crypto::SafetyNumber::between(&[1; 32], &[2; 32]),
        })));
        app.update(Message::Event(Event::Guest(SessionEvent::EpochChanged {
            epoch: 1,
        })));
        app
    }

    fn message(app: &mut App, line: ChatLine) -> Vec<Effect> {
        app.update(Message::Event(Event::Guest(SessionEvent::ChatMessage(
            line,
        ))))
    }

    fn type_text(app: &mut App, text: &str) {
        app.chat.draft = text_editor::Content::with_text(text);
    }

    #[test]
    fn history_replaces_and_messages_append() {
        let mut app = guest_app();
        message(&mut app, line(0, "Bea", "hola", false));
        app.update(Message::Event(Event::Guest(SessionEvent::ChatHistory {
            messages: vec![line(0, "Bea", "uno", false), line(2, "Carla", "dos", false)],
        })));
        let texts: Vec<&str> = app
            .chat
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Message { line, .. } => Some(line.text.as_str()),
                Entry::System { .. } => None,
            })
            .collect();
        assert_eq!(texts, ["uno", "dos"], "el historial sustituye");
        assert_eq!(app.chat.unread, 1, "el historial no cuenta como nuevo");
        message(&mut app, line(2, "Carla", "tres", false));
        assert!(matches!(
            app.chat.entries.last(),
            Some(Entry::Message { line, clock, .. }) if line.text == "tres" && clock == "21:02"
        ));
    }

    #[test]
    fn unread_and_bubbles_with_the_panel_closed() {
        let mut app = guest_app();
        app.settings.room.chat_sound = true;
        let effects = message(&mut app, line(0, "Bea", "uno", false));
        assert_eq!(effects, [Effect::ChatSound], "con el panel cerrado suena");
        for n in 0..4 {
            message(&mut app, line(2, "Carla", &format!("{n}"), false));
        }
        assert_eq!(app.chat.unread, 5);
        assert_eq!(app.chat.bubbles.len(), 5);
        assert!(bubbles(&app).is_some());
        // Caducan a los 5 s.
        assert_eq!(app.chat.next_tick(app.now), Some(app.now + BUBBLE_LIFE));
        app.now += BUBBLE_LIFE;
        app.update(Message::Tick);
        assert!(app.chat.bubbles.is_empty());
        assert_eq!(app.chat.unread, 5, "el contador queda hasta abrir");
        // Abrir el chat borra el contador y enfoca el campo.
        let effects = app.update(Message::Chat(Msg::Open));
        assert_eq!(effects, [Effect::FocusChat(true)]);
        assert_eq!(app.chat.unread, 0);
        assert!(app.chat.composing);
        assert_eq!(app.guest.as_ref().and_then(|g| g.panel), Some(Tab::Chat));
        // Con el panel abierto, ni contador ni burbuja ni sonido.
        let effects = message(&mut app, line(0, "Bea", "cuatro", false));
        assert!(effects.is_empty());
        assert_eq!(app.chat.unread, 0);
        assert!(app.chat.bubbles.is_empty());
        // Lo propio nunca avisa.
        app.update(Message::Chat(Msg::Close));
        assert!(message(&mut app, line(1, "Ana", "mío", true)).is_empty());
        assert_eq!(app.chat.unread, 0);
    }

    #[test]
    fn notifications_only_without_window_focus() {
        let mut app = guest_app();
        app.settings.room.chat_sound = false;
        app.update(Message::Chat(Msg::Open));
        assert!(message(&mut app, line(0, "Bea", "hola", false)).is_empty());
        app.update(Message::WindowFocused(false));
        let effects = message(&mut app, line(2, "Carla", "¿estás?", false));
        assert_eq!(
            effects,
            [Effect::ChatNotify {
                title: "Carla · Sala de Bea".into(),
                body: "¿estás?".into()
            }]
        );
        app.settings.room.chat_notifications = false;
        assert!(message(&mut app, line(2, "Carla", "otra", false)).is_empty());
    }

    #[test]
    fn sending_waits_for_the_echo_and_shows_errors() {
        let mut app = guest_app();
        assert!(
            app.update(Message::Chat(Msg::Send)).is_empty(),
            "vacío no envía"
        );
        type_text(&mut app, "   \n ");
        assert!(
            app.update(Message::Chat(Msg::Send)).is_empty(),
            "solo espacios tampoco"
        );
        type_text(&mut app, "  hola\n");
        assert_eq!(
            app.update(Message::Chat(Msg::Send)),
            [Effect::SendChat("hola".into())]
        );
        assert!(app.chat.draft.text().trim().is_empty(), "el campo se vacía");
        assert!(
            !app.chat
                .entries
                .iter()
                .any(|entry| matches!(entry, Entry::Message { .. })),
            "no se pinta hasta que vuelve"
        );
        // El ritmo: el error se ve bajo el campo y el texto vuelve.
        let reason = coroom_session::remote::chat::rate_message(Duration::from_secs(3));
        app.update(Message::Event(Event::ChatFailed(
            SessionError::Chat(reason.clone()).to_string(),
        )));
        assert!(
            app.chat
                .error
                .as_deref()
                .is_some_and(|e| e.contains("Vas muy rápido"))
        );
        assert_eq!(app.chat.draft.text().trim(), "hola");
        // Escribir borra el error.
        app.update(Message::Chat(Msg::Edit(text_editor::Action::Edit(
            text_editor::Edit::Insert('!'),
        ))));
        assert_eq!(app.chat.error, None);
        // Demasiado largo: no sale.
        type_text(&mut app, &"a".repeat(CHAT_TEXT_MAX_CHARS + 1));
        assert!(app.update(Message::Chat(Msg::Send)).is_empty());
        assert!(app.chat.error.is_some());
    }

    #[test]
    fn escape_returns_the_keyboard_without_closing() {
        let mut app = guest_app();
        app.update(Message::Chat(Msg::Toggle));
        assert!(app.chat.composing && app.chat.open);
        assert!(app.chat.takes_escape());
        assert_eq!(
            app.update(Message::Shortcut(crate::ui::app::Shortcut::Escape)),
            [Effect::FocusChat(false)]
        );
        assert!(!app.chat.composing);
        assert!(app.chat.open, "el panel sigue abierto");
        assert!(!app.chat.takes_escape());
        // Ctrl+Alt+C con el panel abierto sin escribir: vuelve al campo; escribiendo, cierra.
        assert_eq!(
            app.update(Message::Chat(Msg::Toggle)),
            [Effect::FocusChat(true)]
        );
        assert_eq!(
            app.update(Message::Chat(Msg::Toggle)),
            [Effect::FocusChat(false)]
        );
        assert!(!app.chat.open);
        assert_eq!(app.guest.as_ref().and_then(|g| g.panel), None);
    }

    #[test]
    fn opening_in_immersive_mode_releases_the_mouse() {
        let mut app = guest_app();
        if let Some(state) = app.guest.as_mut() {
            state.immersive = true;
        }
        assert_eq!(
            app.update(Message::Chat(Msg::Toggle)),
            [Effect::SetImmersive(false), Effect::FocusChat(true)]
        );
    }

    #[test]
    fn clearing_and_system_lines() {
        let mut app = app();
        app.chat.set_utc_offset(0, app.now);
        // Invitado: entra, llegan y se van personas.
        let mut guest_app = guest_app();
        let member = |guest: u32, name: &str, you: bool| RoomMember {
            guest,
            leaf: guest,
            name: name.into(),
            you,
        };
        let members =
            |list: Vec<RoomMember>| Message::Event(Event::Guest(SessionEvent::Members(list)));
        guest_app.update(members(vec![
            member(0, "Bea", false),
            member(1, "Ana", true),
        ]));
        guest_app.update(members(vec![
            member(0, "Bea", false),
            member(1, "Ana", true),
            member(2, "Carla", false),
        ]));
        guest_app.update(members(vec![
            member(0, "Bea", false),
            member(1, "Ana", true),
        ]));
        let system: Vec<String> = guest_app
            .chat
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::System { spans, .. } => {
                    Some(spans.iter().map(|(t, _)| t.as_str()).collect::<String>())
                }
                Entry::Message { .. } => None,
            })
            .collect();
        assert_eq!(
            system,
            [
                "Entraste a la sala",
                "Carla entró a la sala",
                "Carla salió de la sala"
            ]
        );
        // Permisos que te dan.
        let mut stats = guest::shots::sample_stats();
        stats.allowed = Allowed::default();
        guest_app.update(Message::Event(Event::GuestStats(Box::new(stats.clone()))));
        stats.allowed.keyboard_mouse = true;
        guest_app.update(Message::Event(Event::GuestStats(Box::new(stats))));
        assert!(matches!(
            guest_app.chat.entries.last(),
            Some(Entry::System { spans, .. })
                if spans.iter().any(|(t, bold)| *bold && t == "teclado y ratón")
        ));
        // El anfitrión vació el chat.
        message(&mut guest_app, line(0, "Bea", "hola", false));
        guest_app.update(Message::Event(Event::Guest(SessionEvent::ChatCleared)));
        assert_eq!(guest_app.chat.entries.len(), 1);
        assert_eq!(guest_app.chat.unread, 0);
        // Anfitrión: vaciar pide confirmación y solo él puede.
        app.update(Message::Chat(Msg::AskClear));
        assert!(!app.chat.confirm_clear, "sin sala no hay nada que vaciar");
        assert!(app.update(Message::Chat(Msg::ConfirmClear)).is_empty());
        app.update(Message::Event(Event::Host(SessionEvent::GuestJoined {
            guest: GuestId(3),
            user: 9,
            name: "Ana".into(),
            fingerprint: "ab".repeat(32),
            safety_number: coroom_transport::crypto::SafetyNumber::between(&[1; 32], &[2; 32]),
            permissions: Permissions::VIEW_ONLY,
        })));
        app.update(Message::Event(Event::Host(SessionEvent::GuestLeft {
            guest: GuestId(3),
            kicked: true,
        })));
        assert!(matches!(
            app.chat.entries.last(),
            Some(Entry::System { spans, .. }) if spans.iter().any(|(t, _)| t.contains("Expulsaste"))
        ));
    }

    #[test]
    fn links_are_detected() {
        let text = "Guía: coroom.example.com/guia, y (https://coroom.example.com/x?y=1). No: 3.5, \
                    ana@example.com, 7KQ2-MA, Sr.Ana";
        let links: Vec<(&str, String)> = segments(text)
            .into_iter()
            .filter_map(|(piece, link)| link.map(|url| (piece, url)))
            .collect();
        assert_eq!(
            links,
            [
                (
                    "coroom.example.com/guia",
                    "https://coroom.example.com/guia".to_owned()
                ),
                (
                    "https://coroom.example.com/x?y=1",
                    "https://coroom.example.com/x?y=1".to_owned()
                ),
            ]
        );
        let joined: String = segments(text).into_iter().map(|(piece, _)| piece).collect();
        assert_eq!(joined, text, "los trozos rehacen el texto");
        assert_eq!(
            link_target("example.com"),
            Some("https://example.com".into())
        );
        assert_eq!(link_target("archivo.txt"), None);
        assert_eq!(
            link_target("http://192.0.2.10:8443/a"),
            Some("http://192.0.2.10:8443/a".into())
        );
    }

    #[test]
    fn emphasis_marks_the_values() {
        assert_eq!(
            emphasize("Bea te dio teclado y ratón", &["teclado y ratón"]),
            [
                ("Bea te dio ".to_owned(), false),
                ("teclado y ratón".to_owned(), true)
            ]
        );
    }
}
