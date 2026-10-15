//! PCP (RFC 6887) y NAT-PMP (RFC 6886): una petición UDP al puerto 5351 de la puerta de enlace.
//! Se empieza por PCP; si el router solo entiende NAT-PMP contesta «versión no admitida» (en el
//! formato de NAT-PMP) y se sigue con ese. Las peticiones se repiten con espera doble (250 ms,
//! 500 ms…) hasta el plazo; un ICMP de puerto inalcanzable corta en seguida.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket as StdUdpSocket};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::Instant;
use tracing::debug;

use super::{Failure, Lease, Mapping, MappingProtocol};

/// Versión de PCP.
pub const PCP_VERSION: u8 = 2;
/// Opcode MAP de PCP.
pub const PCP_OP_MAP: u8 = 1;
/// Protocolo UDP en la petición MAP.
pub const PROTO_UDP: u8 = 17;
/// Primera espera antes de repetir la petición (se dobla cada vez).
const FIRST_RETRY: Duration = Duration::from_millis(250);
/// Espera de las operaciones bloqueantes (borrado al soltar la asignación).
const BLOCKING_TIMEOUT: Duration = Duration::from_secs(1);

/// Petición MAP de PCP (60 bytes).
pub fn pcp_map_request(
    client: Ipv4Addr,
    nonce: &[u8; 12],
    internal: u16,
    suggested: SocketAddrV4,
    lifetime_s: u32,
) -> [u8; 60] {
    let mut out = [0u8; 60];
    out[0] = PCP_VERSION;
    out[1] = PCP_OP_MAP;
    out[4..8].copy_from_slice(&lifetime_s.to_be_bytes());
    out[8..24].copy_from_slice(&client.to_ipv6_mapped().octets());
    out[24..36].copy_from_slice(nonce);
    out[36] = PROTO_UDP;
    out[40..42].copy_from_slice(&internal.to_be_bytes());
    out[42..44].copy_from_slice(&suggested.port().to_be_bytes());
    out[44..60].copy_from_slice(&suggested.ip().to_ipv6_mapped().octets());
    out
}

/// Respuesta MAP de PCP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcpMapResponse {
    /// Código de resultado (0: concedido).
    pub result: u8,
    /// Vida concedida, en segundos.
    pub lifetime_s: u32,
    /// El nonce de la petición.
    pub nonce: [u8; 12],
    /// Puerto interno.
    pub internal: u16,
    /// Dirección externa asignada.
    pub external: SocketAddrV4,
}

/// Lee una respuesta MAP de PCP.
pub fn parse_pcp_map(bytes: &[u8]) -> Result<PcpMapResponse, String> {
    if bytes.len() < 60 {
        return Err(format!("respuesta PCP de {} bytes", bytes.len()));
    }
    if bytes[0] != PCP_VERSION {
        return Err(format!("versión PCP {}", bytes[0]));
    }
    if bytes[1] != PCP_OP_MAP | 0x80 {
        return Err(format!("opcode PCP {:#x}", bytes[1]));
    }
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&bytes[24..36]);
    let mut ip = [0u8; 16];
    ip.copy_from_slice(&bytes[44..60]);
    let external_ip = match std::net::Ipv6Addr::from(ip).to_ipv4_mapped() {
        Some(v4) => v4,
        None => Ipv4Addr::UNSPECIFIED,
    };
    Ok(PcpMapResponse {
        result: bytes[3],
        lifetime_s: u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        nonce,
        internal: u16::from_be_bytes([bytes[40], bytes[41]]),
        external: SocketAddrV4::new(external_ip, u16::from_be_bytes([bytes[42], bytes[43]])),
    })
}

/// Texto de un código de resultado de PCP (RFC 6887 §7.4).
pub fn pcp_result_text(code: u8) -> &'static str {
    match code {
        0 => "concedido",
        1 => "versión no admitida",
        2 => "no autorizado",
        3 => "petición mal formada",
        4 => "opcode no admitido",
        5 => "opción no admitida",
        6 => "opción mal formada",
        7 => "fallo de red",
        8 => "sin recursos",
        9 => "protocolo no admitido",
        10 => "cuota del usuario agotada",
        11 => "no puede dar el puerto externo pedido",
        12 => "la dirección del cliente no coincide",
        13 => "demasiados pares remotos",
        _ => "error desconocido",
    }
}

