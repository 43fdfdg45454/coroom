//! Apertura de NAT y conexión al relay.
//!
//! [`Connector::connect`] lanza en paralelo un `connect` QUIC hacia cada candidato remoto (IPv6
//! global primero, luego IPv4 pública —normalmente la reflejada que da el servidor con `Observed`—
//! y por último las locales) mientras acepta las conexiones entrantes del mismo par en el mismo
//! socket: es la apertura simultánea, que abre las dos NAT. Gana la primera conexión establecida y
//! verificada (TLS con la identidad esperada); si en los dos sentidos se establece una, las dos
//! partes se quedan con la que inició el par de clave menor, con una gracia de
//! [`PREFER_GRACE`] para que coincidan. Las demás se cierran. Si en [`PUNCH_TIMEOUT`] no hay
//! ninguna, conecta con el relay del servidor ([`connect_relay`]).
//!
//! Relay: conexión QUIC al servidor con ALPN `coroom-relay/1`; un primer stream de autenticación
//! con `Hello` y el `RelayToken` de la señalización. Después los datagramas y el stream de control
//! se usan igual que en directo.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::debug;

use coroom_proto::codec;
use coroom_proto::control::{ControlMessage, Hello, Role};
use coroom_proto::{Alpn, PROTOCOL_VERSION};

use crate::TransportError;
use crate::quic::{ControlReceiver, Endpoint, HELLO_TIMEOUT, Path, PeerConnection};
use crate::tls::{PublicKey, ServerTrust};

/// Tiempo de la apertura simultánea antes de recurrir al relay.
pub const PUNCH_TIMEOUT: Duration = Duration::from_secs(3);

/// Espera a la conexión preferida cuando la primera en llegar no lo es.
pub const PREFER_GRACE: Duration = Duration::from_millis(250);

/// Separación entre los intentos de cada candidato (en el orden de preferencia).
const ATTEMPT_STAGGER: Duration = Duration::from_millis(10);

/// Tipo del stream de autenticación hacia el relay (primer byte).
pub const STREAM_RELAY_AUTH: u8 = 0x03;

/// Datos para conectar con el relay de la sala.
#[derive(Debug, Clone)]
pub struct RelayInfo {
    /// Dirección UDP del relay (la del `RelayToken`).
    pub addr: SocketAddr,
    /// Nombre del servidor para TLS (el de su certificado).
    pub server_name: String,
    /// Confianza en su certificado.
    pub trust: ServerTrust,
    /// Token del `RelayToken`.
    pub token: Vec<u8>,
    /// Papel con el que se entra.
    pub role: Role,
}

/// Conexión establecida y su ruta.
#[derive(Debug, Clone)]
pub struct Established {
    /// La conexión.
    pub connection: PeerConnection,
    /// Directa o por el relay.
    pub path: Path,
}

fn is_link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                // 100.64.0.0/10 (CGNAT).
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback() || is_link_local(ip) || (v6.segments()[0] & 0xfe00) == 0xfc00
        }
    }
}

/// Candidatos locales: las direcciones de las interfaces con el puerto del `Endpoint`, sin
/// loopback ni enlaces locales. Si el `Endpoint` escucha en una IP concreta, solo esa.
pub fn local_candidates(endpoint: &Endpoint) -> Result<Vec<SocketAddr>, TransportError> {
    let local = endpoint.local_addr()?;
    if !local.ip().is_unspecified() {
        return Ok(vec![crate::quic::canonical(local)]);
    }
    let port = local.port();
    let dual_stack = local.is_ipv6();
    let mut out: Vec<SocketAddr> = if_addrs::get_if_addrs()?
        .into_iter()
        .map(|iface| iface.ip())
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified() && !ip.is_multicast())
        .filter(|ip| !is_link_local(*ip))
        .filter(|ip| dual_stack || ip.is_ipv4())
        .map(|ip| SocketAddr::new(ip, port))
        .collect();
    out.sort();
    out.dedup();
    Ok(out)
}

