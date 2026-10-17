//! Cifras para la interfaz, en español: coma decimal, unidades con espacio fino de separación y
//! los redondeos que se leen de un vistazo (el panel de estadísticas sustituye al antiguo overlay
//! de `egui`).

use coroom_session::remote::QualityLevel;

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

/// Los valores más anchos de [`ms`], [`kbps`] y [`percent`], para medir las ranuras de ancho
/// fijo (`coroom_ui::widget::figure`): un 8 (la cifra más ancha en casi todas las letras) en cada
/// posición de cada forma que pueden tomar.
pub const MS_WIDEST: [&str; 2] = ["88,8 ms", "888 ms"];
/// Ver [`MS_WIDEST`].
pub const KBPS_WIDEST: [&str; 3] = ["888 kbit/s", "88,8 Mbit/s", "888 Mbit/s"];
/// Ver [`MS_WIDEST`].
pub const PERCENT_WIDEST: [&str; 1] = ["88,8 %"];

/// Cuenta atrás en minutos y segundos: `0:42`, `1:05`. Va en una ranura de ancho fijo medida con
/// [`CLOCK_WIDEST`] (`coroom_ui::widget::figure`), porque cambia cada segundo.
pub fn clock(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// La cuenta atrás más ancha de [`clock`] por debajo de diez minutos (las de la interfaz duran uno).
pub const CLOCK_WIDEST: &str = "8:88";

/// Tiempo transcurrido (lo que lleva la transmisión en vivo): `0:42`, `12:04` y, desde la hora,
/// `1:02:03`. Va en una ranura de ancho fijo medida con [`elapsed_widest`].
pub fn elapsed(seconds: u64) -> String {
    if seconds < 3600 {
        clock(seconds)
    } else {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            (seconds / 60) % 60,
            seconds % 60
        )
    }
}

/// La forma más ancha de [`elapsed`] para `seconds`: la ranura no cambia en toda la primera hora
/// (de `0:00` a `59:59`) y solo crece una vez, al pasar a `h:mm:ss`.
pub fn elapsed_widest(seconds: u64) -> &'static str {
    if seconds < 3600 { "88:88" } else { "88:88:88" }
}

/// La latencia de un enlace, con la misma medida y la misma escala en toda la aplicación (la fila
/// de cada invitado en la sala, la píldora y la barra del invitado y Diagnóstico): el tiempo de
/// ida y vuelta (RTT) en ms enteros («14 ms») y su nivel en la escala de la sala, el RTT más el
/// pipeline típico ([`QualityLevel::from_rtt_ms`]), la misma con la que el invitado estima la
/// calidad antes de entrar. Va en una ranura de [`LATENCY_WIDEST`].
pub fn latency(rtt_ms: f64) -> (String, QualityLevel) {
    let rtt_ms = if rtt_ms.is_finite() {
        rtt_ms.max(0.0)
    } else {
        0.0
    };
    (
        format!("{} ms", number(rtt_ms, 0)),
        QualityLevel::from_rtt_ms(rtt_ms),
    )
}

/// La latencia más ancha de [`latency`] (hasta 999 ms; más ya es «Mala» y la ranura crece).
pub const LATENCY_WIDEST: &str = "888 ms";

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
        assert_eq!(elapsed(0), "0:00");
        assert_eq!(elapsed(724), "12:04");
        assert_eq!(elapsed(3_723), "1:02:03");
        assert_eq!(elapsed_widest(3_599), "88:88");
        assert_eq!(elapsed_widest(3_600), "88:88:88");
    }

    /// Una sola escala: 14 ms de ida y vuelta es «Excelente» en la sala y en el invitado (con el
    /// pipeline típico sumado), y la cifra es la misma en todos lados.
    #[test]
    fn latency_has_one_measure_and_one_scale() {
        assert_eq!(latency(14.2), ("14 ms".to_owned(), QualityLevel::Excellent));
        assert_eq!(latency(61.4).1, QualityLevel::Acceptable);
        assert_eq!(latency(f64::NAN).0, "0 ms");
        assert_eq!(latency(61.4).1, QualityLevel::from_rtt_ms(61.4));
    }
}