/// Petición de dirección externa de NAT-PMP.
pub const NATPMP_EXTERNAL_REQUEST: [u8; 2] = [0, 0];

/// Petición de asignación UDP de NAT-PMP (12 bytes).
pub fn natpmp_map_request(internal: u16, suggested_external: u16, lifetime_s: u32) -> [u8; 12] {
    let mut out = [0u8; 12];
    out[1] = 1;
    out[4..6].copy_from_slice(&internal.to_be_bytes());
    out[6..8].copy_from_slice(&suggested_external.to_be_bytes());
    out[8..12].copy_from_slice(&lifetime_s.to_be_bytes());
    out
}

/// Respuesta de NAT-PMP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatPmpResponse {
    /// A la petición de dirección externa.
    External {
        /// Código de resultado (0: bien).
        result: u16,
        /// Dirección externa del router.
        ip: Ipv4Addr,
    },
    /// A la petición de asignación UDP.
    Map {
        /// Código de resultado (0: concedido).
        result: u16,
        /// Puerto interno.
        internal: u16,
        /// Puerto externo asignado.
        external: u16,
        /// Vida concedida, en segundos.
        lifetime_s: u32,
    },
    /// Error a una petición que el router no entiende (por ejemplo, PCP en un router NAT-PMP).
    Error {
        /// Código de resultado.
        result: u16,
    },
}

/// Lee una respuesta de NAT-PMP.
pub fn parse_natpmp(bytes: &[u8]) -> Result<NatPmpResponse, String> {
    if bytes.len() < 8 {
        return Err(format!("respuesta NAT-PMP de {} bytes", bytes.len()));
    }
    if bytes[0] != 0 {
        return Err(format!("versión NAT-PMP {}", bytes[0]));
    }
    let result = u16::from_be_bytes([bytes[2], bytes[3]]);
    match bytes[1] {
        128 if bytes.len() >= 12 => Ok(NatPmpResponse::External {
            result,
            ip: Ipv4Addr::new(bytes[8], bytes[9], bytes[10], bytes[11]),
        }),
        129 if bytes.len() >= 16 => Ok(NatPmpResponse::Map {
            result,
            internal: u16::from_be_bytes([bytes[8], bytes[9]]),
            external: u16::from_be_bytes([bytes[10], bytes[11]]),
            lifetime_s: u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
        }),
        _ if result != 0 => Ok(NatPmpResponse::Error { result }),
        other => Err(format!("opcode NAT-PMP {other}")),
    }
}

/// Texto de un código de resultado de NAT-PMP (RFC 6886 §3.5).
pub fn natpmp_result_text(code: u16) -> &'static str {
    match code {
        0 => "concedido",
        1 => "versión no admitida",
        2 => "no autorizado (asignaciones desactivadas en el router)",
        3 => "fallo de red (el router no tiene dirección externa)",
        4 => "sin recursos",
        5 => "opcode no admitido",
        _ => "error desconocido",
    }
}

fn random_nonce() -> [u8; 12] {
    let mut nonce = [0u8; 12];
    if getrandom::fill(&mut nonce).is_err() {
        // Sin fuente de azar el nonce sale del reloj: solo protege de respuestas viejas.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        nonce[..8].copy_from_slice(&(now as u64).to_be_bytes());
    }
    nonce
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Pcp,
    NatPmp,
}

