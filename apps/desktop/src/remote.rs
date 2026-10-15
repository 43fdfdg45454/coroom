//! `coroom host` y `coroom join`: la sesión remota desde la línea de órdenes.
//!
//! - La identidad se guarda en el llavero del sistema; sin llavero, sin ventana (`--headless`) o
//!   con `--data-dir`, en el fichero `identity.p8` del directorio de datos ([`default_data_dir`]).
//!   El llavero de Linux es bloqueante (D-Bus), así que se lee antes de arrancar la sesión, que es
//!   la que crea su runtime de `tokio`.
//! - El servidor se valida con las raíces del sistema o, en desarrollo, con la huella del
//!   certificado autofirmado (`--trust-fingerprint`, la que imprime `coroom-server` al arrancar).
//! - **Cuentas**: `host` y `join` usan el token guardado por `coroom login` para ese
//!   servidor (y su huella, si se fijó); `--server` puede faltar (el último usado) y
//!   `--dev-token` solo sirve con servidores en modo desarrollo. `coroom host --invite NOMBRE` y la
//!   orden `invite NOMBRE` de la consola invitan a amigos conectados; `coroom join` sin código
//!   espera invitaciones y las lista ([`wait_for_invitation`]).
//! - **Entrada**: `coroom host` atiende órdenes por la consola ([`ConsoleCommand`],
//!   `help` las explica) y `--auto-allow kbm,pad` concede permisos a cada invitado que entra; con
//!   `--source screen` la captura y la inyección comparten **una sola sesión del portal**
//!   (`RemoteDesktop` + `ScreenCast`). `coroom join --headless --input-script FICHERO` reproduce un
//!   guion de entrada ([`play_script`]).
//! - **Audio y voz**: `coroom host --audio none|system|app NOMBRE|process PID --voice
//!   [--mic NOMBRE]` y `coroom join [--mute-game] [--voice] [--ptt TECLA] [--mic] [--speaker]`
//!   ([`HostAudioOptions`], [`JoinAudioOptions`]). Sin ventana (`--headless`) los dispositivos son
//!   sintéticos: tonos en lugar del juego y del micrófono y salidas que no suenan (para los tests).
//! - **Chat**: `say TEXTO` en la consola de `coroom host` y en la de `coroom join --headless`
//!   ([`JoinCommand`]), `clear-chat` en la del anfitrión; los mensajes se imprimen como
//!   `[21:05] Nombre: texto` con la hora local ([`chat_line`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use coroom_render::NullPresenter;
use coroom_session::audio::apps::AppFilter;
use coroom_session::audio::processing::ProcessingOptions;
use coroom_session::audio::synthetic::{CaptureSink, ToneSource};
use coroom_session::audio::{AudioFormat, AudioSink, AudioSource, GameCapture};
use coroom_session::codec::{Backend, Codec, DecodeOutput, DecoderBackend, DecoderSet, EncoderSet};
use coroom_session::input::inject::{SinkOptions, platform_sink};
use coroom_session::input::script::{InputScript, ScriptReport};
use coroom_session::latency::{LatencyLedger, LatencySummary};
use coroom_session::loopback::{FrameSink, LoopbackConfig};
use coroom_session::remote::{
    ChatLine, GuestAudio, GuestInput, GuestStatus, HostAudio, RemoteConfig, RemoteSession,
    SessionEvent, TalkMode, VoiceInput, eis_provider, permissions_label,
};
use coroom_session::{GuestId, Permissions, Session};
use coroom_transport::crypto::{FileStore, Identity, IdentityStore, KeyringStore};
use coroom_transport::proto::signal::SignalMessage;
use coroom_transport::{Endpoint, EndpointOptions, SignalClient};

use crate::account::{self, AccountOptions, AccountStore};
use crate::probe::{PresenterSink, ProbeOptions, Source, make_capturer};

/// Opciones de red comunes a `host` y `join`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetOptions {
    /// URL del servidor (`None` = el último en el que se inició sesión).
    pub server: Option<String>,
    /// Token de desarrollo de la señalización (solo servidores en modo desarrollo); sin él, el
    /// token de la cuenta guardado por `coroom login`.
    pub dev_token: Option<String>,
    /// Huella SHA-256 del certificado autofirmado del servidor de desarrollo.
    pub trust_fingerprint: Option<String>,
    /// Forzar el relay.
    pub force_relay: bool,
    /// Pedir al router que abra el puerto UDP (UPnP, NAT-PMP o PCP) para la conexión directa.
    pub port_mapping: bool,
    /// Directorio de datos (identidad); `None` = llavero o el del usuario.
    pub data_dir: Option<PathBuf>,
    /// Sin ventana (la identidad va a fichero).
    pub headless: bool,
    /// Puerto UDP local fijo (Ajustes → Red); `None` = el que dé el sistema.
    pub local_port: Option<u16>,
    /// Solo en desarrollo: medios sin SFrame.
    #[cfg(debug_assertions)]
    pub insecure_no_sframe: bool,
}

/// Directorio de datos del usuario: `COROOM_DATA_DIR` si está definida y, si no,
/// `$XDG_DATA_HOME/coroom`, `%APPDATA%\coroom` o `~/.local/share/coroom` (en ese orden de
/// preferencia, según lo que defina el sistema).
pub fn default_data_dir() -> PathBuf {
    let from = |var: &str| {
        std::env::var_os(var)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    if let Some(dir) = from("COROOM_DATA_DIR") {
        return dir;
    }
    if let Some(dir) = from("XDG_DATA_HOME") {
        return dir.join("coroom");
    }
    if let Some(dir) = from("APPDATA") {
        return dir.join("coroom");
    }
    if let Some(home) = from("HOME") {
        return home.join(".local").join("share").join("coroom");
    }
    PathBuf::from("coroom-datos")
}

/// Carga (o crea la primera vez) la identidad. Devuelve dónde está guardada.
pub fn load_identity(
    data_dir: Option<&Path>,
    headless: bool,
) -> Result<(Identity, String), String> {
    let file = |dir: &Path| -> Result<(Identity, String), String> {
        let store = FileStore::new(dir);
        let identity = Identity::load_or_create(&store as &dyn IdentityStore)
            .map_err(|err| format!("no se pudo cargar la identidad: {err}"))?;
        Ok((identity, store.path().display().to_string()))
    };
    if let Some(dir) = data_dir {
        return file(dir);
    }
    // Flatpak: la identidad, sellada con la clave del portal `Secret`.
    if !headless && let Some(key) = crate::portal::seal_key() {
        let store = coroom_transport::crypto::SealedFile::identity(default_data_dir(), key);
        let identity = Identity::load_or_create(&store)
            .map_err(|err| format!("no se pudo cargar la identidad: {err}"))?;
        return Ok((
            identity,
            format!("{} (sellada con el portal Secret)", store.path().display()),
        ));
    }
    if !headless && KeyringStore::is_available() {
        match Identity::load_or_create(&KeyringStore::new()) {
            Ok(identity) => return Ok((identity, "llavero del sistema".to_owned())),
            Err(err) => eprintln!("Aviso: el llavero no respondió ({err}); se usa un fichero."),
        }
    }
    file(&default_data_dir())
}

impl NetOptions {
    /// Opciones de cuenta equivalentes (servidor, huella, directorio de datos).
    pub fn account(&self) -> AccountOptions {
        AccountOptions {
            server: self.server.clone(),
            trust_fingerprint: self.trust_fingerprint.clone(),
            tcp_port: None,
            data_dir: self.data_dir.clone(),
            headless: self.headless,
        }
    }

    /// La sesión de la cuenta en el servidor (no en modo desarrollo).
    pub fn session(&self) -> Result<account::Session, String> {
        account::session(&self.account())
    }
}

/// Configuración de la sesión a partir de las opciones: el token de desarrollo si se dio o, si
/// no, el de la cuenta guardada para el servidor (con su huella).
pub fn remote_config(net: &NetOptions) -> Result<RemoteConfig, String> {
    let (server, token, trust) = match net.dev_token.as_deref().filter(|t| !t.is_empty()) {
        Some(dev) => {
            let store = AccountStore::new(net.data_dir.as_deref(), net.headless);
            let server = store.server(net.server.as_deref())?;
            // Sin --trust-fingerprint, la huella guardada al iniciar sesión, si la hay.
            let fingerprint = net.trust_fingerprint.clone().or_else(|| {
                store
                    .load(&server)
                    .ok()
                    .flatten()
                    .and_then(|credentials| credentials.trust_fingerprint)
            });
            let trust = account::trust_for(fingerprint.as_deref())?;
            (server, dev.to_owned(), trust)
        }
        None => {
            let session = net.session()?;
            (
                session.credentials.server.clone(),
                session.credentials.token.clone(),
                session.trust,
            )
        }
    };
    let mut config = RemoteConfig::new(server, token, trust);
    config.force_relay = net.force_relay;
    config.port_mapping = net.port_mapping;
    config.bind = net
        .local_port
        .filter(|port| *port != 0)
        .map(|port| std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port)));
    #[cfg(debug_assertions)]
    {
        config.insecure_no_sframe = net.insecure_no_sframe;
    }
    Ok(config)
}

