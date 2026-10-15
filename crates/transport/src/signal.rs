//! Cliente de señalización: sesión WebTransport `/signal` con el servidor de coroom.
//!
//! [`SignalClient::connect`] abre una conexión QUIC (ALPN `h3`) **desde el mismo `Endpoint`** que
//! los medios, establece la sesión WebTransport (módulo interno `webtransport`), abre su stream
//! bidireccional y envía `Auth` con el token de sesión de la cuenta (o el de desarrollo, si el
//! servidor está en ese modo) y la clave pública de la identidad del `Endpoint`, que el servidor
//! exige vinculada al usuario del token. El servidor responde `Observed` con la dirección de
//! origen (el candidato reflejado para la apertura de NAT) o `Error`.

use std::net::SocketAddr;

use tokio::sync::mpsc;

use coroom_proto::codec::{self, FrameBuffer};
use coroom_proto::signal::SignalMessage;
use coroom_proto::{Alpn, PROTOCOL_VERSION};

use crate::TransportError;
use crate::quic::{Endpoint, canonical, spawn_writer};
use crate::tls::ServerTrust;
use crate::webtransport::Session;

/// Ruta por defecto de la sesión WebTransport de señalización.
pub const SIGNAL_PATH: &str = "/signal";

/// URL `https://host[:puerto][/ruta]` ya separada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerUrl {
    /// Nombre o IP del servidor (sin corchetes), para TLS.
    pub host: String,
    /// Puerto UDP (443 si no se indica).
    pub port: u16,
    /// `host:puerto` tal como va en `:authority`.
    pub authority: String,
    /// Ruta de la sesión ([`SIGNAL_PATH`] si no se indica).
    pub path: String,
}

impl ServerUrl {
    /// Interpreta `https://coroom.example.com`, `https://127.0.0.1:4433`, `https://[::1]:4433/signal`…
    pub fn parse(url: &str) -> Result<Self, TransportError> {
        let invalid = |why: &str| TransportError::InvalidUrl(format!("{url}: {why}"));
        let rest = url
            .strip_prefix("https://")
            .ok_or_else(|| invalid("debe empezar por https://"))?;
        let (authority, path) = match rest.find('/') {
            Some(at) => (&rest[..at], &rest[at..]),
            None => (rest, ""),
        };
        if authority.is_empty() || authority.contains('@') {
            return Err(invalid("falta el servidor"));
        }
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (host, after) = v6
                .split_once(']')
                .ok_or_else(|| invalid("IPv6 sin cerrar"))?;
            let port = match after.strip_prefix(':') {
                Some(port) => Some(port),
                None if after.is_empty() => None,
                None => return Err(invalid("puerto no válido")),
            };
            (host, port)
        } else {
            match authority.rsplit_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            }
        };
        let port = match port {
            Some(port) => port.parse().map_err(|_| invalid("puerto no válido"))?,
            None => 443,
        };
        if host.is_empty() {
            return Err(invalid("falta el servidor"));
        }
        let path = if path.is_empty() || path == "/" {
            SIGNAL_PATH.to_owned()
        } else {
            path.to_owned()
        };
        let authority = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        Ok(Self {
            host: host.to_owned(),
            port,
            authority,
            path,
        })
    }
}

/// Mitad de envío clonable de la señalización.
#[derive(Debug, Clone)]
pub struct SignalSender {
    tx: mpsc::Sender<Vec<u8>>,
}

impl SignalSender {
    /// Envía un mensaje (espera si la cola está llena).
    pub async fn send(&self, message: &SignalMessage) -> Result<(), TransportError> {
        let frame = codec::encode(message)?;
        self.tx
            .send(frame)
            .await
            .map_err(|_| TransportError::Closed)
    }
}

/// Sesión de señalización con el servidor.
pub struct SignalClient {
    session: Session,
    sender: SignalSender,
    recv: quinn::RecvStream,
    buffer: FrameBuffer,
    observed: SocketAddr,
    server_addr: SocketAddr,
}

impl std::fmt::Debug for SignalClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalClient")
            .field("server", &self.server_addr)
            .field("observed", &self.observed)
            .finish_non_exhaustive()
    }
}

