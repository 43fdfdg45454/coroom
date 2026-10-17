//! Cuentas: `coroom login`, `logout`, `whoami`, `friends` e `invite`, y las credenciales
//! de cada servidor.
//!
//! - **Credenciales**: el token de sesión de cada servidor (con el usuario, la huella del
//!   certificado si se fijó y el puerto TCP del respaldo) en el llavero del sistema (`coroom` /
//!   `server:<host>`) o, sin llavero, sin ventana (`--headless`) o con `--data-dir`, en
//!   `servers/<host>.json` (`0600`) dentro del directorio de datos. El último servidor usado queda
//!   en `servers/last` (sin secretos) para no repetir `--server`.
//! - **Login**: pide usuario y contraseña por consola sin eco (o `--user` y `--password-stdin`),
//!   presenta la clave pública de la identidad del equipo y firma el desafío del servidor
//!   ([`ApiClient::login`]); con `--invitation CÓDIGO`, antes crea la cuenta con el código.
//! - Las llamadas a la API se hacen en un runtime de `tokio` propio y corto; el llavero se lee
//!   y escribe fuera de él (en Linux es bloqueante).

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use coroom_transport::api::ApiClient;
use coroom_transport::crypto::{CredentialStore, KeyringStore};
use coroom_transport::proto::signal::{ErrorCode, SignalMessage};
use coroom_transport::signal::ServerUrl;
use coroom_transport::{Endpoint, EndpointOptions, ServerTrust, SignalClient, TransportError};

use crate::remote::{default_data_dir, load_identity};

/// Lo que se guarda de la sesión en cada servidor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    /// URL del servidor (`https://host[:puerto]`).
    pub server: String,
    /// Usuario.
    pub username: String,
    /// Nombre visible.
    pub display_name: String,
    /// Identificador del usuario.
    pub user_id: u64,
    /// Token de sesión.
    pub token: String,
    /// Caducidad (segundos Unix).
    pub expires_at: i64,
    /// Huella del certificado autofirmado del servidor, si se fijó al iniciar sesión.
    #[serde(default)]
    pub trust_fingerprint: Option<String>,
    /// Puerto TCP del respaldo HTTP/2, si no es el de la URL.
    #[serde(default)]
    pub tcp_port: Option<u16>,
}

/// Dónde están las credenciales de este usuario del equipo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountStore {
    data_dir: Option<PathBuf>,
    headless: bool,
}

/// Host de una URL de servidor (para nombrar su credencial).
pub fn server_host(url: &str) -> Result<String, String> {
    ServerUrl::parse(url)
        .map(|url| url.host.to_ascii_lowercase())
        .map_err(|err| err.to_string())
}

impl AccountStore {
    /// Almacén según `--data-dir` y `--headless` (las mismas reglas que la identidad).
    pub fn new(data_dir: Option<&Path>, headless: bool) -> Self {
        Self {
            data_dir: data_dir.map(Path::to_path_buf),
            headless,
        }
    }

    fn base_dir(&self) -> PathBuf {
        self.data_dir.clone().unwrap_or_else(default_data_dir)
    }

    fn uses_keyring(&self) -> bool {
        self.data_dir.is_none() && !self.headless && KeyringStore::is_available()
    }

    fn store(&self, host: &str) -> CredentialStore {
        if self.data_dir.is_none()
            && !self.headless
            && let Some(key) = crate::portal::seal_key()
        {
            return CredentialStore::sealed(self.base_dir(), host, key);
        }
        if self.uses_keyring() {
            CredentialStore::keyring(host)
        } else {
            CredentialStore::file(self.base_dir(), host)
        }
    }

    /// Credenciales guardadas para el servidor `url`.
    pub fn load(&self, url: &str) -> Result<Option<Credentials>, String> {
        let host = server_host(url)?;
        let store = self.store(&host);
        let Some(text) = store
            .load()
            .map_err(|err| format!("no se pudieron leer las credenciales: {err}"))?
        else {
            return Ok(None);
        };
        serde_json::from_str(&text).map(Some).map_err(|err| {
            format!("las credenciales de {host} están dañadas ({err}); vuelve a iniciar sesión")
        })
    }

