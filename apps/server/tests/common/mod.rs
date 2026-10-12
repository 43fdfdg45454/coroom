//! Servidor real en puertos efímeros con SQLite temporal y cuentas, para los tests de integración.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::time::Duration;

use coroom_server::{Config, Server};
use coroom_transport::api::ApiClient;
use coroom_transport::crypto::Identity;
use coroom_transport::proto::api::{CreateUser, SessionToken};
use coroom_transport::{Endpoint, EndpointOptions, ServerTrust, SignalClient};
use tokio::sync::oneshot;

/// Espera máxima de cada paso.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Contraseña de todas las cuentas de prueba (ficticia).
pub const PASSWORD: &str = "change-me-please";

/// Administrador de los tests.
pub const ADMIN: &str = "admin";

/// Servidor en marcha.
pub struct Running {
    /// `https://127.0.0.1:puerto`.
    pub url: String,
    /// Huella del certificado autofirmado.
    pub trust: ServerTrust,
    /// Dirección TCP (TLS).
    pub tcp: SocketAddr,
    /// Configuración con que arrancó.
    pub config: Config,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<Result<(), coroom_server::ServerError>>,
    /// Directorio de datos.
    pub dir: tempfile::TempDir,
}

impl Running {
    /// Para el servidor y espera a que termine.
    pub async fn stop(mut self) -> tempfile::TempDir {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = tokio::time::timeout(TIMEOUT, &mut self.task).await;
        self.dir
    }

    /// Cliente de la API sin sesión.
    pub fn api(&self) -> ApiClient {
        ApiClient::new(&self.url, self.trust.clone())
            .expect("cliente")
            .with_tcp_port(self.tcp.port())
    }

    /// Cliente de la API con `token`.
    pub fn api_with(&self, token: &str) -> ApiClient {
        self.api().with_token(token)
    }

    /// Crea el primer administrador y devuelve un cliente con su sesión web.
    pub async fn admin(&self) -> ApiClient {
        let api = self.api();
        api.setup(ADMIN, PASSWORD)
            .await
            .expect("primer administrador");
        let session = api.admin_login(ADMIN, PASSWORD).await.expect("login web");
        self.api_with(&session.token)
    }

    /// Crea el usuario `name` (con el administrador) y lo conecta desde una identidad nueva.
    pub async fn user(&self, admin: &ApiClient, name: &str) -> (Identity, SessionToken) {
        admin
            .create_user(&CreateUser {
                username: name.to_owned(),
                password: PASSWORD.to_owned(),
                display_name: Some(format!("{name} (prueba)")),
                is_admin: false,
            })
            .await
            .expect("alta del usuario");
        self.login(name).await
    }

    /// Inicia sesión como `name` desde una identidad nueva.
    pub async fn login(&self, name: &str) -> (Identity, SessionToken) {
        let identity = Identity::generate().expect("identidad");
        let token = self
            .api()
            .login(name, PASSWORD, &identity, "equipo de prueba")
            .await
            .expect("login");
        (identity, token)
    }

    /// Señalización con la identidad y el token.
    pub async fn signal(&self, identity: &Identity, token: &str) -> (Endpoint, SignalClient) {
        let endpoint = Endpoint::bind(
            identity,
            Some("127.0.0.1:0".parse().expect("dirección")),
            EndpointOptions::default(),
        )
        .expect("endpoint");
        let signal = tokio::time::timeout(
            TIMEOUT,
            SignalClient::connect(&self.url, token, &endpoint, &self.trust),
        )
        .await
        .expect("señalización a tiempo")
        .expect("señalización");
        (endpoint, signal)
    }
}

/// Configuración de prueba: `127.0.0.1` en puertos efímeros, autofirmado con TLS por TCP, nombre
/// público `127.0.0.1` (el de la URL) y puertos públicos los locales.
pub fn config(dir: &std::path::Path) -> Config {
    let any: SocketAddr = "127.0.0.1:0".parse().expect("dirección");
    let mut config = Config::self_signed(any, any, dir.to_owned());
    config.public_host = "127.0.0.1".to_owned();
    config
}

/// Arranca el servidor con la configuración de prueba retocada por `adjust`.
pub async fn start(adjust: impl FnOnce(&mut Config)) -> Running {
    let dir = tempfile::tempdir().expect("directorio temporal");
    let mut config = config(dir.path());
    adjust(&mut config);
    start_with(config, dir).await
}

/// Arranca el servidor con `config` sobre `dir`.
pub async fn start_with(config: Config, dir: tempfile::TempDir) -> Running {
    let server = Server::bind(&config).await.expect("arranque del servidor");
    let url = format!("https://127.0.0.1:{}", server.udp_addr().port());
    let trust = ServerTrust::Fingerprint(server.fingerprint());
    let tcp = server.tcp_addr();
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let task = tokio::spawn(server.run(async move {
        let _ = stop_rx.await;
    }));
    Running {
        url,
        trust,
        tcp,
        config,
        stop: Some(stop_tx),
        task,
        dir,
    }
}

/// Siguiente mensaje de la señalización.
pub async fn next(signal: &mut SignalClient) -> coroom_transport::proto::signal::SignalMessage {
    tokio::time::timeout(TIMEOUT, signal.next_event())
        .await
        .expect("mensaje a tiempo")
        .expect("mensaje")
        .expect("sesión abierta")
}

/// Siguiente mensaje que cumple `pick`, saltando los demás.
pub async fn next_matching<T>(
    signal: &mut SignalClient,
    mut pick: impl FnMut(coroom_transport::proto::signal::SignalMessage) -> Option<T>,
) -> T {
    loop {
        if let Some(found) = pick(next(signal).await) {
            return found;
        }
    }
}