/// Pide la asignación de `internal` a la puerta de enlace `gateway` hasta `deadline`.
pub(super) async fn map(
    gateway: SocketAddrV4,
    internal: SocketAddrV4,
    lifetime: Duration,
    deadline: Instant,
) -> Result<(Mapping, Lease), Failure> {
    let socket = match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await {
        Ok(socket) => socket,
        Err(err) => return Err(socket_failure(err)),
    };
    if let Err(err) = socket.connect(gateway).await {
        return Err(socket_failure(err));
    }
    let lifetime_s = lifetime.as_secs().min(u64::from(u32::MAX)) as u32;
    let nonce = random_nonce();
    let unspecified = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0);
    let mut mode = Mode::Pcp;
    let mut wait = FIRST_RETRY;
    let mut external_ip: Option<Ipv4Addr> = None;
    let mut natpmp_map: Option<(u16, u32)> = None;
    let mut buf = [0u8; 128];
    loop {
        match mode {
            Mode::Pcp => {
                let request = pcp_map_request(
                    *internal.ip(),
                    &nonce,
                    internal.port(),
                    unspecified,
                    lifetime_s,
                );
                send(&socket, &request).await?;
            }
            Mode::NatPmp => {
                send(&socket, &NATPMP_EXTERNAL_REQUEST).await?;
                let request = natpmp_map_request(internal.port(), internal.port(), lifetime_s);
                send(&socket, &request).await?;
            }
        }
        let until = deadline.min(Instant::now() + wait);
        loop {
            let received = match tokio::time::timeout_at(until, socket.recv(&mut buf)).await {
                Ok(Ok(n)) => &buf[..n],
                Ok(Err(err)) => {
                    // ICMP de puerto inalcanzable: en la puerta de enlace no hay servidor.
                    debug!(%gateway, error = %err, "la puerta de enlace rechaza PCP/NAT-PMP");
                    return Err(Failure::NoResponse);
                }
                Err(_) => break,
            };
            match received.first() {
                Some(&PCP_VERSION) => match parse_pcp_map(received) {
                    Ok(reply) if reply.nonce != nonce || reply.internal != internal.port() => {
                        debug!("respuesta PCP de otra petición");
                    }
                    Ok(reply) if reply.result == 0 => {
                        let lease = Lease::Pcp {
                            gateway,
                            client: *internal.ip(),
                            nonce,
                            internal: internal.port(),
                            external: reply.external,
                        };
                        let mapping = Mapping {
                            external: reply.external,
                            protocol: MappingProtocol::Pcp,
                            lifetime: Some(Duration::from_secs(u64::from(reply.lifetime_s))),
                        };
                        return Ok((mapping, lease));
                    }
                    Ok(reply) if reply.result == 1 => {
                        debug!("el router no habla PCP 2; se prueba NAT-PMP");
                        mode = Mode::NatPmp;
                        wait = FIRST_RETRY;
                        break;
                    }
                    Ok(reply) => {
                        return Err(Failure::Refused {
                            protocol: MappingProtocol::Pcp,
                            error: format!(
                                "{} (código {})",
                                pcp_result_text(reply.result),
                                reply.result
                            ),
                            external: None,
                        });
                    }
                    Err(err) => debug!(error = %err, "respuesta PCP no válida"),
                },
                Some(0) => match parse_natpmp(received) {
                    Ok(NatPmpResponse::Error { result: 1 })
                    | Ok(NatPmpResponse::External { result: 1, .. })
                    | Ok(NatPmpResponse::Map { result: 1, .. })
                        if mode == Mode::Pcp =>
                    {
                        debug!("el router solo habla NAT-PMP");
                        mode = Mode::NatPmp;
                        wait = FIRST_RETRY;
                        break;
                    }
                    Ok(NatPmpResponse::External { result: 0, ip }) => {
                        external_ip = Some(ip);
                    }
                    Ok(NatPmpResponse::Map {
                        result: 0,
                        internal: port,
                        external,
                        lifetime_s,
                    }) if port == internal.port() => {
                        natpmp_map = Some((external, lifetime_s));
                    }
                    Ok(NatPmpResponse::External { result, .. })
                    | Ok(NatPmpResponse::Map { result, .. })
                    | Ok(NatPmpResponse::Error { result }) => {
                        return Err(Failure::Refused {
                            protocol: MappingProtocol::NatPmp,
                            error: format!("{} (código {result})", natpmp_result_text(result)),
                            external: external_ip,
                        });
                    }
                    Err(err) => debug!(error = %err, "respuesta NAT-PMP no válida"),
                },
                _ => debug!("datagrama desconocido de la puerta de enlace"),
            }
            if let (Some(ip), Some((port, lifetime_s))) = (external_ip, natpmp_map) {
                let external = SocketAddrV4::new(ip, port);
                let lease = Lease::NatPmp {
                    gateway,
                    internal: internal.port(),
                    external: port,
                };
                let mapping = Mapping {
                    external,
                    protocol: MappingProtocol::NatPmp,
                    lifetime: Some(Duration::from_secs(u64::from(lifetime_s))),
                };
                return Ok((mapping, lease));
            }
        }
        if Instant::now() >= deadline {
            return match external_ip {
                Some(external) => Err(Failure::Refused {
                    protocol: MappingProtocol::NatPmp,
                    error: "no contestó a la asignación".to_owned(),
                    external: Some(external),
                }),
                None => Err(Failure::NoResponse),
            };
        }
        wait = (wait * 2).min(Duration::from_secs(2));
    }
}

