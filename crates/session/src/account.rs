//! Cuentas: la sesión de cada servidor y las llamadas a su API (iniciar sesión con contraseña o
//! con un código de invitación, recuperar la sesión guardada, amigos, dispositivos, invitar a
//! una sala, avatares y comprobar un servidor con la huella de su certificado), sin nada propio
//! de una plataforma: lo usan la aplicación de escritorio y la de Android.
//!
//! - **Almacén**: dónde se guardan las credenciales de cada servidor, el último servidor usado y
//!   la identidad del equipo lo decide la aplicación al implementar [`AccountStorage`] (el
//!   escritorio, en el llavero, un fichero sellado con el portal o el directorio de datos;
//!   Android, en un fichero cifrado con una clave de Android Keystore). Este módulo guarda ahí
//!   un JSON con las [`Credentials`] por host.
//! - Las llamadas a la API se hacen en un runtime de `tokio` del proceso ([`runtime`]) y con un
//!   cliente por sesión ([`Session::api`]) que reutiliza su conexión HTTP/3 de una petición a la
//!   siguiente: abrir Amigos o Cuenta no paga otra vez el handshake. El almacén se lee y escribe
//!   fuera del runtime (el llavero de Linux es bloqueante).
//! - Nada pregunta por consola: los datos (usuario, contraseña, nombre del dispositivo) los pide
//!   la interfaz de cada aplicación.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use coroom_transport::api::{ApiClient, ServerProbe};
use coroom_transport::crypto::Identity;
use coroom_transport::proto::api::{Devices, Friends, Usage};
use coroom_transport::proto::signal::{ErrorCode, SignalMessage};
use coroom_transport::signal::ServerUrl;
use coroom_transport::{Endpoint, EndpointOptions, ServerTrust, SignalClient, TransportError};

pub use zeroize::Zeroizing;

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

/// Dónde guarda la aplicación lo de las cuentas de este usuario del equipo: las credenciales de
/// cada servidor (un texto opaco por host, con el token), el último servidor usado y la
/// identidad del equipo.
///
/// La aplicación implementa los métodos de almacenamiento; los de [`Credentials`] ([`load`],
/// [`save`], [`delete`] y [`server`]) ya vienen hechos sobre ellos.
///
/// [`load`]: AccountStorage::load
/// [`save`]: AccountStorage::save
/// [`delete`]: AccountStorage::delete
/// [`server`]: AccountStorage::server
pub trait AccountStorage {
    /// El texto guardado para `host` (en minúsculas, sin puerto); `Ok(None)` si no hay nada.
    fn load_secret(&self, host: &str) -> Result<Option<Zeroizing<String>>, String>;

    /// Guarda (o sustituye) el texto de `host`; devuelve dónde quedó, para el usuario.
    fn save_secret(&self, host: &str, secret: &str) -> Result<String, String>;

    /// Borra el texto de `host` (no es un error que no hubiera nada).
    fn delete_secret(&self, host: &str) -> Result<(), String>;

    /// Último servidor en el que se inició sesión (sin secretos).
    fn last_server(&self) -> Option<String>;

    /// Apunta `url` como el último servidor (el que se recupera al abrir la aplicación); si no
    /// se puede guardar, no pasa nada.
    fn set_last_server(&self, url: &str);

    /// Carga (o crea la primera vez) la identidad del equipo; devuelve también dónde está.
    fn load_identity(&self) -> Result<(Identity, String), String>;

    /// Credenciales guardadas para el servidor `url`.
    fn load(&self, url: &str) -> Result<Option<Credentials>, String> {
        let host = server_host(url)?;
        let Some(text) = self
            .load_secret(&host)
            .map_err(|err| format!("no se pudieron leer las credenciales: {err}"))?
        else {
            return Ok(None);
        };
        serde_json::from_str(&text).map(Some).map_err(|err| {
            format!("las credenciales de {host} están dañadas ({err}); vuelve a iniciar sesión")
        })
    }

