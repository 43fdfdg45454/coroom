//! Cuentas: `coroom login`, `logout`, `whoami`, `friends` e `invite`, y las credenciales
//! de cada servidor.
//!
//! La lógica de cuentas que no depende de la plataforma (sesión de cada servidor, llamadas a la
//! API) está en [`coroom_session::account`] y se reexporta aquí; este módulo pone lo del
//! escritorio:
//!
//! - **Credenciales**: [`AccountStore`] implementa [`AccountStorage`]. El token de sesión de cada
//!   servidor (con el usuario, la huella del certificado si se fijó y el puerto TCP del respaldo)
//!   va en el llavero del sistema (`coroom` / `server:<host>`) o, sin llavero, sin ventana
//!   (`--headless`) o con `--data-dir`, en `servers/<host>.json` (`0600`) dentro del directorio de
//!   datos. El último servidor usado queda en `servers/last` (sin secretos) para no repetir
//!   `--server`. La identidad, donde la pone [`load_identity`].
//! - **Login**: pide usuario y contraseña por consola sin eco (o `--user` y `--password-stdin`),
//!   presenta la clave pública de la identidad del equipo y firma el desafío del servidor
//!   ([`ApiClient::login`]); con `--invitation CÓDIGO`, antes crea la cuenta con el código.
//! - Las órdenes de la línea de órdenes y su salida, y el nombre del equipo ([`device_name`]).
//!
//! [`ApiClient::login`]: coroom_transport::api::ApiClient::login

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use coroom_session::account as shared;
pub use coroom_session::account::{
    AccountStorage, AvatarChangeError, Credentials, ServerOptions, Session, SignedIn, Zeroizing,
    accept_request, api_error, decline_request, devices, download_avatars, friend_id, list_friends,
    normalize_fingerprint, probe_server, remove_friend, request_friend, revoke_device, runtime,
    server_host, trust_for, usage,
};
use coroom_transport::crypto::{CredentialStore, Identity, KeyringStore};

use crate::remote::{default_data_dir, load_identity};

/// Dónde están las credenciales de este usuario del equipo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountStore {
    data_dir: Option<PathBuf>,
    headless: bool,
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

    fn last_path(&self) -> PathBuf {
        self.base_dir().join("servers").join("last")
    }
}

impl AccountStorage for AccountStore {
    fn load_secret(&self, host: &str) -> Result<Option<Zeroizing<String>>, String> {
        self.store(host).load().map_err(|err| err.to_string())
    }

    fn save_secret(&self, host: &str, secret: &str) -> Result<String, String> {
        let store = self.store(host);
        store.save(secret).map_err(|err| err.to_string())?;
        Ok(store.describe())
    }

    fn delete_secret(&self, host: &str) -> Result<(), String> {
        self.store(host).delete().map_err(|err| err.to_string())
    }

    fn last_server(&self) -> Option<String> {
        std::fs::read_to_string(self.last_path())
            .ok()
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
    }

    fn set_last_server(&self, url: &str) {
        let path = self.last_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, format!("{url}\n"));
    }

    fn load_identity(&self) -> Result<(Identity, String), String> {
        load_identity(self.data_dir.as_deref(), self.headless)
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

    /// El servidor, la huella y el puerto TCP, sin lo del almacén.
    pub fn server_options(&self) -> ServerOptions {
        ServerOptions {
            server: self.server.clone(),
            trust_fingerprint: self.trust_fingerprint.clone(),
            tcp_port: self.tcp_port,
        }
    }
}

/// Carga la sesión guardada del servidor de `options`.
pub fn session(options: &AccountOptions) -> Result<Session, String> {
    shared::session(&options.store(), &options.server_options())
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
    shared::logout(&options.store(), &options.server_options())
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

/// Inicia sesión sin preguntar nada (la interfaz ya pidió los datos): crea antes la cuenta si se
/// da `invitation` (`(código, nombre visible)`), vincula este equipo y guarda las credenciales.
pub fn sign_in(
    account: &AccountOptions,
    username: &str,
    password: &str,
    device: &str,
    invitation: Option<(&str, Option<&str>)>,
) -> Result<SignedIn, String> {
    shared::sign_in(
        &account.store(),
        &account.server_options(),
        username,
        password,
        device,
        invitation,
    )
}

/// La sesión guardada del servidor de `options`, comprobada con el servidor (`GET /me`): el nombre
/// del dispositivo y la huella de la identidad del equipo.
pub fn resume(options: &AccountOptions) -> Result<(Session, SignedIn), String> {
    shared::resume(&options.store(), &options.server_options())
}

/// Sube el avatar propio (el PNG ya preparado, `crate::avatar::prepare`); devuelve la versión que
/// le dio el servidor.
pub fn upload_avatar(session: &Session, png: Vec<u8>) -> Result<Option<u64>, String> {
    shared::upload_avatar(session, png).map_err(avatar_change_text)
}

/// Quita el avatar propio.
pub fn remove_avatar(session: &Session) -> Result<(), String> {
    shared::remove_avatar(session).map_err(avatar_change_text)
}

/// El texto de un cambio de avatar fallido, con el tope de cambios por minuto del servidor
/// (`too_many_requests`) explicado en la aplicación.
fn avatar_change_text(err: AvatarChangeError) -> String {
    match err {
        AvatarChangeError::TooManyChanges => t!("avatar-demasiados-cambios").to_owned(),
        AvatarChangeError::Other(text) => text,
    }
}

/// `coroom invite NOMBRE --code CÓDIGO`: invita a un amigo conectado a tu sala desde otra
/// sesión de señalización (la sala debe ser tuya).
pub fn invite(options: &AccountOptions, name: &str, code: &str) -> Result<String, String> {
    shared::invite(&options.store(), &options.server_options(), name, code)
}

#[cfg(test)]
mod tests {
    use coroom_transport::{ServerTrust, TransportError};

    use super::*;

    /// Como lo explica la aplicación tras clasificarlo [`coroom_session::account`].
    fn avatar_change_error(err: TransportError) -> String {
        avatar_change_text(AvatarChangeError::from(err))
    }

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