    /// Guarda las credenciales y apunta el servidor como el último; devuelve dónde.
    pub fn save(&self, credentials: &Credentials) -> Result<String, String> {
        let host = server_host(&credentials.server)?;
        let store = self.store(&host);
        let text = serde_json::to_string(credentials)
            .map_err(|err| format!("no se pudieron guardar las credenciales: {err}"))?;
        store
            .save(&text)
            .map_err(|err| format!("no se pudieron guardar las credenciales: {err}"))?;
        self.set_last_server(&credentials.server);
        Ok(store.describe())
    }

    /// Borra las credenciales del servidor `url`.
    pub fn delete(&self, url: &str) -> Result<(), String> {
        let host = server_host(url)?;
        self.store(&host)
            .delete()
            .map_err(|err| format!("no se pudieron borrar las credenciales: {err}"))
    }

    fn last_path(&self) -> PathBuf {
        self.base_dir().join("servers").join("last")
    }

    /// Último servidor en el que se inició sesión.
    pub fn last_server(&self) -> Option<String> {
        std::fs::read_to_string(self.last_path())
            .ok()
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
    }

    /// Apunta `url` como el último servidor (el que se recupera al abrir la aplicación).
    pub fn set_last_server(&self, url: &str) {
        let path = self.last_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, format!("{url}\n"));
    }

    /// El servidor de `--server` o, si falta, el último usado.
    pub fn server(&self, server: Option<&str>) -> Result<String, String> {
        match server {
            Some(server) => Ok(server.trim().trim_end_matches('/').to_owned()),
            None => self.last_server().ok_or_else(|| {
                "falta --server URL (todavía no has iniciado sesión en ningún servidor)".to_owned()
            }),
        }
    }
}

/// Huella SHA-256 en hexadecimal (admite `:` entre bytes y mayúsculas).
pub fn normalize_fingerprint(fingerprint: &str) -> Result<String, String> {
    let fingerprint = fingerprint.trim().replace(':', "").to_ascii_lowercase();
    if fingerprint.len() != 64 || !fingerprint.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(
            "la huella del certificado debe ser SHA-256 en hexadecimal (64 caracteres)".to_owned(),
        );
    }
    Ok(fingerprint)
}

/// Confianza en el servidor: la huella dada o las raíces del sistema.
pub fn trust_for(fingerprint: Option<&str>) -> Result<ServerTrust, String> {
    match fingerprint {
        Some(fingerprint) => Ok(ServerTrust::Fingerprint(normalize_fingerprint(
            fingerprint,
        )?)),
        None => ServerTrust::system_roots().map_err(|err| err.to_string()),
    }
}

/// Nombre del equipo para el dispositivo (`HOSTNAME`, `COMPUTERNAME` o `/etc/hostname`).
pub fn device_name() -> String {
    let from_env = ["HOSTNAME", "COMPUTERNAME"].iter().find_map(|var| {
        std::env::var(var)
            .ok()
            .filter(|name| !name.trim().is_empty())
    });
    let name = from_env.or_else(|| {
        std::fs::read_to_string("/etc/hostname")
            .ok()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
    });
    name.unwrap_or_else(|| "equipo".to_owned())
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("no se pudo arrancar la red: {err}"))
}

fn api_error(err: TransportError) -> String {
    match err {
        TransportError::Api { message, .. } => message,
        other => format!("no se pudo hablar con el servidor: {other}"),
    }
}

/// Opciones comunes de las órdenes de cuenta.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccountOptions {
    /// `--server` (si falta, el último).
    pub server: Option<String>,
    /// `--trust-fingerprint` (si falta, la guardada al iniciar sesión).
    pub trust_fingerprint: Option<String>,
    /// `--tcp-port` del respaldo HTTP/2.
    pub tcp_port: Option<u16>,
    /// `--data-dir`.
    pub data_dir: Option<PathBuf>,
    /// `--headless` (sin llavero).
    pub headless: bool,
}

