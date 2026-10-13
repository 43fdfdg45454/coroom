//! Lectura mínima de un descriptor de informes HID (HID 1.11, §6.2.2), para los mandos «tal cual».
//!
//! El invitado manda el descriptor de su mando y el anfitrión crea un dispositivo idéntico. Antes,
//! los dos lo leen con [`parse`] para saber:
//!
//! - qué **colecciones de aplicación** declara ([`HidSummary::applications`]): un mando
//!   (`Generic Desktop` / `Joystick`, `Game Pad` o `Multi-axis Controller`) o algo que escribe o
//!   mueve el puntero (teclado, teclado numérico, ratón, control del sistema o de consumo), que el
//!   anfitrión solo acepta de un invitado con permiso de teclado y ratón;
//! - qué **informes de característica** tiene y su tamaño ([`HidSummary::features`]): el invitado
//!   los lee del mando al conectarlo y el anfitrión los devuelve cuando el driver los pide (los de
//!   calibración del DualSense, por ejemplo);
//! - el tamaño del mayor **informe de entrada** ([`HidSummary::max_input`]).
//!
//! No interpreta botones ni ejes: el dispositivo del anfitrión recibe los informes tal cual. Es
//! un analizador de datos de la red, sin `unsafe` y con límites: [`MAX_DESCRIPTOR`] bytes, pila de
//! globales de [`MAX_PUSH`] niveles, [`MAX_REPORT_BYTES`] por informe.

use std::collections::BTreeMap;
use std::fmt;

/// Tamaño máximo de un descriptor (`HID_MAX_DESCRIPTOR_SIZE` de Linux).
pub const MAX_DESCRIPTOR: usize = 4096;

/// Tamaño máximo de un informe, con su identificador.
pub const MAX_REPORT_BYTES: usize = 512;

/// Niveles de `Push` como mucho.
pub const MAX_PUSH: usize = 8;

/// Página `Generic Desktop`.
pub const PAGE_GENERIC_DESKTOP: u16 = 0x01;
/// Página `Consumer`.
pub const PAGE_CONSUMER: u16 = 0x0C;

/// Colección de aplicación de primer nivel: página y uso.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Application {
    /// Página de uso.
    pub page: u16,
    /// Uso.
    pub usage: u16,
}

impl Application {
    /// Mando: `Joystick`, `Game Pad` o `Multi-axis Controller`.
    pub const fn is_controller(self) -> bool {
        self.page == PAGE_GENERIC_DESKTOP && matches!(self.usage, 0x04 | 0x05 | 0x08)
    }

    /// Escribe o mueve el puntero: ratón, teclado, teclado numérico, control del sistema o de
    /// consumo (teclas multimedia).
    pub const fn is_keyboard_or_mouse(self) -> bool {
        (self.page == PAGE_GENERIC_DESKTOP
            && matches!(self.usage, 0x01 | 0x02 | 0x06 | 0x07 | 0x80))
            || (self.page == PAGE_CONSUMER && self.usage == 0x01)
    }
}

/// Lo que importa de un descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HidSummary {
    /// Colecciones de aplicación de primer nivel, en orden.
    pub applications: Vec<Application>,
    /// Informes de característica: identificador (0 si el descriptor no usa identificadores) y
    /// bytes, contando el del identificador.
    pub features: Vec<(u8, usize)>,
    /// Bytes del mayor informe de entrada, con su identificador si lo hay.
    pub max_input: usize,
    /// Bytes del mayor informe de salida, con su identificador si lo hay.
    pub max_output: usize,
    /// Si el descriptor usa identificadores de informe.
    pub report_ids: bool,
}

impl HidSummary {
    /// Declara al menos un mando.
    pub fn has_controller(&self) -> bool {
        self.applications.iter().any(|app| app.is_controller())
    }

    /// Declara algo que escribe o mueve el puntero.
    pub fn has_keyboard_or_mouse(&self) -> bool {
        self.applications
            .iter()
            .any(|app| app.is_keyboard_or_mouse())
    }
}

/// Descriptor que no se puede leer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HidError {
    /// Vacío o de más de [`MAX_DESCRIPTOR`] bytes.
    Size(usize),
    /// Un elemento se sale del descriptor.
    Truncated,
    /// `Push` sin sitio o `Pop` sin `Push`.
    Stack,
    /// `End Collection` sin `Collection`.
    Collection,
    /// Un informe de más de [`MAX_REPORT_BYTES`] bytes (o un tamaño absurdo).
    ReportTooLarge,
}

impl fmt::Display for HidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HidError::Size(len) => write!(
                f,
                "descriptor HID de {len} bytes (tiene que tener entre 1 y {MAX_DESCRIPTOR})"
            ),
            HidError::Truncated => f.write_str("descriptor HID truncado"),
            HidError::Stack => f.write_str("descriptor HID con Push y Pop desparejados"),
            HidError::Collection => f.write_str("descriptor HID con colecciones desparejadas"),
            HidError::ReportTooLarge => write!(
                f,
                "descriptor HID con un informe de más de {MAX_REPORT_BYTES} bytes"
            ),
        }
    }
}

