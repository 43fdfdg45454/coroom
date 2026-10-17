//! Cliente de la API de cuentas del servidor ([`coroom_proto::api`]).
//!
//! [`ApiClient`] habla **HTTP/3** con `h3` sobre un `quinn::Endpoint` cliente propio (la API no
//! necesita salir del socket de los medios) y, si el UDP no llega en [`H3_CONNECT_TIMEOUT`], usa
//! el respaldo **HTTP/2 sobre TLS por TCP** (`hyper` + `tokio-rustls`; HTTP/1.1 si el servidor no
//! negocia `h2`) en el mismo puerto o en el de [`ApiClient::with_tcp_port`]. La confianza en el
//! servidor es la misma que la de la señalización ([`ServerTrust`]).
//!
//! [`ApiClient::login`] hace el login con desafío de dispositivo: envía la clave pública de la
//! [`Identity`], firma `server_host || challenge` y recibe el token de sesión. Antes de firmar
//! comprueba que `server_host` es el host de la URL: así un servidor no puede hacer firmar a un
//! cliente el desafío de otro.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use bytes::{Buf, Bytes};
use http::{HeaderValue, Method, Request, StatusCode, header};
use http_body_util::{BodyExt, Full};
use hyper_util::rt::{TokioExecutor, TokioIo};
use quinn::crypto::rustls::QuicClientConfig;
use rustls::pki_types::ServerName;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex;
use tracing::debug;

use coroom_crypto::Identity;
use coroom_proto::api::{
    API_PREFIX, AVATAR_CONTENT_TYPE, AddFriend, AddFriendResult, ApiError, CreateInvitation,
    CreateUser, CreatedInvitation, Devices, Friends, LoginChallenge, LoginComplete, LoginRequest,
    Me, RedeemRequest, SessionToken, SetupRequest, SetupStatus, Usage, User, avatar_path,
    challenge_message,
};
use coroom_proto::{Alpn, hex};

use crate::TransportError;
use crate::eyeballs::first_to_connect;
use crate::quic::canonical;
use crate::signal::ServerUrl;
use crate::tls::{self, ServerTrust};

/// Espera máxima del handshake QUIC antes de pasar al respaldo por TCP.
pub const H3_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Espera máxima de una petición por HTTP/3 antes de pasar al respaldo por TCP.
pub const H3_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// Espera máxima de una petición por TCP.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Tamaño máximo de una respuesta.
const MAX_RESPONSE: usize = 1024 * 1024;

/// Tipo de contenido de los cuerpos JSON.
const JSON: &str = "application/json";

/// Cuerpo de una petición con su tipo de contenido.
type Payload = Option<(Vec<u8>, &'static str)>;

type H3Send = h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>;

struct H3Conn {
    endpoint: quinn::Endpoint,
    connection: quinn::Connection,
    send: H3Send,
    driver: tokio::task::JoinHandle<()>,
}

impl Drop for H3Conn {
    fn drop(&mut self) {
        self.connection.close(quinn::VarInt::from_u32(0), b"fin");
        self.driver.abort();
        self.endpoint.close(quinn::VarInt::from_u32(0), b"fin");
    }
}

/// Cliente de la API `/api/v1` de un servidor de coroom.
pub struct ApiClient {
    url: ServerUrl,
    trust: ServerTrust,
    tcp_port: u16,
    token: Option<String>,
    /// Cabeceras de más en cada petición ([`ApiClient::with_header`]).
    headers: Vec<(header::HeaderName, HeaderValue)>,
    h3: Mutex<Option<H3Conn>>,
    /// Conexiones HTTP/3 abiertas ([`ApiClient::h3_connections`]).
    h3_opened: AtomicU32,
    tcp_only: AtomicBool,
}

impl std::fmt::Debug for ApiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiClient")
            .field("server", &self.url.authority)
            .field("tcp_port", &self.tcp_port)
            .field("authenticated", &self.token.is_some())
            .finish_non_exhaustive()
    }
}

