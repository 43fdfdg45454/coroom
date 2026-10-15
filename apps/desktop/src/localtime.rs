//! Hora local para lo que se enseña al usuario (la hora de los mensajes del chat), sin
//! dependencias nuevas.
//!
//! - **Linux** (y otros Unix): la zona de `TZ` (RFC 8536: `:ruta`, `Zona/Ciudad` bajo
//!   `/usr/share/zoneinfo` o una regla POSIX como `CET-1CEST,M3.5.0,M10.5.0/3`) o, sin `TZ`,
//!   `/etc/localtime`. Se lee el fichero TZif (versión 1, 2 o 3), se busca la última transición
//!   anterior al instante y, más allá de la última, se aplica la regla POSIX del pie del fichero.
//! - **Windows**: `ActiveTimeBias` del registro (`TimeZoneInformation`), el desfase que el sistema
//!   aplica ahora.
//!
//! Si no se puede saber la zona, la hora sale en UTC.

/// Desfase con UTC en segundos (local = UTC + desfase) en el instante `unix_s`.
pub fn utc_offset(unix_s: i64) -> i64 {
    system::utc_offset(unix_s).unwrap_or(0)
}

/// `HH:MM` en hora local del instante Unix `unix_ms` (en ms).
pub fn clock_hhmm(unix_ms: u64) -> String {
    let unix_s = i64::try_from(unix_ms / 1000).unwrap_or(i64::MAX / 2);
    hhmm(unix_s, utc_offset(unix_s))
}

/// `HH:MM` de `unix_s` con el desfase `offset` (s).
pub fn hhmm(unix_s: i64, offset: i64) -> String {
    let seconds = (unix_s + offset).rem_euclid(86_400);
    format!("{:02}:{:02}", seconds / 3600, seconds % 3600 / 60)
}

#[cfg(unix)]
mod system {
    use std::path::{Path, PathBuf};

    use super::{PosixTz, tzif_offset};

    pub(super) fn utc_offset(unix_s: i64) -> Option<i64> {
        match std::env::var("TZ") {
            Ok(tz) if !tz.is_empty() => {
                let name = tz.strip_prefix(':').unwrap_or(&tz);
                let path = if name.starts_with('/') {
                    PathBuf::from(name)
                } else {
                    Path::new("/usr/share/zoneinfo").join(name)
                };
                // Sin `..`: un nombre de zona no sale del directorio de zonas.
                let file = (!name.contains("..")).then(|| std::fs::read(&path).ok());
                match file.flatten() {
                    Some(bytes) => tzif_offset(&bytes, unix_s),
                    None => PosixTz::parse(name).map(|tz| tz.offset(unix_s)),
                }
            }
            _ => std::fs::read("/etc/localtime")
                .ok()
                .and_then(|bytes| tzif_offset(&bytes, unix_s)),
        }
    }
}

#[cfg(target_os = "windows")]
mod system {
    use winreg::RegKey;
    use winreg::enums::HKEY_LOCAL_MACHINE;

    const TIME_ZONE: &str = r"SYSTEM\CurrentControlSet\Control\TimeZoneInformation";

    /// `ActiveTimeBias`: minutos que hay que sumar a la hora local para tener UTC, ahora.
    pub(super) fn utc_offset(_unix_s: i64) -> Option<i64> {
        let bias = RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(TIME_ZONE)
            .and_then(|key| key.get_value::<u32, _>("ActiveTimeBias"))
            .ok()?;
        Some(-i64::from(bias as i32) * 60)
    }
}

#[cfg(not(any(unix, target_os = "windows")))]
mod system {
    pub(super) fn utc_offset(_unix_s: i64) -> Option<i64> {
        None
    }
}

/// Lector de enteros big-endian de un TZif.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(len)?;
        let out = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(out)
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4)?.try_into().ok().map(u32::from_be_bytes)
    }

    fn i32(&mut self) -> Option<i32> {
        self.take(4)?.try_into().ok().map(i32::from_be_bytes)
    }

    fn i64(&mut self) -> Option<i64> {
        self.take(8)?.try_into().ok().map(i64::from_be_bytes)
    }
}

/// Cabecera TZif: versión y los seis contadores.
struct Header {
    version: u8,
    isutcnt: usize,
    isstdcnt: usize,
    leapcnt: usize,
    timecnt: usize,
    typecnt: usize,
    charcnt: usize,
}

