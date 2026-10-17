//! Apertura del puerto UDP del [`Endpoint`](crate::quic::Endpoint) en el router de la red local,
//! para que el otro extremo pueda entrar en directo cuando la apertura simultánea de NAT no basta.
//! Detrás de la NAT de una operadora (CGNAT) la dirección reflejada que ve el servidor no sirve,
//! pero la dirección WAN del router con su puerto abierto sí llega desde los demás clientes de la
//! misma operadora; con IP pública en el router, desde cualquier sitio.
//!
//! [`Mapper::map`] lee la puerta de enlace de la tabla de rutas del sistema (nunca la supone), le
//! pide la asignación por **PCP** (RFC 6887; si solo entiende **NAT-PMP**, RFC 6886, se sigue
//! con ese) y por **UPnP IGD** a la vez, y se queda con la primera que llegue. La asignación
//! ([`PortMapping`]) se renueva a la mitad de su vida y se borra al soltarla. Lo que respondió el
//! router queda en [`MappingReport`]: lo enseña `coroom doctor` y explica por qué se va al relay.

use std::fmt;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

use tokio::time::Instant;
use tracing::debug;

mod gateway;
mod pmp;
mod upnp;

pub use gateway::{DefaultRoute, default_gateway, default_route};
pub use pmp::{
    NATPMP_EXTERNAL_REQUEST, NatPmpResponse, PCP_OP_MAP, PCP_VERSION, PcpMapResponse,
    natpmp_map_request, parse_natpmp, parse_pcp_map, pcp_map_request,
};
pub use upnp::{SSDP_MULTICAST, location_from_response, search_request, xml_text};

/// Puerto UDP de PCP y NAT-PMP en la puerta de enlace.
pub const GATEWAY_PORT: u16 = 5351;
/// Vida que se pide para la asignación (se renueva a la mitad).
pub const MAPPING_LIFETIME: Duration = Duration::from_secs(7200);
/// Cuánto se espera al router: UPnP responde en hasta 1 s (`MX: 1`); PCP y NAT-PMP, en
/// milisegundos o nunca.
pub const MAPPING_TIMEOUT: Duration = Duration::from_millis(2000);
/// Espera antes de volver a intentar una renovación fallida.
const RENEW_RETRY: Duration = Duration::from_secs(60);

/// Ejecuta `read` en un hilo de bloqueo del runtime (una lectura del sistema que puede tardar),
/// o en el hilo actual si no hay runtime.
async fn blocking<T: Send + 'static>(
    read: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    match tokio::runtime::Handle::try_current() {
        Ok(_) => tokio::task::spawn_blocking(read)
            .await
            .unwrap_or_else(|err| Err(format!("la lectura del sistema falló: {err}"))),
        Err(_) => read(),
    }
}

/// Con qué protocolo respondió el router.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingProtocol {
    /// Port Control Protocol (RFC 6887).
    Pcp,
    /// NAT Port Mapping Protocol (RFC 6886).
    NatPmp,
    /// UPnP Internet Gateway Device.
    Upnp,
}

impl fmt::Display for MappingProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            MappingProtocol::Pcp => "PCP",
            MappingProtocol::NatPmp => "NAT-PMP",
            MappingProtocol::Upnp => "UPnP",
        })
    }
}

/// Asignación concedida por el router.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapping {
    /// Dirección externa (la WAN del router y el puerto abierto).
    pub external: SocketAddrV4,
    /// Protocolo con el que se consiguió.
    pub protocol: MappingProtocol,
    /// Vida concedida (`None`: permanente, en routers UPnP sin arrendamientos).
    pub lifetime: Option<Duration>,
}

/// Qué pasó con la petición al router.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingOutcome {
    /// Concedida.
    Mapped(Mapping),
    /// El router dijo su dirección externa pero no concedió la asignación.
    ExternalOnly {
        /// Dirección WAN del router.
        external: Ipv4Addr,
        /// Protocolo que respondió.
        protocol: MappingProtocol,
        /// Por qué no la concedió.
        error: String,
    },
    /// Sin ruta IPv4 por defecto, o sin forma de leerla en este sistema.
    NoGateway(String),
    /// Nadie respondió en el plazo: el router no tiene UPnP, NAT-PMP ni PCP, o están apagados.
    NoResponse,
    /// El router respondió con un error.
    Refused(String),
}