/// Lo que dice [`ApiClient::probe`] de un servidor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerProbe {
    /// Nombre del programa (`coroom-server`).
    pub name: String,
    /// Su versión.
    pub version: String,
    /// Versión del protocolo.
    pub protocol: u16,
    /// Si respondió por HTTP/3 (UDP) desde este equipo; si no, por TCP.
    pub http3: bool,
    /// Lo que el servidor dice de su autocomprobación de UDP (`None` si no lo sabe).
    pub udp_reachable: Option<bool>,
    /// Huella SHA-256 del certificado que presentó.
    pub fingerprint: String,
    /// `None` si las raíces del sistema validan el certificado; si no, por qué (autofirmado).
    pub untrusted: Option<String>,
}

/// Respuesta en bruto.
#[derive(Debug, Clone)]
pub struct RawResponse {
    /// Estado HTTP.
    pub status: StatusCode,
    /// Cuerpo.
    pub body: Bytes,
    /// Si llegó por HTTP/3 (`false`: por TCP).
    pub http3: bool,
}

fn protocol(what: impl std::fmt::Display) -> TransportError {
    TransportError::Protocol(what.to_string())
}

impl ApiClient {
    /// Cliente para `server_url` (`https://host[:puerto]`) sin sesión.
    pub fn new(server_url: &str, trust: ServerTrust) -> Result<Self, TransportError> {
        let url = ServerUrl::parse(server_url)?;
        Ok(Self {
            tcp_port: url.port,
            url,
            trust,
            token: None,
            headers: Vec::new(),
            h3: Mutex::new(None),
            h3_opened: AtomicU32::new(0),
            tcp_only: AtomicBool::new(false),
        })
    }

