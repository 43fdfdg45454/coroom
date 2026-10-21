//! Descarga de ficheros grandes por HTTPS (los paquetes de una versión nueva).
//!
//! [`get`] abre un `GET` por TCP con TLS 1.3 (HTTP/2, o HTTP/1.1 si el servidor no negocia `h2`),
//! sigue las redirecciones (`browser_download_url` de GitHub responde 302 hacia su servidor de
//! ficheros) y entrega el cuerpo por trozos ([`Response::chunk`]) sin juntarlo en memoria. Con
//! `from` pide el resto desde ese byte (`Range: bytes=N-`, RFC 9110 §14): si el servidor lo admite
//! responde 206 y la descarga sigue donde se quedó; si no, 200 con el fichero entero.
//!
//! Solo `https://`: una redirección a otro esquema se rechaza. No envía credenciales.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderValue, Method, Request, StatusCode, header};
use http_body_util::{BodyExt, Empty};
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::pki_types::ServerName;

use crate::TransportError;
use crate::eyeballs::first_to_connect;
use crate::quic::canonical;
use crate::tls::{self, ServerTrust};

/// Espera máxima de la conexión TCP y el handshake TLS.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Espera máxima de la cabecera de la respuesta.
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(20);

/// Espera máxima sin que llegue ningún byte del cuerpo.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Redirecciones que se siguen como mucho.
pub const MAX_REDIRECTS: usize = 5;

/// Una URL `https://` partida.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    host: String,
    port: u16,
    /// `host` o `host:puerto` (sin el 443), como va en `Host` y `:authority`.
    authority: String,
    /// Ruta con la consulta (`/a/b?c=d`).
    path: String,
}

impl Target {
    fn parse(url: &str) -> Result<Self, TransportError> {
        let invalid = |why: &str| TransportError::InvalidUrl(format!("{url}: {why}"));
        let rest = url
            .strip_prefix("https://")
            .ok_or_else(|| invalid("debe empezar por https://"))?;
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(end);
        if authority.is_empty() || authority.contains('@') {
            return Err(invalid("falta el servidor"));
        }
        let (host, port) = match authority.strip_prefix('[') {
            Some(v6) => {
                let (host, after) = v6
                    .split_once(']')
                    .ok_or_else(|| invalid("IPv6 sin cerrar"))?;
                (host, after.strip_prefix(':'))
            }
            None => match authority.rsplit_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            },
        };
        if host.is_empty() {
            return Err(invalid("falta el servidor"));
        }
        let port = match port {
            Some(port) => port.parse().map_err(|_| invalid("puerto no válido"))?,
            None => 443,
        };
        let shown = if host.contains(':') {
            format!("[{host}]")
        } else {
            host.to_owned()
        };
        let authority = if port == 443 {
            shown
        } else {
            format!("{shown}:{port}")
        };
        // Sin el fragmento: no viaja al servidor.
        let tail = tail.split('#').next().unwrap_or_default();
        let path = if tail.starts_with('/') {
            tail.to_owned()
        } else {
            format!("/{tail}")
        };
        Ok(Self {
            host: host.to_owned(),
            port,
            authority,
            path,
        })
    }

    /// Resuelve el `Location` de una redirección (absoluto, relativo a la raíz o al directorio).
    fn join(&self, location: &str) -> Result<Target, TransportError> {
        if location.starts_with("https://") {
            return Target::parse(location);
        }
        if location.contains("://") {
            return Err(TransportError::Protocol(format!(
                "redirección fuera de https: {location}"
            )));
        }
        if let Some(rest) = location.strip_prefix("//") {
            return Target::parse(&format!("https://{rest}"));
        }
        let path = if location.starts_with('/') {
            location.to_owned()
        } else {
            let base = self.path.split('?').next().unwrap_or("/");
            let dir = base.rsplit_once('/').map_or("", |(dir, _)| dir);
            format!("{dir}/{location}")
        };
        Ok(Target {
            path,
            ..self.clone()
        })
    }

    fn url(&self) -> String {
        format!("https://{}{}", self.authority, self.path)
    }
}