impl AccountOptions {
    fn store(&self) -> AccountStore {
        AccountStore::new(self.data_dir.as_deref(), self.headless)
    }
}

/// Sesión iniciada en un servidor, lista para la API y la señalización.
#[derive(Debug, Clone)]
pub struct Session {
    /// Credenciales.
    pub credentials: Credentials,
    /// Confianza en el servidor.
    pub trust: ServerTrust,
}

impl Session {
    /// Cliente de la API con el token.
    pub fn api(&self) -> Result<ApiClient, String> {
        let mut client = ApiClient::new(&self.credentials.server, self.trust.clone())
            .map_err(|err| err.to_string())?
            .with_token(&self.credentials.token);
        if let Some(port) = self.credentials.tcp_port {
            client = client.with_tcp_port(port);
        }
        Ok(client)
    }
}

/// Carga la sesión guardada del servidor de `options`.
pub fn session(options: &AccountOptions) -> Result<Session, String> {
    let store = options.store();
    let server = store.server(options.server.as_deref())?;
    let credentials = store.load(&server)?.ok_or_else(|| {
        format!("no has iniciado sesión en {server}: usa `coroom login --server {server}`")
    })?;
    let fingerprint = options
        .trust_fingerprint
        .clone()
        .or_else(|| credentials.trust_fingerprint.clone());
    let trust = trust_for(fingerprint.as_deref())?;
    let mut credentials = credentials;
    if let Some(port) = options.tcp_port {
        credentials.tcp_port = Some(port);
    }
    Ok(Session { credentials, trust })
}

/// Lee una línea de la entrada estándar tras `prompt` (en la salida de error).
fn prompt_line(prompt: &str) -> Result<String, String> {
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let read = std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|err| format!("no se pudo leer la consola: {err}"))?;
    if read == 0 {
        return Err("la entrada se cerró antes de tiempo".to_owned());
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

/// Contraseña sin eco (o de la entrada estándar con `--password-stdin`).
fn prompt_password(prompt: &str, from_stdin: bool) -> Result<String, String> {
    if from_stdin {
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|err| format!("no se pudo leer la contraseña: {err}"))?;
        return Ok(line.trim_end_matches(['\r', '\n']).to_owned());
    }
    rpassword::prompt_password(prompt).map_err(|err| {
        format!("no se pudo leer la contraseña (¿sin terminal? usa --password-stdin): {err}")
    })
}

/// Opciones de `coroom login`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoginOptions {
    /// Comunes.
    pub account: AccountOptions,
    /// Código de invitación para crear la cuenta antes.
    pub invitation: Option<String>,
    /// Usuario (si no, se pregunta).
    pub user: Option<String>,
    /// Nombre visible de la cuenta nueva (con invitación).
    pub display_name: Option<String>,
    /// Contraseña por la entrada estándar.
    pub password_stdin: bool,
    /// Nombre del dispositivo (por defecto, el del equipo).
    pub device_name: Option<String>,
}

/// `coroom login`: crea la cuenta con la invitación si la hay, inicia sesión y guarda el token.
pub fn login(options: &LoginOptions) -> Result<String, String> {
    let account = &options.account;
    let server = account.store().server(account.server.as_deref())?;
    let (username, password, display_name) = if options.invitation.is_some() {
        eprintln!("Cuenta nueva en {server} con el código de invitación.");
        let username = match &options.user {
            Some(user) => user.clone(),
            None => prompt_line("Nombre de usuario nuevo (letras, dígitos, «.», «-», «_»): ")?,
        };
        let display_name = match &options.display_name {
            Some(name) => Some(name.clone()),
            None if options.password_stdin => None,
            None => Some(prompt_line("Nombre visible (Intro = el de usuario): ")?)
                .filter(|name| !name.trim().is_empty()),
        };
        let password = prompt_password(
            "Contraseña (al menos 12 caracteres): ",
            options.password_stdin,
        )?;
        if !options.password_stdin {
            let again = prompt_password("Repítela: ", false)?;
            if again != password {
                return Err("las contraseñas no coinciden".to_owned());
            }
        }
        (username, password, display_name)
    } else {
        let username = match &options.user {
            Some(user) => user.clone(),
            None => prompt_line("Usuario: ")?,
        };
        let password = prompt_password("Contraseña: ", options.password_stdin)?;
        (username, password, None)
    };
    let device = options.device_name.clone().unwrap_or_else(device_name);
    let invitation = options
        .invitation
        .as_deref()
        .map(|code| (code, display_name.as_deref()));
    let signed = sign_in(account, &username, &password, &device, invitation)?;
    if options.invitation.is_some() {
        eprintln!("Cuenta «{}» creada.", signed.credentials.username);
    }
    Ok(format!(
        "Sesión iniciada como {} ({}) en {server}.\n  Dispositivo «{device}», identidad {} en {}.\n  Credenciales en {}.",
        signed.credentials.username,
        signed.credentials.display_name,
        signed.fingerprint,
        signed.place,
        signed.saved
    ))
}