fn header(reader: &mut Reader<'_>) -> Option<Header> {
    if reader.take(4)? != b"TZif" {
        return None;
    }
    let version = reader.take(1)?[0];
    reader.take(15)?;
    let mut count = || reader.u32().map(|n| n as usize);
    Some(Header {
        version,
        isutcnt: count()?,
        isstdcnt: count()?,
        leapcnt: count()?,
        timecnt: count()?,
        typecnt: count()?,
        charcnt: count()?,
    })
}

/// Desfase en `unix_s` según un fichero TZif (RFC 8536).
pub fn tzif_offset(bytes: &[u8], unix_s: i64) -> Option<i64> {
    let mut reader = Reader { bytes, at: 0 };
    let mut head = header(&mut reader)?;
    let mut time_len = 4;
    if head.version >= b'2' {
        // Se salta el bloque de la versión 1 (tiempos de 32 bits) y se lee el de 64.
        let v1 = head.timecnt * 5
            + head.typecnt * 6
            + head.charcnt
            + head.leapcnt * 8
            + head.isstdcnt
            + head.isutcnt;
        reader.take(v1)?;
        head = header(&mut reader)?;
        time_len = 8;
    }
    let mut times = Vec::with_capacity(head.timecnt);
    for _ in 0..head.timecnt {
        times.push(if time_len == 8 {
            reader.i64()?
        } else {
            i64::from(reader.i32()?)
        });
    }
    let indices = reader.take(head.timecnt)?.to_vec();
    let mut offsets = Vec::with_capacity(head.typecnt);
    for _ in 0..head.typecnt {
        offsets.push(i64::from(reader.i32()?));
        reader.take(2)?;
    }
    reader.take(head.charcnt + head.leapcnt * (time_len + 4) + head.isstdcnt + head.isutcnt)?;
    let footer = if time_len == 8 {
        let rest = reader.bytes.get(reader.at..).unwrap_or_default();
        std::str::from_utf8(rest)
            .ok()
            .and_then(|text| text.strip_prefix('\n'))
            .and_then(|text| text.split('\n').next())
            .filter(|text| !text.is_empty())
            .and_then(PosixTz::parse)
    } else {
        None
    };
    let last = times.partition_point(|&at| at <= unix_s);
    if last == times.len()
        && let Some(footer) = footer
    {
        return Some(footer.offset(unix_s));
    }
    if last == 0 {
        return offsets.first().copied();
    }
    let index = usize::from(*indices.get(last - 1)?);
    offsets.get(index).copied()
}

/// Día de una regla de cambio de hora POSIX.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Day {
    /// `Jn`: día 1-365 sin contar nunca el 29 de febrero.
    Julian(u16),
    /// `n`: día 0-365 contando el 29 de febrero.
    Zero(u16),
    /// `Mm.w.d`: día `d` (0 = domingo) de la semana `w` (5 = la última) del mes `m`.
    Month(u8, u8, u8),
}

/// Cambio de hora de una regla POSIX: el día y la hora local (en s desde las 0:00) a la que pasa.
type Change = (Day, i64);

/// Regla POSIX (`std offset [dst [offset] [,start[/time],end[/time]]]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PosixTz {
    /// Desfase de la hora normal (local = UTC + esto), en s.
    std: i64,
    /// Horario de verano: desfase, inicio y fin (con su hora local en s).
    dst: Option<(i64, Change, Change)>,
}

fn name(text: &str) -> Option<&str> {
    if let Some(rest) = text.strip_prefix('<') {
        let end = rest.find('>')?;
        return Some(&rest[end + 1..]);
    }
    let end = text
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(text.len());
    (end >= 3).then(|| &text[end..])
}

/// `[+-]hh[:mm[:ss]]` en s, y lo que sigue.
fn duration(text: &str) -> Option<(i64, &str)> {
    let (sign, text) = match text.as_bytes().first() {
        Some(b'-') => (-1, &text[1..]),
        Some(b'+') => (1, &text[1..]),
        _ => (1, text),
    };
    let end = text
        .find(|c: char| !(c.is_ascii_digit() || c == ':'))
        .unwrap_or(text.len());
    let mut total = 0i64;
    let mut parts = text[..end].split(':');
    for unit in [3600, 60, 1] {
        match parts.next() {
            Some(part) if !part.is_empty() => total += part.parse::<i64>().ok()? * unit,
            Some(_) => return None,
            None => break,
        }
    }
    if total == 0 && !text[..end].starts_with('0') {
        return None;
    }
    Some((sign * total, &text[end..]))
}

