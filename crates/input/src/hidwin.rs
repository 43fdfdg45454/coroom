//! El mando físico del invitado en crudo, en Windows, para mandarlo tal cual (la misma forma que
//! [`crate::hidraw`] en Linux).
//!
//! - [`find`] busca con `hidapi` la colección HID del mando que lee `gilrs` por su VID/PID cuyo
//!   uso es de mando (Game Pad, Joystick o Multi-axis) y cuyo descriptor da el propio mando
//!   ([`HidOrigin`]). Windows no da el descriptor de informes en modo usuario: `hidapi` lo
//!   reconstruye de los datos *preparsed*, equivalente al original para quien lo lee (los mismos
//!   informes, campos y tamaños).
//! - [`HidOrigin`]: quién publica la colección, según la clase de instalación de su dispositivo
//!   padre en el árbol de dispositivos. Si el padre es de la clase HID, la colección la da un
//!   minidriver de transporte (USB, Bluetooth…) con el descriptor del mando: es el mando y va tal
//!   cual. Si es de otra clase, la armó otro driver: la copia para DirectInput que `xusb22` o
//!   `xboxgip` añaden a un mando XInput (gatillos en un eje, sin Guía ni salidas), que no es el
//!   mando y nunca va tal cual.
//! - [`HidwinDevice`] abre la colección (compartida: Steam o el juego pueden tenerla abierta, y
//!   cada uno recibe su copia de los informes), lee los informes de característica que declara el
//!   descriptor, cada informe de entrada tal cual y escribe los de salida que manda el anfitrión.
//! - [`is_xinput`]: un mando XInput sin colección propia va con sus eventos de siempre y su
//!   identidad, como Xbox 360.

use std::ffi::{CStr, CString};

use coroom_proto::hid::{self, HidSummary};
use hidapi::{BusType, DeviceInfo, HidApi, HidDevice};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_PropertyW, CM_Get_Device_Interface_PropertyW, CM_Get_Parent,
    CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW, CONFIGRET, CR_SUCCESS, GUID_DEVCLASS_HIDCLASS,
};
use windows::Win32::Devices::Properties::{
    DEVPKEY_Device_ClassGuid, DEVPKEY_Device_InstanceId, DEVPKEY_Device_Service, DEVPROPTYPE,
};
use windows::Win32::Foundation::DEVPROPKEY;
use windows::core::{GUID, PCWSTR};

use crate::InputError;
use crate::gamepad::is_xinput_path;

/// Por qué un mando con interfaz HID no se pudo abrir en crudo.
pub const HIDWIN_HELP: &str = "no se puede abrir el mando en crudo y va convertido; si Steam \
     está abierto en este equipo, ciérralo (puede cambiar el modo del mando) y vuelve a \
     conectarlo";

/// Página de uso Generic Desktop y los usos de mando.
const GENERIC_DESKTOP: u16 = 0x01;
const CONTROLLER_USAGES: [u16; 3] = [0x04, 0x05, 0x08];

/// Quién publica una colección HID: su dispositivo padre en el árbol de dispositivos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidOrigin {
    /// Instancia del padre.
    pub parent: String,
    /// Su clase de instalación.
    pub class: GUID,
    /// Su driver (servicio), si Windows lo dice.
    pub service: Option<String>,
}

impl HidOrigin {
    /// Lo que dice Windows de la colección de la ruta `path` (`None` si no la encuentra).
    #[allow(unsafe_code)] // CfgMgr32 sin envoltorio seguro; cada bloque lleva su `SAFETY:`.
    pub fn of(path: &CStr) -> Option<HidOrigin> {
        let interface: Vec<u16> = path
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let instance = property(|kind, buffer, size| {
            // SAFETY: `interface` acaba en 0 y vive toda la llamada; `kind` y `size` apuntan a
            // variables de `property`, y `buffer` es nulo o tiene `*size` bytes.
            unsafe {
                CM_Get_Device_Interface_PropertyW(
                    PCWSTR(interface.as_ptr()),
                    &DEVPKEY_Device_InstanceId,
                    kind,
                    buffer,
                    size,
                    0,
                )
            }
        })?;
        let mut node = 0u32;
        // SAFETY: `instance` es la cadena UTF-16 acabada en 0 que dio Windows.
        let found = unsafe {
            CM_Locate_DevNodeW(
                &mut node,
                PCWSTR(instance.as_ptr()),
                CM_LOCATE_DEVNODE_NORMAL,
            )
        };
        if found != CR_SUCCESS {
            return None;
        }
        let mut parent = 0u32;
        // SAFETY: `node` es un nodo que acaba de dar Windows y `parent` una variable local.
        if unsafe { CM_Get_Parent(&mut parent, node, 0) } != CR_SUCCESS {
            return None;
        }
        let class = node_property(parent, &DEVPKEY_Device_ClassGuid)?;
        let class = <[u8; 16]>::try_from(units_to_bytes(&class).get(..16)?).ok()?;
        Some(HidOrigin {
            parent: node_property(parent, &DEVPKEY_Device_InstanceId)
                .map(|id| text(&id))
                .unwrap_or_default(),
            class: GUID::from_values(
                u32::from_le_bytes([class[0], class[1], class[2], class[3]]),
                u16::from_le_bytes([class[4], class[5]]),
                u16::from_le_bytes([class[6], class[7]]),
                [
                    class[8], class[9], class[10], class[11], class[12], class[13], class[14],
                    class[15],
                ],
            ),
            service: node_property(parent, &DEVPKEY_Device_Service).map(|name| text(&name)),
        })
    }

