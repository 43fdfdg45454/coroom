//! Fechas en UTC sin dependencias: segundos Unix, días desde 1970 y su fecha civil (algoritmo de
//! Howard Hinnant, `days_from_civil` / `civil_from_days`), para la contabilidad por día y mes del
//! relay y las fechas RFC 3339 de `/health`.

use std::time::{SystemTime, UNIX_EPOCH};

/// Segundos por día.
pub const DAY: i64 = 86_400;

/// Segundos Unix ahora.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Día (desde el 1970-01-01) de unos segundos Unix.
pub fn day_of(secs: i64) -> i64 {
    secs.div_euclid(DAY)
}

/// Fecha civil `(año, mes, día)` de un día desde 1970.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Día desde 1970 de una fecha civil.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Primer y último día (incluido) del mes de `day`.
pub fn month_range(day: i64) -> (i64, i64) {
    let (year, month, _) = civil_from_days(day);
    let first = days_from_civil(year, month, 1);
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    (first, days_from_civil(next_year, next_month, 1) - 1)
}

/// `AAAA-MM-DD` de un día.
pub fn day_label(day: i64) -> String {
    let (year, month, day) = civil_from_days(day);
    format!("{year:04}-{month:02}-{day:02}")
}

/// `AAAA-MM` del mes de un día.
pub fn month_label(day: i64) -> String {
    let (year, month, _) = civil_from_days(day);
    format!("{year:04}-{month:02}")
}

/// Primer día del mes `AAAA-MM`.
pub fn parse_month(text: &str) -> Option<i64> {
    let (year, month) = text.split_once('-')?;
    if year.len() != 4 || month.len() != 2 {
        return None;
    }
    let year: i64 = year.parse().ok()?;
    let month: u32 = month.parse().ok()?;
    (1..=12)
        .contains(&month)
        .then(|| days_from_civil(year, month, 1))
}

/// RFC 3339 en UTC (`2026-10-12T08:30:00Z`).
pub fn rfc3339(secs: i64) -> String {
    let day = day_of(secs);
    let rest = secs.rem_euclid(DAY);
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        day_label(day),
        rest / 3600,
        (rest / 60) % 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_round_trip() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 2, 29), 11_016);
        for day in [-1000, 0, 11_016, 19_000, 20_738, 50_000] {
            let (y, m, d) = civil_from_days(day);
            assert_eq!(days_from_civil(y, m, d), day, "{day}");
        }
    }

    #[test]
    fn labels_and_ranges() {
        let day = days_from_civil(2026, 10, 12);
        assert_eq!(day_label(day), "2026-10-12");
        assert_eq!(month_label(day), "2026-10");
        let (first, last) = month_range(day);
        assert_eq!(day_label(first), "2026-10-01");
        assert_eq!(day_label(last), "2026-10-31");
        let (first, last) = month_range(days_from_civil(2028, 2, 10));
        assert_eq!(
            (day_label(first), day_label(last)),
            ("2028-02-01".into(), "2028-02-29".into())
        );
        assert_eq!(parse_month("2026-10"), Some(first_of(2026, 10)));
        assert_eq!(parse_month("2026-13"), None);
        assert_eq!(parse_month("26-1"), None);
        assert_eq!(rfc3339(day * DAY + 3600 + 61), "2026-10-12T01:01:01Z");
    }

    fn first_of(year: i64, month: u32) -> i64 {
        days_from_civil(year, month, 1)
    }
}