fn rule(text: &str) -> Option<Change> {
    let (day, time) = match text.split_once('/') {
        Some((day, time)) => (day, duration(time).filter(|(_, rest)| rest.is_empty())?.0),
        None => (text, 2 * 3600),
    };
    let day = if let Some(n) = day.strip_prefix('J') {
        Day::Julian(n.parse().ok().filter(|n| (1..=365).contains(n))?)
    } else if let Some(m) = day.strip_prefix('M') {
        let mut parts = m.split('.').map(|part| part.parse::<u8>().ok());
        let (m, w, d) = (parts.next()??, parts.next()??, parts.next()??);
        ((1..=12).contains(&m) && (1..=5).contains(&w) && d <= 6).then_some(Day::Month(m, w, d))?
    } else {
        Day::Zero(day.parse().ok().filter(|n| *n <= 365)?)
    };
    Some((day, time))
}

/// Días desde 1970-01-01 de una fecha civil (algoritmo de H. Hinnant).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Año civil de un día desde 1970-01-01.
fn year_of(days: i64) -> i64 {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    yoe + era * 400 + i64::from(month <= 2)
}

fn leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Segundos desde 1970 (en hora local) en que cae `day` a las `time` del año `year`.
fn local_seconds(year: i64, (day, time): Change) -> i64 {
    let jan1 = days_from_civil(year, 1, 1);
    let days = match day {
        Day::Julian(n) => {
            let n = i64::from(n) - 1;
            jan1 + n + i64::from(leap(year) && n >= 59)
        }
        Day::Zero(n) => jan1 + i64::from(n),
        Day::Month(m, w, d) => {
            let first = days_from_civil(year, i64::from(m), 1);
            let weekday = (first + 4).rem_euclid(7);
            let mut day = 1 + (i64::from(d) - weekday).rem_euclid(7) + 7 * (i64::from(w) - 1);
            let next = if m == 12 {
                days_from_civil(year + 1, 1, 1)
            } else {
                days_from_civil(year, i64::from(m) + 1, 1)
            };
            while day > next - first {
                day -= 7;
            }
            first + day - 1
        }
    };
    days * 86_400 + time
}

impl PosixTz {
    /// Lee una regla POSIX; `None` si no lo es.
    pub fn parse(text: &str) -> Option<PosixTz> {
        let rest = name(text)?;
        let (std, rest) = duration(rest)?;
        let std = -std;
        if rest.is_empty() {
            return Some(PosixTz { std, dst: None });
        }
        let rest = name(rest)?;
        let (dst, rest) = match rest.as_bytes().first() {
            Some(b',') | None => (std + 3600, rest),
            Some(_) => {
                let (dst, rest) = duration(rest)?;
                (-dst, rest)
            }
        };
        // Sin reglas, las de POSIX por defecto (las de EE. UU.).
        let rules = rest.strip_prefix(',').unwrap_or("M3.2.0,M11.1.0");
        let (start, end) = rules.split_once(',')?;
        Some(PosixTz {
            std,
            dst: Some((dst, rule(start)?, rule(end)?)),
        })
    }