    /// Con la cabecera `name: value` en cada petición (`User-Agent` y `Accept` para la
    /// API de publicaciones de GitHub en el aviso de versión nueva).
    pub fn with_header(mut self, name: &'static str, value: &str) -> Result<Self, TransportError> {
        let invalid = || protocol(format!("cabecera {name} no válida"));
        let key = header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| invalid())?;
        let value = HeaderValue::from_str(value).map_err(|_| invalid())?;
        self.headers.push((key, value));
        Ok(self)
    }

    /// `GET` de una ruta completa (sin `/api/v1`), con las cabeceras de [`ApiClient::with_header`]:
    /// para servicios que no son un servidor de coroom.
    pub async fn get(&self, path: &str) -> Result<RawResponse, TransportError> {
        self.send(Method::GET, path, None).await
    }

    /// Con el token de sesión `token` en cada petición.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Puerto TCP del respaldo HTTP/2 (por defecto, el de la URL).
    pub fn with_tcp_port(mut self, port: u16) -> Self {
        self.tcp_port = port;
        self
    }

    /// Solo por TCP (sin intentar HTTP/3).
    pub fn tcp_only(self) -> Self {
        self.tcp_only.store(true, Ordering::Relaxed);
        self
    }

    /// Host de la URL.
    pub fn host(&self) -> &str {
        &self.url.host
    }

    /// Token de sesión, si hay.
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Cuántas conexiones HTTP/3 ha abierto este cliente: una mientras siga viva, por muchas
    /// peticiones que haga.
    pub fn h3_connections(&self) -> u32 {
        self.h3_opened.load(Ordering::Relaxed)
    }

    async fn resolve(&self, port: u16) -> Result<Vec<SocketAddr>, TransportError> {
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host((self.url.host.as_str(), port))
            .await?
            .map(canonical)
            .collect();
        if addrs.is_empty() {
            return Err(TransportError::InvalidUrl(format!(
                "{}: no resuelve",
                self.url.host
            )));
        }
        Ok(addrs)
    }

    async fn connect_h3(&self) -> Result<H3Conn, TransportError> {
        let crypto = Arc::new(
            QuicClientConfig::try_from(self.client_config(&[Alpn::H3.as_bytes()])?)
                .map_err(|err| TransportError::Tls(err.to_string()))?,
        );
        let addrs = self.resolve(self.url.port).await?;
        let host = self.url.host.as_str();
        // Todas las direcciones a la vez, escalonadas: una que no contesta no retiene a la otra.
        let (endpoint, connection) = first_to_connect(host, addrs, |addr| {
            let crypto = Arc::clone(&crypto);
            Box::pin(async move {
                let local: SocketAddr = if addr.is_ipv6() {
                    "[::]:0".parse().map_err(protocol)?
                } else {
                    "0.0.0.0:0".parse().map_err(protocol)?
                };
                let endpoint = quinn::Endpoint::client(local)?;
                let config =
                    quinn::ClientConfig::new(crypto as Arc<dyn quinn::crypto::ClientConfig>);
                let connecting = endpoint.connect_with(config, addr, host)?;
                match tokio::time::timeout(H3_CONNECT_TIMEOUT, connecting).await {
                    Ok(Ok(connection)) => Ok((endpoint, connection)),
                    Ok(Err(err)) => Err(err.into()),
                    Err(_) => Err(TransportError::Timeout("handshake HTTP/3 con el servidor")),
                }
            })
        })
        .await?;
        let (mut driver, send) = h3::client::new(h3_quinn::Connection::new(connection.clone()))
            .await
            .map_err(|err| protocol(format!("HTTP/3: {err}")))?;
        let driver = tokio::spawn(async move {
            let _ = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
        });
        Ok(H3Conn {
            endpoint,
            connection,
            send,
            driver,
        })
    }

    /// Configuración TLS según la confianza (la de inspección solo la usa [`ApiClient::probe`]).
    fn client_config(&self, alpns: &[&[u8]]) -> Result<rustls::ClientConfig, TransportError> {
        match &self.trust {
            ServerTrust::Inspect(inspection) => tls::inspecting_client_config(inspection, alpns),
            trust => tls::server_client_config_with(trust, alpns),
        }
    }

    /// `target` es la ruta completa (con `/api/v1` si es de la API).
    fn uri(&self, target: &str) -> String {
        format!("https://{}{target}", self.url.authority)
    }

    fn request(
        &self,
        method: &Method,
        uri: String,
        content_type: Option<&'static str>,
    ) -> Result<http::request::Builder, TransportError> {
        let mut builder = Request::builder().method(method.clone()).uri(uri);
        for (name, value) in &self.headers {
            builder = builder.header(name, value);
        }
        if let Some(content_type) = content_type {
            builder = builder.header(header::CONTENT_TYPE, content_type);
        }
        if let Some(token) = &self.token {
            if matches!(self.trust, ServerTrust::Inspect(_)) {
                return Err(TransportError::Tls(
                    "no se envían credenciales a un servidor sin confianza".to_owned(),
                ));
            }
            let value = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| protocol("token de sesión no válido"))?;
            builder = builder.header(header::AUTHORIZATION, value);
        }
        Ok(builder)
    }

    async fn call_h3(
        &self,
        method: &Method,
        path: &str,
        body: &Payload,
    ) -> Result<RawResponse, TransportError> {
        let mut guard = self.h3.lock().await;
        let reuse = guard
            .as_ref()
            .is_some_and(|conn| conn.connection.close_reason().is_none());
        if !reuse {
            *guard = Some(self.connect_h3().await?);
            self.h3_opened.fetch_add(1, Ordering::Relaxed);
        }
        let Some(conn) = guard.as_ref() else {
            return Err(TransportError::Closed);
        };
        let mut send = conn.send.clone();
        drop(guard);
        let request = self
            .request(method, self.uri(path), body.as_ref().map(|(_, kind)| *kind))?
            .body(())
            .map_err(protocol)?;
        let mut stream = send
            .send_request(request)
            .await
            .map_err(|err| protocol(format!("HTTP/3: {err}")))?;
        if let Some((body, _)) = body {
            stream
                .send_data(Bytes::from(body.clone()))
                .await
                .map_err(|err| protocol(format!("HTTP/3: {err}")))?;
        }
        stream
            .finish()
            .await
            .map_err(|err| protocol(format!("HTTP/3: {err}")))?;
        let response = stream
            .recv_response()
            .await
            .map_err(|err| protocol(format!("HTTP/3: {err}")))?;
        let mut out = Vec::new();
        while let Some(mut chunk) = stream
            .recv_data()
            .await
            .map_err(|err| protocol(format!("HTTP/3: {err}")))?
        {
            while chunk.has_remaining() {
                let part = chunk.chunk();
                if out.len() + part.len() > MAX_RESPONSE {
                    return Err(protocol("respuesta demasiado grande"));
                }
                out.extend_from_slice(part);
                let len = part.len();
                chunk.advance(len);
            }
        }
        Ok(RawResponse {
            status: response.status(),
            body: Bytes::from(out),
            http3: true,
        })
    }

    async fn call_tcp(
        &self,
        method: &Method,
        path: &str,
        body: &Payload,
    ) -> Result<RawResponse, TransportError> {
        let config = self.client_config(&[b"h2", b"http/1.1"])?;
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let name = ServerName::try_from(self.url.host.clone()).map_err(|_| {
            TransportError::InvalidUrl(format!("{}: nombre no válido", self.url.host))
        })?;
        let addrs = self.resolve(self.tcp_port).await?;
        let tcp = first_to_connect(&self.url.host, addrs, |addr| {
            Box::pin(async move { Ok(tokio::net::TcpStream::connect(addr).await?) })
        })
        .await?;
        let tls = connector.connect(name, tcp).await?;
        let h2 = tls.get_ref().1.alpn_protocol() == Some(b"h2");
        let io = TokioIo::new(tls);
        let content_type = body.as_ref().map(|(_, kind)| *kind);
        let payload = Full::new(Bytes::from(
            body.as_ref()
                .map(|(bytes, _)| bytes.clone())
                .unwrap_or_default(),
        ));
        let response = if h2 {
            let (mut sender, connection) =
                hyper::client::conn::http2::handshake(TokioExecutor::new(), io)
                    .await
                    .map_err(|err| protocol(format!("HTTP/2: {err}")))?;
            tokio::spawn(connection);
            let request = self
                .request(method, self.uri(path), content_type)?
                .body(payload)
                .map_err(protocol)?;
            sender
                .send_request(request)
                .await
                .map_err(|err| protocol(format!("HTTP/2: {err}")))?
        } else {
            let (mut sender, connection) = hyper::client::conn::http1::handshake(io)
                .await
                .map_err(|err| protocol(format!("HTTP/1.1: {err}")))?;
            tokio::spawn(connection);
            let request = self
                .request(method, path.to_owned(), content_type)?
                .header(header::HOST, &self.url.authority)
                .body(payload)
                .map_err(protocol)?;
            sender
                .send_request(request)
                .await
                .map_err(|err| protocol(format!("HTTP/1.1: {err}")))?
        };
        let status = response.status();
        let bytes = http_body_util::Limited::new(response.into_body(), MAX_RESPONSE)
            .collect()
            .await
            .map_err(|err| protocol(format!("cuerpo de la respuesta: {err}")))?
            .to_bytes();
        Ok(RawResponse {
            status,
            body: bytes,
            http3: false,
        })
    }

    /// Petición en bruto a `/api/v1{path}` (HTTP/3 y, si falla o no responde en
    /// [`H3_REQUEST_TIMEOUT`], por TCP; desde entonces, siempre por TCP).
    ///
    /// La conexión HTTP/3 se reutiliza entre peticiones: un `ApiClient` debe usarse siempre desde
    /// el mismo runtime de `tokio`.
    pub async fn raw(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<RawResponse, TransportError> {
        self.send(
            method,
            &format!("{API_PREFIX}{path}"),
            body.map(|body| (body, JSON)),
        )
        .await
    }

    /// Como [`ApiClient::raw`], con la ruta completa (`/version`, `/health`).
    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Payload,
    ) -> Result<RawResponse, TransportError> {
        if !self.tcp_only.load(Ordering::Relaxed) {
            match tokio::time::timeout(H3_REQUEST_TIMEOUT, self.call_h3(&method, path, &body)).await
            {
                Ok(Ok(response)) => return Ok(response),
                Ok(Err(err)) => {
                    debug!(error = %err, "HTTP/3 no disponible; se usa el respaldo por TCP");
                }
                Err(_) => debug!("HTTP/3 sin respuesta; se usa el respaldo por TCP"),
            }
            *self.h3.lock().await = None;
            self.tcp_only.store(true, Ordering::Relaxed);
        }
        tokio::time::timeout(REQUEST_TIMEOUT, self.call_tcp(&method, path, &body))
            .await
            .map_err(|_| TransportError::Timeout("respuesta de la API del servidor"))?
    }

    /// Petición JSON: serializa `body`, comprueba el estado y deserializa la respuesta.
    pub async fn json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&(impl Serialize + ?Sized)>,
    ) -> Result<T, TransportError> {
        let body = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|err| protocol(format!("JSON: {err}")))?;
        let response = success(self.raw(method, path, body).await?)?;
        let body: &[u8] = if response.body.is_empty() {
            b"null"
        } else {
            &response.body
        };
        serde_json::from_slice(body)
            .map_err(|err| protocol(format!("respuesta JSON no válida: {err}")))
    }

    /// Comprueba un servidor antes de iniciar sesión ("Añadir servidor"): lee `/version`
    /// y `/health` sin credenciales con [`ServerTrust::inspect`] y dice qué certificado presenta
    /// (y si lo validan las raíces del sistema), si respondió por HTTP/3 (UDP) desde aquí y si el
    /// propio servidor se ve por UDP (`udp_reachable`). `tcp_port`: el del respaldo si no es el de
    /// la URL.
    pub async fn probe(
        server_url: &str,
        tcp_port: Option<u16>,
    ) -> Result<ServerProbe, TransportError> {
        let (trust, inspection) = ServerTrust::inspect();
        let mut client = ApiClient::new(server_url, trust)?;
        if let Some(port) = tcp_port {
            client = client.with_tcp_port(port);
        }
        let get = |path: &'static str| {
            let client = &client;
            async move {
                let response = client.send(Method::GET, path, None).await?;
                if !response.status.is_success() {
                    return Err(protocol(format!(
                        "{path}: el servidor respondió {}",
                        response.status
                    )));
                }
                let value: serde_json::Value = serde_json::from_slice(&response.body)
                    .map_err(|err| protocol(format!("{path}: JSON no válido: {err}")))?;
                Ok::<_, TransportError>((value, response.http3))
            }
        };
        let (version, http3) = get("/version").await?;
        let health = get("/health").await.ok().map(|(health, _)| health);
        let certificate = inspection
            .certificate()
            .ok_or_else(|| protocol("el servidor no presentó certificado"))?;
        let text = |value: &serde_json::Value, key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        Ok(ServerProbe {
            name: text(&version, "name"),
            version: text(&version, "version"),
            protocol: version
                .get("protocol")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u16::try_from(value).ok())
                .unwrap_or_default(),
            http3,
            udp_reachable: health
                .as_ref()
                .and_then(|health| health.get("udp_reachable"))
                .and_then(serde_json::Value::as_bool),
            fingerprint: certificate.fingerprint,
            untrusted: certificate.untrusted,
        })
    }

    /// `GET /setup`.
    pub async fn setup_status(&self) -> Result<SetupStatus, TransportError> {
        self.json(Method::GET, "/setup", None::<&()>).await
    }

    /// `POST /setup`: crea el primer administrador.
    pub async fn setup(&self, username: &str, password: &str) -> Result<User, TransportError> {
        let request = SetupRequest {
            username: username.to_owned(),
            password: password.to_owned(),
            display_name: None,
        };
        self.json(Method::POST, "/setup", Some(&request)).await
    }

    /// Login con desafío de dispositivo: vincula la identidad a la cuenta y devuelve el token.
    pub async fn login(
        &self,
        username: &str,
        password: &str,
        identity: &Identity,
        device_name: &str,
    ) -> Result<SessionToken, TransportError> {
        let request = LoginRequest {
            username: username.to_owned(),
            password: password.to_owned(),
            public_key: Some(hex::encode(&identity.public_key())),
            device_name: Some(device_name.to_owned()),
        };
        let challenge: LoginChallenge = self
            .json(Method::POST, "/auth/login", Some(&request))
            .await?;
        if !challenge.server_host.eq_ignore_ascii_case(&self.url.host) {
            return Err(protocol(format!(
                "el servidor se presenta como «{}» pero la URL es de «{}»; usa la URL con su nombre \
                 público (COROOM_PUBLIC_HOST)",
                challenge.server_host, self.url.host
            )));
        }
        let bytes = hex::decode(&challenge.challenge)
            .filter(|bytes| bytes.len() == 32)
            .ok_or_else(|| protocol("desafío de dispositivo no válido"))?;
        let signature = identity.sign(&challenge_message(&challenge.server_host, &bytes));
        let complete = LoginComplete {
            challenge_id: challenge.challenge_id,
            signature: hex::encode(&signature),
        };
        self.json(Method::POST, "/auth/login/complete", Some(&complete))
            .await
    }

    /// `POST /auth/logout`: revoca el token.
    pub async fn logout(&self) -> Result<(), TransportError> {
        self.json::<serde_json::Value>(Method::POST, "/auth/logout", None::<&()>)
            .await
            .map(|_| ())
    }

    /// `GET /me`.
    pub async fn me(&self) -> Result<Me, TransportError> {
        self.json(Method::GET, "/me", None::<&()>).await
    }

    /// `GET /me/devices`: dispositivos de la cuenta.
    pub async fn devices(&self) -> Result<Devices, TransportError> {
        self.json(Method::GET, "/me/devices", None::<&()>).await
    }

    /// `DELETE /me/devices/{id}`: desvincula un dispositivo (cierra su sesión).
    pub async fn delete_device(&self, device_id: u64) -> Result<(), TransportError> {
        let _: serde_json::Value = self
            .json(
                Method::DELETE,
                &format!("/me/devices/{device_id}"),
                None::<&()>,
            )
            .await?;
        Ok(())
    }

    /// `POST /invitations/redeem`: crea la cuenta con un código de invitación.
    pub async fn redeem_invitation(
        &self,
        code: &str,
        username: &str,
        password: &str,
        display_name: Option<&str>,
    ) -> Result<User, TransportError> {
        let request = RedeemRequest {
            code: code.to_owned(),
            username: username.to_owned(),
            password: password.to_owned(),
            display_name: display_name.map(str::to_owned),
        };
        self.json(Method::POST, "/invitations/redeem", Some(&request))
            .await
    }

    /// `GET /friends`.
    pub async fn friends(&self) -> Result<Friends, TransportError> {
        self.json(Method::GET, "/friends", None::<&()>).await
    }

    /// `POST /friends/requests`: pide amistad (o la acepta si el otro ya la pidió).
    pub async fn add_friend(&self, username: &str) -> Result<AddFriendResult, TransportError> {
        let request = AddFriend {
            username: username.to_owned(),
        };
        self.json(Method::POST, "/friends/requests", Some(&request))
            .await
    }

    /// `POST /friends/requests/{id}/accept`.
    pub async fn accept_friend(&self, request_id: u64) -> Result<AddFriendResult, TransportError> {
        self.json(
            Method::POST,
            &format!("/friends/requests/{request_id}/accept"),
            None::<&()>,
        )
        .await
    }

    /// `DELETE /friends/requests/{id}`: rechaza o retira una solicitud.
    pub async fn decline_friend(&self, request_id: u64) -> Result<(), TransportError> {
        self.json::<serde_json::Value>(
            Method::DELETE,
            &format!("/friends/requests/{request_id}"),
            None::<&()>,
        )
        .await
        .map(|_| ())
    }

    /// `DELETE /friends/{id}`.
    pub async fn remove_friend(&self, user_id: u64) -> Result<(), TransportError> {
        self.json::<serde_json::Value>(Method::DELETE, &format!("/friends/{user_id}"), None::<&()>)
            .await
            .map(|_| ())
    }

    /// `GET /usage`.
    pub async fn usage(&self) -> Result<Usage, TransportError> {
        self.json(Method::GET, "/usage", None::<&()>).await
    }

    /// `PUT /me/avatar`: sube el avatar propio (un PNG cuadrado, ya reducido); el servidor lo
    /// valida y lo vuelve a codificar. Devuelve el usuario con la versión nueva.
    pub async fn set_avatar(&self, png: Vec<u8>) -> Result<User, TransportError> {
        let response = self
            .send(
                Method::PUT,
                &format!("{API_PREFIX}/me/avatar"),
                Some((png, AVATAR_CONTENT_TYPE)),
            )
            .await?;
        serde_json::from_slice(&success(response)?.body)
            .map_err(|err| protocol(format!("respuesta JSON no válida: {err}")))
    }

    /// `DELETE /me/avatar`: quita el avatar propio.
    pub async fn delete_avatar(&self) -> Result<User, TransportError> {
        self.json(Method::DELETE, "/me/avatar", None::<&()>).await
    }

    /// `GET /users/{id}/avatar`: el PNG del avatar de un usuario (`None` si no tiene).
    pub async fn avatar(&self, user_id: u64) -> Result<Option<Vec<u8>>, TransportError> {
        let response = self.raw(Method::GET, &avatar_path(user_id), None).await?;
        match success(response) {
            Ok(response) => Ok(Some(response.body.to_vec())),
            Err(TransportError::Api { status: 404, .. }) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Administrador: `POST /admin/users`.
    pub async fn create_user(&self, user: &CreateUser) -> Result<User, TransportError> {
        self.json(Method::POST, "/admin/users", Some(user)).await
    }

    /// Administrador: `POST /admin/invitations`.
    pub async fn create_invitation(
        &self,
        invitation: &CreateInvitation,
    ) -> Result<CreatedInvitation, TransportError> {
        self.json(Method::POST, "/admin/invitations", Some(invitation))
            .await
    }

    /// Administrador: inicio de sesión de la interfaz web (sin dispositivo).
    pub async fn admin_login(
        &self,
        username: &str,
        password: &str,
    ) -> Result<SessionToken, TransportError> {
        let request = LoginRequest {
            username: username.to_owned(),
            password: password.to_owned(),
            public_key: None,
            device_name: None,
        };
        self.json(Method::POST, "/auth/login", Some(&request)).await
    }
}

/// La respuesta si su estado es de éxito; si no, el error de la API que trae.
fn success(response: RawResponse) -> Result<RawResponse, TransportError> {
    if response.status.is_success() {
        return Ok(response);
    }
    let error: ApiError = serde_json::from_slice(&response.body).unwrap_or(ApiError {
        error: "http".to_owned(),
        message: format!("el servidor respondió {}", response.status),
    });
    Err(TransportError::Api {
        status: response.status.as_u16(),
        code: error.error,
        message: error.message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hallazgo C-2 de `docs/seguridad.md`: lo que firma el dispositivo en el login cae solo en el
    /// dominio del desafío, para cualquier nombre de servidor válido.
    #[test]
    fn the_device_challenge_is_its_own_signature_domain() {
        use coroom_crypto::domains::{DEVICE_CHALLENGE_LENGTH, MAX_HOST_LENGTH, SIGNATURE_DOMAINS};
        let long = "a".repeat(MAX_HOST_LENGTH);
        for host in [
            "coroom.example.com",
            "localhost",
            "127.0.0.1",
            "[::1]",
            "d",
            "dz",
            long.as_str(),
        ] {
            let message = challenge_message(host, &[0x5a; DEVICE_CHALLENGE_LENGTH]);
            let domains: Vec<&str> = SIGNATURE_DOMAINS
                .iter()
                .filter(|domain| domain.admits(&message))
                .map(|domain| domain.name)
                .collect();
            assert_eq!(domains, vec!["desafío de dispositivo"], "{host}");
        }
    }

    #[test]
    fn uris_carry_the_prefix_and_authority() {
        let client = ApiClient::new(
            "https://coroom.example.com",
            ServerTrust::Fingerprint("00".repeat(32)),
        )
        .expect("cliente");
        assert_eq!(
            client.uri(&format!("{API_PREFIX}/me")),
            "https://coroom.example.com:443/api/v1/me"
        );
        assert_eq!(
            client.uri("/health"),
            "https://coroom.example.com:443/health"
        );
        assert_eq!(client.tcp_port, 443);
        let client = client.with_tcp_port(8443).with_token("change-me");
        assert_eq!(client.tcp_port, 8443);
        assert_eq!(client.token(), Some("change-me"));
        assert!(!format!("{client:?}").contains("change-me"));
    }
}