/// Resultado de pedir la asignación, para el diagnóstico.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingReport {
    /// Puerta de enlace a la que se pidió.
    pub gateway: Option<Ipv4Addr>,
    /// Dirección local que se pidió abrir.
    pub internal: Option<SocketAddrV4>,
    /// Qué pasó.
    pub outcome: MappingOutcome,
}

impl MappingReport {
    /// La asignación, si la hay.
    pub fn mapping(&self) -> Option<&Mapping> {
        match &self.outcome {
            MappingOutcome::Mapped(mapping) => Some(mapping),
            _ => None,
        }
    }

    /// La dirección WAN del router, si la dijo (con asignación o sin ella).
    pub fn external_ip(&self) -> Option<Ipv4Addr> {
        match &self.outcome {
            MappingOutcome::Mapped(mapping) => Some(*mapping.external.ip()),
            MappingOutcome::ExternalOnly { external, .. } => Some(*external),
            _ => None,
        }
    }
}

fn lifetime_text(lifetime: Option<Duration>) -> String {
    match lifetime {
        None => "permanente".to_owned(),
        Some(lifetime) if lifetime.as_secs() >= 3600 => {
            format!("{} h", lifetime.as_secs() / 3600)
        }
        Some(lifetime) if lifetime.as_secs() >= 60 => format!("{} min", lifetime.as_secs() / 60),
        Some(lifetime) => format!("{} s", lifetime.as_secs()),
    }
}

impl fmt::Display for MappingReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let port = self.internal.map_or(0, |addr| addr.port());
        let gateway = self
            .gateway
            .map_or_else(|| "desconocido".to_owned(), |ip| ip.to_string());
        match &self.outcome {
            MappingOutcome::Mapped(mapping) => write!(
                f,
                "puerto UDP {port} abierto en el router {gateway} por {}: {} ({})",
                mapping.protocol,
                mapping.external,
                lifetime_text(mapping.lifetime)
            ),
            MappingOutcome::ExternalOnly {
                external,
                protocol,
                error,
            } => write!(
                f,
                "el router {gateway} ({protocol}) no abrió el puerto UDP {port}: {error}; su dirección externa es {external}"
            ),
            MappingOutcome::NoGateway(error) => {
                write!(f, "sin puerta de enlace IPv4: {error}")
            }
            MappingOutcome::NoResponse => write!(
                f,
                "el router {gateway} no respondió a UPnP, NAT-PMP ni PCP (apagados o sin ellos)"
            ),
            MappingOutcome::Refused(error) => {
                write!(
                    f,
                    "el router {gateway} no abrió el puerto UDP {port}: {error}"
                )
            }
        }
    }
}

/// Lo que dice el router sin abrir nada ([`Mapper::inspect`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterInspection {
    /// La puerta de enlace, o por qué no se encontró.
    pub gateway: Result<Ipv4Addr, String>,
    /// Los protocolos que respondieron, con la dirección externa que dio cada uno.
    pub answers: Vec<(MappingProtocol, Ipv4Addr)>,
}

impl fmt::Display for RouterInspection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let gateway = match &self.gateway {
            Ok(gateway) => gateway,
            Err(error) => return write!(f, "sin puerta de enlace IPv4: {error}"),
        };
        if self.answers.is_empty() {
            return write!(
                f,
                "el router {gateway} no respondió a UPnP ni a NAT-PMP (apagados o sin ellos)"
            );
        }
        let answers = self
            .answers
            .iter()
            .map(|(protocol, external)| format!("{protocol} (dirección externa {external})"))
            .collect::<Vec<_>>()
            .join(", ");
        write!(
            f,
            "el router {gateway} responde a {answers}; no se abrió ningún puerto: la sala lo abre al abrirse"
        )
    }
}

/// Por qué falló un protocolo (uso interno de los clientes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// Nadie contestó (o un ICMP dijo que no hay servidor).
    NoResponse,
    /// Contestó con un error.
    Refused {
        /// Quién.
        protocol: MappingProtocol,
        /// Qué.
        error: String,
        /// La dirección externa, si llegó a decirla.
        external: Option<Ipv4Addr>,
    },
}

