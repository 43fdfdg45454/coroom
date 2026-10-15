//! Chat de texto de la sala: validación del texto, límite de ritmo, historial del anfitrión y
//! cifrado con la clave de chat de la época MLS (`coroom_crypto::chat`).
//!
//! - **Topología en estrella**: el invitado sella su mensaje con la clave de la época más nueva
//!   que tiene y lo manda al anfitrión por el control ([`ControlMessage::Chat`]); el anfitrión lo
//!   abre, comprueba que lo selló la hoja MLS de ese invitado, valida el texto y el ritmo, pone
//!   autor (id de invitado, hoja y nombre) y hora, lo guarda y lo vuelve a sellar como suyo para
//!   todos los invitados conectados, también para el autor, que así lo ve en el orden y con la hora
//!   del anfitrión (`own: true`). Lo que escribe el anfitrión va igual, con él como autor.
//! - **Historial**: solo en memoria del anfitrión, los últimos [`CHAT_HISTORY_MAX`] mensajes en
//!   claro (es miembro y lo descifra todo); a quien entra se los recifra con la época actual en un
//!   solo [`ControlMessage::ChatHistory`]. Vaciar ([`ControlMessage::ChatClear`]) lo borra para
//!   todos; expulsar a alguien no borra lo que escribió. Al cerrar la sala desaparece.
//! - **Ritmo**: [`CHAT_RATE_MESSAGES`] mensajes cada [`CHAT_RATE_WINDOW`] por persona. El anfitrión
//!   cuenta desde que le llega cada mensaje y descarta lo que pase; el invitado lo comprueba antes
//!   de enviar contando desde que el anfitrión le devuelve cada mensaje suyo (que es después de
//!   que le llegase), así que lo que el invitado deja salir nunca lo descarta el anfitrión aunque
//!   la red junte los mensajes. Un mensaje que el anfitrión no devuelve deja de contar a los
//!   [`CHAT_ECHO_EXPIRY`].
//! - **Épocas**: el anfitrión sella siempre con la más nueva; un invitado que aún no procesó el
//!   commit guarda lo que llega de una época que no tiene (hasta [`CHAT_PENDING_MAX`]) y lo abre
//!   al instalarla.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use coroom_transport::crypto::chat::{ChatCryptoError, ChatKey, ChatKind, ChatRing, Sealed};
use coroom_transport::proto::codec;
use coroom_transport::proto::control::{
    CHAT_HISTORY_MAX, CHAT_HISTORY_SEALED_MAX, CHAT_MESSAGE_SEALED_MAX, CHAT_TEXT_MAX_BYTES,
    CHAT_TEXT_MAX_CHARS, ChatEntry, ControlMessage, SealedChat,
};

use super::{Core, SessionEvent, lock};

/// Mensajes de chat como mucho por persona en cada [`CHAT_RATE_WINDOW`].
pub const CHAT_RATE_MESSAGES: usize = 10;

/// Ventana del límite de ritmo del chat.
pub const CHAT_RATE_WINDOW: Duration = Duration::from_secs(10);

/// Lo que cuenta en el límite del invitado un mensaje suyo que el anfitrión no devolvió (lo
/// descartó): dos ventanas desde que salió.
pub const CHAT_ECHO_EXPIRY: Duration = Duration::from_secs(20);

/// Mensajes de una época que el invitado aún no tiene que guarda como mucho.
pub const CHAT_PENDING_MAX: usize = 64;

/// Autor de un mensaje del chat.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChatAuthor {
    /// Id del invitado (0 = el anfitrión), el de [`super::RoomMember::guest`].
    pub guest: u32,
    /// Hoja MLS del autor cuando escribió (se puede reutilizar cuando sale alguien).
    pub leaf: u32,
    /// Nombre visible. Vacío solo en el anfitrión, para lo que escribe él (`own`).
    pub name: String,
}

/// Un mensaje del chat ([`SessionEvent::ChatMessage`], [`SessionEvent::ChatHistory`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChatLine {
    /// Quién lo escribió.
    pub author: ChatAuthor,
    /// Texto plano (sin formato; con saltos de línea y emoji).
    pub text: String,
    /// Hora Unix en ms que puso el anfitrión.
    pub at_unix_ms: u64,
    /// Si lo escribió este equipo.
    pub own: bool,
}

impl std::fmt::Display for ChatLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let who = match (self.author.name.as_str(), self.own) {
            ("", true) => "Tú",
            ("", false) => "Anfitrión",
            (name, _) => name,
        };
        write!(f, "{who}: {}", self.text)
    }
}