/// Ordena candidatos: IPv6 global, IPv4 pública (reflejada), y después las privadas y locales.
pub fn order_candidates(candidates: &[SocketAddr]) -> Vec<SocketAddr> {
    let class = |addr: &SocketAddr| match addr.ip() {
        IpAddr::V6(_) if !is_private(addr.ip()) => 0,
        IpAddr::V4(_) if !is_private(addr.ip()) => 1,
        _ => 2,
    };
    let mut ordered: Vec<SocketAddr> = candidates
        .iter()
        .map(|addr| crate::quic::canonical(*addr))
        .collect();
    ordered.sort_by_key(class);
    let mut seen = std::collections::HashSet::new();
    ordered.retain(|addr| seen.insert(*addr));
    ordered
}

/// Conecta con un par en directo o, si no se puede, por el relay.
#[derive(Debug, Clone)]
pub struct Connector {
    endpoint: Endpoint,
    timeout: Duration,
    force_relay: bool,
}

impl Connector {
    /// Conector sobre `endpoint` con [`PUNCH_TIMEOUT`].
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            timeout: PUNCH_TIMEOUT,
            force_relay: false,
        }
    }

    /// Cambia el tiempo de la apertura simultánea.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Salta la conexión directa y va siempre al relay (`--force-relay`).
    pub fn force_relay(mut self, force: bool) -> Self {
        self.force_relay = force;
        self
    }

    /// Conecta con el par `peer` probando `candidates` y, si fallan, `relay`.
    ///
    /// Lo primero es admitir a `peer` en el `Endpoint` (`Endpoint::admit`), antes de lanzar ningún
    /// intento: si el otro extremo llama antes de que arranquen los nuestros, su handshake ya pasa
    /// la verificación de cliente (en TLS 1.3 el cliente da la conexión por buena antes de que el
    /// servidor valide su certificado, así que un rechazo tardío rompería la apertura simultánea).
    /// La sesión también lo admite al anunciar sus candidatos; repetirlo no estorba.
    pub async fn connect(
        &self,
        peer: PublicKey,
        candidates: &[SocketAddr],
        relay: Option<&RelayInfo>,
    ) -> Result<Established, TransportError> {
        self.endpoint.admit(peer);
        if !self.force_relay
            && let Some(connection) = self.connect_direct(peer, candidates).await
        {
            return Ok(Established {
                connection,
                path: Path::Direct,
            });
        }
        let Some(relay) = relay else {
            return Err(TransportError::Unreachable);
        };
        let connection = connect_relay(&self.endpoint, relay).await?;
        Ok(Established {
            connection,
            path: Path::Relayed,
        })
    }

    async fn connect_direct(
        &self,
        peer: PublicKey,
        candidates: &[SocketAddr],
    ) -> Option<PeerConnection> {
        let endpoint = &self.endpoint;
        let mut incoming = endpoint.incoming_from(peer);
        // La conexión preferida es la que inicia el par de clave menor.
        let we_prefer_outgoing = endpoint.public_key() < peer;

        let (tx, mut outgoing) = mpsc::unbounded_channel();
        let attempts: Vec<_> = order_candidates(candidates)
            .into_iter()
            .enumerate()
            .map(|(i, addr)| {
                let endpoint = endpoint.clone();
                let tx = tx.clone();
                let timeout = self.timeout;
                tokio::spawn(async move {
                    tokio::time::sleep(ATTEMPT_STAGGER * i as u32).await;
                    match tokio::time::timeout(timeout, endpoint.connect_peer(addr, peer)).await {
                        Ok(Ok(connection)) => {
                            let _ = tx.send(connection);
                        }
                        Ok(Err(err)) => debug!(%addr, error = %err, "candidato fallido"),
                        Err(_) => debug!(%addr, "candidato sin respuesta"),
                    }
                })
            })
            .collect();
        drop(tx);

        let mut deadline = tokio::time::Instant::now() + self.timeout;
        let mut held: Option<PeerConnection> = None;
        let mut extra = Vec::new();
        let chosen = loop {
            let (connection, ours) = tokio::select! {
                Some(connection) = incoming.recv() => (connection, false),
                Some(connection) = outgoing.recv() => (connection, true),
                () = tokio::time::sleep_until(deadline) => break held.take(),
            };
            let preferred = ours == we_prefer_outgoing;
            if preferred {
                if let Some(previous) = held.take() {
                    extra.push(previous);
                }
                break Some(connection);
            }
            if held.is_none() {
                held = Some(connection);
                deadline = deadline.min(tokio::time::Instant::now() + PREFER_GRACE);
            } else {
                extra.push(connection);
            }
        };

        for attempt in attempts {
            attempt.abort();
        }
        for connection in extra {
            connection.close("conexión duplicada");
        }
        while let Ok(connection) = outgoing.try_recv() {
            connection.close("conexión duplicada");
        }
        // Las entrantes tardías del mismo par se cierran durante un rato.
        let chosen_id = chosen.as_ref().map(|c| c.quinn().stable_id());
        let endpoint = endpoint.clone();
        tokio::spawn(async move {
            let linger = tokio::time::sleep(PUNCH_TIMEOUT);
            tokio::pin!(linger);
            loop {
                tokio::select! {
                    Some(connection) = incoming.recv() => {
                        if Some(connection.quinn().stable_id()) != chosen_id {
                            connection.close("conexión duplicada");
                        }
                    }
                    () = &mut linger => break,
                }
            }
            endpoint.forget(&peer);
        });
        chosen
    }
}

