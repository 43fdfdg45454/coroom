//! El estado de la lógica mientras vive el proceso: el almacén sellado, la sala de espera y la
//! cola de eventos hacia la interfaz.
//!
//! La interfaz llama a [`App::call`] con el nombre de una operación y sus argumentos en JSON, y
//! recibe `{"ok": …}` o `{"error": "…"}`. Las operaciones que tocan la red bloquean: la interfaz
//! las llama fuera del hilo principal.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::Duration;

use coroom_session::account::{self, AccountStorage, ServerOptions, SignedIn};
use coroom_session::lobby;
use coroom_transport::proto::signal::SignalMessage;
use crossbeam_channel::{Receiver, Sender};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use coroom_session::codec::mediacodec::AndroidDecoderInfo;
use coroom_session::input::gamepad::PadKey;
use coroom_session::input::touch::{TouchConfig, TouchMode};

use crate::avatars::AvatarCache;
use crate::events::{self, UiEvent};
use crate::guest::{self, Devices, Guest, JoinRequest};
use crate::store::SealedStore;

/// Eventos que se guardan para la interfaz si no los lee (en segundo plano): los más viejos se
/// descartan.
const EVENT_QUEUE: usize = 1024;

/// Respuesta de `resume` sin ninguna sesión guardada (la interfaz abre la pantalla Servidor).
pub const NOT_SIGNED_IN: &str = "todavía no has iniciado sesión en ningún servidor";

/// Respuesta de lo que solo vale dentro de una sala.
pub const NOT_IN_ROOM: &str = "no estás en ninguna sala";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// La sala de espera abierta.
struct Lobby {
    stop: Arc<AtomicBool>,
    outbox: mpsc::Sender<SignalMessage>,
    thread: thread::JoinHandle<()>,
}

/// La lógica del invitado.
pub struct App {
    store: SealedStore,
    device: String,
    events: (Sender<UiEvent>, Receiver<UiEvent>),
    lobby: Mutex<Option<Lobby>>,
    devices: Arc<Devices>,
    guest: Mutex<Option<Arc<Guest>>>,
    settings: Mutex<Settings>,
}

/// Ajustes de la sala que se aplican al entrar (y en marcha si ya se está dentro).
#[derive(Debug, Clone, Copy)]
struct Settings {
    game_volume: u32,
    join_muted: bool,
}

/// Servidor de la operación (`null`: el último usado).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Server {
    server: Option<String>,
}