/// El texto que se enviaría, o por qué no se puede: sin espacios al principio ni al final,
/// saltos de línea como `\n`, sin caracteres de control (salvo salto de línea y tabulador), y
/// dentro de [`CHAT_TEXT_MAX_CHARS`] caracteres y [`CHAT_TEXT_MAX_BYTES`] bytes.
pub fn validate_text(text: &str) -> Result<String, String> {
    if text.len() > CHAT_TEXT_MAX_BYTES * 2 {
        return Err(too_long(text.chars().count()));
    }
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let text = text.trim();
    if text.is_empty() {
        return Err("El mensaje está vacío.".to_owned());
    }
    let chars = text.chars().count();
    if chars > CHAT_TEXT_MAX_CHARS || text.len() > CHAT_TEXT_MAX_BYTES {
        return Err(too_long(chars));
    }
    if text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err("El mensaje solo puede llevar texto: tiene caracteres de control.".to_owned());
    }
    Ok(text.to_owned())
}

fn too_long(chars: usize) -> String {
    format!(
        "El mensaje es demasiado largo: como mucho {CHAT_TEXT_MAX_CHARS} caracteres (tiene \
         {chars})."
    )
}

/// Error del límite de ritmo, en español, con lo que falta para poder enviar.
pub fn rate_message(wait: Duration) -> String {
    let secs = wait.as_secs_f64().ceil().max(1.0) as u64;
    format!(
        "Vas muy rápido: espera {secs} s antes de enviar otro mensaje (como mucho \
         {CHAT_RATE_MESSAGES} cada {} s).",
        CHAT_RATE_WINDOW.as_secs()
    )
}

/// Hora Unix en ms del reloj del sistema.
pub(crate) fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// Un mensaje que cuenta para el límite: desde cuándo (si ya se confirmó) y cuándo salió.
#[derive(Debug, Clone, Copy)]
struct Slot {
    sent: Instant,
    confirmed: Option<Instant>,
}

impl Slot {
    fn expires(&self) -> Instant {
        match self.confirmed {
            Some(at) => at + CHAT_RATE_WINDOW,
            None => self.sent + CHAT_ECHO_EXPIRY,
        }
    }
}

/// Límite de ritmo de una persona: [`CHAT_RATE_MESSAGES`] por [`CHAT_RATE_WINDOW`].
#[derive(Debug, Default)]
pub(crate) struct RateLimiter {
    slots: VecDeque<Slot>,
}

impl RateLimiter {
    fn expire(&mut self, now: Instant) {
        self.slots.retain(|slot| slot.expires() > now);
    }

    /// `Ok` si cabe un mensaje más ahora; si no, cuánto falta.
    pub(crate) fn check(&mut self, now: Instant) -> Result<(), Duration> {
        self.expire(now);
        if self.slots.len() < CHAT_RATE_MESSAGES {
            return Ok(());
        }
        let first = self.slots.iter().map(Slot::expires).min().unwrap_or(now);
        Err(first.saturating_duration_since(now))
    }

    /// Anota un mensaje: confirmado ya (el anfitrión, que cuenta al recibirlo) o a la espera de
    /// que el anfitrión lo devuelva ([`RateLimiter::confirm`]).
    pub(crate) fn record(&mut self, now: Instant, confirmed: bool) {
        self.slots.push_back(Slot {
            sent: now,
            confirmed: confirmed.then_some(now),
        });
    }

    /// [`RateLimiter::check`] y, si cabe, [`RateLimiter::record`].
    pub(crate) fn try_record(&mut self, now: Instant, confirmed: bool) -> Result<(), Duration> {
        self.check(now)?;
        self.record(now, confirmed);
        Ok(())
    }

    /// El anfitrión devolvió el mensaje propio más antiguo sin confirmar: cuenta desde ahora.
    pub(crate) fn confirm(&mut self, now: Instant) {
        if let Some(slot) = self.slots.iter_mut().find(|slot| slot.confirmed.is_none()) {
            slot.confirmed = Some(now);
        }
    }
}

/// Lo que abrió un invitado de lo que le mandó el anfitrión.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Opened {
    Message(ChatLine),
    History(Vec<ChatLine>),
    Clear,
}

impl Opened {
    pub(crate) fn into_event(self) -> SessionEvent {
        match self {
            Opened::Message(line) => SessionEvent::ChatMessage(line),
            Opened::History(messages) => SessionEvent::ChatHistory { messages },
            Opened::Clear => SessionEvent::ChatCleared,
        }
    }
}

fn to_wire(sealed: Sealed) -> SealedChat {
    SealedChat {
        epoch: sealed.epoch,
        sender_leaf: sealed.sender_leaf,
        counter: sealed.counter,
        ciphertext: sealed.ciphertext,
    }
}

fn from_wire(sealed: &SealedChat) -> Sealed {
    Sealed {
        epoch: sealed.epoch,
        sender_leaf: sealed.sender_leaf,
        counter: sealed.counter,
        ciphertext: sealed.ciphertext.clone(),
    }
}