/// `coroom logout`: revoca el token en el servidor y borra las credenciales.
pub fn logout(options: &AccountOptions) -> Result<String, String> {
    let store = options.store();
    let server = store.server(options.server.as_deref())?;
    let revoked = match session(options) {
        Ok(session) => {
            let api = session.api()?;
            runtime()?.block_on(api.logout()).is_ok()
        }
        Err(_) => false,
    };
    store.delete(&server)?;
    Ok(if revoked {
        format!("Sesión cerrada en {server}.")
    } else {
        format!("Credenciales de {server} borradas (el servidor no confirmó el cierre).")
    })
}

fn human_bytes(bytes: u64) -> String {
    let value = bytes as f64;
    if value >= 1e9 {
        format!("{:.2} GB", value / 1e9)
    } else if value >= 1e6 {
        format!("{:.1} MB", value / 1e6)
    } else if value >= 1e3 {
        format!("{:.1} kB", value / 1e3)
    } else {
        format!("{bytes} B")
    }
}

/// `coroom whoami`: usuario, dispositivo y uso del relay.
pub fn whoami(options: &AccountOptions) -> Result<String, String> {
    let session = session(options)?;
    let api = session.api()?;
    let runtime = runtime()?;
    let me = runtime.block_on(api.me()).map_err(api_error)?;
    let usage = runtime.block_on(api.usage()).map_err(api_error)?;
    let mut out = format!(
        "{} ({}){} en {}",
        me.user.username,
        me.user.display_name,
        if me.user.is_admin {
            ", administrador"
        } else {
            ""
        },
        session.credentials.server
    );
    if let Some(device) = &me.device {
        out.push_str(&format!(
            "\n  Dispositivo «{}», huella {}",
            device.name, device.fingerprint
        ));
    }
    let quota = match usage.quota_bytes {
        Some(quota) => format!(" de {}", human_bytes(quota)),
        None => " (sin límite)".to_owned(),
    };
    out.push_str(&format!(
        "\n  Relay en {}: {}{quota}{}",
        usage.month,
        human_bytes(usage.bytes),
        if usage.relay_enabled {
            ""
        } else {
            " · el servidor no tiene relay"
        }
    ));
    Ok(out)
}

/// Qué hacer con `coroom friends`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FriendsAction {
    /// Lista.
    List,
    /// Pedir amistad.
    Add(String),
    /// Aceptar la solicitud de alguien.
    Accept(String),
    /// Quitar un amigo o retirar/rechazar una solicitud.
    Remove(String),
}

