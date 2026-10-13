//! Arranca `coroom-server` en puertos efímeros y comprueba `/health` y `/version` por HTTP/3 (con
//! un cliente `quinn` + `h3` que confía en el certificado autofirmado generado) y por TCP con TLS
//! (HTTP/2 con `hyper` + `tokio-rustls`), incluida la cabecera `Alt-Svc`, y que la interfaz
//! `/admin` se sirve por las dos pilas.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bytes::{Buf, Bytes};
use coroom_server::{Config, Server, alt_svc_value, tls};
use http::{Request, StatusCode, header};
use http_body_util::{BodyExt, Empty};
use hyper_util::rt::TokioIo;
use rustls::crypto::aws_lc_rs;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use serde_json::{Value, json};
use tokio::sync::oneshot;

const TIMEOUT: Duration = Duration::from_secs(10);

/// `[::1]:0` (IPv6 primero); `127.0.0.1:0` si el sistema no tiene IPv6 (algunos
/// contenedores de CI y entornos aislados).
fn loopback() -> SocketAddr {
    let v6: SocketAddr = "[::1]:0".parse().expect("dirección");
    if std::net::UdpSocket::bind(v6).is_ok() {
        v6
    } else {
        eprintln!("IPv6 no disponible: el test usa 127.0.0.1");
        "127.0.0.1:0".parse().expect("dirección")
    }
}

/// Respuesta reducida a lo que comprueban los tests.
struct Reply {
    status: StatusCode,
    alt_svc: Option<String>,
    content_type: Option<String>,
    json: Value,
    text: String,
}

/// `/health` tiene todos sus campos.
fn check_health(health: &Value) {
    assert_eq!(health["status"], "ok", "{health}");
    for field in [
        "udp_reachable",
        "udp_checked_at",
        "users",
        "rooms",
        "relay_bytes",
        "relay_bytes_today",
        "relay_rooms",
    ] {
        assert!(health.get(field).is_some(), "falta {field}: {health}");
    }
    assert_eq!(health["users"], 0);
    assert_eq!(health["rooms"], 0);
    assert_eq!(health["relay_bytes_today"], 0);
}

#[tokio::test]
async fn health_and_version_over_http3_and_tcp() {
    let data_dir = tempfile::tempdir().expect("directorio temporal");
    let loopback = loopback();
    let config = Config::self_signed(loopback, loopback, data_dir.path().to_owned());

    let server = Server::bind(&config).await.expect("arranque del servidor");
    let udp_addr = server.udp_addr();
    let tcp_addr = server.tcp_addr();
    assert_ne!(udp_addr.port(), 0);
    assert_ne!(tcp_addr.port(), 0);
    assert_eq!(server.fingerprint().len(), 64);

    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let running = tokio::spawn(server.run(async move {
        let _ = stop_rx.await;
    }));

    // HTTP/3.
    let client = Http3Client::connect(udp_addr, data_dir.path()).await;
    let health = client.get("/health").await;
    assert_eq!(health.status, StatusCode::OK);
    assert_eq!(health.content_type.as_deref(), Some("application/json"));
    check_health(&health.json);

    let version = client.get("/version").await;
    assert_eq!(version.status, StatusCode::OK);
    assert_eq!(
        version.json,
        json!({
            "name": "coroom-server",
            "version": env!("CARGO_PKG_VERSION"),
            "protocol": 8,
            "source": "https://github.com/43fdfdg45454/coroom",
        })
    );

    let missing = client.get("/nope").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.json["error"], "not_found");
    let admin = client.get("/admin/").await;
    assert_eq!(admin.status, StatusCode::OK);
    assert!(admin.text.contains("<html"), "{}", admin.text);
    let setup = client.get("/api/v1/setup").await;
    assert_eq!(setup.json["needed"], true);
    client.close().await;

    // TCP con TLS (HTTP/2) y Alt-Svc.
    let expected_alt_svc = alt_svc_value(udp_addr.port());
    let tcp = TcpClient::new(data_dir.path());
    let health = tcp.get(tcp_addr, "/health").await;
    assert_eq!(health.status, StatusCode::OK);
    check_health(&health.json);
    assert_eq!(health.alt_svc.as_deref(), Some(expected_alt_svc.as_str()));

    let version = tcp.get(tcp_addr, "/version").await;
    assert_eq!(version.status, StatusCode::OK);
    assert_eq!(
        version.json,
        json!({
            "name": "coroom-server",
            "version": env!("CARGO_PKG_VERSION"),
            "protocol": 8,
            "source": "https://github.com/43fdfdg45454/coroom",
        })
    );
    assert_eq!(version.alt_svc.as_deref(), Some(expected_alt_svc.as_str()));

    let missing = tcp.get(tcp_addr, "/nope").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.alt_svc.as_deref(), Some(expected_alt_svc.as_str()));
    let admin = tcp.get(tcp_addr, "/admin/admin.css").await;
    assert_eq!(admin.status, StatusCode::OK);
    assert!(admin.text.contains("color-scheme"));

    // Apagado limpio.
    stop_tx.send(()).expect("señal de apagado");
    tokio::time::timeout(TIMEOUT, running)
        .await
        .expect("el servidor se detiene a tiempo")
        .expect("la tarea del servidor no falla")
        .expect("el servidor termina sin error");
}

struct Http3Client {
    endpoint: quinn::Endpoint,
    connection: quinn::Connection,
    send_request: h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>,
    driver: tokio::task::JoinHandle<()>,
}