fn socket_failure(err: std::io::Error) -> Failure {
    Failure::Refused {
        protocol: MappingProtocol::Pcp,
        error: format!("socket UDP: {err}"),
        external: None,
    }
}

async fn send(socket: &UdpSocket, bytes: &[u8]) -> Result<(), Failure> {
    socket.send(bytes).await.map(|_| ()).map_err(|err| {
        debug!(error = %err, "no se pudo enviar a la puerta de enlace");
        Failure::NoResponse
    })
}

/// Renueva la asignación; devuelve la vida concedida.
pub(super) async fn renew(lease: &Lease, lifetime: Duration) -> Result<Option<Duration>, String> {
    let lifetime_s = lifetime.as_secs().min(u64::from(u32::MAX)) as u32;
    let (gateway, request): (SocketAddrV4, Vec<u8>) = match lease {
        Lease::Pcp {
            gateway,
            client,
            nonce,
            internal,
            external,
        } => (
            *gateway,
            pcp_map_request(*client, nonce, *internal, *external, lifetime_s).to_vec(),
        ),
        Lease::NatPmp {
            gateway,
            internal,
            external,
        } => (
            *gateway,
            natpmp_map_request(*internal, *external, lifetime_s).to_vec(),
        ),
        Lease::Upnp { .. } => return Err("no es PCP ni NAT-PMP".to_owned()),
    };
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .await
        .map_err(|err| err.to_string())?;
    socket
        .connect(gateway)
        .await
        .map_err(|err| err.to_string())?;
    let mut buf = [0u8; 128];
    let mut wait = FIRST_RETRY;
    for _ in 0..4 {
        socket.send(&request).await.map_err(|err| err.to_string())?;
        match tokio::time::timeout(wait, socket.recv(&mut buf)).await {
            Ok(Ok(n)) => {
                let bytes = &buf[..n];
                return match lease {
                    Lease::Pcp { .. } => {
                        let reply = parse_pcp_map(bytes)?;
                        if reply.result != 0 {
                            return Err(pcp_result_text(reply.result).to_owned());
                        }
                        Ok(Some(Duration::from_secs(u64::from(reply.lifetime_s))))
                    }
                    _ => match parse_natpmp(bytes)? {
                        NatPmpResponse::Map {
                            result: 0,
                            lifetime_s,
                            ..
                        } => Ok(Some(Duration::from_secs(u64::from(lifetime_s)))),
                        NatPmpResponse::Map { result, .. }
                        | NatPmpResponse::External { result, .. }
                        | NatPmpResponse::Error { result } => {
                            Err(natpmp_result_text(result).to_owned())
                        }
                    },
                };
            }
            Ok(Err(err)) => return Err(err.to_string()),
            Err(_) => wait *= 2,
        }
    }
    Err("sin respuesta a la renovación".to_owned())
}

