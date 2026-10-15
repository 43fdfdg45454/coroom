//! Puerta de enlace IPv4 por defecto del sistema, leída de su tabla de rutas (nunca supuesta), y la
//! dirección local con la que se le habla.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};

/// La puerta de enlace de la ruta IPv4 por defecto (la de menor métrica si hay varias).
pub fn default_gateway() -> Result<Ipv4Addr, String> {
    platform::default_gateway()
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

    pub fn default_gateway() -> Result<Ipv4Addr, String> {
        let text = std::fs::read_to_string("/proc/net/route")
            .map_err(|err| format!("/proc/net/route: {err}"))?;
        parse_proc_route(&text).ok_or_else(|| "sin ruta IPv4 por defecto".to_owned())
    }

    /// Lee `/proc/net/route`: la fila con destino `00000000` y la bandera `RTF_GATEWAY`, de menor
    /// métrica. Las direcciones van en hexadecimal con el orden de bytes del sistema.
    pub fn parse_proc_route(text: &str) -> Option<Ipv4Addr> {
        const RTF_UP: u32 = 0x1;
        const RTF_GATEWAY: u32 = 0x2;
        let mut best: Option<(u32, Ipv4Addr)> = None;
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
            if best.is_none_or(|(current, _)| metric < current) {
                best = Some((metric, gateway));
            }
        }
        best.map(|(_, gateway)| gateway)
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
            assert_eq!(parse_proc_route(text), Some(expected));
            assert_eq!(parse_proc_route("Iface\tDestination\n"), None);
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::net::Ipv4Addr;

    use windows::Win32::NetworkManagement::IpHelper::{
        FreeMibTable, GetIpForwardTable2, MIB_IPFORWARD_TABLE2,
    };
    use windows::Win32::Networking::WinSock::AF_INET;

    /// La fila `0.0.0.0/0` de menor métrica de `GetIpForwardTable2`.
    pub fn default_gateway() -> Result<Ipv4Addr, String> {
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
            let mut best: Option<(u32, Ipv4Addr)> = None;
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
                if best.is_none_or(|(metric, _)| row.Metric < metric) {
                    best = Some((row.Metric, gateway));
                }
            }
            FreeMibTable(table as *const _);
            best
        };
        best.map(|(_, gateway)| gateway)
            .ok_or_else(|| "sin ruta IPv4 por defecto".to_owned())
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod platform {
    use std::net::Ipv4Addr;

    pub fn default_gateway() -> Result<Ipv4Addr, String> {
        Err("la tabla de rutas no se lee en este sistema".to_owned())
    }
}