/// `coroom friends …`.
pub fn friends(options: &AccountOptions, action: &FriendsAction) -> Result<String, String> {
    let session = session(options)?;
    let api = session.api()?;
    let runtime = runtime()?;
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b.trim());
    match action {
        FriendsAction::List => {
            let list = runtime.block_on(api.friends()).map_err(api_error)?;
            let mut out = Vec::new();
            if list.friends.is_empty() {
                out.push("Todavía no tienes amigos: `coroom friends add NOMBRE`.".to_owned());
            } else {
                out.push("Amigos:".to_owned());
                for friend in &list.friends {
                    out.push(format!(
                        "  {} {} ({}){}",
                        if friend.online { "●" } else { "○" },
                        friend.username,
                        friend.display_name,
                        if friend.online { " · conectado" } else { "" }
                    ));
                }
            }
            if !list.incoming.is_empty() {
                out.push("Solicitudes recibidas (`coroom friends accept NOMBRE`):".to_owned());
                for request in &list.incoming {
                    out.push(format!(
                        "  {} ({})",
                        request.user.username, request.user.display_name
                    ));
                }
            }
            if !list.outgoing.is_empty() {
                out.push("Solicitudes enviadas, pendientes:".to_owned());
                for request in &list.outgoing {
                    out.push(format!("  {}", request.user.username));
                }
            }
            Ok(out.join("\n"))
        }
        FriendsAction::Add(name) => {
            let result = runtime.block_on(api.add_friend(name)).map_err(api_error)?;
            Ok(if result.accepted {
                format!("Ahora {} y tú sois amigos.", result.user.username)
            } else {
                format!(
                    "Solicitud enviada a {}; cuando la acepte seréis amigos.",
                    result.user.username
                )
            })
        }
        FriendsAction::Accept(name) => {
            let list = runtime.block_on(api.friends()).map_err(api_error)?;
            let request = list
                .incoming
                .iter()
                .find(|request| same(&request.user.username, name))
                .ok_or_else(|| format!("no tienes ninguna solicitud de «{name}»"))?;
            let result = runtime
                .block_on(api.accept_friend(request.id))
                .map_err(api_error)?;
            Ok(format!("Ahora {} y tú sois amigos.", result.user.username))
        }
        FriendsAction::Remove(name) => {
            let list = runtime.block_on(api.friends()).map_err(api_error)?;
            if let Some(friend) = list.friends.iter().find(|f| same(&f.username, name)) {
                runtime
                    .block_on(api.remove_friend(friend.id))
                    .map_err(api_error)?;
                return Ok(format!(
                    "{} ya no está en tu lista de amigos.",
                    friend.username
                ));
            }
            let request = list
                .incoming
                .iter()
                .chain(list.outgoing.iter())
                .find(|request| same(&request.user.username, name))
                .ok_or_else(|| format!("«{name}» no es tu amigo ni tiene solicitudes contigo"))?;
            runtime
                .block_on(api.decline_friend(request.id))
                .map_err(api_error)?;
            Ok(format!("Solicitud con {} retirada.", request.user.username))
        }
    }
}

// --- Interfaz: las mismas operaciones sin consola y con datos estructurados ---------

/// Lo que devuelve [`sign_in`].
#[derive(Debug, Clone)]
pub struct SignedIn {
    /// Credenciales guardadas.
    pub credentials: Credentials,
    /// Nombre del dispositivo.
    pub device: String,
    /// Huella de la identidad del equipo.
    pub fingerprint: String,
    /// Dónde está la identidad.
    pub place: String,
    /// Dónde se guardaron las credenciales.
    pub saved: String,
    /// Versión del avatar de la cuenta (`None`: no tiene).
    pub avatar: Option<u64>,
}

