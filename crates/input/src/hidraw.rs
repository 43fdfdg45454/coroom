//! El mando físico del invitado en crudo, en Linux (`/dev/hidrawN`), para mandarlo tal cual.
//!
//! - [`find`] busca en sysfs (`/sys/class/hidraw`) el nodo del mando que lee `gilrs` por su
//!   VID/PID, y se queda con la interfaz cuyo descriptor declara un mando (un mando puede tener
//!   varias). De ahí salen el nombre, el bus, la versión y el descriptor, que se leen sin permisos.
//! - [`HidrawDevice`] abre el nodo (eso sí necesita permiso: las reglas udev de Steam lo dan para
//!   muchos mandos, y si no, [`HIDRAW_HELP`]), lee los informes de característica que declara el
//!   descriptor (`HIDIOCGFEATURE`) y después cada informe de entrada tal cual; escribe los de
//!   salida que manda el anfitrión (vibración, luces).
//! - [`is_xpad`]: un mando de Xbox por cable no tiene `hidraw` (lo lleva `xpad`); va con sus
//!   eventos de siempre y su identidad.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use coroom_proto::hid::{self, HidSummary};

use crate::InputError;

/// Directorio de los nodos `hidraw` en sysfs.
pub const SYS_HIDRAW: &str = "/sys/class/hidraw";

/// Directorio de los dispositivos de entrada en sysfs.
pub const SYS_INPUT: &str = "/sys/class/input";

/// Cómo dar acceso de lectura al mando en crudo.
pub const HIDRAW_HELP: &str = "no se puede leer el mando en crudo (/dev/hidraw*) y va convertido; \
     instala las reglas udev de Steam (paquete steam-devices) o añade una para tu mando, por \
     ejemplo `KERNEL==\"hidraw*\", ATTRS{idVendor}==\"2dc8\", TAG+=\"uaccess\"`, y vuelve a \
     conectarlo";

/// Un nodo `hidraw` con lo que dice sysfs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidrawInfo {
    /// Nodo (`/dev/hidrawN`).
    pub node: PathBuf,
    /// Nombre (`HID_NAME`).
    pub name: String,
    /// Bus (`BUS_USB` = 3, `BUS_BLUETOOTH` = 5).
    pub bus: u16,
    /// Fabricante.
    pub vendor: u16,
    /// Producto.
    pub product: u16,
    /// Versión (la del dispositivo de entrada que cuelga de él, si lo hay).
    pub version: u16,
    /// Descriptor de informes.
    pub descriptor: Vec<u8>,
    /// Lo que declara el descriptor.
    pub summary: HidSummary,
}

/// `HID_ID=0003:00002DC8:00006012` → bus, VID, PID.
fn parse_hid_id(value: &str) -> Option<(u16, u16, u16)> {
    let mut parts = value.trim().split(':');
    let bus = u32::from_str_radix(parts.next()?, 16).ok()?;
    let vendor = u32::from_str_radix(parts.next()?, 16).ok()?;
    let product = u32::from_str_radix(parts.next()?, 16).ok()?;
    Some((bus as u16, vendor as u16, product as u16))
}

fn uevent(dir: &Path) -> Option<(u16, u16, u16, String)> {
    let text = std::fs::read_to_string(dir.join("device/uevent")).ok()?;
    let (mut id, mut name) = (None, String::new());
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("HID_ID=") {
            id = parse_hid_id(value);
        } else if let Some(value) = line.strip_prefix("HID_NAME=") {
            name = value.to_owned();
        }
    }
    let (bus, vendor, product) = id?;
    Some((bus, vendor, product, name))
}

/// Versión del dispositivo de entrada que cuelga del HID (`device/input/inputN/id/version`).
fn version(dir: &Path) -> u16 {
    std::fs::read_dir(dir.join("device/input"))
        .ok()
        .and_then(|entries| {
            entries.flatten().find_map(|entry| {
                let text = std::fs::read_to_string(entry.path().join("id/version")).ok()?;
                u16::from_str_radix(text.trim(), 16).ok()
            })
        })
        .unwrap_or(0)
}

/// El nodo `hidraw` del mando `vendor:product` cuyo descriptor declara un mando, sin los de
/// `used` (otro mando igual ya en uso). `sys` es [`SYS_HIDRAW`] y `dev`, `/dev` (otros en los
/// tests).
pub fn find(
    sys: &Path,
    dev: &Path,
    vendor: u16,
    product: u16,
    used: &[PathBuf],
) -> Option<HidrawInfo> {
    let mut entries: Vec<_> = std::fs::read_dir(sys).ok()?.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    entries.into_iter().find_map(|entry| {
        let dir = entry.path();
        let node = dev.join(entry.file_name());
        if used.contains(&node) {
            return None;
        }
        let (bus, v, p, name) = uevent(&dir)?;
        if (v, p) != (vendor, product) {
            return None;
        }
        let descriptor = std::fs::read(dir.join("device/report_descriptor")).ok()?;
        let summary = hid::parse(&descriptor).ok()?;
        if !summary.has_controller() {
            return None;
        }
        Some(HidrawInfo {
            node,
            name,
            bus,
            vendor,
            product,
            version: version(&dir),
            descriptor,
            summary,
        })
    })
}

