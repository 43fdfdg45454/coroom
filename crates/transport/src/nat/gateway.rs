//! Ruta IPv4 por defecto del sistema, leída de su tabla de rutas (nunca supuesta): la puerta de
//! enlace, la MTU del adaptador por el que sale y la dirección local con la que se le habla.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::RwLock;

/// La ruta IPv4 por defecto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultRoute {
    /// Puerta de enlace (la de menor métrica si hay varias).
    pub gateway: Ipv4Addr,
    /// MTU del adaptador por el que sale, si el sistema la dice.
    pub mtu: Option<u32>,
}

/// Ruta que da quien embebe la biblioteca cuando el sistema no deja leer su tabla de rutas: en
/// Android `/proc/net/route` está vedado a las aplicaciones y la ruta sale de `ConnectivityManager`
/// (la aplicación la actualiza con [`set_default_route`] cada vez que cambia la red).
static INJECTED: RwLock<Option<DefaultRoute>> = RwLock::new(None);

/// Fija (o quita, con `None`) la ruta por defecto que da la plataforma. Si hay una, manda sobre la
/// tabla de rutas del sistema.
pub fn set_default_route(route: Option<DefaultRoute>) {
    *INJECTED
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = route;
}

/// La ruta IPv4 por defecto.
pub fn default_route() -> Result<DefaultRoute, String> {
    let injected = INJECTED
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    match injected {
        Some(route) => Ok(route),
        None => platform::default_route(),
    }
}

/// La puerta de enlace de la ruta IPv4 por defecto.
pub fn default_gateway() -> Result<Ipv4Addr, String> {
    default_route().map(|route| route.gateway)
}

