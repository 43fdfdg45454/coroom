//! Socket QUIC del servidor: demultiplexado por ALPN entre HTTP/3 (`h3`, con WebTransport para la
//! señalización) y el relay de medios (`coroom-relay/1`).

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::{Buf, Bytes};
use h3::ext::Protocol;
use h3::server::RequestStream;
use h3_webtransport::server::{AcceptedBi, WebTransportSession};
use http::{Method, Request, Response, StatusCode, header};
use tokio::sync::watch;
use tracing::{debug, info, warn};

use coroom_transport::proto::Alpn;
use coroom_transport::quic::canonical;

use crate::ServerState;
use crate::routes::{self, MAX_BODY, dispatch};
use crate::signal::SIGNAL_PATH;

/// Acepta conexiones QUIC hasta que `shutdown` cambia y luego cierra el endpoint.
pub(crate) async fn serve(
    endpoint: quinn::Endpoint,
    state: Arc<ServerState>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break };
                tokio::spawn(handle_incoming(incoming, Arc::clone(&state)));
            }
            _ = shutdown.changed() => break,
        }
    }
    info!("cerrando el endpoint QUIC");
    endpoint.close(quinn::VarInt::from_u32(0), b"apagado");
    // Da a los pares un momento para recibir el cierre; no se espera indefinidamente.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), endpoint.wait_idle()).await;
}

/// Completa el handshake y despacha por el ALPN negociado.
async fn handle_incoming(incoming: quinn::Incoming, state: Arc<ServerState>) {
    let remote = incoming.remote_address();
    let mut connecting = match incoming.accept() {
        Ok(connecting) => connecting,
        Err(err) => {
            debug!(%remote, error = %err, "conexión QUIC rechazada");
            return;
        }
    };
    let alpn = connecting
        .handshake_data()
        .await
        .ok()
        .and_then(|data| data.downcast::<quinn::crypto::rustls::HandshakeData>().ok())
        .and_then(|data| data.protocol)
        .and_then(|protocol| Alpn::from_bytes(&protocol));
    let connection = match connecting.await {
        Ok(connection) => connection,
        Err(err) => {
            debug!(%remote, error = %err, "handshake QUIC fallido");
            return;
        }
    };
    debug!(%remote, ?alpn, "conexión QUIC aceptada");
    match alpn {
        Some(Alpn::H3) => serve_h3(connection, state).await,
        Some(Alpn::Relay) if state.config.relay_enabled => {
            Arc::clone(&state.relay).handle(connection).await
        }
        _ => connection.close(quinn::VarInt::from_u32(0x0178), b"ALPN no admitido"),
    }
}

/// Dirección del relay que se anuncia en `RelayToken` a un cliente de esta conexión.
fn relay_addr(state: &ServerState, connection: &quinn::Connection) -> SocketAddr {
    if let Some(addr) = state.relay_addr {
        return addr;
    }
    let udp = state.udp_addr;
    if !udp.ip().is_unspecified() {
        return canonical(udp);
    }
    let ip = connection
        .local_ip()
        .map(|ip| ip.to_canonical())
        .unwrap_or_else(|| udp.ip());
    SocketAddr::new(ip, udp.port())
}

async fn serve_h3(connection: quinn::Connection, state: Arc<ServerState>) {
    let remote = canonical(connection.remote_address());
    let relay = relay_addr(&state, &connection);
    let mut h3_conn = match h3::server::builder()
        .enable_webtransport(true)
        .enable_extended_connect(true)
        .enable_datagram(true)
        .max_webtransport_sessions(1)
        .send_grease(true)
        .build(h3_quinn::Connection::new(connection))
        .await
    {
        Ok(conn) => conn,
        Err(err) => {
            // También pasa cuando el cliente cierra nada más conectar (la autocomprobación de UDP).
            debug!(%remote, error = %err, "no se pudo iniciar HTTP/3");
            return;
        }
    };

    loop {
        match h3_conn.accept().await {
            Ok(Some(resolver)) => {
                let (request, stream) = match resolver.resolve_request().await {
                    Ok(parts) => parts,
                    Err(err) => {
                        debug!(error = %err, "petición HTTP/3 no válida");
                        continue;
                    }
                };
                let webtransport = request.method() == Method::CONNECT
                    && request.extensions().get::<Protocol>() == Some(&Protocol::WEB_TRANSPORT);
                if !webtransport {
                    tokio::spawn(handle_request(request, stream, remote, Arc::clone(&state)));
                    continue;
                }
                if request.uri().path() != SIGNAL_PATH {
                    tokio::spawn(reply_status(stream, StatusCode::NOT_FOUND));
                    continue;
                }
                match WebTransportSession::accept(request, stream, h3_conn).await {
                    Ok(session) => serve_signal(session, remote, relay, state).await,
                    Err(err) => debug!(%remote, error = %err, "sesión WebTransport fallida"),
                }
                return;
            }
            Ok(None) => break,
            Err(err) if err.is_h3_no_error() => break,
            Err(err) => {
                debug!(%remote, error = %err, "conexión HTTP/3 terminada");
                break;
            }
        }
    }
}