/// Lo que hace falta para renovar y borrar una asignación.
#[derive(Debug, Clone)]
pub(crate) enum Lease {
    Pcp {
        gateway: SocketAddrV4,
        client: Ipv4Addr,
        nonce: [u8; 12],
        internal: u16,
        external: SocketAddrV4,
    },
    NatPmp {
        gateway: SocketAddrV4,
        internal: u16,
        external: u16,
    },
    Upnp {
        control: upnp::Control,
        client: Ipv4Addr,
        internal: u16,
        external: u16,
        lease_s: u32,
    },
}

impl Lease {
    async fn renew(&self, lifetime: Duration) -> Result<Option<Duration>, String> {
        match self {
            Lease::Pcp { .. } | Lease::NatPmp { .. } => pmp::renew(self, lifetime).await,
            Lease::Upnp { .. } => upnp::renew(self, lifetime).await,
        }
    }

    fn delete_blocking(&self) {
        match self {
            Lease::Pcp { .. } | Lease::NatPmp { .. } => pmp::delete_blocking(self),
            Lease::Upnp { .. } => upnp::delete_blocking(self),
        }
    }
}

/// Pide al router que abra un puerto UDP.
#[derive(Debug, Clone)]
pub struct Mapper {
    gateway: Option<SocketAddrV4>,
    ssdp: SocketAddrV4,
    lifetime: Duration,
    timeout: Duration,
}

impl Default for Mapper {
    fn default() -> Self {
        Self::new()
    }
}

impl Mapper {
    /// La puerta de enlace de la tabla de rutas, SSDP multicast, [`MAPPING_LIFETIME`] y
    /// [`MAPPING_TIMEOUT`].
    pub fn new() -> Self {
        Self {
            gateway: None,
            ssdp: SSDP_MULTICAST,
            lifetime: MAPPING_LIFETIME,
            timeout: MAPPING_TIMEOUT,
        }
    }

    /// Servidor PCP/NAT-PMP concreto en vez de la puerta de enlace del sistema (pruebas).
    pub fn with_gateway(mut self, gateway: SocketAddrV4) -> Self {
        self.gateway = Some(gateway);
        self
    }

    /// Dirección a la que se envía la búsqueda SSDP (unicast en las pruebas).
    pub fn with_ssdp(mut self, ssdp: SocketAddrV4) -> Self {
        self.ssdp = ssdp;
        self
    }

    /// Vida que se pide.
    pub fn with_lifetime(mut self, lifetime: Duration) -> Self {
        self.lifetime = lifetime;
        self
    }

    /// Cuánto se espera al router.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Pregunta al router su dirección externa por NAT-PMP y por UPnP a la vez, **sin abrir ningún
    /// puerto**: lo que usa `coroom doctor`, que no debe dejar reglas en el router.
    pub async fn inspect(&self) -> RouterInspection {
        let deadline = Instant::now() + self.timeout;
        let gateway = match self.gateway {
            Some(gateway) => gateway,
            None => match blocking(gateway::default_gateway).await {
                Ok(ip) => SocketAddrV4::new(ip, GATEWAY_PORT),
                Err(error) => {
                    return RouterInspection {
                        gateway: Err(error),
                        answers: Vec::new(),
                    };
                }
            },
        };
        let gateway_ip = *gateway.ip();
        let local = blocking(move || gateway::local_ip_towards(gateway_ip)).await;
        let ssdp = self.ssdp;
        let upnp = async move { upnp::external(ssdp, local.ok()?, deadline).await.ok() };
        let (pmp, upnp) = tokio::join!(pmp::external(gateway, deadline), upnp);
        let answers = [
            (MappingProtocol::NatPmp, pmp),
            (MappingProtocol::Upnp, upnp),
        ]
        .into_iter()
        .filter_map(|(protocol, external)| Some((protocol, external?)))
        .collect();
        RouterInspection {
            gateway: Ok(gateway_ip),
            answers,
        }
    }

