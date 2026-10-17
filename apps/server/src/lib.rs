//! Servidor autoalojado de coroom (`coroom-server`).
//!
//! Un único binario (`docs/ARQUITECTURA.md` §3 y §4) con dos pilas que comparten la misma lógica de rutas
//! ([`routes::dispatch`]):
//!
//! - **QUIC** en el puerto UDP (`COROOM_BIND_UDP`), demultiplexado por el ALPN negociado:
//!   - `h3`: HTTP/3 con `quinn` + `h3` (`/health`, `/version`, la API `/api/v1` y `/admin`) y la
//!     **señalización** por WebTransport en `/signal` (`h3-webtransport`, [`signal`]);
//!   - `coroom-relay/1`: el **relay** de medios con abanico, caché de `Nack`, cuotas y límite por
//!     sala ([`relay`], [`quota`]).
//! - **HTTP/2 y HTTP/1.1 sobre TLS** en el puerto TCP (`COROOM_BIND_TCP`, [`tcp`]), con la cabecera
//!   `Alt-Svc` que anuncia HTTP/3; sin TLS solo con `COROOM_TCP_PLAIN=true` (desarrollo).
//!
//! Cuentas en SQLite ([`db`]) con Argon2id, tokens de sesión y desafío de dispositivo
//! ([`auth`]), la API ([`api`]) y la administración web ([`admin`]), amigos y presencia, cuotas
//! del relay ([`quota`]), el certificado del operador o autofirmado ([`tls`]) y la
//! autocomprobación de UDP ([`probe`]). Las salas siguen en memoria mientras el anfitrión está
//! conectado. El token de desarrollo `COROOM_DEV_TOKEN` queda como modo explícito de desarrollo.

pub mod admin;
pub mod api;
pub mod auth;
pub mod avatar;
pub mod clock;
pub mod config;
pub mod db;
#[cfg(feature = "fuzzing")]
pub mod fuzzing;
mod http3;
pub mod probe;
pub mod quota;
pub mod relay;
pub mod routes;
pub mod signal;
mod tcp;
pub mod tls;

use std::fmt;
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use quinn::crypto::rustls::QuicServerConfig;

use coroom_transport::rate::{Congestion, CongestionShared};

use auth::AuthState;
use config::Registration;
use db::Db;
use probe::UdpStatus;
use quota::Quota;
use relay::Relay;
use signal::{Hub, HubConfig};
use tls::CertResolver;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::{info, warn};

pub use config::{Config, TlsMode};

/// Límites de la pila TCP, públicos para los tests y la documentación.
pub mod tcp_limits {
    pub use crate::tcp::{BODY_TIMEOUT, HEADER_TIMEOUT, MAX_TCP_CONNECTIONS};
}
pub use tcp::alt_svc_value;
pub use tls::TlsMaterial;

/// Error al arrancar o servir.
#[derive(Debug)]
pub enum ServerError {
    /// Falta una variable de configuración obligatoria para el modo elegido.
    MissingSetting(&'static str),
    /// Combinación de opciones que no puede funcionar.
    Config(String),
    /// Error de la base de datos.
    Database(String),
    /// Solo existe uno de los dos ficheros del certificado autofirmado.
    IncompleteSelfSigned(PathBuf),
    /// Un fichero PEM no se pudo leer o no contiene lo esperado.
    Pem(PathBuf, String),
    /// Error de configuración TLS.
    Tls(String),
    /// Error de entrada/salida en un fichero o socket.
    Io {
        /// Qué se intentaba hacer.
        action: &'static str,
        /// Sobre qué fichero o dirección.
        target: String,
        /// Error del sistema.
        source: std::io::Error,
    },
}

impl ServerError {
    fn io(action: &'static str, target: impl fmt::Debug, source: std::io::Error) -> Self {
        ServerError::Io {
            action,
            target: format!("{target:?}"),
            source,
        }
    }
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerError::MissingSetting(name) => {
                write!(f, "falta {name}, obligatoria con el modo TLS elegido")
            }
            ServerError::Config(reason) => write!(f, "configuración no válida: {reason}"),
            ServerError::Database(reason) => write!(f, "base de datos: {reason}"),
            ServerError::IncompleteSelfSigned(missing) => write!(
                f,
                "falta {missing:?}: borra el otro fichero del certificado autofirmado para \
                 regenerarlo o restaura el que falta"
            ),
            ServerError::Pem(path, reason) => write!(f, "no se pudo leer {path:?}: {reason}"),
            ServerError::Tls(reason) => write!(f, "configuración TLS no válida: {reason}"),
            ServerError::Io {
                action,
                target,
                source,
            } => write!(f, "no se pudo {action} {target}: {source}"),
        }
    }
}