/// Conecta con el relay y se autentica con el token.
pub async fn connect_relay(
    endpoint: &Endpoint,
    relay: &RelayInfo,
) -> Result<PeerConnection, TransportError> {
    let (connection, shared) = endpoint
        .connect_server(relay.addr, &relay.server_name, &relay.trust, Alpn::Relay)
        .await?;
    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        identity_pk: endpoint.public_key(),
        role: relay.role,
        relay_token: Some(relay.token.clone()),
    };
    let (mut send, recv) = connection.open_bi().await?;
    send.write_all(&[STREAM_RELAY_AUTH]).await?;
    send.write_all(&codec::encode(&ControlMessage::Hello(hello))?)
        .await?;
    let mut receiver = ControlReceiver::new(recv);
    let reply = tokio::time::timeout(HELLO_TIMEOUT, receiver.recv())
        .await
        .map_err(|_| TransportError::Timeout("respuesta del relay"))??;
    match reply {
        Some(ControlMessage::Hello(_)) => {}
        _ => {
            connection.close(quinn::VarInt::from_u32(1), b"relay");
            return Err(TransportError::Protocol(
                "el relay rechazó el token".to_owned(),
            ));
        }
    }
    // El stream de autenticación queda abierto mientras dure la conexión.
    tokio::spawn(async move {
        let _keep = send;
        let _ = receiver.recv().await;
    });
    Ok(PeerConnection::new(connection, None, Path::Relayed, shared))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_are_ordered_ipv6_public_then_local() {
        let addrs: Vec<SocketAddr> = [
            "192.168.1.10:5000",
            "[fd00::1]:5000",
            "203.0.113.7:6000",
            "[2001:db8::1]:5000",
            "10.0.0.2:5000",
            "203.0.113.7:6000",
            "[::ffff:198.51.100.2]:7000",
        ]
        .iter()
        .map(|s| s.parse().expect("dirección"))
        .collect();
        let ordered: Vec<String> = order_candidates(&addrs)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            ordered,
            [
                "[2001:db8::1]:5000",
                "203.0.113.7:6000",
                "198.51.100.2:7000",
                "192.168.1.10:5000",
                "[fd00::1]:5000",
                "10.0.0.2:5000",
            ]
        );
    }

    #[test]
    fn link_local_and_private_ranges_are_detected() {
        let ip = |s: &str| s.parse::<IpAddr>().expect("ip");
        assert!(is_link_local(ip("169.254.1.1")));
        assert!(is_link_local(ip("fe80::1")));
        assert!(!is_link_local(ip("2001:db8::1")));
        assert!(is_private(ip("100.64.0.1")));
        assert!(is_private(ip("fd12::1")));
        assert!(!is_private(ip("198.51.100.1")));
    }
}