/// Imprime los eventos de la sesión en un hilo aparte (en la salida de error si `stderr`).
pub fn print_events(session: &RemoteSession, stderr: bool) -> thread::JoinHandle<()> {
    print_events_forwarding(session, stderr, None)
}

/// Como [`print_events`], y además pasa por `joined` el id de cada invitado que entra (para
/// `--auto-allow`).
pub fn print_events_forwarding(
    session: &RemoteSession,
    stderr: bool,
    joined: Option<Sender<GuestId>>,
) -> thread::JoinHandle<()> {
    let events = session.events();
    thread::spawn(move || {
        while let Ok(event) = events.recv() {
            let text = describe_event(&event);
            if stderr {
                eprintln!("{text}");
            } else {
                println!("{text}");
            }
            if let (SessionEvent::GuestJoined { guest, .. }, Some(joined)) = (&event, &joined) {
                let _ = joined.send(*guest);
            }
            if matches!(event, SessionEvent::Ended(_)) {
                break;
            }
        }
    })
}

/// Un mensaje del chat como lo imprime la consola: `[21:05] Bruno: texto`, con la hora local.
pub fn chat_line(line: &ChatLine) -> String {
    format!("[{}] {line}", crate::localtime::clock_hhmm(line.at_unix_ms))
}

/// Un evento como lo imprime la consola: el chat con su hora local ([`chat_line`]); el resto,
/// con su `Display`.
pub fn describe_event(event: &SessionEvent) -> String {
    match event {
        SessionEvent::ChatMessage(line) => chat_line(line),
        SessionEvent::ChatHistory { messages } if !messages.is_empty() => {
            let mut text = format!("Historial del chat ({} mensajes):", messages.len());
            for line in messages {
                text.push_str("\n  ");
                text.push_str(&chat_line(line));
            }
            text
        }
        other => other.to_string(),
    }
}

/// Permisos que `--auto-allow` concede a cada invitado al entrar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AutoAllow {
    /// Teclado y ratón.
    pub keyboard_mouse: bool,
    /// La primera ranura de mando libre.
    pub gamepad: bool,
}

impl AutoAllow {
    /// Lee `kbm`, `pad` o `kbm,pad`.
    pub fn parse(text: &str) -> Result<AutoAllow, String> {
        let mut out = AutoAllow::default();
        for part in text
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            match part {
                "kbm" => out.keyboard_mouse = true,
                "pad" => out.gamepad = true,
                other => {
                    return Err(format!(
                        "permiso desconocido en --auto-allow: {other} (usa kbm, pad o kbm,pad)"
                    ));
                }
            }
        }
        Ok(out)
    }

    /// Si no concede nada.
    pub fn is_empty(&self) -> bool {
        !self.keyboard_mouse && !self.gamepad
    }
}

/// Qué conceder con `allow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    /// Teclado y ratón.
    KeyboardMouse,
    /// Mando en esa ranura.
    Pad(u8),
    /// El mando tal cual (si no, convertido a Xbox 360).
    Raw,
    /// Voz.
    Voice,
}

/// Qué quitar con `deny`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revoke {
    /// Teclado y ratón.
    KeyboardMouse,
    /// El mando.
    Pad,
    /// El mando tal cual: pasa a convertido.
    Raw,
    /// Voz.
    Voice,
}

/// Orden de la consola de `coroom host`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleCommand {
    /// Invitar a un amigo conectado (por su nombre de usuario).
    Invite(String),
    /// Ayuda.
    Help,
    /// Invitados y sus permisos.
    List,
    /// Conceder.
    Allow(u32, Grant),
    /// Quitar.
    Deny(u32, Revoke),
    /// Pausar la entrada de todos.
    Pause,
    /// Reanudarla.
    Resume,
    /// Expulsar.
    Kick(u32),
    /// Escribir en el chat de la sala.
    Say(String),
    /// Vaciar el chat para todos.
    ClearChat,
    /// Cerrar la sala.
    Quit,
}

/// Ayuda de la consola de `coroom host`.
pub const CONSOLE_HELP: &str = "Órdenes (escribe una y pulsa Intro):
  list                     invitados, permisos y latencia de su entrada
  allow N kbm              teclado y ratón para el invitado N
  allow N pad RANURA       mando en esa ranura (de 1 a COROOM_MAX_GAMEPADS, 4 por defecto)
  allow N raw              su mando tal cual, con su VID/PID y su descriptor (por defecto)
  allow N voice            voz: habla a la sala (la oye siempre; la trae al entrar salvo --guests-muted)
  deny N kbm|pad|voice     quitar ese permiso
  deny N raw               su mando, convertido a Xbox 360
  pause / resume           pausar o reanudar la entrada de todos (en pausa se suelta todo)
  kick N                   expulsar al invitado N (rota las claves de la sala)
  invite NOMBRE            invitar a un amigo conectado a la sala
  say TEXTO                escribir en el chat de la sala
  clear-chat               vaciar el chat para todos
  quit                     cerrar la sala y salir
  help                     esta ayuda";

fn guest_number(text: Option<&str>) -> Result<u32, String> {
    let text = text.ok_or("falta el número del invitado (míralo con `list`)")?;
    text.parse::<u32>()
        .map_err(|_| format!("«{text}» no es un número de invitado"))
}