type Session = WebTransportSession<h3_quinn::Connection, Bytes>;

/// Sesión WebTransport `/signal`: el primer stream bidireccional es el de señalización.
async fn serve_signal(
    session: Session,
    remote: SocketAddr,
    relay: SocketAddr,
    state: Arc<ServerState>,
) {
    let mut signal: Option<tokio::task::JoinHandle<()>> = None;
    let mut signal_done = false;
    loop {
        let accepted = match &mut signal {
            Some(task) => tokio::select! {
                accepted = session.accept_bi() => accepted,
                _ = task => {
                    signal_done = true;
                    break;
                }
            },
            None => session.accept_bi().await,
        };
        match accepted {
            Ok(Some(AcceptedBi::BidiStream(_, stream))) if signal.is_none() => {
                let hub = Arc::clone(&state.hub);
                signal = Some(tokio::spawn(hub.run(stream, remote, relay)));
            }
            // Solo hay un stream de señalización por sesión; los demás se ignoran.
            Ok(Some(AcceptedBi::BidiStream(..))) => {}
            Ok(Some(AcceptedBi::Request(_, stream))) => {
                tokio::spawn(reply_status(stream, StatusCode::NOT_FOUND));
            }
            Ok(None) | Err(_) => break,
        }
    }
    match signal {
        // El servidor terminó la sesión (rechazo, expulsión): se espera a que el cliente cierre
        // para que le lleguen los últimos mensajes (el `Error`), con un límite.
        Some(_) if signal_done => {
            let _ = tokio::time::timeout(LINGER, async {
                while let Ok(Some(_)) = session.accept_bi().await {}
            })
            .await;
        }
        // El cliente se fue: la sesión termina sola al fallar la lectura y saca al par de la sala.
        Some(mut task) => {
            if tokio::time::timeout(LINGER, &mut task).await.is_err() {
                task.abort();
            }
        }
        None => {}
    }
}

/// Espera máxima al cierre ordenado de una sesión de señalización.
const LINGER: std::time::Duration = std::time::Duration::from_secs(3);

async fn reply_status<S>(mut stream: RequestStream<S, Bytes>, status: StatusCode)
where
    S: h3::quic::BidiStream<Bytes>,
{
    if let Ok(response) = Response::builder().status(status).body(()) {
        let _ = stream.send_response(response).await;
        let _ = stream.finish().await;
    }
}

async fn handle_request<S>(
    request: Request<()>,
    mut stream: RequestStream<S, Bytes>,
    remote: SocketAddr,
    state: Arc<ServerState>,
) where
    S: h3::quic::BidiStream<Bytes>,
{
    // Cuerpo, como mucho `MAX_BODY`.
    let mut body = Vec::new();
    let mut too_large = false;
    loop {
        match stream.recv_data().await {
            Ok(Some(mut chunk)) => {
                while chunk.has_remaining() {
                    let part = chunk.chunk();
                    if body.len() + part.len() > MAX_BODY {
                        too_large = true;
                    } else {
                        body.extend_from_slice(part);
                    }
                    let len = part.len();
                    chunk.advance(len);
                }
            }
            Ok(None) => break,
            Err(err) => {
                debug!(error = %err, "cuerpo HTTP/3 no válido");
                return;
            }
        }
    }
    let reply = if too_large {
        routes::Response::error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "la petición es demasiado grande",
        )
    } else {
        let (parts, ()) = request.into_parts();
        let request = routes::Request {
            method: parts.method,
            path: parts.uri.path().to_owned(),
            query: parts.uri.query().map(str::to_owned),
            authorization: parts
                .headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            body: Bytes::from(body),
            remote: remote.ip().to_canonical(),
        };
        debug!(method = %request.method, path = %request.path, "HTTP/3");
        dispatch(&state, request).await
    };

    let mut builder = Response::builder()
        .status(reply.status)
        .header(header::CONTENT_TYPE, reply.content_type);
    for (name, value) in &reply.headers {
        builder = builder.header(*name, *value);
    }
    let response = match builder.body(()) {
        Ok(response) => response,
        Err(err) => {
            warn!(error = %err, "respuesta HTTP/3 no válida");
            return;
        }
    };

    let result = async {
        stream.send_response(response).await?;
        stream.send_data(Bytes::from(reply.body)).await?;
        stream.finish().await
    }
    .await;
    if let Err(err) = result {
        debug!(error = %err, "no se pudo enviar la respuesta HTTP/3");
    }
}