/// Raíces con solo el certificado autofirmado de `data_dir`.
fn roots(data_dir: &Path) -> rustls::RootCertStore {
    let cert_path = tls::self_signed_cert_path(data_dir);
    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_file_iter(&cert_path).expect("certificado PEM") {
        roots
            .add(cert.expect("certificado"))
            .expect("raíz de confianza");
    }
    roots
}

impl Http3Client {
    /// Conecta con `localhost` confiando solo en el certificado guardado en `data_dir`.
    async fn connect(server: SocketAddr, data_dir: &Path) -> Self {
        let roots = roots(data_dir);

        let mut tls_config =
            rustls::ClientConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
                .with_protocol_versions(&[&rustls::version::TLS13])
                .expect("TLS 1.3")
                .with_root_certificates(roots)
                .with_no_client_auth();
        tls_config.alpn_protocols = vec![b"h3".to_vec()];
        let quic_config = quinn::crypto::rustls::QuicClientConfig::try_from(tls_config)
            .expect("configuración QUIC");

        // Dirección sin especificar de la misma familia, como hace `coroom-transport`: en Windows,
        // un socket QUIC atado a `[::1]` falla al configurarse (WSAEINVAL).
        let local: SocketAddr = if server.is_ipv6() {
            "[::]:0".parse().expect("dirección")
        } else {
            "0.0.0.0:0".parse().expect("dirección")
        };
        let mut endpoint = quinn::Endpoint::client(local).expect("cliente QUIC");
        endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(quic_config)));

        let connecting = endpoint.connect(server, "localhost").expect("conexión");
        let connection = tokio::time::timeout(TIMEOUT, connecting)
            .await
            .expect("handshake a tiempo")
            .expect("handshake QUIC");
        assert_eq!(
            connection
                .handshake_data()
                .and_then(|data| data.downcast::<quinn::crypto::rustls::HandshakeData>().ok())
                .and_then(|data| data.protocol),
            Some(b"h3".to_vec()),
            "ALPN negociado"
        );

        let (mut h3_conn, send_request) =
            h3::client::new(h3_quinn::Connection::new(connection.clone()))
                .await
                .expect("conexión HTTP/3");
        let driver = tokio::spawn(async move {
            let _ = std::future::poll_fn(|cx| h3_conn.poll_close(cx)).await;
        });

        Self {
            endpoint,
            connection,
            send_request,
            driver,
        }
    }

    async fn get(&self, path: &str) -> Reply {
        let request = Request::get(format!("https://localhost{path}"))
            .body(())
            .expect("petición");
        let mut send_request = self.send_request.clone();
        let exchange = async move {
            let mut stream = send_request.send_request(request).await.expect("envío");
            stream.finish().await.expect("fin de la petición");
            let response = stream.recv_response().await.expect("respuesta");
            let mut body = Vec::new();
            while let Some(mut chunk) = stream.recv_data().await.expect("cuerpo") {
                while chunk.has_remaining() {
                    let part = chunk.chunk();
                    body.extend_from_slice(part);
                    let len = part.len();
                    chunk.advance(len);
                }
            }
            (response, body)
        };
        let (response, body) = tokio::time::timeout(TIMEOUT, exchange)
            .await
            .expect("respuesta HTTP/3 a tiempo");
        reply(response.status(), response.headers(), &body)
    }

    async fn close(self) {
        drop(self.send_request);
        self.connection.close(quinn::VarInt::from_u32(0), b"fin");
        self.driver.abort();
        let _ = tokio::time::timeout(TIMEOUT, self.endpoint.wait_idle()).await;
    }
}

/// Cliente HTTP/2 sobre TLS que confía solo en el certificado autofirmado.
struct TcpClient {
    connector: tokio_rustls::TlsConnector,
}

impl TcpClient {
    fn new(data_dir: &Path) -> Self {
        let mut config =
            rustls::ClientConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("versiones TLS")
                .with_root_certificates(roots(data_dir))
                .with_no_client_auth();
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Self {
            connector: tokio_rustls::TlsConnector::from(Arc::new(config)),
        }
    }

    async fn get(&self, server: SocketAddr, path: &str) -> Reply {
        let exchange = async {
            let stream = tokio::net::TcpStream::connect(server)
                .await
                .expect("conexión TCP");
            let name = rustls::pki_types::ServerName::try_from("localhost").expect("nombre");
            let tls = self
                .connector
                .connect(name, stream)
                .await
                .expect("handshake TLS");
            assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"h2"[..]), "HTTP/2");
            let (mut sender, connection) = hyper::client::conn::http2::handshake(
                hyper_util::rt::TokioExecutor::new(),
                TokioIo::new(tls),
            )
            .await
            .expect("handshake HTTP/2");
            tokio::spawn(connection);
            let request = Request::get(format!("https://localhost{path}"))
                .body(Empty::<Bytes>::new())
                .expect("petición");
            let response = sender.send_request(request).await.expect("respuesta");
            let status = response.status();
            let headers = response.headers().clone();
            let body = response
                .into_body()
                .collect()
                .await
                .expect("cuerpo")
                .to_bytes();
            (status, headers, body)
        };
        let (status, headers, body) = tokio::time::timeout(TIMEOUT, exchange)
            .await
            .expect("respuesta TCP a tiempo");
        reply(status, &headers, &body)
    }
}

fn reply(status: StatusCode, headers: &http::HeaderMap, body: &[u8]) -> Reply {
    let header_text = |name: header::HeaderName| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    Reply {
        status,
        alt_svc: header_text(header::ALT_SVC),
        content_type: header_text(header::CONTENT_TYPE),
        json: serde_json::from_slice(body).unwrap_or(Value::Null),
        text: String::from_utf8_lossy(body).into_owned(),
    }
}