/// El texto de `say TEXTO` (todo lo que sigue a la orden, tal cual), si la línea es esa orden.
fn say_text(line: &str) -> Result<Option<String>, String> {
    let line = line.trim_start();
    let (command, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    if !matches!(command.to_lowercase().as_str(), "say" | "di") {
        return Ok(None);
    }
    let text = rest.trim();
    if text.is_empty() {
        return Err("falta el texto: say TEXTO".to_owned());
    }
    Ok(Some(text.to_owned()))
}

/// Lee una orden de la consola; `Ok(None)` si la línea está vacía.
pub fn parse_console(line: &str) -> Result<Option<ConsoleCommand>, String> {
    if let Some(text) = say_text(line)? {
        return Ok(Some(ConsoleCommand::Say(text)));
    }
    let mut words = line.split_whitespace();
    let Some(command) = words.next() else {
        return Ok(None);
    };
    let command = match command.to_ascii_lowercase().as_str() {
        "help" | "ayuda" | "?" => ConsoleCommand::Help,
        "list" | "ls" => ConsoleCommand::List,
        "pause" => ConsoleCommand::Pause,
        "resume" => ConsoleCommand::Resume,
        "quit" | "exit" => ConsoleCommand::Quit,
        "kick" => ConsoleCommand::Kick(guest_number(words.next())?),
        "clear-chat" => ConsoleCommand::ClearChat,
        "invite" | "invitar" => ConsoleCommand::Invite(
            words
                .next()
                .ok_or("falta el nombre del amigo: invite NOMBRE")?
                .to_owned(),
        ),
        "allow" => {
            let guest = guest_number(words.next())?;
            let grant = match words.next() {
                Some("kbm") => Grant::KeyboardMouse,
                Some("voice") => Grant::Voice,
                Some("raw") => Grant::Raw,
                Some("pad") => {
                    let slot = words
                        .next()
                        .ok_or("falta la ranura del mando: allow N pad RANURA")?;
                    Grant::Pad(
                        slot.parse::<u8>()
                            .map_err(|_| format!("«{slot}» no es una ranura de mando"))?,
                    )
                }
                Some(other) => {
                    return Err(format!("no sé conceder «{other}» (kbm, pad, raw o voice)"));
                }
                None => return Err("falta qué conceder: kbm, pad RANURA, raw o voice".to_owned()),
            };
            ConsoleCommand::Allow(guest, grant)
        }
        "deny" => {
            let guest = guest_number(words.next())?;
            let revoke = match words.next() {
                Some("kbm") => Revoke::KeyboardMouse,
                Some("pad") => Revoke::Pad,
                Some("voice") => Revoke::Voice,
                Some("raw") => Revoke::Raw,
                Some(other) => {
                    return Err(format!("no sé quitar «{other}» (kbm, pad, raw o voice)"));
                }
                None => return Err("falta qué quitar: kbm, pad, raw o voice".to_owned()),
            };
            ConsoleCommand::Deny(guest, revoke)
        }
        other => {
            return Err(format!(
                "orden desconocida: «{other}». Escribe `help` para ver las órdenes."
            ));
        }
    };
    if let Some(extra) = words.next() {
        return Err(format!("sobra «{extra}» en la orden"));
    }
    Ok(Some(command))
}

/// Una línea de `list` por invitado.
pub fn describe_guest(status: &GuestStatus) -> String {
    let path = status
        .path
        .map_or_else(|| "sin conectar".to_owned(), |path| path.to_string());
    let rtt = status
        .rtt_ms
        .map_or_else(String::new, |rtt| format!(" · RTT {rtt:.1} ms"));
    let input = &status.input;
    let latency = if input.samples > 0 {
        format!(
            " · entrada p50 {:.1} ms, p95 {:.1} ms",
            input.p50_us as f64 / 1000.0,
            input.p95_us as f64 / 1000.0
        )
    } else {
        String::new()
    };
    let pad = status.gamepad.as_ref().map_or_else(String::new, |pad| {
        let mode = match (&pad.note, pad.raw) {
            (_, true) => "tal cual".to_owned(),
            (Some(note), false) => format!("convertido: {note}"),
            (None, false) => "convertido".to_owned(),
        };
        format!(
            " · mando {} ({:04x}:{:04x}, {mode})",
            pad.name, pad.vendor, pad.product
        )
    });
    format!(
        "Invitado {} ({}) · {path}{rtt} · {}{pad} · {} eventos aplicados, {} descartados{latency}{}",
        status.guest.0,
        status.name,
        permissions_label(&status.permissions),
        input.applied,
        input.dropped,
        if status.input_key {
            ""
        } else {
            " · SIN clave de entrada"
        }
    )
}

/// Primera ranura de mando libre.
fn free_slot(guests: &[GuestStatus]) -> Option<u8> {
    (1..=coroom_session::input::max_gamepads()).find(|slot| {
        guests
            .iter()
            .all(|g| g.permissions.gamepad_slot != Some(*slot))
    })
}

fn permissions_of(session: &RemoteSession, guest: u32) -> Result<Permissions, String> {
    session
        .guests()
        .iter()
        .find(|status| status.guest.0 == guest)
        .map(|status| status.permissions)
        .ok_or_else(|| format!("no hay ningún invitado {guest} en la sala (mira `list`)"))
}

/// Resuelve el nombre de un amigo a su usuario del servidor (para `invite`).
pub type FriendResolver<'a> = &'a dyn Fn(&str) -> Result<u64, String>;

/// Invita a `name` a la sala de `session`.
pub fn invite_friend(
    session: &RemoteSession,
    resolve: FriendResolver<'_>,
    name: &str,
) -> Result<String, String> {
    let user = resolve(name)?;
    session.invite(user).map_err(|err| err.to_string())?;
    Ok(format!(
        "Invitación enviada a {name} (si no está conectado, verás un aviso)."
    ))
}

/// Aplica una orden de la consola; devuelve lo que hay que imprimir.
pub fn apply_console(
    session: &mut RemoteSession,
    command: ConsoleCommand,
    resolve: FriendResolver<'_>,
) -> Result<String, String> {
    let set = |session: &mut RemoteSession, guest: u32, permissions: Permissions| {
        session
            .set_permissions(GuestId(guest), permissions)
            .map(|()| format!("Invitado {guest}: {}.", permissions_label(&permissions)))
            .map_err(|err| err.to_string())
    };
    match command {
        ConsoleCommand::Help => Ok(CONSOLE_HELP.to_owned()),
        ConsoleCommand::List => {
            let guests = session.guests();
            if guests.is_empty() {
                Ok(format!(
                    "No hay invitados. Código de la sala: {}",
                    session.room_code().unwrap_or_else(|| "(creándose)".into())
                ))
            } else {
                Ok(guests.iter().map(describe_guest).collect::<Vec<_>>().join(
                    "
",
                ))
            }
        }
        ConsoleCommand::Allow(guest, grant) => {
            let mut permissions = permissions_of(session, guest)?;
            match grant {
                Grant::KeyboardMouse => permissions.keyboard_mouse = true,
                Grant::Pad(slot) => permissions.gamepad_slot = Some(slot),
                Grant::Raw => permissions.gamepad_raw = true,
                Grant::Voice => permissions.voice = true,
            }
            set(session, guest, permissions)
        }
        ConsoleCommand::Deny(guest, revoke) => {
            let mut permissions = permissions_of(session, guest)?;
            match revoke {
                Revoke::KeyboardMouse => permissions.keyboard_mouse = false,
                Revoke::Pad => permissions.gamepad_slot = None,
                Revoke::Raw => permissions.gamepad_raw = false,
                Revoke::Voice => permissions.voice = false,
            }
            set(session, guest, permissions)
        }
        ConsoleCommand::Pause => session
            .set_paused(true)
            .map(|()| "Entrada en pausa: se ha soltado todo lo pulsado.".to_owned())
            .map_err(|err| format!("no se pudo pausar: {err}")),
        ConsoleCommand::Resume => session
            .set_paused(false)
            .map(|()| "Entrada reanudada.".to_owned())
            .map_err(|err| format!("no se pudo reanudar: {err}")),
        ConsoleCommand::Kick(guest) => session
            .kick(GuestId(guest))
            .map(|()| format!("Expulsando al invitado {guest}…"))
            .map_err(|err| err.to_string()),
        ConsoleCommand::Invite(name) => invite_friend(session, resolve, &name),
        // El mensaje se imprime cuando la sesión lo reparte (`ChatMessage`).
        ConsoleCommand::Say(text) => session
            .send_chat(&text)
            .map(|()| String::new())
            .map_err(|err| err.to_string()),
        ConsoleCommand::ClearChat => session
            .clear_chat()
            .map(|()| "Vaciando el chat para todos…".to_owned())
            .map_err(|err| err.to_string()),
        ConsoleCommand::Quit => Ok("Cerrando la sala.".to_owned()),
    }
}

/// Orden de la consola de `coroom join --headless`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinCommand {
    /// Escribir en el chat de la sala.
    Say(String),
    /// Ayuda.
    Help,
    /// Salir de la sala.
    Quit,
}