/// El tipo de sellado de un mensaje de control del chat (`None` si no es del chat).
pub(crate) fn kind_of(message: &ControlMessage) -> Option<(ChatKind, &SealedChat)> {
    match message {
        ControlMessage::Chat(sealed) => Some((ChatKind::Message, sealed)),
        ControlMessage::ChatHistory(sealed) => Some((ChatKind::History, sealed)),
        ControlMessage::ChatClear(sealed) => Some((ChatKind::Clear, sealed)),
        _ => None,
    }
}

fn wrap(kind: ChatKind, sealed: SealedChat) -> ControlMessage {
    match kind {
        ChatKind::Message => ControlMessage::Chat(sealed),
        ChatKind::History => ControlMessage::ChatHistory(sealed),
        ChatKind::Clear => ControlMessage::ChatClear(sealed),
    }
}

fn sealed_max(kind: ChatKind) -> usize {
    match kind {
        ChatKind::Message => CHAT_MESSAGE_SEALED_MAX,
        ChatKind::History => CHAT_HISTORY_SEALED_MAX,
        ChatKind::Clear => coroom_transport::proto::control::CHAT_TAG_LEN,
    }
}

/// Estado del chat de una sesión (en [`Core`]).
#[derive(Debug, Default)]
pub(crate) struct ChatState {
    ring: ChatRing,
    /// Anfitrión: los últimos mensajes, en claro.
    history: VecDeque<ChatEntry>,
    /// Límite propio.
    own_rate: RateLimiter,
    /// Anfitrión: el límite de cada invitado.
    guest_rates: BTreeMap<u32, RateLimiter>,
    /// Invitado: lo que llegó de una época que aún no tiene.
    pending: VecDeque<(ChatKind, SealedChat)>,
    /// Invitado: sus ids en la sala (uno nuevo cada vez que vuelve a entrar).
    own_guests: BTreeSet<u32>,
}

impl ChatState {
    /// Instala la clave de una época nueva (ver [`ChatRing::install`]).
    pub(crate) fn install(&mut self, key: ChatKey, forget_older: bool) -> Result<(), String> {
        self.ring
            .install(key, forget_older)
            .map_err(|err| err.to_string())
    }

    /// Época de la clave más nueva.
    pub(crate) fn epoch(&self) -> Option<u64> {
        self.ring.epoch()
    }

    /// Clave más nueva.
    pub(crate) fn newest_key(&self) -> Option<ChatKey> {
        self.ring.newest().cloned()
    }

    fn seal(
        &mut self,
        kind: ChatKind,
        own_leaf: u32,
        plaintext: &[u8],
    ) -> Result<ControlMessage, String> {
        self.ring
            .seal(kind, own_leaf, plaintext)
            .map(|sealed| wrap(kind, to_wire(sealed)))
            .map_err(|err| err.to_string())
    }

    // --- Anfitrión ---------------------------------------------------------------------------

    /// Anfitrión: anota un mensaje propio en el límite, o dice cuánto falta.
    pub(crate) fn own_rate(&mut self, now: Instant, confirmed: bool) -> Result<(), String> {
        self.own_rate
            .try_record(now, confirmed)
            .map_err(rate_message)
    }

    /// Anfitrión: abre un `Chat` del invitado `guest` (hoja `leaf`, nombre `name`) y lo convierte
    /// en la entrada del historial con autor y hora; si no vale, por qué se descarta.
    pub(crate) fn accept_from_guest(
        &mut self,
        guest: u32,
        leaf: u32,
        name: &str,
        sealed: &SealedChat,
        now: Instant,
    ) -> Result<ChatEntry, String> {
        if sealed.sender_leaf != leaf {
            return Err(format!(
                "lo selló la hoja {} y la suya es la {leaf}",
                sealed.sender_leaf
            ));
        }
        if sealed.ciphertext.len() > CHAT_MESSAGE_SEALED_MAX {
            return Err("es demasiado grande".to_owned());
        }
        let plaintext = match self.ring.open(ChatKind::Message, &from_wire(sealed)) {
            Ok(plaintext) => plaintext,
            Err(ChatCryptoError::UnknownEpoch(epoch)) => {
                return Err(format!(
                    "llegó cifrado con claves de la época {epoch}, que la sala ya no usa (se \
                     escribió justo cuando alguien salía)"
                ));
            }
            Err(err) => return Err(err.to_string()),
        };
        let entry: ChatEntry = codec::decode(&plaintext)
            .map_err(|err| format!("no es un mensaje del chat ({err})"))?;
        let text = validate_text(&entry.text).map_err(|err| format!("no es válido: {err}"))?;
        self.guest_rates
            .entry(guest)
            .or_default()
            .try_record(now, true)
            .map_err(|_| {
                format!(
                    "va muy rápido (más de {CHAT_RATE_MESSAGES} mensajes en {} s)",
                    CHAT_RATE_WINDOW.as_secs()
                )
            })?;
        Ok(ChatEntry {
            guest,
            leaf,
            name: super::host::truncate_name(name),
            text,
            // La pone `post`, en el orden del historial.
            at_unix_ms: 0,
        })
    }

