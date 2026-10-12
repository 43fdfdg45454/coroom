//! ACME con un directorio falso en proceso (sin red real): el servidor arranca en modo `acme` con
//! un certificado temporal, crea la cuenta, pide el pedido, resuelve el reto **TLS-ALPN-01** en su
//! propio puerto TCP (el directorio falso lo valida de verdad: se conecta con ALPN `acme-tls/1`,
//! comprueba la extensión `acmeIdentifier` con el SHA-256 de la autorización), finaliza con su
//! CSR, descarga la cadena, la guarda en `acme/` y la instala en caliente. El primer certificado
//! caduca pronto, así que la tarea de renovación pide enseguida otro: se comprueba que llega el
//! segundo y que el servidor lo sirve con su clave. Al rearrancar, lee el guardado sin pedir nada.
//! Con `COROOM_ACME_CHALLENGE=http-01`, el directorio falso valida en el listener sin TLS
//! (`/.well-known/acme-challenge/<token>`), que redirige todo lo demás a HTTPS.

mod common;

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use coroom_server::acme::AcmeHttp;
use coroom_server::clock;
use coroom_server::config::TlsMode;
use coroom_server::tls::ACME_TLS_ALPN;
use coroom_server::{Config, Server};
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const DOMAIN: &str = "coroom.example.com";
const TOKEN: &str = "token-del-reto";

/// OID 1.3.6.1.5.5.7.1.31 (`id-pe-acmeIdentifier`), crítica, con un OCTET STRING de 32 bytes.
const ACME_EXTENSION_PREFIX: [u8; 17] = [
    0x06, 0x08, 0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x01, 0x1f, 0x01, 0x01, 0xff, 0x04, 0x22, 0x04,
    0x20,
];

struct Fake {
    base: String,
    /// Puerto TCP con TLS del servidor (TLS-ALPN-01).
    target: Option<SocketAddr>,
    /// Listener sin TLS del servidor (HTTP-01).
    http_target: Option<SocketAddr>,
    thumbprint: Option<String>,
    orders: usize,
    validated: bool,
    issued: Vec<String>,
    ca_key: rcgen::KeyPair,
    ca_params: rcgen::CertificateParams,
    ca_pem: String,
    nonce: u64,
}

/// Verificador que acepta cualquier certificado y lo guarda.
#[derive(Debug, Default)]
struct Capture(Mutex<Option<Vec<u8>>>);