impl Server {
    fn options(self) -> ServerOptions {
        ServerOptions {
            server: self.server,
            trust_fingerprint: None,
            tcp_port: None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct SignIn {
    server: String,
    username: String,
    password: String,
    #[serde(default)]
    invitation: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    trust_fingerprint: Option<String>,
    #[serde(default)]
    tcp_port: Option<u16>,
}

#[derive(Debug, Deserialize)]
struct Probe {
    url: String,
    #[serde(default)]
    tcp_port: Option<u16>,
}

#[derive(Debug, Deserialize)]
struct Username {
    username: String,
}

#[derive(Debug, Deserialize)]
struct Id {
    id: u64,
}

#[derive(Debug, Deserialize)]
struct User {
    user: u64,
}

#[derive(Debug, Deserialize)]
struct Avatar {
    user: u64,
    version: u64,
    #[serde(default)]
    server: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Route {
    gateway: Option<std::net::Ipv4Addr>,
    #[serde(default)]
    mtu: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct Roots {
    pem: String,
}

#[derive(Debug, Deserialize)]
struct Join {
    code: String,
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    mic: bool,
}

#[derive(Debug, Deserialize)]
struct Decoders {
    decoders: Vec<AndroidDecoderInfo>,
}

#[derive(Debug, Deserialize)]
struct Gestures {
    touch_slop: f32,
    tap_timeout_ms: u64,
    long_press_timeout_ms: u64,
    double_tap_timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
struct Mode {
    mode: String,
}

#[derive(Debug, Deserialize)]
struct Text {
    text: String,
}

#[derive(Debug, Deserialize)]
struct Volume {
    percent: u32,
}

#[derive(Debug, Deserialize)]
struct Muted {
    muted: bool,
}

fn signed_in(signed: &SignedIn) -> Value {
    json!({
        "server": signed.credentials.server,
        "username": signed.credentials.username,
        "display_name": signed.credentials.display_name,
        "user_id": signed.credentials.user_id,
        "avatar": signed.avatar,
        "fingerprint": signed.fingerprint,
    })
}

fn args<T: DeserializeOwned>(text: &str) -> Result<T, String> {
    let text = if text.trim().is_empty() { "{}" } else { text };
    serde_json::from_str(text).map_err(|err| format!("argumentos no válidos: {err}"))
}

impl App {
    /// Lógica con el almacén en `store` y este nombre de dispositivo (el que verá la cuenta en su
    /// lista de dispositivos).
    pub fn new(store: SealedStore, device: String) -> Self {
        Self {
            store,
            device,
            events: crossbeam_channel::bounded(EVENT_QUEUE),
            lobby: Mutex::new(None),
            devices: Arc::new(Devices::default()),
            guest: Mutex::new(None),
            settings: Mutex::new(Settings {
                game_volume: 100,
                join_muted: false,
            }),
        }
    }

    /// Encola un evento para la interfaz; con la cola llena se descarta el más viejo.
    pub fn emit(&self, event: UiEvent) {
        let (tx, rx) = &self.events;
        if let Err(crossbeam_channel::TrySendError::Full(event)) = tx.try_send(event) {
            let _ = rx.try_recv();
            let _ = tx.try_send(event);
        }
    }

    /// El siguiente evento en JSON, esperando hasta `timeout`.
    pub fn next_event(&self, timeout: Duration) -> Option<String> {
        let event = self.events.1.recv_timeout(timeout).ok()?;
        match serde_json::to_string(&event) {
            Ok(json) => Some(json),
            Err(err) => {
                tracing::warn!(error = %err, "evento sin JSON");
                None
            }
        }
    }

    /// Ejecuta la operación `method` con sus argumentos en JSON: `{"ok": …}` o `{"error": …}`.
    pub fn call(self: &Arc<Self>, method: &str, args: &str) -> String {
        let result = self.dispatch(method, args);
        let value = match result {
            Ok(ok) => json!({ "ok": ok }),
            Err(error) => {
                tracing::debug!(method, error = %error, "operación fallida");
                json!({ "error": error })
            }
        };
        value.to_string()
    }

    fn dispatch(self: &Arc<Self>, method: &str, text: &str) -> Result<Value, String> {
        let store = &self.store as &dyn AccountStorage;
        match method {
            "resume" => {
                let server: Server = args(text)?;
                if server.server.is_none() && store.last_server().is_none() {
                    return Err(NOT_SIGNED_IN.to_owned());
                }
                let (_, signed) = account::resume(store, &server.options())?;
                Ok(signed_in(&signed))
            }
            "sign_in" => {
                let request: SignIn = args(text)?;
                let options = ServerOptions {
                    server: Some(request.server),
                    trust_fingerprint: request.trust_fingerprint,
                    tcp_port: request.tcp_port,
                };
                let invitation = request
                    .invitation
                    .as_deref()
                    .filter(|code| !code.trim().is_empty())
                    .map(|code| (code, request.display_name.as_deref()));
                let signed = account::sign_in(
                    store,
                    &options,
                    &request.username,
                    &request.password,
                    &self.device,
                    invitation,
                )?;
                Ok(signed_in(&signed))
            }
            "logout" => {
                self.stop_lobby();
                let server: Server = args(text)?;
                account::logout(store, &server.options()).map(Value::from)
            }
            "probe_server" => {
                let probe: Probe = args(text)?;
                let found = account::probe_server(&probe.url, probe.tcp_port)?;
                Ok(json!({
                    "name": found.name,
                    "version": found.version,
                    "protocol": found.protocol,
                    "http3": found.http3,
                    "udp_reachable": found.udp_reachable,
                    "fingerprint": found.fingerprint,
                    "untrusted": found.untrusted,
                }))
            }
            "friends" => {
                let session = self.session(text)?;
                let friends = account::list_friends(&session)?;
                serde_json::to_value(friends).map_err(|err| err.to_string())
            }
            "request_friend" => {
                let session = self.session("")?;
                let who: Username = args(text)?;
                account::request_friend(&session, &who.username).map(Value::from)
            }
            "accept_request" => {
                let id: Id = args(text)?;
                account::accept_request(&self.session("")?, id.id).map(Value::from)
            }
            "decline_request" => {
                let id: Id = args(text)?;
                account::decline_request(&self.session("")?, id.id).map(Value::from)
            }
            "remove_friend" => {
                let id: Id = args(text)?;
                account::remove_friend(&self.session("")?, id.id).map(Value::from)
            }
            "avatar" => {
                let request: Avatar = args(text)?;
                let path = self.avatar(&request)?;
                Ok(path.map_or(Value::Null, |path| {
                    Value::from(path.to_string_lossy().into_owned())
                }))
            }
            "lobby_start" => {
                let server: Server = args(text)?;
                self.start_lobby(server.options());
                Ok(Value::Null)
            }
            "lobby_stop" => {
                self.stop_lobby();
                Ok(Value::Null)
            }
            "ask_to_join" => {
                let user: User = args(text)?;
                self.send_lobby(SignalMessage::JoinRequest { to_user: user.user })?;
                Ok(Value::Null)
            }
            "set_route" => {
                let route: Route = args(text)?;
                coroom_transport::nat::set_default_route(route.gateway.map(|gateway| {
                    coroom_transport::nat::DefaultRoute {
                        gateway,
                        mtu: route.mtu,
                    }
                }));
                Ok(Value::Null)
            }
            "set_roots" => {
                let roots: Roots = args(text)?;
                Ok(Value::from(coroom_transport::tls::set_platform_roots_pem(
                    &roots.pem,
                )))
            }
            "set_decoders" => {
                let list: Decoders = args(text)?;
                let count = list.decoders.len();
                *lock(&self.devices.decoders) = list.decoders;
                Ok(Value::from(count))
            }
            "set_touch_config" => {
                let gestures: Gestures = args(text)?;
                let config = TouchConfig::new(
                    gestures.touch_slop,
                    gestures.tap_timeout_ms,
                    gestures.long_press_timeout_ms,
                    gestures.double_tap_timeout_ms,
                );
                *lock(&self.devices.touch_config) = config;
                if let Some(guest) = self.guest() {
                    guest.set_touch_config(config);
                }
                Ok(Value::Null)
            }
            "set_touch_mode" => {
                let mode: Mode = args(text)?;
                let mode = match mode.mode.as_str() {
                    "trackpad" => TouchMode::Trackpad,
                    "direct" => TouchMode::Direct,
                    other => return Err(format!("modo del tacto desconocido: {other}")),
                };
                *lock(&self.devices.touch_mode) = mode;
                if let Some(guest) = self.guest() {
                    guest.set_touch_mode(mode);
                }
                Ok(Value::Null)
            }
            "set_game_volume" => {
                let volume: Volume = args(text)?;
                let percent = volume.percent.min(200);
                lock(&self.settings).game_volume = percent;
                if let Some(guest) = self.guest() {
                    guest.audio().set_game_gain(percent as f32 / 100.0);
                }
                Ok(Value::Null)
            }
            "set_join_muted" => {
                let muted: Muted = args(text)?;
                lock(&self.settings).join_muted = muted.muted;
                Ok(Value::Null)
            }
            "set_mic_muted" => {
                let muted: Muted = args(text)?;
                let guest = self.guest().ok_or_else(|| NOT_IN_ROOM.to_owned())?;
                guest.audio().set_mic_muted(muted.muted);
                Ok(Value::Null)
            }
            "pads" => Ok(guest::pad_list(&self.devices)),
            "join" => {
                let join: Join = args(text)?;
                self.join(join)?;
                Ok(Value::Null)
            }
            "leave" => {
                self.leave();
                Ok(Value::Null)
            }
            "send_chat" => {
                let message: Text = args(text)?;
                self.guest()
                    .ok_or_else(|| NOT_IN_ROOM.to_owned())?
                    .send_chat(&message.text)?;
                Ok(Value::Null)
            }
            "stats" => Ok(self.guest().map_or(Value::Null, |guest| guest.stats())),
            _ => Err(format!("operación desconocida: {method}")),
        }
    }

    /// La sesión en la sala, si se está en una.
    pub fn guest(&self) -> Option<Arc<Guest>> {
        lock(&self.guest).clone()
    }

    /// Lo que dura entre sesiones: mandos, superficie, decodificadores, tacto.
    pub fn devices(&self) -> &Arc<Devices> {
        &self.devices
    }

    /// Entra en una sala en otro hilo (la red bloquea): el avance y el final llegan como eventos.
    fn join(self: &Arc<Self>, join: Join) -> Result<(), String> {
        if join.code.trim().is_empty() {
            return Err("falta el código de la sala".to_owned());
        }
        self.leave();
        let settings = *lock(&self.settings);
        let request = JoinRequest {
            code: join.code,
            server: join.server,
            mic: join.mic,
            mic_muted: settings.join_muted,
            game_volume: settings.game_volume,
        };
        let app = Arc::clone(self);
        thread::Builder::new()
            .name("coroom-entrar".to_owned())
            .spawn(move || {
                let emit_app = Arc::clone(&app);
                let emit: guest::Emit = Arc::new(move |event| emit_app.emit(event));
                match Guest::join(&app.store, &app.devices, &request, Arc::clone(&emit)) {
                    Ok(guest) => *lock(&app.guest) = Some(guest),
                    Err(error) => emit(UiEvent::Ended { error: Some(error) }),
                }
            })
            .map_err(|err| format!("no se pudo entrar en la sala: {err}"))?;
        Ok(())
    }

    /// Sale de la sala, si se está en una.
    pub fn leave(&self) {
        let guest = lock(&self.guest).take();
        if let Some(guest) = guest {
            guest.leave();
        }
    }

    /// Se conectó un mando.
    pub fn pad_added(&self, device_id: i32, key: PadKey) {
        {
            let mut pads = lock(&self.devices.pads);
            pads.retain(|(id, _)| *id != device_id);
            pads.push((device_id, key.clone()));
        }
        if let Some(guest) = self.guest() {
            guest.pad_added(device_id, key);
        }
    }

    /// Se desconectó un mando.
    pub fn pad_removed(&self, device_id: i32) {
        lock(&self.devices.pads).retain(|(id, _)| *id != device_id);
        if let Some(guest) = self.guest() {
            guest.pad_removed(device_id);
        }
    }

    /// La ruta del PNG del avatar de la petición, de la caché o descargado; `None` si no tiene.
    fn avatar(&self, request: &Avatar) -> Result<Option<std::path::PathBuf>, String> {
        let cache = AvatarCache::in_data_dir(self.store.dir());
        cache.get_or_fetch(request.user, request.version, || {
            let options = ServerOptions {
                server: request.server.clone(),
                trust_fingerprint: None,
                tcp_port: None,
            };
            let session = account::session(&self.store, &options)?;
            let mut downloaded = Err("el servidor no contestó por el avatar".to_owned());
            account::download_avatars(&session, &[request.user], |_, result| {
                downloaded = result;
            })?;
            downloaded
        })
    }

    fn session(&self, text: &str) -> Result<account::Session, String> {
        let server: Server = args(text)?;
        account::session(&self.store, &server.options())
    }

    /// Abre la sala de espera (la cierra antes si ya estaba abierta, por si cambió el servidor).
    fn start_lobby(self: &Arc<Self>, options: ServerOptions) {
        self.stop_lobby();
        let stop = Arc::new(AtomicBool::new(false));
        let (outbox, inbox) = mpsc::channel();
        let app = Arc::clone(self);
        let thread_stop = Arc::clone(&stop);
        let spawned = thread::Builder::new()
            .name("coroom-lobby".to_owned())
            .spawn(move || {
                let session = || -> Result<_, String> {
                    let session = account::session(&app.store, &options)?;
                    let (identity, _) = app.store.load_identity()?;
                    Ok((session, identity))
                };
                lobby::run(&session, &thread_stop, &inbox, &mut |event| {
                    if let Some(event) = events::from_lobby(event) {
                        app.emit(event);
                    }
                });
            });
        match spawned {
            Ok(thread) => {
                *lock(&self.lobby) = Some(Lobby {
                    stop,
                    outbox,
                    thread,
                });
            }
            Err(err) => tracing::warn!(error = %err, "no arrancó la sala de espera"),
        }
    }

    /// Cierra la sala de espera y espera a que suelte la red.
    pub fn stop_lobby(&self) {
        let Some(lobby) = lock(&self.lobby).take() else {
            return;
        };
        lobby.stop.store(true, Ordering::Release);
        let _ = lobby.thread.join();
    }

    fn send_lobby(&self, message: SignalMessage) -> Result<(), String> {
        let lobby = lock(&self.lobby);
        let Some(lobby) = lobby.as_ref() else {
            return Err("la sala de espera no está abierta".to_owned());
        };
        lobby
            .outbox
            .send(message)
            .map_err(|_| "la sala de espera se cerró".to_owned())
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.leave();
        self.stop_lobby();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> (tempfile::TempDir, Arc<App>) {
        let dir = tempfile::tempdir().expect("directorio");
        let store = SealedStore::new(dir.path(), &[3u8; 32]).expect("almacén");
        (
            dir,
            Arc::new(App::new(store, "Teléfono de prueba".to_owned())),
        )
    }

    fn call(app: &Arc<App>, method: &str, args: &str) -> Value {
        serde_json::from_str(&app.call(method, args)).expect("respuesta en JSON")
    }

    #[test]
    fn calls_answer_ok_or_error_in_json() {
        let (_dir, app) = app();
        let unknown = call(&app, "no_existe", "");
        assert!(
            unknown["error"]
                .as_str()
                .is_some_and(|e| e.contains("no_existe"))
        );
        let bad = call(&app, "ask_to_join", "{\"user\": \"ana\"}");
        assert!(
            bad["error"]
                .as_str()
                .is_some_and(|e| e.contains("argumentos"))
        );
        // Sin sesión iniciada no hay a qué servidor ir.
        assert_eq!(call(&app, "resume", ""), json!({ "error": NOT_SIGNED_IN }));
        assert!(call(&app, "friends", "{}").get("error").is_some());
        // Sin sala de espera no se puede pedir entrar.
        assert!(
            call(&app, "ask_to_join", "{\"user\": 4}")["error"]
                .as_str()
                .is_some_and(|e| e.contains("sala de espera"))
        );
    }

    #[test]
    fn avatars_come_from_the_cache_and_need_an_account_otherwise() {
        let (dir, app) = app();
        assert!(
            call(&app, "avatar", r#"{"user": 7}"#)["error"]
                .as_str()
                .is_some_and(|e| e.contains("argumentos"))
        );
        // Sin cuenta no hay de dónde descargarlo.
        assert!(
            call(&app, "avatar", r#"{"user": 7, "version": 1}"#)
                .get("error")
                .is_some()
        );
        // Con el fichero ya en la caché se contesta sin ir a la red.
        let png = b"\x89PNG de ejemplo".to_vec();
        let version =
            coroom_transport::proto::api::avatar_version(&coroom_transport::crypto::sha256(&png));
        let path = AvatarCache::in_data_dir(dir.path()).path(7, version);
        std::fs::create_dir_all(path.parent().expect("padre")).expect("directorio");
        std::fs::write(&path, &png).expect("escribir");
        assert_eq!(
            call(
                &app,
                "avatar",
                &format!(r#"{{"user": 7, "version": {version}}}"#)
            ),
            json!({ "ok": path.to_string_lossy() })
        );
    }

    #[test]
    fn route_and_roots_reach_the_transport() {
        let (_dir, app) = app();
        assert_eq!(
            call(
                &app,
                "set_route",
                r#"{"gateway": "198.51.100.9", "mtu": 1400}"#
            ),
            json!({ "ok": null })
        );
        assert_eq!(
            coroom_transport::nat::default_gateway(),
            Ok(std::net::Ipv4Addr::new(198, 51, 100, 9))
        );
        assert_eq!(
            call(&app, "set_route", r#"{"gateway": null}"#),
            json!({ "ok": null })
        );
        assert_eq!(
            call(&app, "set_roots", r#"{"pem": ""}"#),
            json!({ "ok": 0 })
        );
    }

    #[test]
    fn room_settings_are_kept_and_room_calls_need_a_room() {
        let (_dir, app) = app();
        assert_eq!(
            call(&app, "set_touch_mode", r#"{"mode": "direct"}"#),
            json!({ "ok": null })
        );
        assert!(
            call(&app, "set_touch_mode", r#"{"mode": "otro"}"#)
                .get("error")
                .is_some()
        );
        assert_eq!(
            call(&app, "set_game_volume", r#"{"percent": 150}"#),
            json!({ "ok": null })
        );
        assert_eq!(
            call(&app, "set_join_muted", r#"{"muted": true}"#),
            json!({ "ok": null })
        );
        assert_eq!(
            call(&app, "set_mic_muted", r#"{"muted": true}"#),
            json!({ "error": NOT_IN_ROOM })
        );
        assert_eq!(call(&app, "stats", ""), json!({ "ok": null }));
        assert!(
            call(&app, "join", r#"{"code": "  "}"#)
                .get("error")
                .is_some()
        );
        let decoders = r#"{"decoders": [{"name": "c2.ejemplo.avc.decoder", "mime": "video/avc",
            "hardware": true, "max_width": 1920, "max_height": 1080, "max_fps": 60}]}"#;
        assert_eq!(call(&app, "set_decoders", decoders), json!({ "ok": 1 }));
        let gestures = r#"{"touch_slop": 21.0, "tap_timeout_ms": 100,
            "long_press_timeout_ms": 400, "double_tap_timeout_ms": 300}"#;
        assert_eq!(
            call(&app, "set_touch_config", gestures),
            json!({ "ok": null })
        );
    }

    #[test]
    fn joining_without_an_account_ends_with_the_reason() {
        let (_dir, app) = app();
        assert_eq!(
            call(&app, "join", r#"{"code": "K7Q2MZ"}"#),
            json!({ "ok": null })
        );
        let event = app.next_event(Duration::from_secs(10)).expect("evento");
        let event: Value = serde_json::from_str(&event).expect("json");
        assert_eq!(event["type"], json!("ended"));
        assert!(event["error"].as_str().is_some());
        assert!(app.guest().is_none());
    }

    #[test]
    fn pads_are_remembered_between_rooms() {
        let (_dir, app) = app();
        let key = PadKey {
            vendor: 1,
            product: 2,
            name: "Mando de ejemplo".to_owned(),
        };
        app.pad_added(7, key.clone());
        app.pad_added(7, key);
        assert_eq!(
            call(&app, "pads", "")["ok"].as_array().map(Vec::len),
            Some(1)
        );
        app.pad_removed(7);
        assert_eq!(call(&app, "pads", ""), json!({ "ok": [] }));
    }

    #[test]
    fn events_queue_drops_the_oldest_when_full() {
        let (_dir, app) = app();
        for user in 0..(EVENT_QUEUE as u64 + 5) {
            app.emit(UiEvent::Presence { user, online: true });
        }
        let first = app.next_event(Duration::ZERO).expect("evento");
        let first: Value = serde_json::from_str(&first).expect("json");
        assert_eq!(first["user"], json!(5));
        assert_eq!(first["type"], json!("presence"));
    }

    #[test]
    fn a_lobby_without_an_account_reports_offline_and_stops() {
        let (_dir, app) = app();
        assert_eq!(call(&app, "lobby_start", ""), json!({ "ok": null }));
        let event = app.next_event(Duration::from_secs(10)).expect("evento");
        let event: Value = serde_json::from_str(&event).expect("json");
        assert_eq!(
            event,
            json!({"type": "server_status", "online": false, "rtt_ms": null})
        );
        assert_eq!(call(&app, "lobby_stop", ""), json!({ "ok": null }));
    }
}