    /// Anfitrión: pone la hora a `entry` (nunca anterior a la del último mensaje, para que el
    /// orden del historial y el de las horas coincidan), lo guarda y lo sella para todos.
    pub(crate) fn post(
        &mut self,
        mut entry: ChatEntry,
        own_leaf: u32,
    ) -> Result<(ControlMessage, ChatEntry), String> {
        let last = self.history.back().map_or(0, |last| last.at_unix_ms);
        entry.at_unix_ms = unix_ms_now().max(last);
        let plaintext = codec::encode_body(&entry).map_err(|err| err.to_string())?;
        let message = self.seal(ChatKind::Message, own_leaf, &plaintext)?;
        self.history.push_back(entry.clone());
        while self.history.len() > CHAT_HISTORY_MAX {
            self.history.pop_front();
        }
        Ok((message, entry))
    }

    /// Anfitrión: los últimos [`CHAT_HISTORY_MAX`] mensajes, sellados con la época actual.
    pub(crate) fn history_message(&mut self, own_leaf: u32) -> Result<ControlMessage, String> {
        let history: Vec<&ChatEntry> = self.history.iter().collect();
        let plaintext = codec::encode_body(&history).map_err(|err| err.to_string())?;
        self.seal(ChatKind::History, own_leaf, &plaintext)
    }

    /// Anfitrión: vacía el historial y sella la orden para todos.
    pub(crate) fn clear(&mut self, own_leaf: u32) -> Result<ControlMessage, String> {
        let message = self.seal(ChatKind::Clear, own_leaf, &[])?;
        self.history.clear();
        Ok(message)
    }

    /// Anfitrión: el invitado salió; su límite se olvida (lo que escribió se queda).
    pub(crate) fn forget_guest(&mut self, guest: u32) {
        self.guest_rates.remove(&guest);
    }

    /// Anfitrión: mensajes guardados.
    #[cfg(test)]
    pub(crate) fn history(&self) -> Vec<ChatEntry> {
        self.history.iter().cloned().collect()
    }

    // --- Invitado ----------------------------------------------------------------------------

    /// Invitado: su id en la sala (`your_peer_id` de `Joined`).
    pub(crate) fn add_own_guest(&mut self, guest: u32) {
        self.own_guests.insert(guest);
    }

    /// Invitado: sella `text` (ya validado) hacia el anfitrión, si hay clave y lo deja el límite.
    pub(crate) fn seal_own(
        &mut self,
        own_leaf: u32,
        text: &str,
        now: Instant,
    ) -> Result<ControlMessage, String> {
        if self.ring.epoch().is_none() {
            return Err("El chat aún no está disponible: faltan las claves de la sala.".to_owned());
        }
        self.own_rate.check(now).map_err(rate_message)?;
        // El anfitrión pone autor y hora: aquí solo cuenta el texto.
        let entry = ChatEntry {
            guest: 0,
            leaf: own_leaf,
            name: String::new(),
            text: text.to_owned(),
            at_unix_ms: 0,
        };
        let plaintext = codec::encode_body(&entry).map_err(|err| err.to_string())?;
        let message = self.seal(ChatKind::Message, own_leaf, &plaintext)?;
        self.own_rate.record(now, false);
        Ok(message)
    }

    fn line(&self, entry: ChatEntry, host_name: &str) -> ChatLine {
        let own = entry.guest != 0 && self.own_guests.contains(&entry.guest);
        let name = if entry.guest == 0 && entry.name.is_empty() {
            host_name.to_owned()
        } else {
            entry.name
        };
        ChatLine {
            author: ChatAuthor {
                guest: entry.guest,
                leaf: entry.leaf,
                name,
            },
            text: entry.text,
            at_unix_ms: entry.at_unix_ms,
            own,
        }
    }