/// Si el mando `vendor:product` lo lleva `xpad` (un Xbox por cable, sin `hidraw`). `sys` es
/// [`SYS_INPUT`].
pub fn is_xpad(sys: &Path, vendor: u16, product: u16) -> bool {
    let Ok(entries) = std::fs::read_dir(sys) else {
        return false;
    };
    let hex = |path: PathBuf| {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| u16::from_str_radix(text.trim(), 16).ok())
    };
    entries.flatten().any(|entry| {
        let dir = entry.path();
        hex(dir.join("id/vendor")) == Some(vendor)
            && hex(dir.join("id/product")) == Some(product)
            && std::fs::read_link(dir.join("device/driver"))
                .ok()
                .and_then(|driver| driver.file_name().map(|name| name == "xpad"))
                .unwrap_or(false)
    })
}

/// `HIDIOCGFEATURE(512)`: `_IOC(_IOC_READ | _IOC_WRITE, 'H', 0x07, 512)`.
const FEATURE_LEN: usize = hid::MAX_REPORT_BYTES;
const HIDIOCGFEATURE: rustix::ioctl::Opcode =
    rustix::ioctl::opcode::read_write::<[u8; FEATURE_LEN]>(b'H', 0x07);

/// El mando abierto en crudo.
#[derive(Debug)]
pub struct HidrawDevice {
    file: File,
}