/// Inicia sesión sin preguntar nada (la interfaz ya pidió los datos): crea antes la cuenta si se
/// da `invitation` (`(código, nombre visible)`), vincula este equipo y guarda las credenciales.
pub fn sign_in(
    account: &AccountOptions,
    username: &str,
    password: &str,
    device: &str,
    invitation: Option<(&str, Option<&str>)>,
) -> Result<SignedIn, String> {
    let store = account.store();
    let server = store.server(account.server.as_deref())?;
    let trust = trust_for(account.trust_fingerprint.as_deref())?;
    let (identity, place) = load_identity(account.data_dir.as_deref(), account.headless)?;
    // Un solo runtime para todas las llamadas: el cliente reutiliza su conexión HTTP/3.
    let runtime = runtime()?;
    let mut api = ApiClient::new(&server, trust).map_err(|err| err.to_string())?;
    if let Some(port) = account.tcp_port {
        api = api.with_tcp_port(port);
    }
    if let Some((code, display_name)) = invitation {
        runtime
            .block_on(api.redeem_invitation(code.trim(), username.trim(), password, display_name))
            .map_err(api_error)?;
    }
    let token = runtime
        .block_on(api.login(username.trim(), password, &identity, device))
        .map_err(api_error)?;
    let avatar = token.user.avatar;
    let credentials = Credentials {
        server: server.clone(),
        username: token.user.username.clone(),
        display_name: token.user.display_name.clone(),
        user_id: token.user.id,
        token: token.token,
        expires_at: token.expires_at,
        trust_fingerprint: account
            .trust_fingerprint
            .as_deref()
            .map(normalize_fingerprint)
            .transpose()?,
        tcp_port: account.tcp_port,
    };
    let saved = store.save(&credentials)?;
    Ok(SignedIn {
        credentials,
        device: device.to_owned(),
        fingerprint: identity.fingerprint(),
        place,
        saved,
        avatar,
    })
}

/// La sesión guardada del servidor de `options`, comprobada con el servidor (`GET /me`): el nombre
/// del dispositivo y la huella de la identidad del equipo.
pub fn resume(options: &AccountOptions) -> Result<(Session, SignedIn), String> {
    let session = session(options)?;
    let (identity, place) = load_identity(options.data_dir.as_deref(), options.headless)?;
    let api = session.api()?;
    let me = runtime()?.block_on(api.me()).map_err(api_error)?;
    let signed = SignedIn {
        credentials: Credentials {
            username: me.user.username.clone(),
            display_name: me.user.display_name.clone(),
            ..session.credentials.clone()
        },
        device: me.device.map(|device| device.name).unwrap_or_default(),
        fingerprint: identity.fingerprint(),
        place,
        saved: String::new(),
        avatar: me.user.avatar,
    };
    Ok((session, signed))
}

/// Lista de amigos.
pub fn list_friends(session: &Session) -> Result<coroom_transport::proto::api::Friends, String> {
    let api = session.api()?;
    runtime()?.block_on(api.friends()).map_err(api_error)
}

/// Pide amistad a `name`; devuelve el texto para el usuario.
pub fn request_friend(session: &Session, name: &str) -> Result<String, String> {
    let api = session.api()?;
    let result = runtime()?
        .block_on(api.add_friend(name.trim()))
        .map_err(api_error)?;
    Ok(if result.accepted {
        format!("Ahora {} y tú sois amigos", result.user.display_name)
    } else {
        format!("Solicitud enviada a {}", result.user.display_name)
    })
}

/// Acepta la solicitud `request`.
pub fn accept_request(session: &Session, request: u64) -> Result<String, String> {
    let api = session.api()?;
    let result = runtime()?
        .block_on(api.accept_friend(request))
        .map_err(api_error)?;
    Ok(format!(
        "Ahora {} y tú sois amigos",
        result.user.display_name
    ))
}

/// Rechaza o retira la solicitud `request`.
pub fn decline_request(session: &Session, request: u64) -> Result<String, String> {
    let api = session.api()?;
    runtime()?
        .block_on(api.decline_friend(request))
        .map_err(api_error)?;
    Ok("Solicitud retirada".to_owned())
}

/// Quita al amigo `user`.
pub fn remove_friend(session: &Session, user: u64) -> Result<String, String> {
    let api = session.api()?;
    runtime()?
        .block_on(api.remove_friend(user))
        .map_err(api_error)?;
    Ok("Ya no sois amigos".to_owned())
}

/// Dispositivos de la cuenta.
pub fn devices(session: &Session) -> Result<coroom_transport::proto::api::Devices, String> {
    let api = session.api()?;
    runtime()?.block_on(api.devices()).map_err(api_error)
}

/// Desvincula el dispositivo `device`.
pub fn revoke_device(session: &Session, device: u64) -> Result<String, String> {
    let api = session.api()?;
    runtime()?
        .block_on(api.delete_device(device))
        .map_err(api_error)?;
    Ok("Sesión del dispositivo cerrada".to_owned())
}