/// Borra la asignación (vida 0), sin esperar respuesta más que un momento.
pub(super) fn delete_blocking(lease: &Lease) {
    let (gateway, request): (SocketAddrV4, Vec<u8>) = match lease {
        Lease::Pcp {
            gateway,
            client,
            nonce,
            internal,
            ..
        } => (
            *gateway,
            pcp_map_request(
                *client,
                nonce,
                *internal,
                SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0),
                0,
            )
            .to_vec(),
        ),
        Lease::NatPmp {
            gateway, internal, ..
        } => (*gateway, natpmp_map_request(*internal, 0, 0).to_vec()),
        Lease::Upnp { .. } => return,
    };
    let Ok(socket) = StdUdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else {
        return;
    };
    let _ = socket.set_read_timeout(Some(BLOCKING_TIMEOUT));
    if socket.connect(gateway).is_err() {
        return;
    }
    let mut buf = [0u8; 128];
    for _ in 0..2 {
        if socket.send(&request).is_err() {
            return;
        }
        if socket.recv(&mut buf).is_ok() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcp_map_requests_follow_rfc_6887() {
        let nonce = [7u8; 12];
        let request = pcp_map_request(
            Ipv4Addr::new(192, 168, 1, 10),
            &nonce,
            47_000,
            SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0),
            7200,
        );
        assert_eq!(request[0], 2);
        assert_eq!(request[1], 1);
        assert_eq!(&request[4..8], &7200u32.to_be_bytes());
        assert_eq!(&request[8..20], &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff]);
        assert_eq!(&request[20..24], &[192, 168, 1, 10]);
        assert_eq!(&request[24..36], &nonce);
        assert_eq!(request[36], 17);
        assert_eq!(&request[40..42], &47_000u16.to_be_bytes());

        let mut reply = request;
        reply[1] = 0x81;
        reply[3] = 0;
        reply[4..8].copy_from_slice(&3600u32.to_be_bytes());
        reply[42..44].copy_from_slice(&47_001u16.to_be_bytes());
        reply[44..60].copy_from_slice(&Ipv4Addr::new(100, 64, 3, 4).to_ipv6_mapped().octets());
        let parsed = parse_pcp_map(&reply).expect("respuesta");
        assert_eq!(parsed.result, 0);
        assert_eq!(parsed.lifetime_s, 3600);
        assert_eq!(parsed.nonce, nonce);
        assert_eq!(parsed.internal, 47_000);
        assert_eq!(
            parsed.external,
            SocketAddrV4::new(Ipv4Addr::new(100, 64, 3, 4), 47_001)
        );
        assert!(parse_pcp_map(&reply[..59]).is_err());
        assert_eq!(pcp_result_text(2), "no autorizado");
    }

    #[test]
    fn natpmp_messages_follow_rfc_6886() {
        let request = natpmp_map_request(47_000, 47_000, 7200);
        assert_eq!(&request[..4], &[0, 1, 0, 0]);
        assert_eq!(&request[4..6], &47_000u16.to_be_bytes());
        assert_eq!(&request[8..12], &7200u32.to_be_bytes());

        let mut external = vec![0u8, 128, 0, 0, 0, 0, 0, 10];
        external.extend_from_slice(&[203, 0, 113, 9]);
        assert_eq!(
            parse_natpmp(&external).expect("externa"),
            NatPmpResponse::External {
                result: 0,
                ip: Ipv4Addr::new(203, 0, 113, 9),
            }
        );
        let mut map = vec![0u8, 129, 0, 0, 0, 0, 0, 10];
        map.extend_from_slice(&47_000u16.to_be_bytes());
        map.extend_from_slice(&47_002u16.to_be_bytes());
        map.extend_from_slice(&3600u32.to_be_bytes());
        assert_eq!(
            parse_natpmp(&map).expect("asignación"),
            NatPmpResponse::Map {
                result: 0,
                internal: 47_000,
                external: 47_002,
                lifetime_s: 3600,
            }
        );
        // Un router NAT-PMP contesta a PCP con «versión no admitida» en su propio formato.
        let unsupported = [0u8, 0, 0, 1, 0, 0, 0, 0];
        assert_eq!(
            parse_natpmp(&unsupported).expect("error"),
            NatPmpResponse::Error { result: 1 }
        );
        assert!(parse_natpmp(&[2, 0, 0, 0, 0, 0, 0, 0]).is_err());
    }
}