impl ServerCertVerifier for Capture {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if let Ok(mut cert) = self.0.lock() {
            *cert = Some(end_entity.as_ref().to_vec());
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Certificado que presenta `target` con el ALPN `alpn` y el SNI `DOMAIN` (si hay handshake).
async fn presented(target: SocketAddr, alpn: &[u8]) -> Option<Vec<u8>> {
    let capture = Arc::new(Capture::default());
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .ok()?
    .dangerous()
    .with_custom_certificate_verifier(Arc::clone(&capture) as Arc<dyn ServerCertVerifier>)
    .with_no_client_auth();
    config.alpn_protocols = vec![alpn.to_vec()];
    let stream = tokio::net::TcpStream::connect(target).await.ok()?;
    let _tls = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from(DOMAIN).ok()?, stream)
        .await
        .ok()?;
    capture.0.lock().ok()?.clone()
}

/// `GET path` en claro: (estado, cuerpo); con la cabecera `Location` si es una redirección.
async fn http_get(target: SocketAddr, path: &str) -> Option<(u16, String)> {
    let stream = tokio::net::TcpStream::connect(target).await.ok()?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .ok()?;
    tokio::spawn(connection);
    let request = Request::get(path)
        .header("host", DOMAIN)
        .body(http_body_util::Empty::<Bytes>::new())
        .ok()?;
    let response = sender.send_request(request).await.ok()?;
    let status = response.status().as_u16();
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response.into_body().collect().await.ok()?.to_bytes();
    Some((
        status,
        location.unwrap_or_else(|| String::from_utf8_lossy(&body).into_owned()),
    ))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Elemento DER: (etiqueta, contenido, resto).
fn der(input: &[u8]) -> (u8, &[u8], &[u8]) {
    let tag = input[0];
    let (len, start) = if input[1] < 0x80 {
        (usize::from(input[1]), 2)
    } else {
        let count = usize::from(input[1] & 0x7f);
        let len = input[2..2 + count]
            .iter()
            .fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
        (len, 2 + count)
    };
    (tag, &input[start..start + len], &input[start + len..])
}

/// Clave pública EC P-256 de una CSR (la que generó el servidor).
struct CsrKey(Vec<u8>);

impl rcgen::PublicKeyData for CsrKey {
    fn der_bytes(&self) -> &[u8] {
        &self.0
    }

    fn algorithm(&self) -> &'static rcgen::SignatureAlgorithm {
        &rcgen::PKCS_ECDSA_P256_SHA256
    }
}

fn csr_key(csr: &[u8]) -> CsrKey {
    let (_, request, _) = der(csr);
    let (_, info, _) = der(request);
    let (_, _version, rest) = der(info);
    let (_, _subject, rest) = der(rest);
    let (_, spki, _) = der(rest);
    let (_, _algorithm, rest) = der(spki);
    let (tag, bits, _) = der(rest);
    assert_eq!(tag, 0x03, "BIT STRING");
    CsrKey(bits[1..].to_vec())
}

/// Validez del certificado entre dos días desde 1970.
fn set_validity(params: &mut rcgen::CertificateParams, from: i64, to: i64) {
    let ymd = |day: i64| {
        let (year, month, day) = clock::civil_from_days(day);
        (i32::try_from(year).expect("año"), month as u8, day as u8)
    };
    let (y, m, d) = ymd(from);
    params.not_before = rcgen::date_time_ymd(y, m, d);
    let (y, m, d) = ymd(to);
    params.not_after = rcgen::date_time_ymd(y, m, d);
}

fn reply(
    state: &mut Fake,
    status: StatusCode,
    body: Value,
    location: Option<String>,
) -> Response<Full<Bytes>> {
    state.nonce += 1;
    let mut builder = Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("replay-nonce", format!("nonce-{}", state.nonce));
    if let Some(location) = location {
        builder = builder.header("location", location);
    }
    builder
        .body(Full::new(Bytes::from(body.to_string())))
        .expect("respuesta")
}

fn order_json(state: &Fake) -> Value {
    let base = &state.base;
    let n = state.orders;
    let status = if state.issued.len() >= n {
        "valid"
    } else if state.validated {
        "ready"
    } else {
        "pending"
    };
    json!({
        "status": status,
        "identifiers": [{ "type": "dns", "value": DOMAIN }],
        "authorizations": [format!("{base}/authz/{n}")],
        "finalize": format!("{base}/finalize/{n}"),
        "certificate": format!("{base}/cert/{n}"),
    })
}

async fn fake_acme(
    state: Arc<tokio::sync::Mutex<Fake>>,
    request: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    let body = request
        .into_body()
        .collect()
        .await
        .map(|body| body.to_bytes())
        .unwrap_or_default();
    let jws: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let decode = |field: &str| -> Value {
        jws[field]
            .as_str()
            .filter(|text| !text.is_empty())
            .and_then(|text| URL_SAFE_NO_PAD.decode(text).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Value::Null)
    };
    let payload = decode("payload");
    let protected = decode("protected");

    let mut fake = state.lock().await;
    let base = fake.base.clone();
    let response = match (method.as_str(), path.as_str()) {
        ("GET", "/directory") => reply(
            &mut fake,
            StatusCode::OK,
            json!({
                "newNonce": format!("{base}/nonce"),
                "newAccount": format!("{base}/account"),
                "newOrder": format!("{base}/order"),
            }),
            None,
        ),
        ("HEAD" | "GET", "/nonce") => reply(&mut fake, StatusCode::OK, Value::Null, None),
        ("POST", "/account") => {
            // Huella RFC 7638 de la clave de la cuenta, para comprobar la autorización del reto.
            let jwk = &protected["jwk"];
            let canonical = format!(
                r#"{{"crv":"{}","kty":"{}","x":"{}","y":"{}"}}"#,
                jwk["crv"].as_str().unwrap_or_default(),
                jwk["kty"].as_str().unwrap_or_default(),
                jwk["x"].as_str().unwrap_or_default(),
                jwk["y"].as_str().unwrap_or_default()
            );
            fake.thumbprint = Some(URL_SAFE_NO_PAD.encode(Sha256::digest(canonical.as_bytes())));
            reply(
                &mut fake,
                StatusCode::CREATED,
                json!({ "status": "valid" }),
                Some(format!("{base}/acct/1")),
            )
        }
        ("POST", "/order") => {
            fake.orders += 1;
            fake.validated = false;
            let order = order_json(&fake);
            let n = fake.orders;
            reply(
                &mut fake,
                StatusCode::CREATED,
                order,
                Some(format!("{base}/order/{n}")),
            )
        }
        ("POST", path) if path.starts_with("/authz/") => {
            let status = if fake.validated { "valid" } else { "pending" };
            let n = fake.orders;
            let body = json!({
                "status": status,
                "identifier": { "type": "dns", "value": DOMAIN },
                "challenges": [{
                    "type": "tls-alpn-01",
                    "url": format!("{base}/chall/{n}"),
                    "token": TOKEN,
                    "status": status,
                }, {
                    "type": "http-01",
                    "url": format!("{base}/chall-http/{n}"),
                    "token": TOKEN,
                    "status": status,
                }],
            });
            reply(&mut fake, StatusCode::OK, body, None)
        }
        ("POST", path) if path.starts_with("/chall/") => {
            // Validación real del reto contra el servidor (RFC 8737 §3).
            let target = fake.target.expect("dirección del servidor");
            let key_authorization =
                format!("{TOKEN}.{}", fake.thumbprint.clone().unwrap_or_default());
            let digest = Sha256::digest(key_authorization.as_bytes());
            drop(fake);
            let cert = presented(target, ACME_TLS_ALPN).await;
            let mut expected = ACME_EXTENSION_PREFIX.to_vec();
            expected.extend_from_slice(&digest);
            let valid = cert.is_some_and(|cert| contains(&cert, &expected));
            let mut fake = state.lock().await;
            fake.validated = valid;
            let n = fake.orders;
            let body = json!({
                "type": "tls-alpn-01",
                "url": format!("{base}/chall/{n}"),
                "token": TOKEN,
                "status": if valid { "valid" } else { "invalid" },
            });
            return Ok(reply(&mut fake, StatusCode::OK, body, None));
        }
        ("POST", path) if path.starts_with("/chall-http/") => {
            // HTTP-01 (RFC 8555 §8.3): GET /.well-known/acme-challenge/<token> sin TLS.
            let target = fake.http_target.expect("listener HTTP del servidor");
            let key_authorization =
                format!("{TOKEN}.{}", fake.thumbprint.clone().unwrap_or_default());
            drop(fake);
            let served = http_get(target, &format!("/.well-known/acme-challenge/{TOKEN}")).await;
            let valid =
                served.is_some_and(|(status, body)| status == 200 && body == key_authorization);
            let mut fake = state.lock().await;
            fake.validated = valid;
            let n = fake.orders;
            let body = json!({
                "type": "http-01",
                "url": format!("{base}/chall-http/{n}"),
                "token": TOKEN,
                "status": if valid { "valid" } else { "invalid" },
            });
            return Ok(reply(&mut fake, StatusCode::OK, body, None));
        }
        ("POST", path) if path.starts_with("/order/") => {
            let order = order_json(&fake);
            reply(&mut fake, StatusCode::OK, order, None)
        }
        ("POST", path) if path.starts_with("/finalize/") => {
            let csr = payload["csr"]
                .as_str()
                .and_then(|csr| URL_SAFE_NO_PAD.decode(csr).ok())
                .expect("CSR");
            let key = csr_key(&csr);
            let today = clock::day_of(clock::now());
            let mut params =
                rcgen::CertificateParams::new(vec![DOMAIN.to_owned()]).expect("parámetros");
            // El primero ya está en la ventana de renovación; el segundo, no.
            if fake.issued.is_empty() {
                set_validity(&mut params, today - 80, today + 10);
            } else {
                set_validity(&mut params, today - 1, today + 89);
            }
            let issuer = rcgen::Issuer::from_params(&fake.ca_params, &fake.ca_key);
            let leaf = params.signed_by(&key, &issuer).expect("certificado");
            let chain = format!("{}{}", leaf.pem(), fake.ca_pem);
            fake.issued.push(chain);
            let order = order_json(&fake);
            reply(&mut fake, StatusCode::OK, order, None)
        }
        ("POST", path) if path.starts_with("/cert/") => {
            let chain = fake.issued.last().cloned().unwrap_or_default();
            fake.nonce += 1;
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/pem-certificate-chain")
                .header("replay-nonce", format!("nonce-{}", fake.nonce))
                .body(Full::new(Bytes::from(chain)))
                .expect("respuesta")
        }
        _ => reply(
            &mut fake,
            StatusCode::NOT_FOUND,
            json!({ "type": "urn:ietf:params:acme:error:malformed", "detail": path }),
            None,
        ),
    };
    Ok(response)
}

async fn start_fake() -> (Arc<tokio::sync::Mutex<Fake>>, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("directorio ACME falso");
    let base = format!("http://{}", listener.local_addr().expect("dirección"));
    let ca_key = rcgen::KeyPair::generate().expect("clave de la CA");
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("CA");
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "CA de pruebas de coroom");
    let ca_cert = ca_params
        .self_signed(&ca_key)
        .expect("certificado de la CA");
    let state = Arc::new(tokio::sync::Mutex::new(Fake {
        base: base.clone(),
        target: None,
        http_target: None,
        thumbprint: None,
        orders: 0,
        validated: false,
        issued: Vec::new(),
        ca_key,
        ca_params,
        ca_pem: ca_cert.pem(),
        nonce: 0,
    }));
    let shared = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let state = Arc::clone(&shared);
            tokio::spawn(async move {
                let service = hyper::service::service_fn(move |request| {
                    fake_acme(Arc::clone(&state), request)
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    (state, base)
}

/// Cliente HTTP sin TLS para hablar con el directorio falso.
fn plain_http() -> AcmeHttp {
    AcmeHttp::Custom(Arc::new(|| {
        Box::new(
            hyper_util::client::legacy::Client::builder(TokioExecutor::new())
                .build_http::<instant_acme::BodyWrapper<Bytes>>(),
        )
    }))
}

fn acme_config(dir: &std::path::Path, directory: &str) -> Config {
    let mut config = common::config(dir);
    config.public_host = DOMAIN.to_owned();
    config.tls_mode = TlsMode::Acme;
    config.acme_directory = format!("{directory}/directory");
    config.acme_contact = Some("admin@example.com".to_owned());
    config
}

async fn wait_for(what: &str, mut done: impl AsyncFnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done().await {
        assert!(
            Instant::now() < deadline,
            "tiempo agotado esperando: {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acme_issues_renews_and_hot_swaps_the_certificate() {
    let (fake, directory) = start_fake().await;
    let dir = tempfile::tempdir().expect("directorio temporal");
    let server = Server::bind(&acme_config(dir.path(), &directory))
        .await
        .expect("arranque")
        .with_acme_http(plain_http());
    let tcp = server.tcp_addr();
    let certs = server.certificates();
    let temporary = server.fingerprint();
    fake.lock().await.target = Some(tcp);
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(server.run(async move {
        let _ = stop_rx.await;
    }));

    // Sin el ALPN del reto y sin reto en curso, el certificado normal (el temporal).
    let normal = presented(tcp, b"h2").await.expect("handshake normal");
    assert_eq!(
        coroom_transport::crypto::sha256_fingerprint(&normal),
        temporary
    );

    // Dos emisiones: la primera cae en la ventana de renovación y se renueva al momento.
    wait_for("dos certificados", async || {
        fake.lock().await.issued.len() >= 2
    })
    .await;
    wait_for("el segundo instalado", async || {
        certs
            .validity()
            .is_some_and(|(_, not_after)| not_after > clock::now() + 80 * clock::DAY)
    })
    .await;
    assert_eq!(fake.lock().await.orders, 2);
    let installed = certs.fingerprint();
    assert_ne!(installed, temporary);

    // El servidor sirve el certificado nuevo con su clave (handshake completo) y el reto ya no.
    let served = presented(tcp, b"h2").await.expect("handshake con el nuevo");
    assert_eq!(
        coroom_transport::crypto::sha256_fingerprint(&served),
        installed
    );
    assert!(
        presented(tcp, ACME_TLS_ALPN).await.is_none(),
        "sin reto en curso"
    );

    // Cuenta y certificado guardados (clave y cuenta solo para el propietario).
    let acme = dir.path().join("acme");
    for file in ["account.json", "cert.pem", "key.pem"] {
        assert!(acme.join(file).is_file(), "{file}");
    }
    #[cfg(unix)]
    for file in ["account.json", "key.pem"] {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(acme.join(file))
            .expect("metadatos")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{file}");
    }
    let account = std::fs::read_to_string(acme.join("account.json")).expect("cuenta");
    assert!(
        account.contains(&directory),
        "la cuenta recuerda su directorio"
    );
    let _ = stop_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(10), task).await;

    // Al rearrancar se usa el guardado: nada de pedidos nuevos.
    let again = Server::bind(&acme_config(dir.path(), &directory))
        .await
        .expect("rearranque")
        .with_acme_http(plain_http());
    assert_eq!(again.fingerprint(), installed);
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(again.run(async move {
        let _ = stop_rx.await;
    }));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(fake.lock().await.orders, 2);
    let _ = stop_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(10), task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acme_http_01_is_served_on_the_plain_listener() {
    let (fake, directory) = start_fake().await;
    let dir = tempfile::tempdir().expect("directorio temporal");
    let mut config = acme_config(dir.path(), &directory);
    config.acme_challenge = coroom_server::config::AcmeChallenge::Http01;
    config.bind_http = Some("127.0.0.1:0".parse().expect("dirección"));
    let server = Server::bind(&config)
        .await
        .expect("arranque")
        .with_acme_http(plain_http());
    let http = server.http_addr().expect("listener HTTP");
    let tcp = server.tcp_addr();
    let certs = server.certificates();
    fake.lock().await.http_target = Some(http);
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(server.run(async move {
        let _ = stop_rx.await;
    }));

    // Lo que no es un reto se redirige a HTTPS (con el puerto TCP público, aquí el local).
    let redirect = http_get(http, "/admin/").await.expect("redirección");
    assert_eq!(
        redirect,
        (308, format!("https://{DOMAIN}:{}/admin/", tcp.port()))
    );
    assert_eq!(
        http_get(http, "/.well-known/acme-challenge/otro")
            .await
            .map(|(status, _)| status),
        Some(404)
    );

    wait_for("dos certificados por HTTP-01", async || {
        fake.lock().await.issued.len() >= 2
    })
    .await;
    wait_for("el segundo instalado", async || {
        certs
            .validity()
            .is_some_and(|(_, not_after)| not_after > clock::now() + 80 * clock::DAY)
    })
    .await;
    let served = presented(tcp, b"h2").await.expect("handshake");
    assert_eq!(
        coroom_transport::crypto::sha256_fingerprint(&served),
        certs.fingerprint()
    );
    let _ = stop_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(10), task).await;
}