/// Uso del relay este mes.
pub fn usage(session: &Session) -> Result<coroom_transport::proto::api::Usage, String> {
    let api = session.api()?;
    runtime()?.block_on(api.usage()).map_err(api_error)
}

/// Sube el avatar propio (el PNG ya preparado, `crate::avatar::prepare`); devuelve la versión que
/// le dio el servidor.
pub fn upload_avatar(session: &Session, png: Vec<u8>) -> Result<Option<u64>, String> {
    let api = session.api()?;
    let user = runtime()?
        .block_on(api.set_avatar(png))
        .map_err(avatar_change_error)?;
    Ok(user.avatar)
}

/// Quita el avatar propio.
pub fn remove_avatar(session: &Session) -> Result<(), String> {
    let api = session.api()?;
    runtime()?
        .block_on(api.delete_avatar())
        .map_err(avatar_change_error)?;
    Ok(())
}

/// Como [`api_error`], con el tope de cambios de avatar por minuto del servidor
/// (`too_many_requests`) explicado en la aplicación.
fn avatar_change_error(err: TransportError) -> String {
    match err {
        TransportError::Api { code, .. } if code == "too_many_requests" => {
            t!("avatar-demasiados-cambios").to_owned()
        }
        other => api_error(other),
    }
}

/// Descarga el avatar de cada usuario de `users` por la misma conexión y se lo da a `each`
/// (`Ok(None)`: no tiene).
pub fn download_avatars(
    session: &Session,
    users: &[u64],
    mut each: impl FnMut(u64, Result<Option<Vec<u8>>, String>),
) -> Result<(), String> {
    let api = session.api()?;
    let runtime = runtime()?;
    for user in users {
        each(
            *user,
            runtime.block_on(api.avatar(*user)).map_err(api_error),
        );
    }
    Ok(())
}

/// Comprueba un servidor sin credenciales (ver [`ApiClient::probe`]).
pub fn probe_server(
    url: &str,
    tcp_port: Option<u16>,
) -> Result<coroom_transport::api::ServerProbe, String> {
    runtime()?
        .block_on(ApiClient::probe(url, tcp_port))
        .map_err(api_error)
}

/// Busca el id de un amigo por su nombre de usuario.
pub fn friend_id(session: &Session, name: &str) -> Result<u64, String> {
    let api = session.api()?;
    let list = runtime()?.block_on(api.friends()).map_err(api_error)?;
    list.friends
        .iter()
        .find(|friend| friend.username.eq_ignore_ascii_case(name.trim()))
        .map(|friend| friend.id)
        .ok_or_else(|| format!("«{name}» no es tu amigo (`coroom friends add {name}`)"))
}