impl HidrawDevice {
    /// Abre el nodo para leer los informes y escribir los de salida.
    pub fn open(node: &Path) -> Result<HidrawDevice, InputError> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(node)
            .map(|file| HidrawDevice { file })
            .map_err(|err| match err.kind() {
                ErrorKind::PermissionDenied | ErrorKind::NotFound => InputError::PermissionDenied,
                _ => InputError::Backend(format!("{}: {err}", node.display())),
            })
    }

    /// Lee los informes de característica de `summary` (los que fallen se omiten).
    #[allow(unsafe_code)] // `HIDIOCGFEATURE` sin envoltorio seguro; cada bloque lleva su `SAFETY:`.
    pub fn read_features(&self, summary: &HidSummary) -> Vec<(u8, Vec<u8>)> {
        summary
            .features
            .iter()
            .filter_map(|(id, len)| {
                let mut buffer = [0u8; FEATURE_LEN];
                buffer[0] = *id;
                let ioctl =
                    // SAFETY: el opcode es `HIDIOCGFEATURE` con el tamaño de `buffer`, que vive
                    // mientras dura la llamada; el kernel escribe como mucho esos bytes en él.
                    unsafe { rustix::ioctl::Updater::<HIDIOCGFEATURE, _>::new(&mut buffer) };
                // SAFETY: `ioctl` describe un `HIDIOCGFEATURE` válido sobre un nodo `hidraw`.
                unsafe { rustix::ioctl::ioctl(&self.file, ioctl) }.ok()?;
                Some((*id, buffer[..(*len).min(FEATURE_LEN)].to_vec()))
            })
            .collect()
    }

    /// Otro manejador del mismo nodo (para leer en un hilo y escribir en otro).
    pub fn try_clone(&self) -> Result<HidrawDevice, InputError> {
        self.file
            .try_clone()
            .map(|file| HidrawDevice { file })
            .map_err(|err| InputError::Backend(format!("hidraw: {err}")))
    }

    /// Espera un informe de entrada hasta `timeout_ms`: `Ok(None)` si no llegó ninguno.
    pub fn read_report(
        &mut self,
        buffer: &mut [u8],
        timeout_ms: u32,
    ) -> Result<Option<usize>, InputError> {
        let mut fds = [rustix::event::PollFd::new(
            &self.file,
            rustix::event::PollFlags::IN,
        )];
        let timeout = rustix::time::Timespec {
            tv_sec: i64::from(timeout_ms / 1000),
            tv_nsec: i64::from(timeout_ms % 1000) * 1_000_000,
        };
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) | Err(rustix::io::Errno::INTR) => return Ok(None),
            Ok(_) => {}
            Err(err) => return Err(InputError::Backend(format!("hidraw: {err}"))),
        }
        match self.file.read(buffer) {
            Ok(0) => Err(InputError::Backend("el mando se desconectó".to_owned())),
            Ok(read) => Ok(Some(read)),
            Err(err) if err.kind() == ErrorKind::WouldBlock => Ok(None),
            Err(err) => Err(InputError::Backend(format!("hidraw: {err}"))),
        }
    }

    /// Escribe un informe de salida (con su identificador si el descriptor los usa).
    pub fn write_output(&mut self, data: &[u8]) -> Result<(), InputError> {
        self.file
            .write_all(data)
            .map_err(|err| InputError::Backend(format!("hidraw: {err}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Descriptor mínimo de un mando: Generic Desktop / Game Pad con un byte de botones.
    const PAD: &[u8] = &[
        0x05, 0x01, 0x09, 0x05, 0xA1, 0x01, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02, 0xC0,
    ];
    /// Y uno de ratón (otra interfaz del mismo receptor).
    const MOUSE: &[u8] = &[
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x75, 0x08, 0x95, 0x03, 0x81, 0x06, 0xC0,
    ];

    fn node(sys: &Path, name: &str, id: &str, descriptor: &[u8], version: Option<&str>) {
        let device = sys.join(name).join("device");
        std::fs::create_dir_all(&device).unwrap();
        std::fs::write(
            device.join("uevent"),
            format!("DRIVER=hid-generic\nHID_ID={id}\nHID_NAME=8BitDo Ultimate 2\n"),
        )
        .unwrap();
        std::fs::write(device.join("report_descriptor"), descriptor).unwrap();
        if let Some(version) = version {
            let input = device.join("input/input7/id");
            std::fs::create_dir_all(&input).unwrap();
            std::fs::write(input.join("version"), version).unwrap();
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("coroom-hidraw-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn hid_ids_are_parsed() {
        assert_eq!(
            parse_hid_id("0003:00002DC8:00006012"),
            Some((3, 0x2dc8, 0x6012))
        );
        assert_eq!(
            parse_hid_id("0005:0000054C:00000CE6\n"),
            Some((5, 0x054c, 0x0ce6))
        );
        assert_eq!(parse_hid_id("basura"), None);
    }

    #[test]
    fn the_controller_interface_of_the_right_pad_is_found() {
        let sys = temp("find");
        // Otro mando, y el receptor del nuestro con una interfaz de ratón delante.
        node(&sys, "hidraw0", "0003:0000045E:00000B12", PAD, None);
        node(&sys, "hidraw1", "0003:00002DC8:00006012", MOUSE, None);
        node(
            &sys,
            "hidraw2",
            "0003:00002DC8:00006012",
            PAD,
            Some("0111\n"),
        );
        let found = find(&sys, Path::new("/dev"), 0x2dc8, 0x6012, &[]).expect("mando");
        assert_eq!(found.node, PathBuf::from("/dev/hidraw2"));
        assert_eq!(found.name, "8BitDo Ultimate 2");
        assert_eq!((found.bus, found.version), (3, 0x0111));
        assert_eq!(found.descriptor, PAD);
        assert!(found.summary.has_controller());
        // Ya en uso (otro mando igual en otra ranura): no hay más.
        assert_eq!(
            find(
                &sys,
                Path::new("/dev"),
                0x2dc8,
                0x6012,
                &[PathBuf::from("/dev/hidraw2")]
            ),
            None
        );
        assert_eq!(find(&sys, Path::new("/dev"), 0x1234, 0x5678, &[]), None);
        let _ = std::fs::remove_dir_all(&sys);
    }

    #[test]
    fn wired_xbox_pads_are_told_by_their_driver() {
        let sys = temp("xpad");
        let input = sys.join("input3");
        std::fs::create_dir_all(input.join("id")).unwrap();
        std::fs::create_dir_all(input.join("device")).unwrap();
        std::fs::write(input.join("id/vendor"), "045e\n").unwrap();
        std::fs::write(input.join("id/product"), "028e\n").unwrap();
        let driver = sys.join("drivers/xpad");
        std::fs::create_dir_all(&driver).unwrap();
        std::os::unix::fs::symlink(&driver, input.join("device/driver")).unwrap();
        assert!(is_xpad(&sys, 0x045e, 0x028e));
        assert!(!is_xpad(&sys, 0x2dc8, 0x6012));
        let _ = std::fs::remove_dir_all(&sys);
    }

    #[test]
    fn the_feature_opcode_matches_the_kernel_macro() {
        // _IOC(3, 'H', 7, 512) = (3 << 30) | (512 << 16) | ('H' << 8) | 7.
        assert_eq!(
            u64::from(HIDIOCGFEATURE),
            (3u64 << 30) | (512 << 16) | (u64::from(b'H') << 8) | 7
        );
    }
}