    /// Pide abrir el puerto UDP `internal_port` de este equipo. Siempre devuelve algo: si no se
    /// consiguió, el [`PortMapping`] solo lleva el informe.
    pub async fn map(&self, internal_port: u16) -> PortMapping {
        let deadline = Instant::now() + self.timeout;
        // La tabla de rutas se lee con llamadas al sistema que bloquean (netlink, `GetIpForwardTable2`):
        // fuera de los hilos del runtime, que llevan los medios.
        let gateway = match self.gateway {
            Some(gateway) => gateway,
            None => match blocking(gateway::default_gateway).await {
                Ok(ip) => SocketAddrV4::new(ip, GATEWAY_PORT),
                Err(error) => {
                    return PortMapping::unmapped(MappingReport {
                        gateway: None,
                        internal: None,
                        outcome: MappingOutcome::NoGateway(error),
                    });
                }
            },
        };
        let gateway_ip = *gateway.ip();
        let local_ip = match blocking(move || gateway::local_ip_towards(gateway_ip)).await {
            Ok(ip) => ip,
            Err(error) => {
                return PortMapping::unmapped(MappingReport {
                    gateway: Some(*gateway.ip()),
                    internal: None,
                    outcome: MappingOutcome::NoGateway(error),
                });
            }
        };
        let internal = SocketAddrV4::new(local_ip, internal_port);
        let report = |outcome| MappingReport {
            gateway: Some(*gateway.ip()),
            internal: Some(internal),
            outcome,
        };

        let pmp = pmp::map(gateway, internal, self.lifetime, deadline);
        let upnp = upnp::map(self.ssdp, internal, self.lifetime, deadline);
        tokio::pin!(pmp);
        tokio::pin!(upnp);
        let mut pmp_failure: Option<Failure> = None;
        let mut upnp_failure: Option<Failure> = None;
        let granted = loop {
            tokio::select! {
                result = &mut pmp, if pmp_failure.is_none() => match result {
                    Ok(granted) => break Some(granted),
                    Err(failure) => pmp_failure = Some(failure),
                },
                result = &mut upnp, if upnp_failure.is_none() => match result {
                    Ok(granted) => break Some(granted),
                    Err(failure) => upnp_failure = Some(failure),
                },
            }
            if pmp_failure.is_some() && upnp_failure.is_some() {
                break None;
            }
        };
        match granted {
            Some((mapping, lease)) => {
                debug!(%mapping.external, protocol = %mapping.protocol, "puerto abierto en el router");
                PortMapping::granted(
                    report(MappingOutcome::Mapped(mapping)),
                    lease,
                    self.lifetime,
                )
            }
            None => {
                let failures = [pmp_failure, upnp_failure].into_iter().flatten();
                PortMapping::unmapped(report(combine(failures)))
            }
        }
    }
}