    /// Invitado: abre lo que manda el anfitrión (hoja `host_leaf`). `Ok(None)` si es de una
    /// época que aún no tiene (se guarda y se abre al instalarla).
    pub(crate) fn open_from_host(
        &mut self,
        kind: ChatKind,
        sealed: &SealedChat,
        host_leaf: u32,
        host_name: &str,
        now: Instant,
    ) -> Result<Option<Opened>, String> {
        if sealed.sender_leaf != host_leaf {
            return Err(format!(
                "lo selló la hoja {}, que no es el anfitrión",
                sealed.sender_leaf
            ));
        }
        if sealed.ciphertext.len() > sealed_max(kind) {
            return Err("es demasiado grande".to_owned());
        }
        let plaintext = match self.ring.open(kind, &from_wire(sealed)) {
            Ok(plaintext) => plaintext,
            Err(ChatCryptoError::UnknownEpoch(epoch))
                if self.ring.epoch().is_none_or(|newest| epoch > newest) =>
            {
                if self.pending.len() >= CHAT_PENDING_MAX {
                    self.pending.pop_front();
                }
                self.pending.push_back((kind, sealed.clone()));
                return Ok(None);
            }
            Err(err) => return Err(err.to_string()),
        };
        let opened = match kind {
            ChatKind::Message => {
                let entry: ChatEntry = codec::decode(&plaintext).map_err(|err| err.to_string())?;
                if !entry.within_limits() {
                    return Err("fuera de los límites".to_owned());
                }
                let line = self.line(entry, host_name);
                if line.own {
                    self.own_rate.confirm(now);
                }
                Opened::Message(line)
            }
            ChatKind::History => {
                let entries: Vec<ChatEntry> =
                    codec::decode(&plaintext).map_err(|err| err.to_string())?;
                if !ChatEntry::history_within_limits(&entries) {
                    return Err("historial fuera de los límites".to_owned());
                }
                Opened::History(
                    entries
                        .into_iter()
                        .map(|entry| self.line(entry, host_name))
                        .collect(),
                )
            }
            ChatKind::Clear => Opened::Clear,
        };
        Ok(Some(opened))
    }

    /// Invitado: vuelve a intentar lo guardado tras instalar una época.
    pub(crate) fn flush_pending(
        &mut self,
        host_leaf: u32,
        host_name: &str,
        now: Instant,
    ) -> Vec<Result<Opened, String>> {
        let pending = std::mem::take(&mut self.pending);
        pending
            .into_iter()
            .filter_map(|(kind, sealed)| {
                self.open_from_host(kind, &sealed, host_leaf, host_name, now)
                    .transpose()
            })
            .collect()
    }
}

/// Invitado: instala la clave del chat de la época que acaba de procesar y abre lo que esperaba
/// por ella.
pub(crate) fn install_guest_key(core: &Core, key: ChatKey, forget_older: bool) {
    let host_leaf = core.host_leaf.load(std::sync::atomic::Ordering::Acquire);
    let host_name = lock(&core.link).host_name.clone();
    let opened = {
        let mut chat = lock(&core.chat);
        if let Err(err) = chat.install(key, forget_older) {
            tracing::debug!(%err, "clave del chat no instalada");
            return;
        }
        chat.flush_pending(host_leaf, &host_name, Instant::now())
    };
    for result in opened {
        match result {
            Ok(opened) => core.emit(opened.into_event()),
            Err(err) => tracing::debug!(%err, "mensaje del chat descartado"),
        }
    }
}