impl std::error::Error for ServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ServerError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Dirección IPv4 equivalente a `[::]:puerto`, para sistemas sin IPv6 (contenedores y entornos
/// con IPv6 desactivado en el núcleo). Solo se usa si falla el bind de la dirección pedida; si el
/// fallo es otro (puerto ocupado, permisos), el bind IPv4 falla igual y se informa el error original.
fn ipv4_fallback(addr: SocketAddr) -> Option<SocketAddr> {
    match addr {
        SocketAddr::V6(v6) if v6.ip().is_unspecified() => {
            Some(SocketAddr::from((Ipv4Addr::UNSPECIFIED, v6.port())))
        }
        _ => None,
    }
}

/// Bitrate con el que se dimensiona la ventana de las conexiones del servidor (relay incluido):
/// holgada para que QUIC no frene el abanico de datagramas.
const SERVER_TARGET_KBPS: u32 = 100_000;

/// Estado compartido por las dos pilas.
#[derive(Debug)]
pub(crate) struct ServerState {
    pub(crate) config: Config,
    pub(crate) db: Db,
    pub(crate) auth: AuthState,
    pub(crate) hub: Arc<Hub>,
    pub(crate) relay: Arc<Relay>,
    pub(crate) quota: Arc<Quota>,
    pub(crate) certs: Arc<CertResolver>,
    pub(crate) udp: Arc<UdpStatus>,
    pub(crate) udp_addr: SocketAddr,
    pub(crate) tcp_addr: SocketAddr,
    pub(crate) relay_addr: Option<SocketAddr>,
}

impl ServerState {
    /// Puerto UDP público (`COROOM_PUBLIC_UDP_PORT`, o el local si es `0`).
    pub(crate) fn public_udp_port(&self) -> u16 {
        match self.config.public_udp_port {
            0 => self.udp_addr.port(),
            port => port,
        }
    }

    /// Puerto TCP público (`COROOM_PUBLIC_TCP_PORT`, o el local si es `0`).
    pub(crate) fn public_tcp_port(&self) -> u16 {
        match self.config.public_tcp_port {
            0 => self.tcp_addr.port(),
            port => port,
        }
    }
}

async fn bind_tcp(addr: SocketAddr, what: &'static str) -> Result<TcpListener, ServerError> {
    match TcpListener::bind(addr).await {
        Ok(listener) => Ok(listener),
        Err(err) => match ipv4_fallback(addr) {
            Some(v4) => {
                let listener = TcpListener::bind(v4)
                    .await
                    .map_err(|_| ServerError::io(what, addr, err))?;
                warn!(tcp = %v4, "IPv6 no disponible; TCP escucha solo en IPv4");
                Ok(listener)
            }
            None => Err(ServerError::io(what, addr, err)),
        },
    }
}

fn database_error(err: db::DbError) -> ServerError {
    ServerError::Database(err.to_string())
}

/// Abre la base de datos de `config` (y aplica las migraciones).
pub async fn open_database(config: &Config) -> Result<Db, ServerError> {
    Db::open(&config.database_path())
        .await
        .map_err(database_error)
}

/// `coroom-server migrate`: aplica las migraciones y termina.
pub async fn migrate(config: &Config) -> Result<(), ServerError> {
    let db = open_database(config).await?;
    db.close().await;
    Ok(())
}

/// `coroom-server backup RUTA`: copia en línea y consistente de la base en `path`.
pub async fn backup(config: &Config, path: &Path) -> Result<(), ServerError> {
    let db = open_database(config).await?;
    let result = db.backup(path).await.map_err(database_error);
    db.close().await;
    result
}

/// `coroom-server admin reset-password NOMBRE`: contraseña nueva y sesiones cerradas.
pub async fn reset_password(
    config: &Config,
    username: &str,
    password: &str,
) -> Result<(), ServerError> {
    auth::validate_password(password).map_err(ServerError::Config)?;
    let db = open_database(config).await?;
    let user = db
        .user_by_name(username)
        .await
        .map_err(database_error)?
        .ok_or_else(|| ServerError::Config(format!("no existe el usuario «{username}»")))?;
    let password = password.to_owned();
    let hash = tokio::task::spawn_blocking(move || auth::hash_password(&password))
        .await
        .map_err(|err| ServerError::Config(err.to_string()))?
        .map_err(ServerError::Config)?;
    db.set_password(user.id, &hash)
        .await
        .map_err(database_error)?;
    db.close().await;
    Ok(())
}

/// Servidor con sus sockets ya abiertos, listo para [`Server::run`].
pub struct Server {
    endpoint: quinn::Endpoint,
    listener: TcpListener,
    acceptor: Option<tokio_rustls::TlsAcceptor>,
    udp_addr: SocketAddr,
    tcp_addr: SocketAddr,
    state: Arc<ServerState>,
    quota_cuts: tokio::sync::mpsc::UnboundedReceiver<u64>,
}

impl Server {
    /// Abre la base de datos, obtiene el certificado y abre los sockets UDP y TCP.
    ///
    /// Con puerto `0` el sistema elige uno libre; [`Server::udp_addr`] y [`Server::tcp_addr`]
    /// devuelven los reales.
    pub async fn bind(config: &Config) -> Result<Self, ServerError> {
        if !(config.relay_quota_gb_per_month >= 0.0 && config.relay_quota_gb_per_month.is_finite())
        {
            return Err(ServerError::Config(
                "COROOM_RELAY_QUOTA_GB_PER_MONTH debe ser 0 o un número positivo".to_owned(),
            ));
        }
        let db = open_database(config).await?;
        let material = TlsMaterial::load(config)?;
        let certs = Arc::new(CertResolver::new(&material)?);
        let quic_config = QuicServerConfig::try_from(tls::quic_server_config(Arc::clone(&certs))?)
            .map_err(|err| ServerError::Tls(err.to_string()))?;
        let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_config));
        // Mismo transporte que los clientes (MTU, keep-alive, búferes de datagramas) con una
        // ventana amplia fija para el relay.
        server_config.transport_config(Arc::new(coroom_transport::quic::transport_config(
            &CongestionShared::new(SERVER_TARGET_KBPS),
            Congestion::Media,
        )));

        // El socket con los búferes ampliados de los clientes (`SO_RCVBUF`/`SO_SNDBUF` de 8 MB,
        // lo que el sistema conceda: en Linux, `net.core.rmem_max`/`wmem_max`, ver
        // `deploy/README.md`). Con los 208 KB por defecto, el relay perdía ráfagas enteras (un
        // intra que llega del anfitrión a la vez que varios invitados piden reenvíos).
        let runtime = quinn::default_runtime().ok_or_else(|| {
            ServerError::Config("el servidor necesita un runtime de tokio".into())
        })?;
        let open = |addr: SocketAddr, server_config: quinn::ServerConfig| {
            let socket = coroom_transport::quic::bind_udp_socket(addr)?;
            quinn::Endpoint::new(
                quinn::EndpointConfig::default(),
                Some(server_config),
                socket,
                Arc::clone(&runtime),
            )
        };
        let endpoint = match open(config.bind_udp, server_config.clone()) {
            Ok(endpoint) => endpoint,
            Err(err) => match ipv4_fallback(config.bind_udp) {
                Some(v4) => {
                    let endpoint = open(v4, server_config).map_err(|_| {
                        ServerError::io("abrir el socket UDP", config.bind_udp, err)
                    })?;
                    warn!(udp = %v4, "IPv6 no disponible; HTTP/3 escucha solo en IPv4");
                    endpoint
                }
                None => return Err(ServerError::io("abrir el socket UDP", config.bind_udp, err)),
            },
        };
        let udp_addr = endpoint
            .local_addr()
            .map_err(|err| ServerError::io("leer la dirección UDP", config.bind_udp, err))?;

        let listener = bind_tcp(config.bind_tcp, "abrir el socket TCP").await?;
        let tcp_addr = listener
            .local_addr()
            .map_err(|err| ServerError::io("leer la dirección TCP", config.bind_tcp, err))?;
        let acceptor = if config.tcp_plain {
            warn!("COROOM_TCP_PLAIN=true: TCP sin TLS, solo para desarrollo");
            None
        } else {
            Some(tokio_rustls::TlsAcceptor::from(Arc::new(
                tls::tcp_server_config(Arc::clone(&certs))?,
            )))
        };

        if config.dev_token.is_some() {
            warn!(
                "modo desarrollo: COROOM_DEV_TOKEN está definido y la señalización acepta ese                  token sin cuenta; no lo uses en un servidor público"
            );
        }
        let quota = Arc::new(Quota::new(db.clone(), config.quota_bytes()));
        let relay = Relay::with_limits(Some(Arc::clone(&quota)), config.relay_max_kbps);
        let (cuts_tx, quota_cuts) = tokio::sync::mpsc::unbounded_channel();
        relay.set_exceeded_sender(cuts_tx);
        let hub = Hub::with_accounts(
            HubConfig {
                dev_token: config.dev_token.clone(),
                max_guests: config.max_guests_per_room,
                relay_enabled: config.relay_enabled,
            },
            Arc::clone(&relay),
            db.clone(),
            Arc::clone(&quota),
        );
        let state = Arc::new(ServerState {
            config: config.clone(),
            db,
            auth: AuthState::default(),
            hub,
            relay,
            quota,
            certs,
            udp: Arc::new(UdpStatus::default()),
            udp_addr,
            tcp_addr,
            relay_addr: config.relay_addr,
        });

        Ok(Self {
            endpoint,
            listener,
            acceptor,
            udp_addr,
            tcp_addr,
            state,
            quota_cuts,
        })
    }

    /// Dirección UDP real de HTTP/3.
    pub fn udp_addr(&self) -> SocketAddr {
        self.udp_addr
    }

    /// Dirección TCP real de HTTP/2 y HTTP/1.1.
    pub fn tcp_addr(&self) -> SocketAddr {
        self.tcp_addr
    }

    /// Huella SHA-256 (hex) del certificado del servidor en uso.
    pub fn fingerprint(&self) -> String {
        self.state.certs.fingerprint()
    }

    /// Resolutor de certificados (para comprobar el cambio en caliente).
    pub fn certificates(&self) -> Arc<CertResolver> {
        Arc::clone(&self.state.certs)
    }

    /// Sirve las dos pilas hasta que `shutdown` termina y luego las cierra limpiamente.
    pub async fn run<F>(self, shutdown: F) -> Result<(), ServerError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let state = Arc::clone(&self.state);
        let config = &state.config;
        info!(
            udp = %self.udp_addr,
            tcp = %self.tcp_addr,
            tls_mode = %config.tls_mode,
            tcp_tls = !config.tcp_plain,
            registration = %config.registration,
            fingerprint = %state.certs.fingerprint(),
            "coroom-server escuchando (HTTP/3, señalización y relay por UDP; HTTP/2 y HTTP/1.1 por \
             TCP)"
        );
        match state.db.has_admin().await {
            Ok(false) => {
                let port = state.public_tcp_port();
                let host = if port == 443 {
                    config.public_host.clone()
                } else {
                    format!("{}:{port}", config.public_host)
                };
                info!("no hay administrador: abre https://{host}/admin para crearlo");
            }
            Ok(true) => {}
            Err(err) => warn!(error = %err, "no se pudo leer la base de datos"),
        }
        if config.registration == Registration::Admin {
            info!("COROOM_REGISTRATION=admin: solo el administrador da de alta usuarios");
        }

        let (stop_tx, stop_rx) = watch::channel(false);
        let trigger = tokio::spawn(async move {
            shutdown.await;
            info!("apagando");
            let _ = stop_tx.send(true);
        });

        let mut background = Vec::new();
        background.push(tokio::spawn(
            Arc::clone(&state.hub).watch_quota(self.quota_cuts),
        ));
        {
            let quota = Arc::clone(&state.quota);
            let db = state.db.clone();
            let mut stop = stop_rx.clone();
            background.push(tokio::spawn(async move {
                let mut flush = tokio::time::interval(quota::FLUSH_PERIOD);
                let mut purge = tokio::time::interval(Duration::from_secs(3600));
                loop {
                    tokio::select! {
                        _ = flush.tick() => quota.flush().await,
                        _ = purge.tick() => {
                            if let Err(err) = db.purge_expired().await {
                                warn!(error = %err, "no se pudieron borrar las sesiones caducadas");
                            }
                        }
                        _ = stop.changed() => break,
                    }
                }
            }));
        }
        background.push(tokio::spawn(probe::run(
            Arc::clone(&state.udp),
            config.public_host.clone(),
            state.public_udp_port(),
            Arc::clone(&state.certs),
            stop_rx.clone(),
        )));

        let tcp_addr = self.tcp_addr;
        let (_, tcp_result) = tokio::join!(
            http3::serve(self.endpoint, Arc::clone(&state), stop_rx.clone()),
            tcp::serve(
                self.listener,
                self.acceptor,
                state.public_udp_port(),
                Arc::clone(&state),
                stop_rx
            ),
        );
        trigger.abort();
        for task in background {
            task.abort();
        }
        state.quota.flush().await;
        state.db.close().await;
        tcp_result.map_err(|err| ServerError::io("servir por TCP en", tcp_addr, err))?;
        info!("servidor detenido");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_ipv6_wildcard_falls_back_to_ipv4() {
        let wildcard: SocketAddr = "[::]:4433".parse().expect("dirección");
        assert_eq!(
            ipv4_fallback(wildcard),
            Some("0.0.0.0:4433".parse().expect("dirección"))
        );
        assert_eq!(
            ipv4_fallback("[::1]:4433".parse().expect("dirección")),
            None
        );
        assert_eq!(
            ipv4_fallback("0.0.0.0:4433".parse().expect("dirección")),
            None
        );
    }
}
