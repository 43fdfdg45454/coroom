//! Apertura de NAT y conexión al relay.
//!
//! [`Connector::connect`] lanza en paralelo un `connect` QUIC hacia cada candidato remoto (IPv6
//! global primero, luego IPv4 pública —normalmente la reflejada que da el servidor con `Observed`—
//! y por último las locales) mientras acepta las conexiones entrantes del mismo par en el mismo
//! socket: es la apertura simultánea, que abre las dos NAT. Gana la primera conexión establecida y
//! verificada (TLS con la identidad esperada); si en los dos sentidos se establece una, las dos
//! partes se quedan con la que inició el par de clave menor, con una gracia de
//! [`PREFER_GRACE`] para que coincidan. Las demás se cierran. Si en [`PUNCH_TIMEOUT`] no hay
//! ninguna, se usa el relay del servidor ([`connect_relay`]), cuya conexión se abre y autentica
//! **a la vez** que la apertura simultánea (desde el primer instante, no al vencer el plazo): si
//! la directa llega, la del relay se cierra; si no, el relay ya está listo y no se suman sus
//! idas y vueltas a los 3 s de la apertura. Los candidatos que el otro extremo anuncie después
//! (la dirección WAN que su router abre al cabo de un par de segundos) entran por
//! [`Connector::connect_with`] mientras dura la apertura.
//!
//! Relay: conexión QUIC al servidor con ALPN `coroom-relay/1`; un primer stream de autenticación
//! con `Hello` y el `RelayToken` de la señalización. Después los datagramas y el stream de control
//! se usan igual que en directo.

use std::fmt;
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

/// Candidatos del otro extremo que llegan con la apertura ya en marcha (un segundo `Candidates`
/// de la señalización con la dirección WAN del router).
pub type ExtraCandidates = mpsc::UnboundedReceiver<Vec<SocketAddr>>;

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
    /// Límite de vídeo por sala del `RelayToken` en kbit/s (`0` = sin límite).
    pub max_kbps: u32,
    /// Papel con el que se entra.
    pub role: Role,
}

/// Clase de un candidato, en el orden en que se prueban.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateClass {
    /// IPv6 global.
    Ipv6Global,
    /// IPv4 pública (normalmente la reflejada que vio el servidor).
    Ipv4Public,
    /// IPv4 de la NAT de una operadora (100.64.0.0/10): la WAN de un router detrás de CGNAT con
    /// su puerto abierto, que llega desde los demás clientes de la misma operadora.
    CarrierNat,
    /// Privada o local (solo sirve en la misma red).
    Private,
}

impl fmt::Display for CandidateClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CandidateClass::Ipv6Global => "IPv6 global",
            CandidateClass::Ipv4Public => "IPv4 pública",
            CandidateClass::CarrierNat => "IPv4 de la operadora (CGNAT)",
            CandidateClass::Private => "red local",
        })
    }
}

/// Qué pasó con un candidato durante la apertura simultánea.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateOutcome {
    /// Por esta dirección se estableció la conexión que se usa.
    Connected,
    /// Respondió y se estableció una conexión, pero se eligió otra (la del par de clave menor).
    Duplicate,
    /// El handshake falló (rechazo, identidad distinta, error de red).
    Failed(String),
    /// Nada en [`PUNCH_TIMEOUT`]: lo más común detrás de una NAT que no abre (CGNAT).
    NoResponse,
    /// La apertura terminó antes de que a este le tocara o respondiera.
    Cancelled,
}

impl fmt::Display for CandidateOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CandidateOutcome::Connected => f.write_str("conectado"),
            CandidateOutcome::Duplicate => f.write_str("respondió, se eligió otra"),
            CandidateOutcome::Failed(err) => write!(f, "falló: {err}"),
            CandidateOutcome::NoResponse => f.write_str("sin respuesta"),
            CandidateOutcome::Cancelled => f.write_str("no se llegó a probar"),
        }
    }
}

/// Resultado de un candidato de conexión directa, para el diagnóstico de la ruta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateReport {
    /// Dirección probada.
    pub addr: SocketAddr,
    /// Su clase.
    pub class: CandidateClass,
    /// Qué pasó.
    pub outcome: CandidateOutcome,
}

impl fmt::Display for CandidateReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}): {}", self.addr, self.class, self.outcome)
    }
}

/// Intento de conexión directa: la conexión, si la hubo, y lo que pasó con cada candidato.
#[derive(Debug)]
pub struct DirectAttempt {
    /// La conexión directa, si alguna se estableció.
    pub connection: Option<PeerConnection>,
    /// Cada candidato con su resultado, en el orden en que se probaron.
    pub candidates: Vec<CandidateReport>,
}