/// Invitado: un mensaje del chat del anfitrión.
pub(crate) fn on_host_message(core: &Core, kind: ChatKind, sealed: &SealedChat) {
    let host_leaf = core.host_leaf.load(std::sync::atomic::Ordering::Acquire);
    let host_name = lock(&core.link).host_name.clone();
    let opened =
        lock(&core.chat).open_from_host(kind, sealed, host_leaf, &host_name, Instant::now());
    match opened {
        Ok(Some(opened)) => core.emit(opened.into_event()),
        Ok(None) => tracing::debug!(
            epoch = sealed.epoch,
            "mensaje del chat de una época que aún no está; se guarda"
        ),
        Err(err) => tracing::debug!(%err, "mensaje del chat del anfitrión descartado"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(epoch: u64) -> ChatKey {
        ChatKey::new(epoch, [epoch as u8; 32])
    }

    #[test]
    fn chat_events_read_in_spanish() {
        let line = ChatLine {
            author: ChatAuthor {
                guest: 2,
                leaf: 1,
                name: "Bruno".into(),
            },
            text: "¿empezamos? 🎮".into(),
            at_unix_ms: 0,
            own: false,
        };
        assert_eq!(
            SessionEvent::ChatMessage(line.clone()).to_string(),
            "Bruno: ¿empezamos? 🎮"
        );
        let mine = ChatLine {
            author: ChatAuthor {
                guest: 0,
                leaf: 0,
                name: String::new(),
            },
            own: true,
            ..line.clone()
        };
        assert_eq!(mine.to_string(), "Tú: ¿empezamos? 🎮");
        assert_eq!(
            SessionEvent::ChatHistory {
                messages: vec![line.clone(), mine]
            }
            .to_string(),
            "Historial del chat (2 mensajes):\n  Bruno: ¿empezamos? 🎮\n  Tú: ¿empezamos? 🎮"
        );
        assert_eq!(
            SessionEvent::ChatHistory { messages: vec![] }.to_string(),
            "Historial del chat: vacío."
        );
        assert_eq!(
            SessionEvent::ChatCleared.to_string(),
            "El anfitrión ha vaciado el chat."
        );
        assert_eq!(
            SessionEvent::ChatRejected {
                reason: "Bruno va muy rápido".into()
            }
            .to_string(),
            "Mensaje del chat descartado: Bruno va muy rápido."
        );
        assert_eq!(
            crate::SessionError::Chat(rate_message(Duration::from_millis(1_500))).to_string(),
            "Vas muy rápido: espera 2 s antes de enviar otro mensaje (como mucho 10 cada 10 s)."
        );
    }

    #[test]
    fn texts_are_trimmed_and_checked_in_spanish() {
        assert_eq!(
            validate_text("  hola 👋\r\nqué tal  "),
            Ok("hola 👋\nqué tal".into())
        );
        assert_eq!(validate_text("👨‍👩‍👧 familia"), Ok("👨‍👩‍👧 familia".into()));
        assert_eq!(
            validate_text(" \n\t "),
            Err("El mensaje está vacío.".into())
        );
        assert!(validate_text(&"a".repeat(CHAT_TEXT_MAX_CHARS)).is_ok());
        let long = validate_text(&"ñ".repeat(CHAT_TEXT_MAX_CHARS + 1)).unwrap_err();
        assert!(long.contains("como mucho 2000 caracteres"), "{long}");
        assert!(long.contains("2001"), "{long}");
        assert!(validate_text(&"😀".repeat(CHAT_TEXT_MAX_CHARS)).is_ok());
        assert!(validate_text(&"x".repeat(CHAT_TEXT_MAX_BYTES * 3)).is_err());
        assert!(
            validate_text("hola\u{7}")
                .unwrap_err()
                .contains("caracteres de control")
        );
        assert!(validate_text("a\tb").is_ok());
    }

    #[test]
    fn the_rate_limit_allows_ten_messages_every_ten_seconds() {
        let start = Instant::now();
        let mut limit = RateLimiter::default();
        for i in 0..CHAT_RATE_MESSAGES {
            let at = start + Duration::from_millis(100 * i as u64);
            assert_eq!(limit.try_record(at, true), Ok(()), "mensaje {i}");
        }
        let at = start + Duration::from_millis(1_000);
        let wait = limit.try_record(at, true).unwrap_err();
        assert_eq!(wait, Duration::from_secs(9));
        let text = rate_message(wait);
        assert!(text.starts_with("Vas muy rápido"), "{text}");
        assert!(text.contains("espera 9 s"), "{text}");
        assert!(text.contains("como mucho 10 cada 10 s"), "{text}");
        // Un rechazo no ocupa sitio: a los 10 s del primero cabe otro, y solo uno.
        let later = start + CHAT_RATE_WINDOW;
        assert_eq!(limit.try_record(later, true), Ok(()));
        assert!(limit.try_record(later, true).is_err());
        let all_out = start + CHAT_RATE_WINDOW + Duration::from_millis(1_000);
        for _ in 1..CHAT_RATE_MESSAGES {
            assert_eq!(limit.try_record(all_out, true), Ok(()));
        }
        assert!(limit.try_record(all_out, true).is_err());
    }

    /// El invitado cuenta desde que el anfitrión le devuelve cada mensaje: lo que deja salir
    /// nunca lo descarta el anfitrión, aunque la red retrase el primero y adelante el último.
    #[test]
    fn the_guest_limit_counts_from_the_echo_and_never_exceeds_the_host() {
        let start = Instant::now();
        let mut guest = RateLimiter::default();
        let mut host = RateLimiter::default();
        // Diez mensajes seguidos; el primero llega con 3 s de retraso (reenvíos) y su eco igual.
        for i in 0..CHAT_RATE_MESSAGES {
            assert_eq!(guest.try_record(start, false), Ok(()), "mensaje {i}");
        }
        let delayed = start + Duration::from_secs(3);
        for _ in 0..CHAT_RATE_MESSAGES {
            assert_eq!(host.try_record(delayed, true), Ok(()));
            guest.confirm(delayed);
        }
        // A los 10 s de enviarlos el invitado aún no puede: el anfitrión los contó a los 3 s.
        let naive = start + CHAT_RATE_WINDOW;
        let wait = guest.check(naive).unwrap_err();
        assert_eq!(wait, Duration::from_secs(3));
        assert!(host.check(naive).is_err(), "el anfitrión lo descartaría");
        // Cuando el invitado ya puede, el anfitrión también (aunque llegue al instante).
        let allowed = naive + wait;
        assert_eq!(guest.try_record(allowed, false), Ok(()));
        assert_eq!(host.try_record(allowed, true), Ok(()));
        // Un mensaje que el anfitrión nunca devuelve deja de contar a los CHAT_ECHO_EXPIRY.
        let mut lost = RateLimiter::default();
        for _ in 0..CHAT_RATE_MESSAGES {
            lost.record(start, false);
        }
        assert!(lost.check(start + CHAT_RATE_WINDOW).is_err());
        assert_eq!(lost.check(start + CHAT_ECHO_EXPIRY), Ok(()));
    }

    fn sealed_of(message: ControlMessage) -> SealedChat {
        match message {
            ControlMessage::Chat(sealed)
            | ControlMessage::ChatHistory(sealed)
            | ControlMessage::ChatClear(sealed) => sealed,
            other => panic!("no es del chat: {other:?}"),
        }
    }

    /// Un anfitrión (hoja 0) y un invitado (hoja 1) con la misma clave.
    fn pair() -> (ChatState, ChatState) {
        let mut host = ChatState::default();
        let mut guest = ChatState::default();
        host.install(key(3), true).expect("clave");
        guest.install(key(3), true).expect("clave");
        guest.add_own_guest(7);
        (host, guest)
    }

    #[test]
    fn the_host_stamps_author_and_time_and_echoes_to_the_author() {
        let (mut host, mut guest) = pair();
        let now = Instant::now();
        let sealed = sealed_of(guest.seal_own(1, "hola", now).expect("sellar"));
        assert_eq!(sealed.sender_leaf, 1);
        assert!(!sealed.ciphertext.windows(4).any(|w| w == b"hola"));
        // De otra hoja (o de otro invitado con la clave de grupo) no vale.
        assert!(host.accept_from_guest(7, 2, "Bruno", &sealed, now).is_err());
        let entry = host
            .accept_from_guest(7, 1, "Bruno", &sealed, now)
            .expect("aceptar");
        assert_eq!(
            (entry.guest, entry.leaf, entry.name.as_str()),
            (7, 1, "Bruno")
        );
        // Repetido: no.
        assert!(host.accept_from_guest(7, 1, "Bruno", &sealed, now).is_err());
        let before = unix_ms_now();
        let (echo, entry) = host.post(entry, 0).expect("reenviar");
        let echo = sealed_of(echo);
        assert!(entry.at_unix_ms >= before);
        assert_eq!(host.history(), std::slice::from_ref(&entry));
        let opened = guest
            .open_from_host(ChatKind::Message, &echo, 0, "Ana", now)
            .expect("abrir")
            .expect("época conocida");
        let Opened::Message(line) = opened else {
            panic!("se esperaba un mensaje");
        };
        assert!(line.own);
        assert_eq!(line.author.name, "Bruno");
        assert_eq!(line.text, "hola");
        // Lo que escribe el anfitrión: sin nombre en el cable, con el de la señalización aquí.
        let mine = ChatEntry {
            guest: 0,
            leaf: 0,
            name: String::new(),
            text: "bienvenidos".into(),
            at_unix_ms: 1,
        };
        let (sealed, mine) = host.post(mine, 0).expect("enviar");
        // La hora del anfitrión: nunca anterior a la del mensaje previo.
        assert!(mine.at_unix_ms >= entry.at_unix_ms);
        let sealed = sealed_of(sealed);
        let Some(Opened::Message(line)) = guest
            .open_from_host(ChatKind::Message, &sealed, 0, "Ana", now)
            .expect("abrir")
        else {
            panic!("se esperaba un mensaje");
        };
        assert!(!line.own);
        assert_eq!((line.author.guest, line.author.name.as_str()), (0, "Ana"));
        // Solo el anfitrión: lo sellado por otra hoja no se acepta.
        let forged = sealed_of(guest.seal_own(1, "soy el anfitrión", now).expect("sellar"));
        assert!(
            guest
                .open_from_host(ChatKind::Message, &forged, 0, "Ana", now)
                .is_err()
        );
    }

    #[test]
    fn the_host_drops_what_exceeds_the_rate() {
        let (mut host, _) = pair();
        let mut guest = ChatState::default();
        guest.install(key(3), true).expect("clave");
        let now = Instant::now();
        let mut sealed = Vec::new();
        for i in 0..=CHAT_RATE_MESSAGES {
            // El invitado de verdad no deja salir el undécimo; aquí se salta su límite.
            guest.own_rate = RateLimiter::default();
            sealed.push(sealed_of(
                guest.seal_own(1, &format!("{i}"), now).expect("sellar"),
            ));
        }
        for (i, sealed) in sealed.iter().enumerate() {
            let accepted = host.accept_from_guest(4, 1, "Carla", sealed, now);
            if i < CHAT_RATE_MESSAGES {
                assert!(accepted.is_ok(), "{i}: {accepted:?}");
            } else {
                assert!(accepted.unwrap_err().contains("va muy rápido"));
            }
        }
        // Otro invitado tiene su propio límite.
        let mut other = ChatState::default();
        other.install(key(3), true).expect("clave");
        let theirs = sealed_of(other.seal_own(2, "yo también", now).expect("sellar"));
        assert!(host.accept_from_guest(5, 2, "Dani", &theirs, now).is_ok());
    }

    #[test]
    fn the_history_keeps_the_last_fifty_and_clears() {
        let (mut host, mut guest) = pair();
        let now = Instant::now();
        for i in 0..CHAT_HISTORY_MAX + 7 {
            let entry = ChatEntry {
                guest: 0,
                leaf: 0,
                name: String::new(),
                text: format!("mensaje {i}"),
                at_unix_ms: i as u64,
            };
            host.post(entry, 0).expect("enviar");
        }
        let history = host.history();
        assert_eq!(history.len(), CHAT_HISTORY_MAX);
        assert_eq!(history[0].text, "mensaje 7");
        assert_eq!(history[CHAT_HISTORY_MAX - 1].text, "mensaje 56");
        let sealed = sealed_of(host.history_message(0).expect("historial"));
        let Some(Opened::History(lines)) = guest
            .open_from_host(ChatKind::History, &sealed, 0, "Ana", now)
            .expect("abrir")
        else {
            panic!("se esperaba el historial");
        };
        assert_eq!(lines.len(), CHAT_HISTORY_MAX);
        assert_eq!(lines[0].text, "mensaje 7");
        assert!(lines.iter().all(|line| line.author.name == "Ana"));
        // Un historial no se puede hacer pasar por un mensaje.
        assert!(
            guest
                .open_from_host(ChatKind::Message, &sealed, 0, "Ana", now)
                .is_err()
        );
        let clear = sealed_of(host.clear(0).expect("vaciar"));
        assert!(host.history().is_empty());
        assert_eq!(
            guest.open_from_host(ChatKind::Clear, &clear, 0, "Ana", now),
            Ok(Some(Opened::Clear))
        );
        let empty = sealed_of(host.history_message(0).expect("historial"));
        assert_eq!(
            guest.open_from_host(ChatKind::History, &empty, 0, "Ana", now),
            Ok(Some(Opened::History(Vec::new())))
        );
    }

    #[test]
    fn messages_of_an_epoch_not_yet_processed_wait_for_it() {
        let (mut host, mut guest) = pair();
        let now = Instant::now();
        host.install(key(4), false).expect("entra alguien");
        let entry = ChatEntry {
            guest: 0,
            leaf: 0,
            name: String::new(),
            text: "ya en la época 4".into(),
            at_unix_ms: 9,
        };
        let sealed = sealed_of(host.post(entry, 0).expect("enviar").0);
        assert_eq!(sealed.epoch, 4);
        assert_eq!(
            guest.open_from_host(ChatKind::Message, &sealed, 0, "Ana", now),
            Ok(None)
        );
        // Al instalar la época sale lo que esperaba.
        guest.install(key(4), false).expect("commit");
        let opened = guest.flush_pending(0, "Ana", now);
        assert_eq!(opened.len(), 1);
        assert!(matches!(&opened[0], Ok(Opened::Message(line)) if line.text == "ya en la época 4"));
        assert!(guest.flush_pending(0, "Ana", now).is_empty());
        // Una época ya olvidada no se guarda: se descarta.
        guest.install(key(5), true).expect("sale alguien");
        let old = sealed_of(host.history_message(0).expect("historial"));
        assert!(
            guest
                .open_from_host(ChatKind::History, &old, 0, "Ana", now)
                .is_err()
        );
        // Lo que se guarda está acotado.
        for _ in 0..CHAT_PENDING_MAX + 5 {
            let future = SealedChat {
                epoch: 9,
                sender_leaf: 0,
                counter: 0,
                ciphertext: vec![0; 32],
            };
            let _ = guest.open_from_host(ChatKind::Message, &future, 0, "Ana", now);
        }
        assert_eq!(guest.pending.len(), CHAT_PENDING_MAX);
    }

    /// Tras una salida, el anfitrión ya no acepta mensajes de épocas anteriores (el que salió las
    /// conoce y podría falsificarlos con la ayuda del relay).
    #[test]
    fn after_a_departure_older_epochs_are_refused() {
        let (mut host, mut guest) = pair();
        let now = Instant::now();
        let stale = sealed_of(
            guest
                .seal_own(1, "antes de la salida", now)
                .expect("sellar"),
        );
        host.install(key(4), true).expect("sale alguien");
        let refused = host
            .accept_from_guest(7, 1, "Bruno", &stale, now)
            .unwrap_err();
        assert!(refused.contains("época 3"), "{refused}");
        // Tras una entrada la época anterior sigue valiendo (sus miembros siguen todos).
        host.install(key(5), false).expect("entra alguien");
        guest.install(key(4), true).expect("commit de la salida");
        let previous = sealed_of(guest.seal_own(1, "con la 4", now).expect("sellar"));
        assert!(
            host.accept_from_guest(7, 1, "Bruno", &previous, now)
                .is_ok()
        );
    }
}