    /// Guarda las credenciales y apunta el servidor como el último; devuelve dónde.
    fn save(&self, credentials: &Credentials) -> Result<String, String> {
        let host = server_host(&credentials.server)?;
        let text = serde_json::to_string(credentials)
            .map_err(|err| format!("no se pudieron guardar las credenciales: {err}"))?;
        let place = self
            .save_secret(&host, &text)
            .map_err(|err| format!("no se pudieron guardar las credenciales: {err}"))?;
        self.set_last_server(&credentials.server);
        Ok(place)
    }

    /// Borra las credenciales del servidor `url`.
    fn delete(&self, url: &str) -> Result<(), String> {
        let host = server_host(url)?;
        self.delete_secret(&host)
            .map_err(|err| format!("no se pudieron borrar las credenciales: {err}"))
    }

    /// El servidor de `--server` o, si falta, el último usado.
    fn server(&self, server: Option<&str>) -> Result<String, String> {
        match server {
            Some(server) => Ok(server.trim().trim_end_matches('/').to_owned()),
            None => self.last_server().ok_or_else(|| {
                "falta --server URL (todavía no has iniciado sesión en ningún servidor)".to_owned()
            }),
        }
    }
}

/// Host de una URL de servidor (para nombrar su credencial).
pub fn server_host(url: &str) -> Result<String, String> {
    ServerUrl::parse(url)
        .map(|url| url.host.to_ascii_lowercase())
        .map_err(|err| err.to_string())
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

/// Runtime de las llamadas a la API, el mismo durante todo el proceso: la conexión HTTP/3 de un
/// cliente vive en el runtime donde se abrió, y así sigue abierta entre peticiones.
pub fn runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("coroom-api")
                .enable_all()
                .build()
                .map_err(|err| format!("no se pudo arrancar la red: {err}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Lo que distingue un cliente de la API de otro: servidor, token, huella fijada y puerto TCP.
type ClientKey = (String, String, Option<String>, Option<u16>);

/// El cliente de la API de cada servidor, con su conexión abierta ([`Session::api`]).
static CLIENTS: LazyLock<Mutex<HashMap<ClientKey, Arc<ApiClient>>>> = LazyLock::new(Mutex::default);

fn client_key(credentials: &Credentials, trust: &ServerTrust) -> ClientKey {
    let fingerprint = match trust {
        ServerTrust::Fingerprint(fingerprint) => Some(fingerprint.clone()),
        _ => None,
    };
    (
        credentials.server.clone(),
        credentials.token.clone(),
        fingerprint,
        credentials.tcp_port,
    )
}

/// Guarda `client` como el de `key` (el de su servidor deja de valer).
fn keep_client(key: ClientKey, client: ApiClient) -> Arc<ApiClient> {
    let client = Arc::new(client);
    if let Ok(mut clients) = CLIENTS.lock() {
        clients.retain(|cached, _| cached.0 != key.0);
        clients.insert(key, Arc::clone(&client));
    }
    client
}

/// Olvida el cliente de `server` (sesión cerrada: su token ya no vale).
fn forget_client(server: &str) {
    if let Ok(mut clients) = CLIENTS.lock() {
        clients.retain(|cached, _| cached.0 != server);
    }
}

/// Texto para el usuario de un error de la API: el mensaje del servidor si respondió y, si no,
/// por qué no se pudo hablar con él.
pub fn api_error(err: TransportError) -> String {
    match err {
        TransportError::Api { message, .. } => message,
        other => format!("no se pudo hablar con el servidor: {other}"),
    }
}

/// A qué servidor se habla y cómo confiar en él.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServerOptions {
    /// URL del servidor (si falta, el último).
    pub server: Option<String>,
    /// Huella del certificado (si falta, la guardada al iniciar sesión).
    pub trust_fingerprint: Option<String>,
    /// Puerto TCP del respaldo HTTP/2.
    pub tcp_port: Option<u16>,
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
    /// Cliente de la API con el token, el mismo para todas las peticiones de esta sesión (con su
    /// conexión ya abierta); se usa con el runtime de [`runtime`].
    pub fn api(&self) -> Result<Arc<ApiClient>, String> {
        let key = client_key(&self.credentials, &self.trust);
        if let Some(client) = CLIENTS.lock().ok().and_then(|c| c.get(&key).cloned()) {
            return Ok(client);
        }
        let mut client = ApiClient::new(&self.credentials.server, self.trust.clone())
            .map_err(|err| err.to_string())?
            .with_token(&self.credentials.token);
        if let Some(port) = self.credentials.tcp_port {
            client = client.with_tcp_port(port);
        }
        Ok(keep_client(key, client))
    }
}

/// Carga la sesión guardada del servidor de `options`.
pub fn session(storage: &dyn AccountStorage, options: &ServerOptions) -> Result<Session, String> {
    let server = storage.server(options.server.as_deref())?;
    let credentials = storage.load(&server)?.ok_or_else(|| {
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

/// Revoca el token en el servidor y borra las credenciales.
pub fn logout(storage: &dyn AccountStorage, options: &ServerOptions) -> Result<String, String> {
    let server = storage.server(options.server.as_deref())?;
    let revoked = match session(storage, options) {
        Ok(session) => {
            let api = session.api()?;
            runtime()?.block_on(api.logout()).is_ok()
        }
        Err(_) => false,
    };
    forget_client(&server);
    storage.delete(&server)?;
    Ok(if revoked {
        format!("Sesión cerrada en {server}.")
    } else {
        format!("Credenciales de {server} borradas (el servidor no confirmó el cierre).")
    })
}

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
/// da `invitation` (`(código, nombre visible)`), vincula este equipo con el nombre `device` y
/// guarda las credenciales.
pub fn sign_in(
    storage: &dyn AccountStorage,
    options: &ServerOptions,
    username: &str,
    password: &str,
    device: &str,
    invitation: Option<(&str, Option<&str>)>,
) -> Result<SignedIn, String> {
    let server = storage.server(options.server.as_deref())?;
    let trust = trust_for(options.trust_fingerprint.as_deref())?;
    let (identity, place) = storage.load_identity()?;
    // El cliente reutiliza su conexión HTTP/3 en todas las llamadas, y luego en las de la sesión.
    let runtime = runtime()?;
    let mut api = ApiClient::new(&server, trust.clone()).map_err(|err| err.to_string())?;
    if let Some(port) = options.tcp_port {
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
        trust_fingerprint: options
            .trust_fingerprint
            .as_deref()
            .map(normalize_fingerprint)
            .transpose()?,
        tcp_port: options.tcp_port,
    };
    keep_client(
        client_key(&credentials, &trust),
        api.with_token(&credentials.token),
    );
    let saved = storage.save(&credentials)?;
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
pub fn resume(
    storage: &dyn AccountStorage,
    options: &ServerOptions,
) -> Result<(Session, SignedIn), String> {
    let session = session(storage, options)?;
    let (identity, place) = storage.load_identity()?;
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
pub fn list_friends(session: &Session) -> Result<Friends, String> {
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
pub fn devices(session: &Session) -> Result<Devices, String> {
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
pub fn usage(session: &Session) -> Result<Usage, String> {
    let api = session.api()?;
    runtime()?.block_on(api.usage()).map_err(api_error)
}

/// Por qué no se pudo cambiar el avatar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AvatarChangeError {
    /// El tope de cambios de avatar por minuto del servidor (`too_many_requests`): cada
    /// aplicación lo explica con su propio texto.
    TooManyChanges,
    /// Cualquier otro fallo, con el texto de [`api_error`].
    Other(String),
}

impl From<TransportError> for AvatarChangeError {
    fn from(err: TransportError) -> Self {
        match err {
            TransportError::Api { code, .. } if code == "too_many_requests" => {
                AvatarChangeError::TooManyChanges
            }
            other => AvatarChangeError::Other(api_error(other)),
        }
    }
}

/// Sube el avatar propio (el PNG ya preparado por la aplicación); devuelve la versión que le dio
/// el servidor.
pub fn upload_avatar(session: &Session, png: Vec<u8>) -> Result<Option<u64>, AvatarChangeError> {
    let api = session.api().map_err(AvatarChangeError::Other)?;
    let user = runtime()
        .map_err(AvatarChangeError::Other)?
        .block_on(api.set_avatar(png))?;
    Ok(user.avatar)
}

/// Quita el avatar propio.
pub fn remove_avatar(session: &Session) -> Result<(), AvatarChangeError> {
    let api = session.api().map_err(AvatarChangeError::Other)?;
    runtime()
        .map_err(AvatarChangeError::Other)?
        .block_on(api.delete_avatar())?;
    Ok(())
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
pub fn probe_server(url: &str, tcp_port: Option<u16>) -> Result<ServerProbe, String> {
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

/// Invita a un amigo conectado a tu sala `code` desde otra sesión de señalización (la sala debe
/// ser tuya).
pub fn invite(
    storage: &dyn AccountStorage,
    options: &ServerOptions,
    name: &str,
    code: &str,
) -> Result<String, String> {
    let session = session(storage, options)?;
    let user = friend_id(&session, name)?;
    let (identity, _) = storage.load_identity()?;
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

    /// Almacén en memoria, como el que implementaría una aplicación.
    #[derive(Default)]
    struct MemoryStorage {
        secrets: Mutex<HashMap<String, String>>,
        last: Mutex<Option<String>>,
        identity: OnceLock<Vec<u8>>,
    }

    impl AccountStorage for MemoryStorage {
        fn load_secret(&self, host: &str) -> Result<Option<Zeroizing<String>>, String> {
            let secrets = self.secrets.lock().map_err(|err| err.to_string())?;
            Ok(secrets.get(host).cloned().map(Zeroizing::new))
        }

        fn save_secret(&self, host: &str, secret: &str) -> Result<String, String> {
            let mut secrets = self.secrets.lock().map_err(|err| err.to_string())?;
            secrets.insert(host.to_owned(), secret.to_owned());
            Ok(format!("memoria ({host})"))
        }

        fn delete_secret(&self, host: &str) -> Result<(), String> {
            let mut secrets = self.secrets.lock().map_err(|err| err.to_string())?;
            secrets.remove(host);
            Ok(())
        }

        fn last_server(&self) -> Option<String> {
            self.last.lock().ok().and_then(|last| last.clone())
        }

        fn set_last_server(&self, url: &str) {
            if let Ok(mut last) = self.last.lock() {
                *last = Some(url.to_owned());
            }
        }

        fn load_identity(&self) -> Result<(Identity, String), String> {
            let der = self.identity.get_or_init(|| {
                Identity::generate()
                    .and_then(|identity| identity.to_pkcs8_der())
                    .expect("identidad nueva")
            });
            let identity = Identity::from_pkcs8_der(der).map_err(|err| err.to_string())?;
            Ok((identity, "memoria".to_owned()))
        }
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
    fn credentials_live_per_host_in_the_app_storage() {
        let storage = MemoryStorage::default();
        assert_eq!(storage.last_server(), None);
        assert!(storage.server(None).is_err());
        let saved = storage
            .save(&credentials("https://Coroom.example.com:4433"))
            .expect("guardar");
        // Un texto por host, en minúsculas y sin el puerto.
        assert_eq!(saved, "memoria (coroom.example.com)");
        assert_eq!(
            storage.server(None).as_deref(),
            Ok("https://Coroom.example.com:4433")
        );
        assert_eq!(
            storage
                .server(Some(" https://otro.example.com/ "))
                .as_deref(),
            Ok("https://otro.example.com")
        );
        let loaded = storage
            .load("https://coroom.example.com")
            .expect("leer")
            .expect("guardadas");
        assert_eq!(loaded, credentials("https://Coroom.example.com:4433"));
        assert_eq!(
            storage.load("https://otro.example.com").expect("leer"),
            None
        );
        storage
            .delete("https://coroom.example.com")
            .expect("borrar");
        assert_eq!(
            storage.load("https://coroom.example.com").expect("leer"),
            None
        );
    }

    #[test]
    fn damaged_credentials_ask_to_sign_in_again() {
        let storage = MemoryStorage::default();
        storage
            .save_secret("coroom.example.com", "{no es json")
            .expect("guardar");
        let err = storage
            .load("https://coroom.example.com")
            .expect_err("dañadas");
        assert!(err.contains("vuelve a iniciar sesión"), "{err}");
    }

    #[test]
    fn sessions_need_a_login_first() {
        let storage = MemoryStorage::default();
        let options = ServerOptions {
            server: Some("https://coroom.example.com".to_owned()),
            ..ServerOptions::default()
        };
        let err = session(&storage, &options).expect_err("sin sesión");
        assert!(err.contains("coroom login --server"), "{err}");
        storage
            .save(&credentials("https://coroom.example.com"))
            .expect("guardar");
        let found = session(&storage, &options).expect("sesión");
        // La huella guardada al iniciar sesión.
        assert!(
            matches!(&found.trust, ServerTrust::Fingerprint(f) if *f == "ab".repeat(32)),
            "{:?}",
            found.trust
        );
        assert_eq!(found.credentials.tcp_port, Some(8443));
        // La huella y el puerto que se den mandan sobre los guardados.
        let options = ServerOptions {
            trust_fingerprint: Some("CD:".repeat(31) + "CD"),
            tcp_port: Some(9443),
            ..options
        };
        let found = session(&storage, &options).expect("sesión");
        assert!(
            matches!(&found.trust, ServerTrust::Fingerprint(f) if *f == "cd".repeat(32)),
            "{:?}",
            found.trust
        );
        assert_eq!(found.credentials.tcp_port, Some(9443));
    }

    #[test]
    fn the_identity_comes_from_the_app_storage() {
        let storage = MemoryStorage::default();
        let (first, place) = storage.load_identity().expect("identidad");
        let (again, _) = storage.load_identity().expect("identidad");
        assert_eq!(place, "memoria");
        assert_eq!(first.fingerprint(), again.fingerprint());
    }

    #[test]
    fn fingerprints_are_normalized() {
        assert_eq!(
            normalize_fingerprint(&("AB:".repeat(31) + "AB")),
            Ok("ab".repeat(32))
        );
        assert!(normalize_fingerprint("1234").is_err());
        assert!(trust_for(Some("1234")).is_err());
        assert_eq!(
            server_host("https://Coroom.Example.com:4433").as_deref(),
            Ok("coroom.example.com")
        );
    }

    #[test]
    fn the_avatar_change_limit_is_told_apart() {
        // El 429 del tope de cambios por minuto: la aplicación pone su texto.
        let limited = TransportError::Api {
            status: 429,
            code: "too_many_requests".to_owned(),
            message: "demasiadas peticiones seguidas".to_owned(),
        };
        assert_eq!(
            AvatarChangeError::from(limited),
            AvatarChangeError::TooManyChanges
        );
        // Los demás errores de la API, con el texto del servidor.
        let invalid = TransportError::Api {
            status: 400,
            code: "invalid_avatar".to_owned(),
            message: "el avatar debe ser una imagen PNG".to_owned(),
        };
        assert_eq!(
            AvatarChangeError::from(invalid),
            AvatarChangeError::Other("el avatar debe ser una imagen PNG".to_owned())
        );
    }
}