impl DirectAttempt {
    /// Por qué no hubo conexión directa, en una frase para el usuario.
    pub fn relay_reason(&self) -> String {
        if self.candidates.is_empty() {
            return "el otro extremo no anunció direcciones".to_owned();
        }
        let failed = self
            .candidates
            .iter()
            .filter(|c| matches!(c.outcome, CandidateOutcome::Failed(_)))
            .count();
        let public = self.candidates.iter().any(|c| {
            matches!(
                c.class,
                CandidateClass::Ipv6Global | CandidateClass::Ipv4Public
            )
        });
        let carrier = self
            .candidates
            .iter()
            .any(|c| c.class == CandidateClass::CarrierNat);
        match (failed, public, carrier) {
            (0, false, false) => {
                "solo había direcciones de red local y ninguna respondió".to_owned()
            }
            (0, _, true) => "ninguna dirección respondió, tampoco la del router abierta detrás de \
                             la NAT de la operadora (CGNAT): la operadora no pasa tráfico entre \
                             sus clientes"
                .to_owned(),
            (0, true, false) => {
                "ninguna dirección respondió: la NAT de uno de los dos no abre (CGNAT)".to_owned()
            }
            _ => format!("{failed} de {} direcciones fallaron", self.candidates.len()),
        }
    }
}

/// Conexión establecida y su ruta.
#[derive(Debug, Clone)]
pub struct Established {
    /// La conexión.
    pub connection: PeerConnection,
    /// Directa o por el relay.
    pub path: Path,
    /// Lo que pasó con cada candidato directo (vacío si se fue al relay sin probar).
    pub candidates: Vec<CandidateReport>,
    /// Por qué se usa el relay, si se usa.
    pub relay_reason: Option<String>,
}

fn is_link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// 100.64.0.0/10: el espacio compartido de las NAT de operadora (RFC 6598).
fn is_carrier_nat(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V4(v4) if v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
}

fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private() || v4.is_loopback() || v4.is_link_local() || is_carrier_nat(ip)
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

/// Clase de un candidato ([`order_candidates`] las prueba en este orden).
pub fn candidate_class(addr: SocketAddr) -> CandidateClass {
    match addr.ip() {
        IpAddr::V6(_) if !is_private(addr.ip()) => CandidateClass::Ipv6Global,
        ip @ IpAddr::V4(_) if is_carrier_nat(ip) => CandidateClass::CarrierNat,
        IpAddr::V4(_) if !is_private(addr.ip()) => CandidateClass::Ipv4Public,
        _ => CandidateClass::Private,
    }
}