/// Ayuda de la consola de `coroom join --headless`.
pub const JOIN_CONSOLE_HELP: &str = "Órdenes (escribe una y pulsa Intro):
  say TEXTO                escribir en el chat de la sala
  quit                     salir de la sala (también Intro en una línea vacía)
  help                     esta ayuda";

/// Lee una orden de la consola del invitado; una línea vacía es salir.
pub fn parse_join_console(line: &str) -> Result<JoinCommand, String> {
    if let Some(text) = say_text(line)? {
        return Ok(JoinCommand::Say(text));
    }
    let mut words = line.split_whitespace();
    let command = match words.next().map(str::to_ascii_lowercase).as_deref() {
        None | Some("quit" | "exit") => JoinCommand::Quit,
        Some("help" | "ayuda" | "?") => JoinCommand::Help,
        Some(other) => {
            return Err(format!(
                "orden desconocida: «{other}». Escribe `help` para ver las órdenes."
            ));
        }
    };
    if let Some(extra) = words.next() {
        return Err(format!("sobra «{extra}» en la orden"));
    }
    Ok(command)
}

/// Consola de `coroom join --headless`: `say` escribe en el chat, hasta `quit` (o una línea
/// vacía), el final de la entrada o el de la sesión.
pub fn run_join_console(session: &RemoteSession, lines: &Receiver<String>) {
    eprintln!("{JOIN_CONSOLE_HELP}");
    loop {
        if session.wait(Duration::from_millis(50)) {
            return;
        }
        loop {
            match lines.try_recv() {
                Ok(line) => match parse_join_console(&line) {
                    Ok(JoinCommand::Quit) => return,
                    Ok(JoinCommand::Help) => eprintln!("{JOIN_CONSOLE_HELP}"),
                    Ok(JoinCommand::Say(text)) => {
                        if let Err(err) = session.send_chat(&text) {
                            eprintln!("error: {err}");
                        }
                    }
                    Err(err) => eprintln!("error: {err}"),
                },
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                // Sin consola (en segundo plano): se sigue hasta que la sesión termine.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    while !session.wait(Duration::from_millis(250)) {}
                    return;
                }
            }
        }
    }
}

/// `--auto-allow`: concede a `guest` lo pedido (la primera ranura libre para `pad`).
pub fn auto_allow_guest(
    session: &mut RemoteSession,
    guest: GuestId,
    auto: AutoAllow,
) -> Result<String, String> {
    let guests = session.guests();
    let mut permissions = guests
        .iter()
        .find(|status| status.guest == guest)
        .map(|status| status.permissions)
        .ok_or_else(|| format!("el invitado {} ya no está", guest.0))?;
    permissions.keyboard_mouse |= auto.keyboard_mouse;
    if auto.gamepad && permissions.gamepad_slot.is_none() {
        permissions.gamepad_slot = free_slot(&guests);
        if permissions.gamepad_slot.is_none() {
            eprintln!(
                "Aviso: no quedan ranuras de mando libres para el invitado {}.",
                guest.0
            );
        }
    }
    session
        .set_permissions(guest, permissions)
        .map(|()| {
            format!(
                "Invitado {} con permiso automático: {}.",
                guest.0,
                permissions_label(&permissions)
            )
        })
        .map_err(|err| err.to_string())
}