/// `coroom invite NOMBRE --code CÓDIGO`: invita a un amigo conectado a tu sala desde otra
/// sesión de señalización (la sala debe ser tuya).
pub fn invite(options: &AccountOptions, name: &str, code: &str) -> Result<String, String> {
    let session = session(options)?;
    let user = friend_id(&session, name)?;
    let (identity, _) = load_identity(options.data_dir.as_deref(), options.headless)?;
    let code = code.trim().to_ascii_uppercase();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .map_err(|err| format!("no se pudo arrancar la red: {err}"))?;
    runtime.block_on(async {
        let endpoint = Endpoint::bind(&identity, None, EndpointOptions::default())
            .map_err(|err| err.to_string())?;
        let mut signal = SignalClient::connect(
            &session.credentials.server,
            &session.credentials.token,
            &endpoint,
            &session.trust,
        )
        .await
        .map_err(|err| err.to_string())?;
        signal
            .send(&SignalMessage::Invite {
                to_user: user,
                code: code.clone(),
            })
            .await
            .map_err(|err| err.to_string())?;
        // El servidor solo responde si algo falla.
        let reply = loop {
            match tokio::time::timeout(Duration::from_secs(2), signal.next_event()).await {
                Ok(Ok(Some(SignalMessage::Error { code, message }))) => {
                    break Some((code, message));
                }
                Ok(Ok(Some(_))) => continue,
                _ => break None,
            }
        };
        signal.close();
        endpoint.close();
        match reply {
            Some((ErrorCode::UserOffline, message) | (ErrorCode::NotFriend, message)) => {
                Err(message)
            }
            Some((_, message)) => Err(message),
            None => Ok(format!("Invitación a la sala {code} enviada a {name}.")),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials(server: &str) -> Credentials {
        Credentials {
            server: server.to_owned(),
            username: "ana".to_owned(),
            display_name: "Ana".to_owned(),
            user_id: 2,
            token: "change-me".to_owned(),
            expires_at: 0,
            trust_fingerprint: Some("ab".repeat(32)),
            tcp_port: Some(8443),
        }
    }

    #[test]
    fn credentials_live_per_server_in_the_data_directory() {
        let dir = tempfile::tempdir().expect("directorio temporal");
        let store = AccountStore::new(Some(dir.path()), true);
        assert_eq!(store.last_server(), None);
        assert!(store.server(None).is_err());
        let saved = store
            .save(&credentials("https://coroom.example.com:4433"))
            .expect("guardar");
        assert!(saved.ends_with("coroom.example.com.json"), "{saved}");
        assert_eq!(
            store.last_server().as_deref(),
            Some("https://coroom.example.com:4433")
        );
        assert_eq!(
            store.server(None).as_deref(),
            Ok("https://coroom.example.com:4433")
        );
        let loaded = store
            .load("https://coroom.example.com")
            .expect("leer")
            .expect("guardadas");
        assert_eq!(loaded, credentials("https://coroom.example.com:4433"));
        assert_eq!(store.load("https://otro.example.com").expect("leer"), None);
        store.delete("https://coroom.example.com").expect("borrar");
        assert_eq!(
            store.load("https://coroom.example.com").expect("leer"),
            None
        );
    }

    #[test]
    fn sessions_need_a_login_first() {
        let dir = tempfile::tempdir().expect("directorio temporal");
        let options = AccountOptions {
            server: Some("https://coroom.example.com".to_owned()),
            data_dir: Some(dir.path().to_owned()),
            headless: true,
            ..AccountOptions::default()
        };
        let err = session(&options).expect_err("sin sesión");
        assert!(err.contains("coroom login --server"), "{err}");
        AccountStore::new(Some(dir.path()), true)
            .save(&credentials("https://coroom.example.com"))
            .expect("guardar");
        let session = session(&options).expect("sesión");
        assert!(matches!(session.trust, ServerTrust::Fingerprint(_)));
        assert_eq!(session.credentials.tcp_port, Some(8443));
    }

    #[test]
    fn fingerprints_and_sizes_are_formatted() {
        assert_eq!(
            normalize_fingerprint(&("AB:".repeat(31) + "AB")),
            Ok("ab".repeat(32))
        );
        assert!(normalize_fingerprint("1234").is_err());
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1_500_000), "1.5 MB");
        assert_eq!(human_bytes(2_000_000_000), "2.00 GB");
        assert!(!device_name().is_empty());
    }

    #[test]
    fn the_avatar_change_limit_is_explained_in_the_app() {
        // El 429 del tope de cambios por minuto: el texto del catálogo, no el del servidor.
        let limited = TransportError::Api {
            status: 429,
            code: "too_many_requests".to_owned(),
            message: "demasiadas peticiones seguidas".to_owned(),
        };
        let text = avatar_change_error(limited);
        assert_eq!(text, t!("avatar-demasiados-cambios"));
        assert!(text.contains("minuto"), "{text}");
        // Los demás errores de la API, con el texto del servidor.
        let invalid = TransportError::Api {
            status: 400,
            code: "invalid_avatar".to_owned(),
            message: "el avatar debe ser una imagen PNG".to_owned(),
        };
        assert_eq!(
            avatar_change_error(invalid),
            "el avatar debe ser una imagen PNG"
        );
    }
}