    /// Si el descriptor lo da el propio mando: el padre es de la clase HID (un minidriver de
    /// transporte). Si no, la colección la armó otro driver y no es el mando.
    pub fn from_device(&self) -> bool {
        self.class == GUID_DEVCLASS_HIDCLASS
    }
}

/// Una propiedad de un nodo del árbol de dispositivos.
#[allow(unsafe_code)] // CfgMgr32 sin envoltorio seguro; el bloque lleva su `SAFETY:`.
fn node_property(node: u32, key: &DEVPROPKEY) -> Option<Vec<u16>> {
    property(|kind, buffer, size| {
        // SAFETY: `node` lo dio Windows, `key` es una constante; `kind` y `size` apuntan a
        // variables de `property`, y `buffer` es nulo o tiene `*size` bytes.
        unsafe { CM_Get_DevNode_PropertyW(node, key, kind, buffer, size, 0) }
    })
}

/// Pide una propiedad dos veces: su tamaño y luego su valor (en unidades de 16 bits, alineadas
/// para leerla como cadena).
fn property(
    get: impl Fn(*mut DEVPROPTYPE, Option<*mut u8>, *mut u32) -> CONFIGRET,
) -> Option<Vec<u16>> {
    let mut kind = DEVPROPTYPE(0);
    let mut size = 0u32;
    let _ = get(&mut kind, None, &mut size);
    if size == 0 {
        return None;
    }
    let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
    let mut capacity = u32::try_from(buffer.len() * 2).ok()?;
    if get(&mut kind, Some(buffer.as_mut_ptr().cast()), &mut capacity) != CR_SUCCESS {
        return None;
    }
    Some(buffer)
}

/// Los bytes de una propiedad leída en unidades de 16 bits.
fn units_to_bytes(units: &[u16]) -> Vec<u8> {
    units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
}

/// Una cadena UTF-16 de Windows (hasta el primer 0).
fn text(units: &[u16]) -> String {
    let end = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

/// Si `info` es una colección de mando (por su uso).
fn is_controller(info: &DeviceInfo) -> bool {
    info.usage_page() == GENERIC_DESKTOP && CONTROLLER_USAGES.contains(&info.usage())
}

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

/// La colección HID de mando de `vendor:product` (la primera que se pueda leer) cuyo descriptor
/// da el propio mando ([`HidOrigin::from_device`]).
pub fn find(vendor: u16, product: u16) -> Option<HidwinInfo> {
    let api = HidApi::new().ok()?;
    api.device_list()
        .filter(|info| info.vendor_id() == vendor && info.product_id() == product)
        .filter(|info| is_controller(info))
        .filter(|info| HidOrigin::of(info.path()).is_some_and(|origin| origin.from_device()))
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

/// Si XInput ve el mando `vendor:product`: Windows marca alguna de sus colecciones como de un
/// mando XInput ([`is_xinput_path`]).
pub fn is_xinput(vendor: u16, product: u16) -> bool {
    HidApi::new().is_ok_and(|api| {
        api.device_list().any(|info| {
            info.vendor_id() == vendor
                && info.product_id() == product
                && is_xinput_path(info.path().to_bytes())
        })
    })
}

/// Para el diagnóstico: cada colección HID de mando conectada, quién la publica y cómo iría.
pub fn report() -> Vec<String> {
    let Ok(api) = HidApi::new() else {
        return vec!["no se pudieron listar los dispositivos HID".to_owned()];
    };
    let lines: Vec<String> = api
        .device_list()
        .filter(|info| is_controller(info))
        .map(|info| {
            let name = info.product_string().unwrap_or_default();
            let id = format!("{:04x}:{:04x}", info.vendor_id(), info.product_id());
            let xinput = if is_xinput_path(info.path().to_bytes()) {
                ", marcada para XInput"
            } else {
                ""
            };
            match HidOrigin::of(info.path()) {
                Some(origin) => format!(
                    "«{name}» {id}{xinput}: padre {} (clase {:?}, driver {}): {}",
                    origin.parent,
                    origin.class,
                    origin.service.as_deref().unwrap_or("?"),
                    if origin.from_device() {
                        "la da el mando, va tal cual"
                    } else if xinput.is_empty() {
                        "la armó otro driver, va convertido"
                    } else {
                        "la armó otro driver, va como Xbox 360 (XInput)"
                    }
                ),
                None => format!("«{name}» {id}{xinput}: Windows no dice de quién es"),
            }
        })
        .collect();
    if lines.is_empty() {
        vec!["ningún mando HID conectado".to_owned()]
    } else {
        lines
    }
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