/// Ordena candidatos: IPv6 global, IPv4 pública (reflejada), la de la operadora (CGNAT) y después
/// las privadas y locales.
pub fn order_candidates(candidates: &[SocketAddr]) -> Vec<SocketAddr> {
    let class = |addr: &SocketAddr| candidate_class(*addr) as u8;
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

    /// Conecta con el par `peer` probando `candidates` y, si fallan, `relay`
    /// ([`Connector::connect_with`] sin candidatos tardíos).
    pub async fn connect(
        &self,
        peer: PublicKey,
        candidates: &[SocketAddr],
        relay: Option<&RelayInfo>,
    ) -> Result<Established, TransportError> {
        self.connect_with(peer, candidates, relay, None).await
    }

    /// Conecta con el par `peer` probando `candidates` (y los que lleguen por `extra` mientras
    /// dura la apertura) y, si fallan, `relay`. La conexión con el relay se abre y autentica desde
    /// el principio, en paralelo con la apertura simultánea: si la directa gana, se cierra.
    ///
    /// Lo primero es admitir a `peer` en el `Endpoint` (`Endpoint::admit`), antes de lanzar ningún
    /// intento: si el otro extremo llama antes de que arranquen los nuestros, su handshake ya pasa
    /// la verificación de cliente (en TLS 1.3 el cliente da la conexión por buena antes de que el
    /// servidor valide su certificado, así que un rechazo tardío rompería la apertura simultánea).
    /// La sesión también lo admite al anunciar sus candidatos; repetirlo no estorba.
    pub async fn connect_with(
        &self,
        peer: PublicKey,
        candidates: &[SocketAddr],
        relay: Option<&RelayInfo>,
        extra: Option<ExtraCandidates>,
    ) -> Result<Established, TransportError> {
        self.endpoint.admit(peer);
        let relay_task = relay.map(|relay| {
            let endpoint = self.endpoint.clone();
            let relay = relay.clone();
            tokio::spawn(async move { connect_relay(&endpoint, &relay).await })
        });
        let (candidates, relay_reason) = if self.force_relay {
            (Vec::new(), Some("se pidió usar el relay".to_owned()))
        } else {
            let attempt = self.try_direct_with(peer, candidates, extra).await;
            if let Some(connection) = attempt.connection {
                if let Some(task) = relay_task {
                    // La del relay ya no hace falta: se cierra en cuanto termine de abrirse.
                    tokio::spawn(async move {
                        if let Ok(Ok(connection)) = task.await {
                            connection.close("hay conexión directa");
                        }
                    });
                }
                return Ok(Established {
                    connection,
                    path: Path::Direct,
                    candidates: attempt.candidates,
                    relay_reason: None,
                });
            }
            let reason = attempt.relay_reason();
            (attempt.candidates, Some(reason))
        };
        let Some(task) = relay_task else {
            return Err(TransportError::Unreachable);
        };
        let connection = task.await.map_err(|_| {
            TransportError::Protocol("la conexión con el relay se canceló".into())
        })??;
        Ok(Established {
            connection,
            path: Path::Relayed,
            candidates,
            relay_reason,
        })
    }

    /// Solo la apertura simultánea, con el resultado de cada candidato (el anfitrión decide por
    /// su cuenta qué hacer con el relay).
    pub async fn try_direct(&self, peer: PublicKey, candidates: &[SocketAddr]) -> DirectAttempt {
        self.try_direct_with(peer, candidates, None).await
    }

    /// Como [`Connector::try_direct`], probando también los candidatos que lleguen por `extra`
    /// antes de que venza el plazo (sin alargarlo: a un candidato que llega a los 2 s le queda
    /// un segundo, de sobra para el handshake de QUIC).
    pub async fn try_direct_with(
        &self,
        peer: PublicKey,
        candidates: &[SocketAddr],
        mut extra: Option<ExtraCandidates>,
    ) -> DirectAttempt {
        let endpoint = &self.endpoint;
        let mut incoming = endpoint.incoming_from(peer);
        // La conexión preferida es la que inicia el par de clave menor.
        let we_prefer_outgoing = endpoint.public_key() < peer;

        let mut ordered = order_candidates(candidates);
        let (tx, mut outgoing) = mpsc::unbounded_channel();
        let (result_tx, mut results) = mpsc::unbounded_channel::<(usize, CandidateOutcome)>();
        let spawn_attempt = |i: usize, addr: SocketAddr, stagger: Duration| {
            let endpoint = endpoint.clone();
            let tx = tx.clone();
            let result_tx = result_tx.clone();
            let timeout = self.timeout;
            tokio::spawn(async move {
                tokio::time::sleep(stagger).await;
                match tokio::time::timeout(timeout, endpoint.connect_peer(addr, peer)).await {
                    Ok(Ok(connection)) => {
                        let _ = result_tx.send((i, CandidateOutcome::Duplicate));
                        let _ = tx.send(connection);
                    }
                    Ok(Err(err)) => {
                        debug!(%addr, error = %err, "candidato fallido");
                        let _ = result_tx.send((i, CandidateOutcome::Failed(err.to_string())));
                    }
                    Err(_) => {
                        debug!(%addr, "candidato sin respuesta");
                        let _ = result_tx.send((i, CandidateOutcome::NoResponse));
                    }
                }
            })
        };
        let mut attempts: Vec<_> = ordered
            .iter()
            .copied()
            .enumerate()
            .map(|(i, addr)| spawn_attempt(i, addr, ATTEMPT_STAGGER * i as u32))
            .collect();

        let mut deadline = tokio::time::Instant::now() + self.timeout;
        let mut held: Option<PeerConnection> = None;
        let mut duplicates = Vec::new();
        let chosen = loop {
            let (connection, ours) = tokio::select! {
                Some(connection) = incoming.recv() => (connection, false),
                Some(connection) = outgoing.recv() => (connection, true),
                more = recv_extra(&mut extra) => {
                    match more {
                        Some(more) => {
                            let new: Vec<SocketAddr> = order_candidates(&more)
                                .into_iter()
                                .filter(|addr| !ordered.contains(addr))
                                .collect();
                            for (j, addr) in new.into_iter().enumerate() {
                                debug!(%addr, "candidato tardío");
                                ordered.push(addr);
                                attempts.push(spawn_attempt(
                                    ordered.len() - 1,
                                    addr,
                                    ATTEMPT_STAGGER * j as u32,
                                ));
                            }
                        }
                        None => extra = None,
                    }
                    continue;
                }
                () = tokio::time::sleep_until(deadline) => break held.take(),
            };
            let preferred = ours == we_prefer_outgoing;
            if preferred {
                if let Some(previous) = held.take() {
                    duplicates.push(previous);
                }
                break Some(connection);
            }
            if held.is_none() {
                held = Some(connection);
                deadline = deadline.min(tokio::time::Instant::now() + PREFER_GRACE);
            } else {
                duplicates.push(connection);
            }
        };
        drop(tx);
        drop(result_tx);

        for attempt in attempts {
            attempt.abort();
        }
        let mut reports: Vec<CandidateReport> = ordered
            .iter()
            .map(|&addr| CandidateReport {
                addr,
                class: candidate_class(addr),
                outcome: CandidateOutcome::Cancelled,
            })
            .collect();
        while let Ok((index, outcome)) = results.try_recv() {
            if let Some(report) = reports.get_mut(index) {
                report.outcome = outcome;
            }
        }
        if let Some(chosen) = &chosen {
            let remote = crate::quic::canonical(chosen.remote_address());
            let class = candidate_class(remote);
            // Si entró por una dirección que no estaba en la lista (la NAT del par reescribió el
            // puerto), se anota en la primera de su clase.
            let index = reports
                .iter()
                .position(|r| r.addr == remote)
                .or_else(|| reports.iter().position(|r| r.class == class));
            if let Some(report) = index.and_then(|index| reports.get_mut(index)) {
                report.outcome = CandidateOutcome::Connected;
            }
        }
        for connection in duplicates {
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
        DirectAttempt {
            connection: chosen,
            candidates: reports,
        }
    }
}

/// Siguiente lote de candidatos tardíos; espera para siempre si no hay canal (así la rama del
/// `select!` que lo usa no se dispara).
async fn recv_extra(extra: &mut Option<ExtraCandidates>) -> Option<Vec<SocketAddr>> {
    match extra {
        Some(extra) => extra.recv().await,
        None => std::future::pending().await,
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
            "100.64.3.4:4000",
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
                "100.64.3.4:4000",
                "192.168.1.10:5000",
                "[fd00::1]:5000",
                "10.0.0.2:5000",
            ]
        );
    }

    #[test]
    fn the_relay_reason_explains_what_failed() {
        let addr = |s: &str| s.parse::<SocketAddr>().expect("dirección");
        let report = |addr: SocketAddr, outcome| CandidateReport {
            addr,
            class: candidate_class(addr),
            outcome,
        };
        let attempt = |candidates| DirectAttempt {
            connection: None,
            candidates,
        };
        assert_eq!(
            attempt(vec![]).relay_reason(),
            "el otro extremo no anunció direcciones"
        );
        let cgnat = attempt(vec![
            report(addr("203.0.113.7:6000"), CandidateOutcome::NoResponse),
            report(addr("192.168.1.10:5000"), CandidateOutcome::NoResponse),
        ]);
        assert!(
            cgnat.relay_reason().contains("CGNAT"),
            "{}",
            cgnat.relay_reason()
        );
        let local = attempt(vec![report(
            addr("10.0.0.2:5000"),
            CandidateOutcome::NoResponse,
        )]);
        assert!(
            local.relay_reason().contains("red local"),
            "{}",
            local.relay_reason()
        );
        let carrier = attempt(vec![
            report(addr("203.0.113.7:6000"), CandidateOutcome::NoResponse),
            report(addr("100.64.3.4:4000"), CandidateOutcome::NoResponse),
        ]);
        assert!(
            carrier.relay_reason().contains("operadora"),
            "{}",
            carrier.relay_reason()
        );
        assert_eq!(
            carrier.candidates[1].to_string(),
            "100.64.3.4:4000 (IPv4 de la operadora (CGNAT)): sin respuesta"
        );
        let failed = attempt(vec![
            report(
                addr("203.0.113.7:6000"),
                CandidateOutcome::Failed("identidad".into()),
            ),
            report(addr("192.168.1.10:5000"), CandidateOutcome::NoResponse),
        ]);
        assert_eq!(failed.relay_reason(), "1 de 2 direcciones fallaron");
        assert_eq!(
            failed.candidates[0].to_string(),
            "203.0.113.7:6000 (IPv4 pública): falló: identidad"
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