/// Respuesta en curso: el estado, lo que dicen sus cabeceras del tamaño y el cuerpo por trozos.
#[derive(Debug)]
pub struct Response {
    /// Estado HTTP (tras las redirecciones).
    pub status: StatusCode,
    /// URL final (tras las redirecciones).
    pub url: String,
    /// `Content-Length` del cuerpo, si lo dice.
    pub content_length: Option<u64>,
    /// Con un 206, el primer byte que llega y el tamaño total (`Content-Range: bytes A-B/T`).
    pub range: Option<(u64, Option<u64>)>,
    body: Incoming,
    connection: tokio::task::JoinHandle<()>,
}

impl Drop for Response {
    fn drop(&mut self) {
        self.connection.abort();
    }
}

impl Response {
    /// El siguiente trozo del cuerpo (`None` al terminar). Falla si en [`IDLE_TIMEOUT`] no llega
    /// nada.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, TransportError> {
        loop {
            let frame = tokio::time::timeout(IDLE_TIMEOUT, self.body.frame())
                .await
                .map_err(|_| TransportError::Timeout("datos de la descarga"))?;
            match frame {
                None => return Ok(None),
                Some(Err(err)) => {
                    return Err(TransportError::Protocol(format!(
                        "cuerpo de la descarga: {err}"
                    )));
                }
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data()
                        && !data.is_empty()
                    {
                        return Ok(Some(data));
                    }
                }
            }
        }
    }
}

