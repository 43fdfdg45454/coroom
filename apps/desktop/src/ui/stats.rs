//! Cifras para la interfaz, en español: coma decimal, unidades con espacio fino de separación y
//! los redondeos que se leen de un vistazo (el panel de estadísticas sustituye al antiguo overlay
//! de `egui`).

/// Número con `decimals` decimales y coma decimal.
pub fn number(value: f64, decimals: usize) -> String {
    format!("{value:.decimals$}").replace('.', ",")
}

/// Milisegundos: `12,3 ms` por debajo de 100 y `140 ms` por encima.
pub fn ms(value: f64) -> String {
    if !value.is_finite() {
        return "—".to_owned();
    }
    if value < 100.0 {
        format!("{} ms", number(value, 1))
    } else {
        format!("{} ms", number(value, 0))
    }
}

/// `p50 / p95` en ms desde µs.
pub fn p50_p95(p50_us: u64, p95_us: u64) -> String {
    format!(
        "{} / {}",
        ms(p50_us as f64 / 1000.0),
        ms(p95_us as f64 / 1000.0)
    )
}

/// Bitrate: `850 kbit/s` o `8,4 Mbit/s`.
pub fn kbps(value: f64) -> String {
    if value >= 1000.0 {
        let mbps = value / 1000.0;
        let decimals = if mbps >= 100.0 { 0 } else { 1 };
        format!("{} Mbit/s", number(mbps, decimals))
    } else {
        format!("{} kbit/s", number(value, 0))
    }
}

/// Porcentaje con un decimal.
pub fn percent(value: f64) -> String {
    format!("{} %", number(value, 1))
}

/// Bytes: `512 B`, `1,5 MB`, `8,20 GB`.
pub fn bytes(value: u64) -> String {
    let v = value as f64;
    if v >= 1e9 {
        format!("{} GB", number(v / 1e9, 2))
    } else if v >= 1e6 {
        format!("{} MB", number(v / 1e6, 1))
    } else if v >= 1e3 {
        format!("{} kB", number(v / 1e3, 1))
    } else {
        format!("{value} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_in_spanish() {
        assert_eq!(ms(12.34), "12,3 ms");
        assert_eq!(ms(140.4), "140 ms");
        assert_eq!(ms(f64::NAN), "—");
        assert_eq!(kbps(850.0), "850 kbit/s");
        assert_eq!(kbps(8_400.0), "8,4 Mbit/s");
        assert_eq!(kbps(120_000.0), "120 Mbit/s");
        assert_eq!(percent(0.25), "0,2 %");
        assert_eq!(bytes(1_500_000), "1,5 MB");
        assert_eq!(bytes(8_200_000_000), "8,20 GB");
        assert_eq!(p50_p95(3_200, 4_100), "3,2 ms / 4,1 ms");
    }
}