    /// Desfase en `unix_s`.
    pub fn offset(&self, unix_s: i64) -> i64 {
        let Some((dst, start, end)) = self.dst else {
            return self.std;
        };
        let year = year_of((unix_s + self.std).div_euclid(86_400));
        // El inicio va en hora normal y el fin en hora de verano.
        let starts = local_seconds(year, start) - self.std;
        let ends = local_seconds(year, end) - dst;
        let summer = if starts < ends {
            (starts..ends).contains(&unix_s)
        } else {
            unix_s >= starts || unix_s < ends
        };
        if summer { dst } else { self.std }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-07-01 12:00:00 UTC y 2026-01-15 12:00:00 UTC.
    const SUMMER: i64 = 1_782_907_200;
    const WINTER: i64 = 1_768_478_400;

    #[test]
    fn civil_dates_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2026, 7, 1) * 86_400 + 12 * 3600, SUMMER);
        assert_eq!(days_from_civil(2026, 1, 15) * 86_400 + 12 * 3600, WINTER);
        for days in [-800_000, -1, 0, 59, 365, 20_000, 2_000_000] {
            let year = year_of(days);
            assert!(days_from_civil(year, 1, 1) <= days, "{days}");
            assert!(days < days_from_civil(year + 1, 1, 1), "{days}");
        }
    }

    #[test]
    fn posix_rules_give_summer_and_winter_time() {
        let europe = PosixTz::parse("CET-1CEST,M3.5.0,M10.5.0/3").expect("regla");
        assert_eq!(europe.offset(SUMMER), 2 * 3600);
        assert_eq!(europe.offset(WINTER), 3600);
        // Cambios de 2026: 29 de marzo a la 01:00 UTC y 25 de octubre a la 01:00 UTC.
        let march = days_from_civil(2026, 3, 29) * 86_400 + 3600;
        assert_eq!(europe.offset(march - 1), 3600);
        assert_eq!(europe.offset(march), 7200);
        let october = days_from_civil(2026, 10, 25) * 86_400 + 3600;
        assert_eq!(europe.offset(october - 1), 7200);
        assert_eq!(europe.offset(october), 3600);
        // Hemisferio sur: verano de diciembre a abril.
        let south = PosixTz::parse("<-04>4<-03>,M9.1.6/24,M4.1.6/24").expect("regla");
        assert_eq!(south.offset(WINTER), -3 * 3600);
        assert_eq!(south.offset(SUMMER), -4 * 3600);
        // Sin horario de verano, con nombre entre <>.
        let fixed = PosixTz::parse("<-03>3").expect("regla");
        assert_eq!(
            (fixed.offset(SUMMER), fixed.offset(WINTER)),
            (-3 * 3600, -3 * 3600)
        );
        assert_eq!(PosixTz::parse("UTC0").expect("utc").offset(SUMMER), 0);
        assert_eq!(
            PosixTz::parse("IST-5:30")
                .expect("media hora")
                .offset(SUMMER),
            5 * 3600 + 1800
        );
        let us = PosixTz::parse("EST5EDT").expect("reglas por defecto");
        assert_eq!(
            (us.offset(SUMMER), us.offset(WINTER)),
            (-4 * 3600, -5 * 3600)
        );
        for bad in ["", "Europe/Madrid", "X1", "CET-1CEST,M13.1.0,M10.5.0"] {
            assert_eq!(PosixTz::parse(bad), None, "{bad}");
        }
    }

    /// Un TZif v2 hecho a mano: dos tipos (UTC+1 y UTC+2), una transición en 2000 y el pie POSIX
    /// para lo que viene después.
    fn tzif(footer: &str) -> Vec<u8> {
        let header = |timecnt: u32, typecnt: u32, charcnt: u32| {
            let mut out = b"TZif2".to_vec();
            out.extend_from_slice(&[0; 15]);
            for count in [0, 0, 0, timecnt, typecnt, charcnt] {
                out.extend_from_slice(&u32::to_be_bytes(count));
            }
            out
        };
        let types = |out: &mut Vec<u8>| {
            for (offset, dst, idx) in [(3600i32, 0u8, 0u8), (7200, 1, 4)] {
                out.extend_from_slice(&offset.to_be_bytes());
                out.extend_from_slice(&[dst, idx]);
            }
            out.extend_from_slice(b"CET\0CEST\0");
        };
        let transition: i64 = 946_684_800; // 2000-01-01
        let mut out = header(1, 2, 9);
        out.extend_from_slice(&(transition as i32).to_be_bytes());
        out.push(1);
        types(&mut out);
        out.extend(header(1, 2, 9));
        out.extend_from_slice(&transition.to_be_bytes());
        out.push(1);
        types(&mut out);
        out.extend_from_slice(format!("\n{footer}\n").as_bytes());
        out
    }

    #[test]
    fn tzif_files_use_transitions_and_then_the_footer() {
        let file = tzif("CET-1CEST,M3.5.0,M10.5.0/3");
        assert_eq!(tzif_offset(&file, 0), Some(3600), "antes: el primer tipo");
        assert_eq!(tzif_offset(&file, 946_684_800 - 1), Some(3600));
        assert_eq!(tzif_offset(&file, SUMMER), Some(7200), "después: el pie");
        assert_eq!(tzif_offset(&file, WINTER), Some(3600));
        // Sin pie, lo de la última transición.
        assert_eq!(tzif_offset(&tzif(""), WINTER), Some(7200));
        assert_eq!(tzif_offset(b"no es TZif", 0), None);
        assert_eq!(tzif_offset(&file[..60], 0), None, "truncado");
    }

    #[test]
    fn clocks_print_hours_and_minutes() {
        assert_eq!(hhmm(SUMMER, 7200), "14:00");
        assert_eq!(hhmm(SUMMER + 59, -3 * 3600), "09:00");
        assert_eq!(hhmm(WINTER - 12 * 3600 - 60, 0), "23:59");
        assert_eq!(clock_hhmm(0).len(), 5);
    }
}