/// Dirección IPv4 local con la que el sistema habla con `gateway` (la del socket que la ruta
/// elige; no se envía nada).
pub(super) fn local_ip_towards(gateway: Ipv4Addr) -> Result<Ipv4Addr, String> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(|err| err.to_string())?;
    socket
        .connect(SocketAddrV4::new(gateway, super::GATEWAY_PORT))
        .map_err(|err| err.to_string())?;
    match socket.local_addr().map_err(|err| err.to_string())? {
        std::net::SocketAddr::V4(addr) if !addr.ip().is_unspecified() => Ok(*addr.ip()),
        other => Err(format!(
            "el sistema no da una dirección IPv4 hacia {gateway} ({other})"
        )),
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::net::Ipv4Addr;

    use super::DefaultRoute;

    pub fn default_route() -> Result<DefaultRoute, String> {
        let text = std::fs::read_to_string("/proc/net/route")
            .map_err(|err| format!("/proc/net/route: {err}"))?;
        let (gateway, iface) =
            parse_proc_route(&text).ok_or_else(|| "sin ruta IPv4 por defecto".to_owned())?;
        let mtu = std::fs::read_to_string(format!("/sys/class/net/{iface}/mtu"))
            .ok()
            .and_then(|text| text.trim().parse().ok());
        Ok(DefaultRoute { gateway, mtu })
    }

    /// Lee `/proc/net/route`: la fila con destino `00000000` y la bandera `RTF_GATEWAY`, de menor
    /// métrica, con su interfaz. Las direcciones van en hexadecimal con el orden de bytes del
    /// sistema.
    pub fn parse_proc_route(text: &str) -> Option<(Ipv4Addr, String)> {
        const RTF_UP: u32 = 0x1;
        const RTF_GATEWAY: u32 = 0x2;
        let mut best: Option<(u32, Ipv4Addr, String)> = None;
        for line in text.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 7 || fields[1] != "00000000" {
                continue;
            }
            let flags = u32::from_str_radix(fields[3], 16).unwrap_or(0);
            if flags & (RTF_UP | RTF_GATEWAY) != RTF_UP | RTF_GATEWAY {
                continue;
            }
            let Ok(raw) = u32::from_str_radix(fields[2], 16) else {
                continue;
            };
            let gateway = Ipv4Addr::from(raw.to_ne_bytes());
            if gateway.is_unspecified() {
                continue;
            }
            let metric = fields[6].parse::<u32>().unwrap_or(u32::MAX);
            if best
                .as_ref()
                .is_none_or(|(current, _, _)| metric < *current)
            {
                best = Some((metric, gateway, fields[0].to_owned()));
            }
        }
        best.map(|(_, gateway, iface)| (gateway, iface))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_default_route_is_read_from_proc() {
            let text = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                        wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
                        eth0\t00000000\t0100000A\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
                        eth0\t0000000A\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n";
            let expected = Ipv4Addr::from(0x0100_000Au32.to_ne_bytes());
            assert_eq!(
                parse_proc_route(text),
                Some((expected, "eth0".to_owned())),
                "la de menor métrica, con su interfaz"
            );
            assert_eq!(parse_proc_route("Iface\tDestination\n"), None);
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::net::Ipv4Addr;

    use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR};
    use windows::Win32::NetworkManagement::IpHelper::{
        FreeMibTable, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
        GAA_FLAG_SKIP_UNICAST, GetAdaptersAddresses, GetIpForwardTable2, IP_ADAPTER_ADDRESSES_LH,
        MIB_IPFORWARD_TABLE2,
    };
    use windows::Win32::Networking::WinSock::{AF_INET, AF_UNSPEC};

    use super::DefaultRoute;

    /// La fila `0.0.0.0/0` de menor métrica de `GetIpForwardTable2` y la MTU de su adaptador.
    #[allow(unsafe_code)] // Win32 sin envoltorio seguro; cada bloque lleva su `SAFETY:`.
    pub fn default_route() -> Result<DefaultRoute, String> {
        let mut table: *mut MIB_IPFORWARD_TABLE2 = std::ptr::null_mut();
        // SAFETY: `GetIpForwardTable2` rellena `table` con una tabla que es nuestra hasta
        // `FreeMibTable`; solo se lee dentro de este bloque y se libera una vez.
        let best = unsafe {
            let status = GetIpForwardTable2(AF_INET, &mut table);
            if status.is_err() || table.is_null() {
                return Err(format!("GetIpForwardTable2: error {}", status.0));
            }
            let count = (*table).NumEntries as usize;
            let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), count);
            let mut best: Option<(u32, Ipv4Addr, u32)> = None;
            for row in rows {
                if row.DestinationPrefix.PrefixLength != 0
                    || row.Loopback
                    || row.NextHop.si_family != AF_INET
                {
                    continue;
                }
                let gateway = Ipv4Addr::from(row.NextHop.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes());
                if gateway.is_unspecified() {
                    continue;
                }
                if best.is_none_or(|(metric, _, _)| row.Metric < metric) {
                    best = Some((row.Metric, gateway, row.InterfaceIndex));
                }
            }
            FreeMibTable(table as *const _);
            best
        };
        let (_, gateway, index) = best.ok_or_else(|| "sin ruta IPv4 por defecto".to_owned())?;
        Ok(DefaultRoute {
            gateway,
            mtu: adapter_mtu(index),
        })
    }

    /// La MTU del adaptador con índice `index` (`GetAdaptersAddresses`).
    #[allow(unsafe_code)] // Win32 sin envoltorio seguro; cada bloque lleva su `SAFETY:`.
    fn adapter_mtu(index: u32) -> Option<u32> {
        let flags = GAA_FLAG_SKIP_UNICAST
            | GAA_FLAG_SKIP_ANYCAST
            | GAA_FLAG_SKIP_MULTICAST
            | GAA_FLAG_SKIP_DNS_SERVER;
        let mut size: u32 = 16 * 1024;
        for _ in 0..3 {
            // Alineado a 8 bytes, como pide `IP_ADAPTER_ADDRESSES_LH`.
            let mut buf = vec![0u64; (size as usize).div_ceil(8)];
            let first = buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
            // SAFETY: `first` apunta a `size` bytes nuestros; el sistema escribe en ellos una lista
            // enlazada de adaptadores (o pide más sitio en `size`) y solo se recorre si dijo que
            // la rellenó.
            unsafe {
                let status = GetAdaptersAddresses(
                    u32::from(AF_UNSPEC.0),
                    flags,
                    None,
                    Some(first),
                    &mut size,
                );
                if status == ERROR_BUFFER_OVERFLOW.0 {
                    continue;
                }
                if status != NO_ERROR.0 {
                    return None;
                }
                let mut current = first;
                while !current.is_null() {
                    let adapter = &*current;
                    if adapter.Anonymous1.Anonymous.IfIndex == index {
                        return Some(adapter.Mtu);
                    }
                    current = adapter.Next;
                }
                return None;
            }
        }
        None
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod platform {
    use super::DefaultRoute;

    pub fn default_route() -> Result<DefaultRoute, String> {
        Err(
            "la tabla de rutas no se lee en este sistema y la aplicación aún no dio la ruta"
                .to_owned(),
        )
    }
}