impl std::error::Error for HidError {}

#[derive(Debug, Clone, Copy, Default)]
struct Globals {
    page: u16,
    report_size: u32,
    report_count: u32,
    report_id: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Input,
    Output,
    Feature,
}

/// Lee `descriptor`.
pub fn parse(descriptor: &[u8]) -> Result<HidSummary, HidError> {
    if descriptor.is_empty() || descriptor.len() > MAX_DESCRIPTOR {
        return Err(HidError::Size(descriptor.len()));
    }
    let mut summary = HidSummary::default();
    let mut globals = Globals::default();
    let mut stack: Vec<Globals> = Vec::new();
    // Último uso local (con su página si venía en 32 bits) para la colección que lo sigue.
    let mut usage: Option<(Option<u16>, u16)> = None;
    let mut depth = 0usize;
    // Bits de cada informe por tipo e identificador.
    let mut bits: BTreeMap<(Kind, u8), u64> = BTreeMap::new();
    let mut at = 0usize;
    while at < descriptor.len() {
        let prefix = descriptor[at];
        at += 1;
        if prefix == 0xFE {
            // Elemento largo: tamaño y etiqueta, y se salta.
            let (Some(&size), Some(_tag)) = (descriptor.get(at), descriptor.get(at + 1)) else {
                return Err(HidError::Truncated);
            };
            at += 2 + usize::from(size);
            if at > descriptor.len() {
                return Err(HidError::Truncated);
            }
            continue;
        }
        let size = match prefix & 0x03 {
            3 => 4,
            n => usize::from(n),
        };
        let Some(data) = descriptor.get(at..at + size) else {
            return Err(HidError::Truncated);
        };
        at += size;
        let value = data
            .iter()
            .rev()
            .fold(0u32, |value, byte| (value << 8) | u32::from(*byte));
        let kind = (prefix >> 2) & 0x03;
        let tag = prefix >> 4;
        match (kind, tag) {
            // Main.
            (0, 0x8) | (0, 0x9) | (0, 0xB) => {
                let report = match tag {
                    0x8 => Kind::Input,
                    0x9 => Kind::Output,
                    _ => Kind::Feature,
                };
                let add = u64::from(globals.report_size) * u64::from(globals.report_count);
                let total = bits.entry((report, globals.report_id)).or_default();
                *total = total.saturating_add(add);
                if *total > (MAX_REPORT_BYTES as u64) * 8 {
                    return Err(HidError::ReportTooLarge);
                }
                usage = None;
            }
            (0, 0xA) => {
                // Collection: la de aplicación (0x01) de primer nivel se apunta.
                if depth == 0 && value == 0x01 {
                    let (page, id) = usage.unwrap_or((None, 0));
                    summary.applications.push(Application {
                        page: page.unwrap_or(globals.page),
                        usage: id,
                    });
                }
                depth += 1;
                usage = None;
            }
            (0, 0xC) => {
                depth = depth.checked_sub(1).ok_or(HidError::Collection)?;
                usage = None;
            }
            // Global.
            (1, 0x0) => globals.page = value as u16,
            (1, 0x7) => globals.report_size = value,
            (1, 0x8) => globals.report_id = value as u8,
            (1, 0x9) => globals.report_count = value,
            (1, 0xA) => {
                if stack.len() >= MAX_PUSH {
                    return Err(HidError::Stack);
                }
                stack.push(globals);
            }
            (1, 0xB) => globals = stack.pop().ok_or(HidError::Stack)?,
            // Local: Usage (en 32 bits trae su página en los 16 altos).
            (2, 0x0) => {
                usage = Some(if size == 4 {
                    (Some((value >> 16) as u16), value as u16)
                } else {
                    (None, value as u16)
                });
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err(HidError::Collection);
    }
    summary.report_ids = bits.keys().any(|(_, id)| *id != 0);
    let bytes = |bits: u64, id: u8| bits.div_ceil(8) as usize + usize::from(id != 0);
    for ((report, id), total) in &bits {
        let len = bytes(*total, *id);
        match report {
            Kind::Input => summary.max_input = summary.max_input.max(len),
            Kind::Output => summary.max_output = summary.max_output.max(len),
            Kind::Feature => summary.features.push((*id, len)),
        }
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El del mando virtual de Windows (`drivers/windows-vhid/report_descriptor.inc`): un mando
    /// con 16 botones, 6 ejes y cruceta, sin identificadores.
    const GAMEPAD: &[u8] = &[
        0x05, 0x01, 0x09, 0x05, 0xA1, 0x01, // Generic Desktop / Game Pad / Application
        0x05, 0x09, 0x19, 0x01, 0x29, 0x10, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x10, 0x81,
        0x02, // 16 botones
        0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x33, 0x09, 0x34, 0x16, 0x00, 0x80, 0x26, 0xFF,
        0x7F, 0x75, 0x10, 0x95, 0x04, 0x81, 0x02, // 4 ejes de 16 bits
        0x09, 0x32, 0x09, 0x35, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x02, 0x81,
        0x02, // 2 gatillos de 8 bits
        0x09, 0x39, 0x15, 0x00, 0x25, 0x07, 0x75, 0x04, 0x95, 0x01, 0x81, 0x42, 0x75, 0x04, 0x95,
        0x01, 0x81, 0x03, // cruceta y relleno
        0xC0,
    ];

    /// Teclado de arranque (HID 1.11, apéndice B.1), recortado.
    const KEYBOARD: &[u8] = &[
        0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25,
        0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x06, 0x75, 0x08, 0x26, 0xFF, 0x00, 0x05,
        0x07, 0x19, 0x00, 0x29, 0xFF, 0x81, 0x00, 0xC0,
    ];

    #[test]
    fn a_gamepad_is_a_controller_with_its_input_size() {
        let summary = parse(GAMEPAD).expect("descriptor");
        assert_eq!(
            summary.applications,
            vec![Application { page: 1, usage: 5 }]
        );
        assert!(summary.has_controller());
        assert!(!summary.has_keyboard_or_mouse());
        // 2 + 8 + 2 + 1 bytes.
        assert_eq!(summary.max_input, 13);
        assert!(summary.features.is_empty());
        assert!(!summary.report_ids);
    }

    #[test]
    fn a_keyboard_is_flagged() {
        let summary = parse(KEYBOARD).expect("descriptor");
        assert!(summary.has_keyboard_or_mouse());
        assert!(!summary.has_controller());
    }

    #[test]
    fn feature_reports_are_listed_with_their_id() {
        // Mando con informe de entrada 0x01 y dos de característica (0x05 de 41 bytes, 0x20 de 64),
        // como los de calibración y firmware de un DualSense.
        let mut descriptor = vec![0x05, 0x01, 0x09, 0x05, 0xA1, 0x01];
        descriptor.extend([0x85, 0x01, 0x75, 0x08, 0x95, 0x09, 0x81, 0x02]);
        descriptor.extend([0x06, 0x00, 0xFF, 0x09, 0x21]); // página de fabricante
        descriptor.extend([0x85, 0x05, 0x95, 0x28, 0xB1, 0x02]);
        descriptor.extend([0x85, 0x20, 0x95, 0x3F, 0xB1, 0x02]);
        descriptor.extend([0x85, 0x02, 0x95, 0x2F, 0x91, 0x02]);
        descriptor.push(0xC0);
        let summary = parse(&descriptor).expect("descriptor");
        assert!(summary.report_ids);
        assert_eq!(summary.features, vec![(0x05, 41), (0x20, 64)]);
        assert_eq!(summary.max_input, 10);
        assert_eq!(summary.max_output, 48);
    }

    #[test]
    fn a_32_bit_usage_carries_its_page() {
        // Usage 0x000C0001 (Consumer Control) con la página global en Generic Desktop.
        let descriptor = [
            0x05, 0x01, 0x0B, 0x01, 0x00, 0x0C, 0x00, 0xA1, 0x01, 0x75, 0x08, 0x95, 0x01, 0x81,
            0x00, 0xC0,
        ];
        let summary = parse(&descriptor).expect("descriptor");
        assert_eq!(
            summary.applications,
            vec![Application {
                page: 0x0C,
                usage: 0x01
            }]
        );
        assert!(summary.has_keyboard_or_mouse());
    }

    #[test]
    fn broken_descriptors_are_rejected() {
        assert_eq!(parse(&[]), Err(HidError::Size(0)));
        assert_eq!(
            parse(&vec![0; MAX_DESCRIPTOR + 1]),
            Err(HidError::Size(MAX_DESCRIPTOR + 1))
        );
        assert_eq!(parse(&[0x05]), Err(HidError::Truncated));
        assert_eq!(parse(&[0xA1, 0x01]), Err(HidError::Collection));
        assert_eq!(parse(&[0xC0]), Err(HidError::Collection));
        assert_eq!(parse(&[0xB4]), Err(HidError::Stack));
        assert_eq!(parse(&[0xA4; MAX_PUSH + 1]), Err(HidError::Stack));
        // 4096 bits = 512 bytes de informe: el máximo justo; uno más, fuera.
        assert!(parse(&[0x75, 0x08, 0x96, 0x00, 0x02, 0x81, 0x02]).is_ok());
        assert_eq!(
            parse(&[0x75, 0x08, 0x96, 0x01, 0x02, 0x81, 0x02]),
            Err(HidError::ReportTooLarge)
        );
        // Un elemento largo que se sale.
        assert_eq!(parse(&[0xFE, 0x05, 0x00, 0x01]), Err(HidError::Truncated));
    }
}