/// Resume lo que dijeron los protocolos cuando ninguno concedió la asignación.
fn combine(failures: impl Iterator<Item = Failure>) -> MappingOutcome {
    let mut refused: Vec<(MappingProtocol, String, Option<Ipv4Addr>)> = Vec::new();
    for failure in failures {
        if let Failure::Refused {
            protocol,
            error,
            external,
        } = failure
        {
            refused.push((protocol, error, external));
        }
    }
    if refused.is_empty() {
        return MappingOutcome::NoResponse;
    }
    if let Some((protocol, error, Some(external))) = refused
        .iter()
        .find(|(_, _, external)| external.is_some())
        .cloned()
    {
        return MappingOutcome::ExternalOnly {
            external,
            protocol,
            error,
        };
    }
    MappingOutcome::Refused(
        refused
            .into_iter()
            .map(|(protocol, error, _)| format!("{protocol}: {error}"))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

/// Una asignación viva (o el informe de por qué no la hay). Se renueva sola y se borra al
/// soltarla.
#[derive(Debug)]
pub struct PortMapping {
    report: MappingReport,
    lease: Option<Lease>,
    renew: Option<tokio::task::JoinHandle<()>>,
}

impl PortMapping {
    fn unmapped(report: MappingReport) -> Self {
        Self {
            report,
            lease: None,
            renew: None,
        }
    }

    fn granted(report: MappingReport, lease: Lease, lifetime: Duration) -> Self {
        let granted = report.mapping().and_then(|mapping| mapping.lifetime);
        let renew = granted.map(|granted| {
            let lease = lease.clone();
            tokio::spawn(renew_loop(lease, lifetime, granted))
        });
        Self {
            report,
            lease: Some(lease),
            renew,
        }
    }

    /// Qué pasó con la petición.
    pub fn report(&self) -> &MappingReport {
        &self.report
    }

    /// La asignación, si la hay.
    pub fn mapping(&self) -> Option<&Mapping> {
        self.report.mapping()
    }
}

impl Drop for PortMapping {
    fn drop(&mut self) {
        if let Some(renew) = self.renew.take() {
            renew.abort();
        }
        if let Some(lease) = self.lease.take() {
            // Fuera del runtime (la sesión puede estar cerrándose): un hilo corto y bloqueante, que
            // el proceso espera al salir ([`wait_for_deletions`]).
            let deleting = Deleting::start();
            let _ = std::thread::Builder::new()
                .name("coroom-nat-borrado".into())
                .spawn(move || {
                    lease.delete_blocking();
                    drop(deleting);
                });
        }
    }
}

/// Borrados de asignaciones en curso: cuántos y el aviso de que terminó uno.
static DELETING: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());

/// Un borrado en curso mientras viva.
struct Deleting;

impl Deleting {
    fn start() -> Self {
        *DELETING.0.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        Deleting
    }
}

impl Drop for Deleting {
    fn drop(&mut self) {
        let mut pending = DELETING.0.lock().unwrap_or_else(PoisonError::into_inner);
        *pending = pending.saturating_sub(1);
        DELETING.1.notify_all();
    }
}

/// Espera hasta `timeout` a que terminen los borrados de las asignaciones que se soltaron (cada
/// [`PortMapping`] borra la suya en un hilo aparte). Se llama antes de que termine el proceso:
/// si no, el puerto se queda abierto en el router, y en los que solo admiten asignaciones
/// permanentes, para siempre. `false` si alguno no terminó a tiempo.
pub fn wait_for_deletions(timeout: Duration) -> bool {
    let pending = DELETING.0.lock().unwrap_or_else(PoisonError::into_inner);
    let (pending, _) = DELETING
        .1
        .wait_timeout_while(pending, timeout, |pending| *pending > 0)
        .unwrap_or_else(PoisonError::into_inner);
    *pending == 0
}

async fn renew_loop(lease: Lease, lifetime: Duration, mut granted: Duration) {
    loop {
        tokio::time::sleep(granted / 2).await;
        match lease.renew(lifetime).await {
            Ok(Some(renewed)) => granted = renewed,
            Ok(None) => return,
            Err(err) => {
                debug!(error = %err, "no se pudo renovar el puerto del router");
                granted = (RENEW_RETRY * 2).min(granted);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_reads_well() {
        let internal = SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 10), 47_000);
        let gateway = Ipv4Addr::new(192, 168, 1, 1);
        let mapped = MappingReport {
            gateway: Some(gateway),
            internal: Some(internal),
            outcome: MappingOutcome::Mapped(Mapping {
                external: SocketAddrV4::new(Ipv4Addr::new(100, 64, 3, 4), 47_000),
                protocol: MappingProtocol::Pcp,
                lifetime: Some(Duration::from_secs(7200)),
            }),
        };
        assert_eq!(
            mapped.to_string(),
            "puerto UDP 47000 abierto en el router 192.168.1.1 por PCP: 100.64.3.4:47000 (2 h)"
        );
        assert_eq!(mapped.external_ip(), Some(Ipv4Addr::new(100, 64, 3, 4)));
        let silent = MappingReport {
            gateway: Some(gateway),
            internal: Some(internal),
            outcome: MappingOutcome::NoResponse,
        };
        assert!(
            silent
                .to_string()
                .contains("no respondió a UPnP, NAT-PMP ni PCP")
        );
        assert!(silent.mapping().is_none());
    }

    #[test]
    fn failures_are_combined_into_one_outcome() {
        assert_eq!(
            combine([Failure::NoResponse, Failure::NoResponse].into_iter()),
            MappingOutcome::NoResponse
        );
        let refused = Failure::Refused {
            protocol: MappingProtocol::Upnp,
            error: "no permite abrir puertos".to_owned(),
            external: Some(Ipv4Addr::new(203, 0, 113, 9)),
        };
        assert!(matches!(
            combine([Failure::NoResponse, refused].into_iter()),
            MappingOutcome::ExternalOnly {
                protocol: MappingProtocol::Upnp,
                ..
            }
        ));
        let both = [
            Failure::Refused {
                protocol: MappingProtocol::Pcp,
                error: "no autorizado".to_owned(),
                external: None,
            },
            Failure::Refused {
                protocol: MappingProtocol::Upnp,
                error: "HTTP 500".to_owned(),
                external: None,
            },
        ];
        assert_eq!(
            combine(both.into_iter()),
            MappingOutcome::Refused("PCP: no autorizado; UPnP: HTTP 500".to_owned())
        );
    }
}