/// `GET url` con las raíces de confianza del sistema, siguiendo redirecciones, con `headers` de
/// más (el `User-Agent`) y, si `from` es mayor que 0, pidiendo desde ese byte.
pub async fn get(
    url: &str,
    headers: &[(&'static str, String)],
    from: u64,
) -> Result<Response, TransportError> {
    let trust = ServerTrust::system_roots()?;
    get_with(url, &trust, headers, from).await
}

/// Como [`get`], con otra confianza (las pruebas usan un servidor con certificado propio).
pub async fn get_with(
    url: &str,
    trust: &ServerTrust,
    headers: &[(&'static str, String)],
    from: u64,
) -> Result<Response, TransportError> {
    let mut extra = Vec::with_capacity(headers.len() + 1);
    for (name, value) in headers {
        let invalid = || TransportError::Protocol(format!("cabecera {name} no válida"));
        let key = header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| invalid())?;
        let value = HeaderValue::from_str(value).map_err(|_| invalid())?;
        extra.push((key, value));
    }
    if from > 0 {
        let value = HeaderValue::from_str(&format!("bytes={from}-"))
            .map_err(|_| TransportError::Protocol("rango no válido".to_owned()))?;
        extra.push((header::RANGE, value));
    }
    let config = Arc::new(tls::server_client_config_with(
        trust,
        &[b"h2", b"http/1.1"],
    )?);
    let mut target = Target::parse(url)?;
    for _ in 0..=MAX_REDIRECTS {
        let response = tokio::time::timeout(
            CONNECT_TIMEOUT + RESPONSE_TIMEOUT,
            request(&target, &config, &extra),
        )
        .await
        .map_err(|_| TransportError::Timeout("respuesta del servidor de la descarga"))??;
        let status = response.status;
        if status.is_redirection() && status != StatusCode::NOT_MODIFIED {
            let location = response.location.as_deref().ok_or_else(|| {
                TransportError::Protocol(format!("redirección {status} sin Location"))
            })?;
            target = target.join(location)?;
            continue;
        }
        return Ok(Response {
            status,
            url: target.url(),
            content_length: response.content_length,
            range: response.range,
            body: response.body,
            connection: response.connection,
        });
    }
    Err(TransportError::Protocol(format!(
        "más de {MAX_REDIRECTS} redirecciones"
    )))
}

struct Raw {
    status: StatusCode,
    location: Option<String>,
    content_length: Option<u64>,
    range: Option<(u64, Option<u64>)>,
    body: Incoming,
    connection: tokio::task::JoinHandle<()>,
}

/// `Content-Range: bytes A-B/T` (o `/*`): el primer byte y el total si se sabe.
fn content_range(value: &str) -> Option<(u64, Option<u64>)> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let (span, total) = rest.split_once('/')?;
    let (start, _) = span.split_once('-')?;
    let total = match total.trim() {
        "*" => None,
        total => Some(total.parse().ok()?),
    };
    Some((start.trim().parse().ok()?, total))
}

async fn request(
    target: &Target,
    config: &Arc<rustls::ClientConfig>,
    extra: &[(header::HeaderName, HeaderValue)],
) -> Result<Raw, TransportError> {
    let connector = tokio_rustls::TlsConnector::from(Arc::clone(config));
    let name = ServerName::try_from(target.host.clone())
        .map_err(|_| TransportError::InvalidUrl(format!("{}: nombre no válido", target.host)))?;
    let addrs: Vec<_> = tokio::net::lookup_host((target.host.as_str(), target.port))
        .await?
        .map(canonical)
        .collect();
    if addrs.is_empty() {
        return Err(TransportError::InvalidUrl(format!(
            "{}: no resuelve",
            target.host
        )));
    }
    let tls = tokio::time::timeout(CONNECT_TIMEOUT, async {
        let tcp = first_to_connect(&target.host, addrs, |addr| {
            Box::pin(async move { Ok(tokio::net::TcpStream::connect(addr).await?) })
        })
        .await?;
        Ok::<_, TransportError>(connector.connect(name, tcp).await?)
    })
    .await
    .map_err(|_| TransportError::Timeout("conexión TLS con el servidor de la descarga"))??;
    let h2 = tls.get_ref().1.alpn_protocol() == Some(b"h2");
    let io = TokioIo::new(tls);
    let build = |uri: String| {
        let mut builder = Request::builder().method(Method::GET).uri(uri);
        for (name, value) in extra {
            builder = builder.header(name, value);
        }
        builder
    };
    let protocol = |what: &str, err: &dyn std::fmt::Display| {
        TransportError::Protocol(format!("{what}: {err}"))
    };
    let (response, connection) = if h2 {
        let (mut sender, connection) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), io)
                .await
                .map_err(|err| protocol("HTTP/2", &err))?;
        let connection = tokio::spawn(async move {
            let _ = connection.await;
        });
        let request = build(target.url())
            .body(Empty::<Bytes>::new())
            .map_err(|err| protocol("petición", &err))?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|err| protocol("HTTP/2", &err))?;
        (response, connection)
    } else {
        let (mut sender, connection) = hyper::client::conn::http1::handshake(io)
            .await
            .map_err(|err| protocol("HTTP/1.1", &err))?;
        let connection = tokio::spawn(async move {
            let _ = connection.await;
        });
        let request = build(target.path.clone())
            .header(header::HOST, &target.authority)
            .body(Empty::<Bytes>::new())
            .map_err(|err| protocol("petición", &err))?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|err| protocol("HTTP/1.1", &err))?;
        (response, connection)
    };
    let headers = response.headers();
    let text = |name: header::HeaderName| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    let location = text(header::LOCATION);
    let content_length = text(header::CONTENT_LENGTH).and_then(|value| value.trim().parse().ok());
    let range = text(header::CONTENT_RANGE).and_then(|value| content_range(&value));
    Ok(Raw {
        status: response.status(),
        location,
        content_length,
        range,
        body: response.into_body(),
        connection,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn urls_and_redirections_resolve() {
        let target = Target::parse("https://github.com/ana/coroom/releases/download/v2.1.0/x.zip")
            .expect("url");
        assert_eq!(target.host, "github.com");
        assert_eq!(target.port, 443);
        assert_eq!(target.authority, "github.com");
        assert_eq!(target.path, "/ana/coroom/releases/download/v2.1.0/x.zip");
        let moved = target
            .join("https://files.example.com:8443/a/b?firma=1#frag")
            .expect("absoluta");
        assert_eq!(moved.authority, "files.example.com:8443");
        assert_eq!(moved.path, "/a/b?firma=1");
        assert_eq!(target.join("/otra").expect("raíz").path, "/otra");
        assert_eq!(
            target.join("y.zip").expect("relativa").path,
            "/ana/coroom/releases/download/v2.1.0/y.zip"
        );
        assert_eq!(
            target
                .join("//cdn.example.com/z")
                .expect("sin esquema")
                .host,
            "cdn.example.com"
        );
        assert!(target.join("http://example.com/x").is_err());
        assert!(Target::parse("http://example.com").is_err());
        assert_eq!(
            Target::parse("https://[::1]:4433").expect("v6").authority,
            "[::1]:4433"
        );
        assert_eq!(content_range("bytes 10-99/100"), Some((10, Some(100))));
        assert_eq!(content_range("bytes 10-99/*"), Some((10, None)));
        assert_eq!(content_range("otra cosa"), None);
    }

    /// Servidor HTTP/1.1 por TLS mínimo: `/a` redirige a `/b`, y `/b` sirve `body` respetando
    /// `Range` solo si `ranges`.
    async fn server(body: Vec<u8>, ranges: bool) -> (std::net::SocketAddr, String) {
        let key = rcgen::KeyPair::generate().expect("clave");
        let params = rcgen::CertificateParams::new(vec!["localhost".to_owned()]).expect("params");
        let cert = params.self_signed(&key).expect("certificado");
        let fingerprint = coroom_crypto::sha256_fingerprint(cert.der());
        let der = rustls::pki_types::PrivateKeyDer::try_from(key.serialize_der()).expect("clave");
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("versiones")
        .with_no_client_auth()
        .with_single_cert(vec![cert.der().clone()], der)
        .expect("config");
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("escuchar");
        let addr = listener.local_addr().expect("dirección");
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let body = body.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let mut request = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match tls.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&buf[..n]),
                        }
                    }
                    let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
                    let head = if request.starts_with("get /a ") {
                        "HTTP/1.1 302 Found\r\nlocation: /b\r\ncontent-length: 0\r\n\r\n"
                            .to_owned()
                            .into_bytes()
                    } else {
                        let from = request
                            .lines()
                            .find_map(|line| line.strip_prefix("range: bytes="))
                            .and_then(|rest| rest.trim_end_matches('-').parse::<usize>().ok())
                            .filter(|_| ranges);
                        let mut out = match from {
                            Some(from) => format!(
                                "HTTP/1.1 206 Partial Content\r\ncontent-range: bytes {from}-{}/{}\r\ncontent-length: {}\r\n\r\n",
                                body.len() - 1,
                                body.len(),
                                body.len() - from
                            )
                            .into_bytes(),
                            None => format!(
                                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n",
                                body.len()
                            )
                            .into_bytes(),
                        };
                        out.extend_from_slice(&body[from.unwrap_or(0)..]);
                        out
                    };
                    let _ = tls.write_all(&head).await;
                    let _ = tls.shutdown().await;
                });
            }
        });
        (addr, fingerprint)
    }

    async fn read_all(response: &mut Response) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(chunk) = response.chunk().await.expect("trozo") {
            out.extend_from_slice(&chunk);
        }
        out
    }

    #[tokio::test]
    async fn follows_redirects_and_resumes_with_range() {
        let body: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
        let (addr, fingerprint) = server(body.clone(), true).await;
        let trust = ServerTrust::Fingerprint(fingerprint);
        let url = format!("https://localhost:{}/a", addr.port());
        let headers = [("user-agent", "coroom/prueba".to_owned())];
        let mut whole = get_with(&url, &trust, &headers, 0).await.expect("get");
        assert_eq!(whole.status, StatusCode::OK);
        assert!(whole.url.ends_with("/b"), "{}", whole.url);
        assert_eq!(whole.content_length, Some(body.len() as u64));
        assert_eq!(read_all(&mut whole).await, body);

        let mut rest = get_with(&url, &trust, &headers, 20_000)
            .await
            .expect("rango");
        assert_eq!(rest.status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(rest.range, Some((20_000, Some(body.len() as u64))));
        assert_eq!(read_all(&mut rest).await, body[20_000..]);
    }

    #[tokio::test]
    async fn a_server_without_ranges_sends_everything_again() {
        let body = b"coroom de ejemplo".repeat(100);
        let (addr, fingerprint) = server(body.clone(), false).await;
        let trust = ServerTrust::Fingerprint(fingerprint);
        let url = format!("https://localhost:{}/b", addr.port());
        let mut response = get_with(&url, &trust, &[], 300).await.expect("get");
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.range, None);
        assert_eq!(read_all(&mut response).await, body);
    }
}