/// Lee líneas de la entrada estándar en un hilo bloqueante (termina cuando se cierra).
pub fn stdin_lines() -> Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut line = String::new();
        loop {
            line.clear();
            match std::io::BufRead::read_line(&mut stdin.lock(), &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(line.trim().to_owned()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

/// Consola de `coroom host`: atiende órdenes y `--auto-allow` hasta `quit`, hasta que la sesión
/// termine o, si `frames` (`--frames`), hasta que se envíen.
pub fn run_host_console(
    session: &mut RemoteSession,
    lines: &Receiver<String>,
    joined: &Receiver<GuestId>,
    auto: AutoAllow,
    invites: &[String],
    resolve: FriendResolver<'_>,
) {
    let mut pending: Vec<String> = invites.to_vec();
    loop {
        if session.wait(Duration::from_millis(50)) {
            return;
        }
        if !pending.is_empty() && session.room_code().is_some() {
            for name in pending.drain(..) {
                match invite_friend(session, resolve, &name) {
                    Ok(message) => eprintln!("{message}"),
                    Err(err) => eprintln!("Aviso: no se pudo invitar a {name}: {err}"),
                }
            }
        }
        for guest in joined.try_iter() {
            if auto.is_empty() {
                continue;
            }
            match auto_allow_guest(session, guest, auto) {
                Ok(message) => eprintln!("{message}"),
                Err(err) => eprintln!("Aviso: {err}"),
            }
        }
        loop {
            match lines.try_recv() {
                Ok(line) => match parse_console(&line) {
                    Ok(None) => {}
                    Ok(Some(ConsoleCommand::Quit)) => {
                        eprintln!("Cerrando la sala.");
                        return;
                    }
                    Ok(Some(command)) => match apply_console(session, command, resolve) {
                        Ok(message) if message.is_empty() => {}
                        Ok(message) => eprintln!("{message}"),
                        Err(err) => eprintln!("error: {err}"),
                    },
                    Err(err) => eprintln!("error: {err}"),
                },
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                // Sin consola (en segundo plano): se sigue hasta que la sesión termine.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
    }
}

/// Frecuencia del tono sintético que hace de audio del juego sin ventana.
pub const SYNTHETIC_GAME_HZ: f64 = 440.0;

/// Frecuencia del tono sintético que hace de micrófono del anfitrión sin ventana.
pub const SYNTHETIC_HOST_VOICE_HZ: f64 = 550.0;

/// Frecuencia del tono sintético que hace de micrófono del invitado sin ventana.
pub const SYNTHETIC_GUEST_VOICE_HZ: f64 = 660.0;

/// Audio de `coroom host`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostAudioOptions {
    /// Qué audio del juego se captura (`--audio`).
    pub game: GameCapture,
    /// Voz del anfitrión (`--voice`).
    pub voice: bool,
    /// Micrófono por nombre (`--mic`).
    pub mic: Option<String>,
    /// Salida de la voz de los invitados por nombre (`--speaker`).
    pub speaker: Option<String>,
    /// Duración de cada paquete de audio (10 o 20 ms, `--audio-frame-ms`).
    pub frame_ms: u8,
    /// Supresión de ruido en el micrófono (Ajustes → Audio).
    pub noise_suppression: bool,
}

impl Default for HostAudioOptions {
    fn default() -> Self {
        Self {
            game: GameCapture::System,
            voice: false,
            mic: None,
            speaker: None,
            frame_ms: 10,
            noise_suppression: true,
        }
    }
}

/// Lee `--audio none|system|app NOMBRE|process PID`.
pub fn parse_game_capture(values: &[String]) -> Result<GameCapture, String> {
    let mode = values.first().map(|value| value.to_ascii_lowercase());
    let arg = values.get(1);
    match (mode.as_deref(), arg) {
        (Some("none" | "ninguno"), None) => Ok(GameCapture::None),
        (Some("system" | "sistema"), None) => Ok(GameCapture::System),
        (Some("app"), Some(name)) => Ok(GameCapture::App(name.clone())),
        (Some("app"), None) => Err("falta el nombre de la aplicación: --audio app NOMBRE".into()),
        (Some("process"), Some(pid)) => pid
            .parse::<u32>()
            .map(GameCapture::Process)
            .map_err(|_| format!("«{pid}» no es un PID: --audio process PID")),
        (Some("process"), None) => Err("falta el PID: --audio process PID".into()),
        (Some(mode @ ("apps-except" | "apps-only")), list) => {
            let keys: Vec<String> = list
                .map(|list| {
                    list.split(',')
                        .map(str::trim)
                        .filter(|key| !key.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            Ok(GameCapture::Apps(if mode == "apps-except" {
                AppFilter::AllExcept(keys)
            } else {
                AppFilter::Only(keys)
            }))
        }
        (Some(other), _) => Err(format!(
            "--audio no entiende «{other}» (usa none, system, apps-except A,B, apps-only A,B, app \
             NOMBRE o process PID)"
        )),
        (None, _) => Err("--audio necesita un modo".into()),
    }
}

/// Audio de la plataforma o sintético (sin ventana) para el anfitrión.
pub fn host_audio(options: &HostAudioOptions, headless: bool) -> HostAudio {
    let frame_ms = options.frame_ms;
    if headless {
        let game = (options.game != GameCapture::None)
            .then(|| Box::new(ToneSource::new(SYNTHETIC_GAME_HZ, 2)) as Box<dyn AudioSource>);
        return HostAudio {
            game,
            game_bitrate: 0,
            frame_ms,
            voice: options
                .voice
                .then(|| synthetic_voice(SYNTHETIC_HOST_VOICE_HZ)),
            speaker: Some(Box::new(CaptureSink::new(1)) as Box<dyn AudioSink>),
        };
    }
    let game = match coroom_session::audio::game_source(&options.game) {
        Ok(game) => {
            match &game {
                Some(source) => tracing::info!(
                    capture = ?options.game,
                    format = ?source.format(),
                    "audio del juego"
                ),
                None => tracing::info!(capture = ?options.game, "sin audio del juego"),
            }
            game
        }
        Err(err) => {
            tracing::warn!(error = %err, "sin audio del juego");
            eprintln!("Aviso: sin audio del juego: {err}");
            None
        }
    };
    let voice = if options.voice {
        match coroom_session::audio::mic_source(options.mic.as_deref()) {
            Ok(mic) => {
                tracing::info!(mic = ?options.mic, format = ?mic.format(), "micrófono");
                let mut voice = VoiceInput::new(mic);
                voice.processing.noise_suppression = options.noise_suppression;
                Some(voice)
            }
            Err(err) => {
                tracing::warn!(error = %err, "sin micrófono");
                eprintln!("Aviso: sin micrófono: {err}");
                None
            }
        }
    } else {
        None
    };
    let speaker = match coroom_session::audio::playback_sink(
        options.speaker.as_deref(),
        AudioFormat::VOICE,
    ) {
        Ok(sink) => {
            tracing::info!(speaker = ?options.speaker, format = ?sink.format(), "salida de audio");
            Some(sink)
        }
        Err(err) => {
            tracing::warn!(error = %err, "sin salida para la voz de los invitados");
            eprintln!("Aviso: no se oirá la voz de los invitados: {err}");
            None
        }
    };
    HostAudio {
        game,
        game_bitrate: 0,
        frame_ms,
        voice,
        speaker,
    }
}

/// Micrófono sintético: un tono sin procesado (la supresión de ruido atenúa los tonos fijos).
fn synthetic_voice(freq: f64) -> VoiceInput {
    VoiceInput {
        processing: ProcessingOptions::NONE,
        ..VoiceInput::new(Box::new(ToneSource::new(freq, 1)))
    }
}

/// Opciones de `coroom host`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostOptions {
    /// Red.
    pub net: NetOptions,
    /// Fuente de vídeo.
    pub source: Source,
    /// Con [`Source::Screen`]: pantalla o aplicación, y el destino si la interfaz lo eligió.
    pub share: coroom_session::capture::ShareChoice,
    /// Vídeo (`frames`: termina tras enviar tantos fotogramas).
    pub video: LoopbackConfig,
    /// Códec forzado (`None`: el que se negocie).
    pub codec: Option<Codec>,
    /// Backend del codificador forzado (`None`: el mejor que funcione).
    pub encoder: Option<Backend>,
    /// Permisos para cada invitado que entra.
    pub auto_allow: AutoAllow,
    /// Los invitados entran callados: sin permiso de voz hasta que se les dé.
    pub guests_muted: bool,
    /// Windows: mandos como teclas en vez del driver de mando virtual.
    pub gamepad_to_keys: bool,
    /// Audio del juego y voz.
    pub audio: HostAudioOptions,
    /// Amigos a los que invitar en cuanto exista la sala (`--invite`).
    pub invites: Vec<String>,
}

/// Codificadores del anfitrión según las opciones (los que pasaron el sondeo).
pub fn host_encoders(codec: Option<Codec>, encoder: Option<Backend>) -> Result<EncoderSet, String> {
    let set = EncoderSet::probe().only_codec(codec).only_backend(encoder);
    if set.is_empty() {
        return Err(format!(
            "este equipo no tiene ese codificador ({} con {}); mira `coroom doctor`",
            codec.map_or_else(|| "cualquier códec".to_owned(), |c| c.to_string()),
            encoder.map_or_else(|| "cualquier backend".to_owned(), |b| b.to_string())
        ));
    }
    let available: Vec<String> = set
        .available()
        .iter()
        .map(|(codec, backend)| format!("{codec}/{backend}"))
        .collect();
    tracing::info!(?codec, ?encoder, codificadores = ?available, "codificadores del anfitrión");
    Ok(set)
}

/// Prepara y arranca la sesión del anfitrión. Con la pantalla, la captura y la inyección de
/// teclado y ratón comparten una sola sesión del portal (un diálogo y un token en Linux).
pub fn start_host(identity: Identity, options: &HostOptions) -> Result<RemoteSession, String> {
    let mut session = prepare_host(identity, options)?;
    start_prepared_host(&mut session)?;
    Ok(session)
}

/// Arranca la sala preparada con [`prepare_host`] (conecta con el servidor y la crea).
pub fn start_prepared_host(session: &mut RemoteSession) -> Result<(), String> {
    session
        .start()
        .map_err(|err| format!("no se pudo arrancar la sala: {err}"))
}

/// Prepara la sesión del anfitrión sin arrancarla: codificadores, captura (en Linux, el diálogo
/// del portal si no hay token), entrada y audio.
pub fn prepare_host(identity: Identity, options: &HostOptions) -> Result<RemoteSession, String> {
    let mut config = remote_config(&options.net)?;
    config.guests_muted = options.guests_muted;
    let encoders = host_encoders(options.codec, options.encoder)?;
    let (capturer, share) = share_source(options.source, &options.share, options.video)?;
    let sink = platform_sink(SinkOptions {
        gamepad_to_keys: options.gamepad_to_keys,
        eis: share.map(eis_provider),
    });
    let audio = host_audio(&options.audio, options.net.headless);
    Ok(RemoteSession::host_with_sink(
        identity,
        config,
        capturer,
        encoders,
        options.video,
        Arc::new(LatencyLedger::new()),
        sink,
    )
    .with_host_audio(audio))
}

/// Captura de lo elegido (al abrir la sala o al cambiar de fuente con ella abierta) y, en Linux, la
/// conexión EIS de su sesión del portal para el teclado y el ratón.
pub fn share_source(
    source: Source,
    share: &coroom_session::capture::ShareChoice,
    video: LoopbackConfig,
) -> Result<
    (
        Box<dyn coroom_session::capture::Capturer>,
        Option<coroom_session::capture::RemoteDesktopShare>,
    ),
    String,
> {
    match source {
        Source::Screen => coroom_session::capture::share_capturer(share)
            .map_err(|err| format!("no se puede capturar la pantalla: {err}")),
        Source::Synthetic => Ok((make_capturer(&ProbeOptions::h264(source, video))?, None)),
    }
}

/// Audio de `coroom join`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinAudioOptions {
    /// Sin audio (ni juego ni voces).
    pub disabled: bool,
    /// Empieza con el juego silenciado (`--mute-game`).
    pub mute_game: bool,
    /// Voz propia (`--voice`).
    pub voice: bool,
    /// Tecla de pulsar para hablar (`--ptt TECLA`; sin ella, activación por voz).
    pub ptt: Option<String>,
    /// Micrófono por nombre (`--mic`).
    pub mic: Option<String>,
    /// Salida por nombre (`--speaker`).
    pub speaker: Option<String>,
    /// Supresión de ruido en el micrófono (Ajustes → Audio).
    pub noise_suppression: bool,
}

impl Default for JoinAudioOptions {
    fn default() -> Self {
        Self {
            disabled: false,
            mute_game: false,
            voice: false,
            ptt: None,
            mic: None,
            speaker: None,
            noise_suppression: true,
        }
    }
}

/// Audio de la plataforma o sintético (sin ventana) para el invitado.
pub fn guest_audio(options: &JoinAudioOptions, headless: bool) -> Option<GuestAudio> {
    if options.disabled {
        return None;
    }
    let talk = if options.ptt.is_some() {
        TalkMode::PushToTalk
    } else {
        TalkMode::VoiceActivity
    };
    if headless {
        return Some(GuestAudio {
            speaker: Some(Box::new(CaptureSink::new(2))),
            voice: options.voice.then(|| VoiceInput {
                talk,
                ..synthetic_voice(SYNTHETIC_GUEST_VOICE_HZ)
            }),
            mute_game: options.mute_game,
            frame_ms: 10,
        });
    }
    let speaker = match coroom_session::audio::playback_sink(
        options.speaker.as_deref(),
        AudioFormat::GAME,
    ) {
        Ok(sink) => {
            tracing::info!(speaker = ?options.speaker, format = ?sink.format(), "salida de audio");
            Some(sink)
        }
        Err(err) => {
            tracing::warn!(error = %err, "sin salida de audio");
            eprintln!("Aviso: sin salida de audio: {err}");
            None
        }
    };
    let voice = if options.voice {
        match coroom_session::audio::mic_source(options.mic.as_deref()) {
            Ok(mic) => {
                tracing::info!(mic = ?options.mic, format = ?mic.format(), "micrófono");
                let mut voice = VoiceInput {
                    talk,
                    ..VoiceInput::new(mic)
                };
                voice.processing.noise_suppression = options.noise_suppression;
                Some(voice)
            }
            Err(err) => {
                tracing::warn!(error = %err, "sin micrófono");
                eprintln!("Aviso: sin micrófono: {err}");
                None
            }
        }
    } else {
        None
    };
    Some(GuestAudio {
        speaker,
        voice,
        mute_game: options.mute_game,
        frame_ms: 10,
    })
}

/// Opciones de `coroom join`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinOptions {
    /// Red.
    pub net: NetOptions,
    /// Código de la sala (vacío: esperar una invitación).
    pub code: String,
    /// Sin código: entrar con la primera invitación que llegue, sin preguntar.
    pub first_invitation: bool,
    /// Fotogramas que presentar; `None` = hasta cerrar.
    pub frames: Option<u64>,
    /// Backend del decodificador forzado (`None`: el mejor de cada códec).
    pub decoder: Option<DecoderBackend>,
    /// Guion de entrada (`--input-script`, solo sin ventana).
    pub input_script: Option<PathBuf>,
    /// Audio del juego y voz.
    pub audio: JoinAudioOptions,
    /// Aceptar 4:4:4 si el anfitrión lo envía y este equipo lo decodifica.
    pub yuv444: bool,
    /// Aceptar HDR10 si el anfitrión lo envía y este equipo lo decodifica.
    pub hdr: bool,
}

/// `coroom join` sin código: abre la señalización con la cuenta, lista las invitaciones que
/// lleguen de los amigos y devuelve el código elegido (con `first`, el de la primera).
pub fn wait_for_invitation(
    identity: &Identity,
    net: &NetOptions,
    first: bool,
) -> Result<String, String> {
    let session = net.session()?;
    let names: std::collections::HashMap<u64, String> = session
        .api()
        .ok()
        .and_then(|api| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            runtime.block_on(api.friends()).ok()
        })
        .map(|list| {
            list.friends
                .into_iter()
                .map(|friend| (friend.id, friend.display_name))
                .collect()
        })
        .unwrap_or_default();
    let lines = if first { None } else { Some(stdin_lines()) };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .map_err(|err| format!("no se pudo arrancar la red: {err}"))?;
    runtime.block_on(async {
        let endpoint = Endpoint::bind(identity, None, EndpointOptions::default())
            .map_err(|err| err.to_string())?;
        let mut signal = SignalClient::connect(
            &session.credentials.server,
            &session.credentials.token,
            &endpoint,
            &session.trust,
        )
        .await
        .map_err(|err| err.to_string())?;
        eprintln!(
            "Esperando invitaciones de tus amigos como {} (Ctrl+C para salir)…",
            session.credentials.username
        );
        let mut invitations: Vec<String> = Vec::new();
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let chosen = loop {
            tokio::select! {
                event = signal.next_event() => match event {
                    Ok(Some(SignalMessage::Invited { from_name, code, .. })) => {
                        invitations.push(code.clone());
                        if first {
                            eprintln!("{from_name} te invita a la sala {code}: entrando.");
                            break Ok(code);
                        }
                        eprintln!(
                            "[{}] {from_name} te invita a la sala {code}. Escribe {} y pulsa \
                             Intro para entrar (Intro sola: la última).",
                            invitations.len(),
                            invitations.len()
                        );
                    }
                    Ok(Some(SignalMessage::Presence { user, online })) => {
                        let name = names.get(&user).cloned().unwrap_or_else(|| format!("usuario {user}"));
                        eprintln!("{name} {}.", if online { "está en línea" } else { "ya no está en línea" });
                    }
                    Ok(Some(SignalMessage::Error { message, .. })) => {
                        eprintln!("Aviso: el servidor dice: {message}");
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => break Err("el servidor cerró la señalización".to_owned()),
                    Err(err) => break Err(err.to_string()),
                },
                _ = tick.tick() => {
                    let Some(lines) = &lines else { continue };
                    while let Ok(line) = lines.try_recv() {
                        if invitations.is_empty() {
                            eprintln!("Todavía no hay invitaciones.");
                            continue;
                        }
                        let pick = if line.trim().is_empty() {
                            Some(invitations.len())
                        } else {
                            line.trim().parse::<usize>().ok()
                        };
                        match pick.and_then(|n| invitations.get(n.wrapping_sub(1))) {
                            Some(code) => {
                                let code = code.clone();
                                signal.close();
                                endpoint.close();
                                return Ok(code);
                            }
                            None => eprintln!("Escribe el número de una invitación de la lista."),
                        }
                    }
                }
            }
        };
        signal.close();
        endpoint.close();
        chosen
    })
}

/// Prepara y arranca la sesión del invitado presentando en `sink`, con la imagen decodificada
/// donde diga `output`.
pub fn start_guest(
    identity: Identity,
    options: &JoinOptions,
    sink: Box<dyn FrameSink>,
    output: DecodeOutput,
) -> Result<RemoteSession, String> {
    let mut config = remote_config(&options.net)?;
    config.accept_yuv444 = options.yuv444;
    config.accept_hdr = options.hdr;
    let decoders = DecoderSet::probe()
        .only_backend(options.decoder)
        .with_output(output);
    if decoders.is_empty() {
        tracing::warn!(decoder = ?options.decoder, "sin decodificador");
        return Err("este equipo no tiene ese decodificador; mira `coroom doctor`".to_owned());
    }
    let available: Vec<String> = decoders
        .available()
        .iter()
        .map(|(codec, backend)| format!("{codec}/{backend}"))
        .collect();
    tracing::info!(
        decoder = ?options.decoder,
        decodificadores = ?available,
        "decodificadores del invitado"
    );
    let mut session = RemoteSession::guest(
        identity,
        config,
        &options.code,
        decoders,
        sink,
        options.frames,
        Arc::new(LatencyLedger::new()),
    );
    if let Some(audio) = guest_audio(&options.audio, options.net.headless) {
        session = session.with_guest_audio(audio);
    }
    session
        .start()
        .map_err(|err| format!("no se pudo entrar en la sala: {err}"))?;
    Ok(session)
}

/// Cierra la sesión y devuelve su resumen, o por qué terminó si fue con error.
pub fn finish_session(mut session: RemoteSession) -> Result<LatencySummary, String> {
    let reason = session.end_reason();
    session.close();
    match reason {
        Some(err) => Err(err.to_string()),
        None => Ok(session.summary()),
    }
}

/// Espera máxima a que el canal de entrada esté abierto antes de reproducir un guion.
pub const SCRIPT_READY_TIMEOUT: Duration = Duration::from_secs(20);

/// Reproduce `script` en la entrada del invitado en cuanto el canal está abierto.
pub fn play_script(input: &GuestInput, script: &InputScript) -> Result<ScriptReport, String> {
    let deadline = Instant::now() + SCRIPT_READY_TIMEOUT;
    while !input.ready() {
        if Instant::now() >= deadline {
            return Err(format!(
                "la entrada no estuvo lista en {} s (¿hay clave pareja a pareja?)",
                SCRIPT_READY_TIMEOUT.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
    script.play(input).map_err(|err| err.to_string())
}

/// `coroom join --headless`: presentación nula hasta presentar `frames` (o hasta que termine la
/// sesión o el guion de entrada) y resumen final.
pub fn run_join_headless(
    identity: Identity,
    options: &JoinOptions,
    json: bool,
    until_stopped: impl FnOnce(&RemoteSession),
) -> Result<LatencySummary, String> {
    let script = options
        .input_script
        .as_deref()
        .map(InputScript::from_file)
        .transpose()
        .map_err(|err| err.to_string())?;
    let sink = Box::new(PresenterSink::new(NullPresenter::new()));
    // Sin ventana, la imagen de los decodificadores de GPU se queda en la GPU.
    let session = start_guest(identity, options, sink, DecodeOutput::Gpu { vulkan: None })?;
    let printer = print_events(&session, json);
    let player = script.map(|script| {
        let input = session.input();
        thread::spawn(move || {
            let result = play_script(&input, &script);
            match &result {
                Ok(report) => eprintln!(
                    "Guion de entrada: {} eventos en {:.2} s.",
                    report.events,
                    report.elapsed.as_secs_f64()
                ),
                Err(err) => eprintln!("error: guion de entrada: {err}"),
            }
            result
        })
    });
    if options.frames.is_some() {
        while !session.wait(Duration::from_millis(250)) {}
    } else if let Some(player) = player {
        let played = player.join();
        // Lo último del guion aún va de camino: un momento antes de cerrar.
        thread::sleep(Duration::from_millis(500));
        if let Ok(Err(err)) = played {
            let _ = finish_session(session);
            let _ = printer.join();
            return Err(format!("guion de entrada: {err}"));
        }
    } else {
        until_stopped(&session);
    }
    let finished = finish_session(session);
    let _ = printer.join();
    finished
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_persist_in_the_data_directory() {
        let dir = tempfile::tempdir().expect("directorio temporal");
        let (first, place) = load_identity(Some(dir.path()), true).expect("identidad");
        assert!(place.ends_with("identity.p8"), "{place}");
        let (second, _) = load_identity(Some(dir.path()), true).expect("identidad");
        assert_eq!(first.public_key(), second.public_key());
    }

    fn net() -> NetOptions {
        NetOptions {
            server: Some("https://coroom.example.com".to_owned()),
            dev_token: Some("change-me".to_owned()),
            trust_fingerprint: Some("AB:".repeat(31) + "AB"),
            force_relay: true,
            port_mapping: true,
            data_dir: None,
            headless: true,
            local_port: None,
            #[cfg(debug_assertions)]
            insecure_no_sframe: false,
        }
    }

    #[test]
    fn fingerprints_are_normalised_and_checked() {
        let config = remote_config(&net()).expect("configuración");
        assert!(config.force_relay);
        match config.trust {
            coroom_transport::ServerTrust::Fingerprint(fingerprint) => {
                assert_eq!(fingerprint, "ab".repeat(32))
            }
            other => panic!("se esperaba una huella: {other:?}"),
        }
        let bad = NetOptions {
            trust_fingerprint: Some("1234".to_owned()),
            ..net()
        };
        assert!(remote_config(&bad).is_err());
        // Sin token de desarrollo hace falta una sesión guardada.
        let dir = tempfile::tempdir().expect("directorio temporal");
        let no_login = NetOptions {
            dev_token: None,
            data_dir: Some(dir.path().to_owned()),
            ..net()
        };
        let err = remote_config(&no_login).expect_err("sin sesión");
        assert!(err.contains("coroom login"), "{err}");
    }

    #[test]
    fn console_commands_parse_with_spanish_errors() {
        assert_eq!(parse_console("   "), Ok(None));
        assert_eq!(parse_console("list"), Ok(Some(ConsoleCommand::List)));
        assert_eq!(parse_console("help"), Ok(Some(ConsoleCommand::Help)));
        assert_eq!(
            parse_console("allow 3 kbm"),
            Ok(Some(ConsoleCommand::Allow(3, Grant::KeyboardMouse)))
        );
        assert_eq!(
            parse_console("allow 3 pad 2"),
            Ok(Some(ConsoleCommand::Allow(3, Grant::Pad(2))))
        );
        assert_eq!(
            parse_console("deny 3 pad"),
            Ok(Some(ConsoleCommand::Deny(3, Revoke::Pad)))
        );
        assert_eq!(
            parse_console("deny 3 raw"),
            Ok(Some(ConsoleCommand::Deny(3, Revoke::Raw)))
        );
        assert_eq!(
            parse_console("allow 3 raw"),
            Ok(Some(ConsoleCommand::Allow(3, Grant::Raw)))
        );
        assert_eq!(
            parse_console("  kick 7 "),
            Ok(Some(ConsoleCommand::Kick(7)))
        );
        assert_eq!(parse_console("pause"), Ok(Some(ConsoleCommand::Pause)));
        assert_eq!(parse_console("resume"), Ok(Some(ConsoleCommand::Resume)));
        assert_eq!(parse_console("quit"), Ok(Some(ConsoleCommand::Quit)));
        let err = |line: &str| parse_console(line).expect_err(line);
        assert!(err("allow tres kbm").contains("no es un número"));
        assert!(err("allow 3 pad").contains("ranura"));
        assert!(err("allow 3 volar").contains("kbm, pad, raw o voice"));
        assert!(err("bailar").contains("help"));
        assert!(err("kick 1 2").contains("sobra"));
        assert!(err("kick").contains("falta"));
        assert_eq!(
            parse_console("invite ana"),
            Ok(Some(ConsoleCommand::Invite("ana".to_owned())))
        );
        assert!(err("invite").contains("NOMBRE"));
        assert!(CONSOLE_HELP.contains("allow N pad RANURA"));
        // El chat: todo lo que sigue a `say`, con sus espacios y emoji.
        assert_eq!(
            parse_console("say  hola,   ¿empezamos? 🎮 "),
            Ok(Some(ConsoleCommand::Say(
                "hola,   ¿empezamos? 🎮".to_owned()
            )))
        );
        assert_eq!(
            parse_console("SAY kick 3"),
            Ok(Some(ConsoleCommand::Say("kick 3".to_owned())))
        );
        assert!(err("say").contains("say TEXTO"));
        assert!(err("say    ").contains("falta el texto"));
        assert_eq!(
            parse_console("clear-chat"),
            Ok(Some(ConsoleCommand::ClearChat))
        );
        assert!(err("clear-chat ya").contains("sobra"));
        assert!(CONSOLE_HELP.contains("say TEXTO") && CONSOLE_HELP.contains("clear-chat"));
    }

    #[test]
    fn the_join_console_says_and_quits() {
        assert_eq!(
            parse_join_console("say buenas noches"),
            Ok(JoinCommand::Say("buenas noches".to_owned()))
        );
        assert_eq!(parse_join_console(""), Ok(JoinCommand::Quit));
        assert_eq!(parse_join_console("  quit "), Ok(JoinCommand::Quit));
        assert_eq!(parse_join_console("help"), Ok(JoinCommand::Help));
        assert!(
            parse_join_console("kick 2")
                .unwrap_err()
                .contains("orden desconocida")
        );
        assert!(parse_join_console("say").unwrap_err().contains("say TEXTO"));
        assert!(JOIN_CONSOLE_HELP.contains("say TEXTO"));
    }

    #[test]
    fn chat_lines_carry_the_local_time() {
        use coroom_session::remote::ChatAuthor;
        let line = ChatLine {
            author: ChatAuthor {
                guest: 3,
                leaf: 2,
                name: "Bruno".to_owned(),
            },
            text: "texto".to_owned(),
            at_unix_ms: 1_782_907_200_000,
            own: false,
        };
        let offset = crate::localtime::utc_offset(1_782_907_200);
        let expected = format!(
            "[{}] Bruno: texto",
            crate::localtime::hhmm(1_782_907_200, offset)
        );
        assert_eq!(chat_line(&line), expected);
        assert_eq!(
            describe_event(&SessionEvent::ChatMessage(line.clone())),
            expected
        );
        let history = describe_event(&SessionEvent::ChatHistory {
            messages: vec![line.clone(), line],
        });
        assert_eq!(
            history,
            format!("Historial del chat (2 mensajes):\n  {expected}\n  {expected}")
        );
        assert_eq!(
            describe_event(&SessionEvent::ChatCleared),
            "El anfitrión ha vaciado el chat."
        );
    }

    #[test]
    fn the_game_audio_option_parses() {
        let parse = |values: &[&str]| {
            parse_game_capture(&values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>())
        };
        assert_eq!(parse(&["none"]), Ok(GameCapture::None));
        assert_eq!(parse(&["system"]), Ok(GameCapture::System));
        assert_eq!(
            parse(&["app", "Juego"]),
            Ok(GameCapture::App("Juego".into()))
        );
        assert_eq!(parse(&["process", "4242"]), Ok(GameCapture::Process(4242)));
        assert!(parse(&["process", "cuatro"]).is_err_and(|err| err.contains("PID")));
        assert!(parse(&["app"]).is_err_and(|err| err.contains("NOMBRE")));
        assert!(parse(&["todo"]).is_err_and(|err| err.contains("none, system")));
        assert_eq!(
            parse(&["apps-except", "discord.exe, steam.exe"]),
            Ok(GameCapture::Apps(AppFilter::AllExcept(vec![
                "discord.exe".into(),
                "steam.exe".into()
            ])))
        );
        assert_eq!(
            parse(&["apps-except"]),
            Ok(GameCapture::Apps(AppFilter::AllExcept(Vec::new())))
        );
        assert_eq!(
            parse(&["apps-only", "juego"]),
            Ok(GameCapture::Apps(AppFilter::Only(vec!["juego".into()])))
        );
        assert!(parse(&["system", "sobra"]).is_err());
    }

    #[test]
    fn headless_audio_is_synthetic() {
        let host = host_audio(
            &HostAudioOptions {
                voice: true,
                ..HostAudioOptions::default()
            },
            true,
        );
        assert!(host.game.is_some() && host.voice.is_some() && host.speaker.is_some());
        let quiet = host_audio(
            &HostAudioOptions {
                game: GameCapture::None,
                ..HostAudioOptions::default()
            },
            true,
        );
        assert!(quiet.game.is_none() && quiet.voice.is_none());
        let guest = guest_audio(
            &JoinAudioOptions {
                voice: true,
                ptt: Some("F12".into()),
                ..JoinAudioOptions::default()
            },
            true,
        )
        .expect("audio");
        assert!(guest.speaker.is_some());
        assert_eq!(
            guest.voice.as_ref().map(|v| v.talk),
            Some(TalkMode::PushToTalk)
        );
        assert!(
            guest_audio(
                &JoinAudioOptions {
                    disabled: true,
                    ..JoinAudioOptions::default()
                },
                true
            )
            .is_none()
        );
    }

    #[test]
    fn auto_allow_reads_kbm_and_pad() {
        assert_eq!(
            AutoAllow::parse("kbm,pad"),
            Ok(AutoAllow {
                keyboard_mouse: true,
                gamepad: true
            })
        );
        assert_eq!(
            AutoAllow::parse("pad"),
            Ok(AutoAllow {
                keyboard_mouse: false,
                gamepad: true
            })
        );
        assert!(AutoAllow::parse("").is_ok_and(|auto| auto.is_empty()));
        assert!(AutoAllow::parse("kbm,voz").is_err());
    }

    #[test]
    fn the_data_directory_is_named_coroom() {
        assert!(
            default_data_dir().ends_with("coroom") || default_data_dir().ends_with("coroom-datos")
        );
    }
}
