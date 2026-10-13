//! El mando físico del invitado en crudo, en Windows, para mandarlo tal cual (la misma forma que
//! [`crate::hidraw`] en Linux).
//!
//! - [`find`] busca con `hidapi` la colección HID del mando que lee `gilrs` por su VID/PID cuyo
//!   uso es de mando (Game Pad, Joystick o Multi-axis). Windows no da el descriptor de informes
//!   en modo usuario: `hidapi` lo reconstruye de los datos *preparsed*, equivalente al original
//!   para quien lo lee (los mismos informes, campos y tamaños).
//! - [`HidwinDevice`] abre la colección (compartida: Steam o el juego pueden tenerla abierta, y
//!   cada uno recibe su copia de los informes), lee los informes de característica que declara el
//!   descriptor, cada informe de entrada tal cual y escribe los de salida que manda el anfitrión.
//! - [`is_xinput`]: un mando de Xbox por cable (`xusb22`) no tiene interfaz HID; va con sus
//!   eventos de siempre y su identidad.

use std::ffi::CString;

use coroom_proto::hid::{self, HidSummary};
use hidapi::{BusType, HidApi, HidDevice};

use crate::InputError;

/// Por qué un mando con interfaz HID no se pudo abrir en crudo.
pub const HIDWIN_HELP: &str = "no se puede abrir el mando en crudo y va convertido; si Steam \
     está abierto en este equipo, ciérralo (puede cambiar el modo del mando) y vuelve a \
     conectarlo";

/// Página de uso Generic Desktop y los usos de mando.
const GENERIC_DESKTOP: u16 = 0x01;
const CONTROLLER_USAGES: [u16; 3] = [0x04, 0x05, 0x08];

/// Microsoft: sus mandos por cable van por `xusb22`, sin HID.
const MICROSOFT: u16 = 0x045E;

/// Una colección HID de mando con lo que dice Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidwinInfo {
    /// Ruta de la colección para abrirla.
    pub path: CString,
    /// Nombre (cadena de producto).
    pub name: String,
    /// Bus (`BUS_USB` = 3, `BUS_BLUETOOTH` = 5, como en Linux).
    pub bus: u16,
    /// Fabricante.
    pub vendor: u16,
    /// Producto.
    pub product: u16,
    /// Versión.
    pub version: u16,
    /// Descriptor de informes (reconstruido).
    pub descriptor: Vec<u8>,
    /// Lo que declara el descriptor.
    pub summary: HidSummary,
}

fn backend(err: impl std::fmt::Display) -> InputError {
    InputError::Backend(format!("hid: {err}"))
}

/// La colección HID de mando de `vendor:product` (la primera que se pueda leer).
pub fn find(vendor: u16, product: u16) -> Option<HidwinInfo> {
    let api = HidApi::new().ok()?;
    api.device_list()
        .filter(|info| info.vendor_id() == vendor && info.product_id() == product)
        .filter(|info| {
            info.usage_page() == GENERIC_DESKTOP && CONTROLLER_USAGES.contains(&info.usage())
        })
        .find_map(|info| {
            let device = info.open_device(&api).ok()?;
            let mut buffer = vec![0u8; hid::MAX_DESCRIPTOR];
            let len = device.get_report_descriptor(&mut buffer).ok()?;
            buffer.truncate(len);
            let summary = hid::parse(&buffer).ok()?;
            if !summary.has_controller() {
                return None;
            }
            Some(HidwinInfo {
                path: info.path().to_owned(),
                name: info.product_string().map(str::to_owned).unwrap_or_default(),
                bus: match info.bus_type() {
                    BusType::Usb => 3,
                    BusType::Bluetooth => 5,
                    _ => 0,
                },
                vendor,
                product,
                version: info.release_number(),
                descriptor: buffer,
                summary,
            })
        })
}

/// Si el mando `vendor:product` no tiene interfaz HID pero es de Microsoft: un Xbox por cable
/// (XInput).
pub fn is_xinput(vendor: u16, product: u16) -> bool {
    vendor == MICROSOFT && find(vendor, product).is_none()
}

/// El mando abierto en crudo.
pub struct HidwinDevice {
    device: HidDevice,
    path: CString,
}

impl std::fmt::Debug for HidwinDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HidwinDevice")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl HidwinDevice {
    /// Abre la colección para leer los informes y escribir los de salida.
    pub fn open(info: &HidwinInfo) -> Result<HidwinDevice, InputError> {
        let api = HidApi::new().map_err(backend)?;
        let device = api
            .open_path(&info.path)
            .map_err(|_| InputError::PermissionDenied)?;
        Ok(HidwinDevice {
            device,
            path: info.path.clone(),
        })
    }

    /// Lee los informes de característica de `summary` (los que fallen se omiten).
    pub fn read_features(&self, summary: &HidSummary) -> Vec<(u8, Vec<u8>)> {
        summary
            .features
            .iter()
            .filter_map(|(id, len)| {
                let mut buffer = vec![0u8; (*len).clamp(1, hid::MAX_REPORT_BYTES)];
                buffer[0] = *id;
                let read = self.device.get_feature_report(&mut buffer).ok()?;
                buffer.truncate(read.min(buffer.len()));
                (!buffer.is_empty()).then_some((*id, buffer))
            })
            .collect()
    }

    /// Otro manejador de la misma colección (para leer en un hilo y escribir en otro).
    pub fn try_clone(&self) -> Result<HidwinDevice, InputError> {
        let api = HidApi::new().map_err(backend)?;
        let device = api.open_path(&self.path).map_err(backend)?;
        Ok(HidwinDevice {
            device,
            path: self.path.clone(),
        })
    }

    /// Espera un informe de entrada hasta `timeout_ms`: `Ok(None)` si no llegó ninguno.
    pub fn read_report(
        &mut self,
        buffer: &mut [u8],
        timeout_ms: u32,
    ) -> Result<Option<usize>, InputError> {
        let timeout = i32::try_from(timeout_ms).unwrap_or(i32::MAX);
        match self.device.read_timeout(buffer, timeout) {
            Ok(0) => Ok(None),
            Ok(read) => Ok(Some(read)),
            Err(err) => Err(backend(err)),
        }
    }

    /// Escribe un informe de salida (el identificador delante, 0 si el descriptor no los usa).
    pub fn write_output(&mut self, data: &[u8]) -> Result<(), InputError> {
        self.device.write(data).map(|_| ()).map_err(backend)
    }
}
