//! Pila TCP: HTTP/2 y HTTP/1.1 sobre TLS (`tokio-rustls` + `hyper`) con `Alt-Svc` hacia HTTP/3.
//!
//! - Con TLS (por defecto), cada conexión hace el handshake con el [`CertResolver`] compartido
//!   con QUIC y se sirve HTTP/2 o HTTP/1.1 según el ALPN con las rutas comunes
//!   ([`crate::routes::dispatch`]).
//! - `COROOM_TCP_PLAIN=true` (solo desarrollo) sirve lo mismo sin TLS.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderValue, Request, Response, StatusCode, header};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;
use tracing::debug;

use crate::ServerState;
use crate::routes::{self, MAX_BODY, dispatch};

/// Espera máxima del handshake TLS.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Espera máxima de las cabeceras de cada petición HTTP/1.1, también entre peticiones de una
/// conexión que se mantiene abierta (`docs/seguridad.md`, hallazgo S-4: sin ella, un cliente que
/// envía las cabeceras byte a byte ocupaba la conexión para siempre).
pub const HEADER_TIMEOUT: Duration = Duration::from_secs(10);

/// Espera máxima del cuerpo de una petición (como mucho `MAX_BODY`, 64 KiB).
pub const BODY_TIMEOUT: Duration = Duration::from_secs(20);

/// Intervalo de los `PING` de HTTP/2 y espera de su respuesta: una conexión HTTP/2 que no responde
/// se cierra.
const HTTP2_KEEP_ALIVE: Duration = Duration::from_secs(30);

/// Conexiones TCP abiertas a la vez como mucho; las demás se cierran al aceptarlas.
pub const MAX_TCP_CONNECTIONS: usize = 1024;

/// Valor de `Alt-Svc` que anuncia HTTP/3 en `udp_port` durante un día.
pub fn alt_svc_value(udp_port: u16) -> String {
    format!("h3=\":{udp_port}\"; ma=86400")
}

type Body = Full<Bytes>;

fn simple(
    status: StatusCode,
    content_type: &'static str,
    body: impl Into<Bytes>,
) -> Response<Body> {
    let mut response = Response::new(Full::new(body.into()));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

async fn handle(
    state: Arc<ServerState>,
    remote: SocketAddr,
    alt_svc: HeaderValue,
    request: Request<Incoming>,
) -> Result<Response<Body>, Infallible> {
    let (parts, body) = request.into_parts();
    let collected =
        tokio::time::timeout(BODY_TIMEOUT, Limited::new(body, MAX_BODY).collect()).await;
    let body = match collected {
        Ok(Ok(collected)) => collected.to_bytes(),
        Err(_) => {
            let response = routes::Response::error(
                StatusCode::REQUEST_TIMEOUT,
                "timeout",
                "la petición tardó demasiado en llegar",
            );
            return Ok(simple(
                response.status,
                response.content_type,
                response.body,
            ));
        }
        Ok(Err(_)) => {
            let response = routes::Response::error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
                "la petición es demasiado grande",
            );
            return Ok(simple(
                response.status,
                response.content_type,
                response.body,
            ));
        }
    };
    let request = routes::Request {
        method: parts.method.clone(),
        path: parts.uri.path().to_owned(),
        query: parts.uri.query().map(str::to_owned),
        authorization: parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        body,
        remote: remote.ip().to_canonical(),
    };
    let reply = dispatch(&state, request).await;
    debug!(method = %parts.method, path = parts.uri.path(), status = %reply.status, "TCP");
    let mut response = simple(reply.status, reply.content_type, reply.body);
    let headers = response.headers_mut();
    for (name, value) in reply.headers {
        headers.insert(name, HeaderValue::from_static(value));
    }
    headers.insert(header::ALT_SVC, alt_svc);
    Ok(response)
}

async fn serve_connection<S>(
    io: S,
    state: Arc<ServerState>,
    remote: SocketAddr,
    alt_svc: HeaderValue,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let service =
        service_fn(move |request| handle(Arc::clone(&state), remote, alt_svc.clone(), request));
    let mut builder = auto::Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_TIMEOUT);
    builder
        .http2()
        .timer(TokioTimer::new())
        .keep_alive_interval(HTTP2_KEEP_ALIVE)
        .keep_alive_timeout(HTTP2_KEEP_ALIVE);
    if let Err(err) = builder.serve_connection(TokioIo::new(io), service).await {
        debug!(%remote, error = %err, "conexión TCP terminada");
    }
}

/// Sirve las rutas por TCP (con TLS si `acceptor`) hasta que `shutdown` cambia.
pub(crate) async fn serve(
    listener: TcpListener,
    acceptor: Option<TlsAcceptor>,
    udp_port: u16,
    state: Arc<ServerState>,
    mut shutdown: watch::Receiver<bool>,
) -> std::io::Result<()> {
    let alt_svc = HeaderValue::from_str(&alt_svc_value(udp_port))
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    let mut connections = JoinSet::new();
    loop {
        let (stream, remote) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(err) => {
                    debug!(error = %err, "accept TCP fallido");
                    continue;
                }
            },
            _ = shutdown.changed() => break,
        };
        while connections.try_join_next().is_some() {}
        if connections.len() >= MAX_TCP_CONNECTIONS {
            debug!(%remote, "demasiadas conexiones TCP; se cierra la nueva");
            drop(stream);
            continue;
        }
        let state = Arc::clone(&state);
        let alt_svc = alt_svc.clone();
        let acceptor = acceptor.clone();
        connections.spawn(async move {
            let _ = stream.set_nodelay(true);
            let Some(acceptor) = acceptor else {
                serve_connection(stream, state, remote, alt_svc).await;
                return;
            };
            let tls = match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                Ok(Ok(tls)) => tls,
                Ok(Err(err)) => {
                    debug!(%remote, error = %err, "handshake TLS fallido");
                    return;
                }
                Err(_) => return,
            };
            serve_connection(tls, state, remote, alt_svc).await;
        });
    }
    // Un momento para las respuestas en curso; después se cortan.
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    connections.abort_all();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_svc_announces_the_udp_port() {
        assert_eq!(alt_svc_value(443), "h3=\":443\"; ma=86400");
    }
}