impl SignalClient {
    /// Conecta con `server_url` desde `endpoint`, se autentica con `token` (el de sesión de la
    /// cuenta, vinculado a la identidad del `endpoint`, o el de desarrollo) y espera `Observed`.
    pub async fn connect(
        server_url: &str,
        token: &str,
        endpoint: &Endpoint,
        trust: &ServerTrust,
    ) -> Result<Self, TransportError> {
        let url = ServerUrl::parse(server_url)?;
        let local = endpoint.local_addr()?;
        let mut addrs: Vec<SocketAddr> = tokio::net::lookup_host((url.host.as_str(), url.port))
            .await?
            .map(canonical)
            .collect();
        // Con el socket solo IPv4 no sirven las direcciones IPv6.
        if local.is_ipv4() {
            addrs.retain(SocketAddr::is_ipv4);
        }
        let server_addr = *addrs
            .first()
            .ok_or_else(|| TransportError::InvalidUrl(format!("{server_url}: no resuelve")))?;
        let (connection, _) = endpoint
            .connect_server(server_addr, &url.host, trust, Alpn::H3)
            .await?;
        let session = Session::connect(connection, &url.authority, &url.path).await?;
        let (send, recv) = session.open_bi().await?;
        let sender = SignalSender {
            tx: spawn_writer(send),
        };
        sender
            .send(&SignalMessage::Auth {
                protocol_version: PROTOCOL_VERSION,
                token: token.to_owned(),
                device_pk: endpoint.public_key(),
            })
            .await?;
        let mut client = Self {
            session,
            sender,
            recv,
            buffer: FrameBuffer::new(),
            observed: server_addr,
            server_addr,
        };
        match client.next_event().await? {
            Some(SignalMessage::Observed { addr }) => client.observed = canonical(addr),
            Some(SignalMessage::Error { code, message }) => {
                return Err(TransportError::Rejected { code, message });
            }
            Some(other) => {
                return Err(TransportError::Protocol(format!(
                    "respuesta inesperada a Auth: {other:?}"
                )));
            }
            None => return Err(TransportError::Closed),
        }
        Ok(client)
    }

    /// Dirección de origen que vio el servidor (candidato reflejado).
    pub fn observed(&self) -> SocketAddr {
        self.observed
    }

    /// Dirección del servidor (la del relay salvo que el `RelayToken` diga otra).
    pub fn server_addr(&self) -> SocketAddr {
        self.server_addr
    }

    /// Mitad de envío clonable.
    pub fn sender(&self) -> SignalSender {
        self.sender.clone()
    }

    /// Envía un mensaje.
    pub async fn send(&self, message: &SignalMessage) -> Result<(), TransportError> {
        self.sender.send(message).await
    }

    /// Siguiente mensaje del servidor; `None` si cerró la sesión. Se puede cancelar sin perder
    /// datos.
    pub async fn next_event(&mut self) -> Result<Option<SignalMessage>, TransportError> {
        loop {
            if let Some(message) = self.buffer.next_message::<SignalMessage>()? {
                return Ok(Some(message));
            }
            match self
                .recv
                .read_chunk(codec::MAX_MESSAGE_LEN, true)
                .await
                .map_err(|err| TransportError::Protocol(err.to_string()))?
            {
                Some(chunk) => self.buffer.extend(&chunk.bytes),
                None if self.buffer.pending() == 0 => return Ok(None),
                None => return Err(TransportError::Closed),
            }
        }
    }

    /// RTT suavizado de la conexión QUIC con el servidor.
    pub fn rtt(&self) -> std::time::Duration {
        self.session.connection().rtt()
    }

    /// Cierra la sesión.
    pub fn close(&self) {
        self.session
            .connection()
            .close(quinn::VarInt::from_u32(0), b"fin");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_parsed() {
        let url = ServerUrl::parse("https://coroom.example.com").expect("url");
        assert_eq!(url.host, "coroom.example.com");
        assert_eq!(url.port, 443);
        assert_eq!(url.authority, "coroom.example.com:443");
        assert_eq!(url.path, SIGNAL_PATH);

        let url = ServerUrl::parse("https://127.0.0.1:4433/").expect("url");
        assert_eq!((url.host.as_str(), url.port), ("127.0.0.1", 4433));
        assert_eq!(url.path, SIGNAL_PATH);

        let url = ServerUrl::parse("https://[::1]:4433/otra").expect("url");
        assert_eq!(url.host, "::1");
        assert_eq!(url.authority, "[::1]:4433");
        assert_eq!(url.path, "/otra");

        for bad in [
            "http://coroom.example.com",
            "https://",
            "https://[::1",
            "https://coroom.example.com:x",
            "https://user@coroom.example.com",
        ] {
            assert!(ServerUrl::parse(bad).is_err(), "{bad}");
        }
    }
}
