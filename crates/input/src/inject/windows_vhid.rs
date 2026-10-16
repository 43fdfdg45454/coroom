//! Mandos virtuales en Windows (feature `windows-vhid`): cliente del driver `coroomvhid.dll`
//! (`drivers/windows-vhid/`, UMDF2, protocolo 3).
//!
//! Windows no tiene API de usuario para crear un mando. El driver tiene dos clases de dispositivo,
//! y los dos los crea `install.ps1` una vez:
//!
//! - **Control** (`Root\coroomvhid`): una fuente de la Virtual HID Framework. Por él se crean
//!   mandos HID al momento, con el VID/PID, la versión y el descriptor que se quiera: el del mando
//!   del invitado **tal cual** ([`VhidGamepads::connect_raw`]) o el genérico de
//!   `report_descriptor.inc` ([`PadReport`]). Los informes de salida que manda el sistema
//!   (vibración, luces) esperan en el driver hasta que [`VhidGamepads::take_outputs`] los recoge.
//! - **Xbox 360** ([`XUSB_SLOTS`], `Root\coroomxusb`): mandos de XInput. Son los que reciben los
//!   mandos **convertidos** ([`XusbState`]) mientras haya libres (los ven todos los juegos, también
//!   los que solo usan XInput) y los de Xbox por cable tal cual, con su VID/PID.
//!
//! - **Formato** (independiente de la plataforma y probado en Linux): las estructuras y los IOCTL
//!   de `coroom_vhid.h` ([`create_request`], [`input_request`], [`parse_output`],
//!   [`XusbState::to_bytes`]) y los informes del mando genérico. Los tests leen la cabecera, el
//!   descriptor, el INF y los scripts del driver y comprueban que este módulo dice lo mismo.
//! - **Ranuras**: las del cable van de 1 a 8; los mandos HID del driver, de 0 a 7 (la ranura
//!   menos uno), y los Xbox 360, de 0 a 3 (el primero libre).
//! - **Ritmo** (mandos convertidos): un informe en cada cambio, con tope de 500 Hz
//!   ([`REPORT_INTERVAL`]) para los ejes; los botones y la cruceta salen siempre al momento. Un
//!   hilo (`coroom-vhid`) manda lo que esperaba su turno y repite el estado cada
//!   [`KEEPALIVE_INTERVAL`]: si el driver pasa [`WATCHDOG`] sin noticias, deja el mando en
//!   reposo. Los informes tal cual pasan sin tocar (el invitado repite el último cada 100 ms).
//! - [`VhidGamepads`] funciona sobre cualquier [`VhidDevice`]; en Windows, `DriverHandle` busca los
//!   dispositivos por su interfaz (`CM_Get_Device_Interface_List`), comprueba la versión
//!   (`COROOM_VHID_IOCTL_GET_INFO`) y les habla con `DeviceIoControl`. El de control es exclusivo:
//!   al cerrar su handle (o morir coroom) el driver quita sus mandos.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use coroom_proto::hid::{self, HidSummary};
use coroom_proto::input::{GamepadDevice, GamepadTransport};

use crate::{GamepadAxis, GamepadButton, GuestId, InputError, InputEvent};

/// Versión del protocolo del driver que habla este cliente (`COROOM_VHID_PROTOCOL_VERSION`).
pub const PROTOCOL_VERSION: u32 = 3;

/// Mandos HID que puede tener el driver a la vez (`COROOM_VHID_MAX_PADS`), uno por ranura.
pub const DRIVER_PADS: u8 = 8;

/// Mandos de Xbox 360 (`COROOM_VHID_XUSB_SLOTS`; XInput tiene cuatro).
pub const XUSB_SLOTS: u8 = 4;

/// Identificador de hardware del dispositivo de control (`COROOM_VHID_HARDWARE_ID`).
pub const HARDWARE_ID: &str = r"Root\coroomvhid";

/// Identificador de hardware de los Xbox 360 (`COROOM_VHID_XUSB_HARDWARE_ID`).
pub const XUSB_HARDWARE_ID: &str = r"Root\coroomxusb";

/// Interfaz privada de los dos (`COROOM_VHID_INTERFACE_GUID`).
pub const INTERFACE_GUID: u128 = 0x7C6B2C3E_55D1_4C7A_9C4B_2E0B6F1D8A41;

/// Límites de un mando (`COROOM_VHID_MAX_*`), los mismos que los del protocolo de coroom.
pub const MAX_REPORT: usize = 512;

/// Informes de característica de un mando, como mucho (`COROOM_VHID_MAX_FEATURES`).
pub const MAX_FEATURES: usize = 32;

/// Bytes de informes de característica, como mucho (`COROOM_VHID_MAX_FEATURE_BYTES`).
pub const MAX_FEATURE_BYTES: usize = 8 * 1024;

/// IOCTL del driver (`COROOM_VHID_IOCTL_*`).
pub mod ioctl {
    /// Versión, clase y ranura del dispositivo.
    pub const GET_INFO: u32 = 0x0022_E400;
    /// Crea (o sustituye) un mando HID.
    pub const CREATE: u32 = 0x0022_E404;
    /// Quita un mando HID.
    pub const DESTROY: u32 = 0x0022_E408;
    /// Informe de entrada de un mando HID.
    pub const INPUT: u32 = 0x0022_E40C;
    /// La salida más antigua que espera.
    pub const TAKE_OUTPUT: u32 = 0x0022_E410;
    /// Activa un Xbox 360 con un VID/PID.
    pub const XUSB_ACTIVATE: u32 = 0x0022_E440;
    /// Lo desactiva (los juegos dejan de verlo).
    pub const XUSB_DEACTIVATE: u32 = 0x0022_E444;
    /// Su estado (`XINPUT_GAMEPAD`).
    pub const XUSB_STATE: u32 = 0x0022_E448;
}

/// `COROOM_VHID_INFO.Kind` del dispositivo de control.
pub const KIND_CONTROL: u8 = 0;

/// `COROOM_VHID_INFO.Kind` de un Xbox 360.
pub const KIND_XUSB: u8 = 1;

/// `COROOM_VHID_OUTPUT.Type` de un informe de salida.
pub const OUTPUT_REPORT: u8 = 1;

/// `COROOM_VHID_OUTPUT.Type` de una escritura de característica.
pub const OUTPUT_FEATURE: u8 = 2;

/// Bytes de las estructuras de `coroom_vhid.h` (`COROOM_VHID_*_SIZE`).
pub mod size {
    /// `COROOM_VHID_INFO`.
    pub const INFO: usize = 8;
    /// `COROOM_VHID_CREATE`.
    pub const CREATE: usize = 16;
    /// `COROOM_VHID_INPUT`.
    pub const INPUT: usize = 4;
    /// `COROOM_VHID_OUTPUT`.
    pub const OUTPUT_HEADER: usize = 8;
    /// `COROOM_VHID_XUSB_ID`.
    pub const XUSB_ID: usize = 4;
    /// `COROOM_VHID_XUSB_STATE`.
    pub const XUSB_STATE: usize = 12;
}

/// El Xbox 360 de los mandos convertidos (`COROOM_VHID_XBOX360_*`).
pub const XBOX360_VENDOR: u16 = 0x045E;

/// Producto del Xbox 360 por cable.
pub const XBOX360_PRODUCT: u16 = 0x028E;

/// Identificador del informe de entrada del mando genérico.
pub const REPORT_ID: u8 = 1;

/// Bytes del informe del mando genérico, con el identificador.
pub const REPORT_LEN: usize = 16;

/// Valor máximo de un stick (y de un gatillo) en el informe genérico.
pub const AXIS_MAX: i16 = 32767;

/// Hat en reposo.
pub const HAT_NULL: u8 = 8;

/// El descriptor del mando genérico (`report_descriptor.inc`; los tests comprueban que coinciden).
pub const GENERIC_DESCRIPTOR: [u8; 85] = [
    0x05, 0x01, 0x09, 0x05, 0xA1, 0x01, 0x85, 0x01, 0x05, 0x09, 0x19, 0x01, 0x29, 0x10, 0x15, 0x00,
    0x25, 0x01, 0x75, 0x01, 0x95, 0x10, 0x81, 0x02, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x33,
    0x09, 0x34, 0x16, 0x01, 0x80, 0x26, 0xFF, 0x7F, 0x75, 0x10, 0x95, 0x04, 0x81, 0x02, 0x09, 0x32,
    0x09, 0x35, 0x15, 0x00, 0x26, 0xFF, 0x7F, 0x95, 0x02, 0x81, 0x02, 0x09, 0x39, 0x15, 0x00, 0x25,
    0x07, 0x35, 0x00, 0x46, 0x3B, 0x01, 0x65, 0x14, 0x75, 0x04, 0x95, 0x01, 0x81, 0x42, 0x65, 0x00,
    0x45, 0x00, 0x81, 0x03, 0xC0,
];

/// Tope de ritmo de los informes de ejes por mando: 500 Hz.
pub const REPORT_INTERVAL: Duration = Duration::from_millis(2);

/// Sin informes durante este tiempo, el driver deja el mando en reposo
/// (`COROOM_VHID_WATCHDOG_MS`).
pub const WATCHDOG: Duration = Duration::from_millis(1000);

/// Cada cuánto se repite el último estado de un mando sin cambios, para que el driver sepa que
/// coroom sigue ahí (bastante menos que [`WATCHDOG`]).
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(250);

/// Posición de cada campo en el informe genérico; los ejes son `i16` little endian.
pub mod offset {
    /// 16 botones, un bit cada uno (botón HID `n` = bit `n - 1`), `u16` little endian.
    pub const BUTTONS: usize = 1;
    /// Stick izquierdo, horizontal.
    pub const X: usize = 3;
    /// Stick izquierdo, vertical (positivo hacia abajo, como en HID).
    pub const Y: usize = 5;
    /// Stick derecho, horizontal.
    pub const RX: usize = 7;
    /// Stick derecho, vertical (positivo hacia abajo).
    pub const RY: usize = 9;
    /// Gatillo izquierdo.
    pub const Z: usize = 11;
    /// Gatillo derecho.
    pub const RZ: usize = 13;
    /// Hat (bits 0 a 3) y relleno.
    pub const HAT: usize = 15;
}

/// Qué hacer si el driver no está.
pub const DRIVER_HELP: &str = "instala el driver de mando virtual de coroom (guía en \
    drivers/windows-vhid/README.md); mientras, `coroom host --gamepad-to-keys` convierte los \
    mandos en teclas";

/// Mando HID del driver (0 a 7) de una ranura del cable (1 a 8).
pub fn driver_pad(slot: u8) -> Result<u8, InputError> {
    if (1..=DRIVER_PADS).contains(&slot) {
        Ok(slot - 1)
    } else {
        Err(InputError::NoSuchSlot(slot))
    }
}

/// Bit del botón en el campo de botones del informe genérico (botón HID `bit + 1`); la cruceta
/// va al hat.
pub const fn button_bit(button: GamepadButton) -> Option<u8> {
    Some(match button {
        GamepadButton::A => 0,
        GamepadButton::B => 1,
        GamepadButton::X => 2,
        GamepadButton::Y => 3,
        GamepadButton::LeftBumper => 4,
        GamepadButton::RightBumper => 5,
        GamepadButton::Back => 6,
        GamepadButton::Start => 7,
        GamepadButton::LeftStick => 8,
        GamepadButton::RightStick => 9,
        GamepadButton::Guide => 10,
        GamepadButton::DPadUp
        | GamepadButton::DPadDown
        | GamepadButton::DPadLeft
        | GamepadButton::DPadRight => return None,
    })
}

/// Bit de `XINPUT_GAMEPAD.wButtons` de cada botón (`XINPUT_GAMEPAD_*`; Guide es el 0x0400 que
/// leen `XInputGetStateEx` y Windows.Gaming.Input).
pub const fn xinput_bit(button: GamepadButton) -> u16 {
    match button {
        GamepadButton::DPadUp => 0x0001,
        GamepadButton::DPadDown => 0x0002,
        GamepadButton::DPadLeft => 0x0004,
        GamepadButton::DPadRight => 0x0008,
        GamepadButton::Start => 0x0010,
        GamepadButton::Back => 0x0020,
        GamepadButton::LeftStick => 0x0040,
        GamepadButton::RightStick => 0x0080,
        GamepadButton::LeftBumper => 0x0100,
        GamepadButton::RightBumper => 0x0200,
        GamepadButton::Guide => 0x0400,
        GamepadButton::A => 0x1000,
        GamepadButton::B => 0x2000,
        GamepadButton::X => 0x4000,
        GamepadButton::Y => 0x8000,
    }
}

/// Estado de un mando con los valores del cable (sticks con positivo = derecha y arriba,
/// gatillos en `0..=i16::MAX`); [`PadReport::to_bytes`] lo convierte al informe genérico y
/// [`PadReport::to_xusb`] al de un Xbox 360.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PadReport {
    buttons: u16,
    xinput: u16,
    axes: [i16; 6],
    /// Arriba, abajo, izquierda, derecha.
    dpad: [bool; 4],
}

fn axis_index(axis: GamepadAxis) -> usize {
    match axis {
        GamepadAxis::LeftX => 0,
        GamepadAxis::LeftY => 1,
        GamepadAxis::RightX => 2,
        GamepadAxis::RightY => 3,
        GamepadAxis::LeftTrigger => 4,
        GamepadAxis::RightTrigger => 5,
    }
}

impl PadReport {
    /// Mando en reposo.
    pub fn neutral() -> Self {
        Self::default()
    }

    /// Aplica un evento de mando (los demás se ignoran).
    pub fn apply(&mut self, event: &InputEvent) {
        match *event {
            InputEvent::GamepadButton {
                button, pressed, ..
            } => self.set_button(button, pressed),
            InputEvent::GamepadAxis { axis, value, .. } => self.set_axis(axis, value),
            _ => {}
        }
    }

    /// Pulsa o suelta un botón.
    pub fn set_button(&mut self, button: GamepadButton, pressed: bool) {
        let bit = xinput_bit(button);
        if pressed {
            self.xinput |= bit;
        } else {
            self.xinput &= !bit;
        }
        let dpad = match button {
            GamepadButton::DPadUp => Some(0),
            GamepadButton::DPadDown => Some(1),
            GamepadButton::DPadLeft => Some(2),
            GamepadButton::DPadRight => Some(3),
            _ => None,
        };
        if let Some(index) = dpad {
            self.dpad[index] = pressed;
        } else if let Some(bit) = button_bit(button) {
            if pressed {
                self.buttons |= 1 << bit;
            } else {
                self.buttons &= !(1 << bit);
            }
        }
    }

    /// Fija un eje con el valor del cable.
    pub fn set_axis(&mut self, axis: GamepadAxis, value: i16) {
        self.axes[axis_index(axis)] = value;
    }

    /// Valor del hat: 0 arriba y en el sentido de las agujas del reloj de 45 en 45 grados;
    /// [`HAT_NULL`] en reposo. Direcciones opuestas a la vez se anulan.
    pub fn hat(&self) -> u8 {
        let [up, down, left, right] = self.dpad.map(i8::from);
        match (up - down, right - left) {
            (1, 0) => 0,
            (1, 1) => 1,
            (0, 1) => 2,
            (-1, 1) => 3,
            (-1, 0) => 4,
            (-1, -1) => 5,
            (0, -1) => 6,
            (1, -1) => 7,
            _ => HAT_NULL,
        }
    }

    /// Informe de entrada del mando genérico (`report_descriptor.inc`).
    pub fn to_bytes(&self) -> [u8; REPORT_LEN] {
        let stick = |value: i16| value.clamp(-AXIS_MAX, AXIS_MAX);
        // HID: Y positivo hacia abajo; el cable: positivo hacia arriba.
        let flipped = |value: i16| -stick(value);
        let trigger = |value: i16| value.max(0);
        let [lx, ly, rx, ry, lt, rt] = self.axes;
        let mut out = [0u8; REPORT_LEN];
        out[0] = REPORT_ID;
        out[offset::BUTTONS..offset::BUTTONS + 2].copy_from_slice(&self.buttons.to_le_bytes());
        for (at, value) in [
            (offset::X, stick(lx)),
            (offset::Y, flipped(ly)),
            (offset::RX, stick(rx)),
            (offset::RY, flipped(ry)),
            (offset::Z, trigger(lt)),
            (offset::RZ, trigger(rt)),
        ] {
            out[at..at + 2].copy_from_slice(&value.to_le_bytes());
        }
        out[offset::HAT] = self.hat();
        out
    }

    /// Estado de un Xbox 360 (`XINPUT_GAMEPAD`): la cruceta son botones, los gatillos van de 0 a
    /// 255 y los sticks son los del cable (positivo arriba, como en XInput).
    pub fn to_xusb(&self) -> XusbState {
        let [lx, ly, rx, ry, lt, rt] = self.axes;
        let trigger = |value: i16| (i32::from(value.max(0)) * 255 / i32::from(i16::MAX)) as u8;
        XusbState {
            buttons: self.xinput,
            left_trigger: trigger(lt),
            right_trigger: trigger(rt),
            thumb_lx: lx,
            thumb_ly: ly,
            thumb_rx: rx,
            thumb_ry: ry,
        }
    }
}

/// `COROOM_VHID_XUSB_STATE` (`XINPUT_GAMEPAD`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct XusbState {
    /// `XINPUT_GAMEPAD_*`.
    pub buttons: u16,
    /// Gatillo izquierdo, 0 a 255.
    pub left_trigger: u8,
    /// Gatillo derecho.
    pub right_trigger: u8,
    /// Stick izquierdo.
    pub thumb_lx: i16,
    /// Positivo arriba.
    pub thumb_ly: i16,
    /// Stick derecho.
    pub thumb_rx: i16,
    /// Positivo arriba.
    pub thumb_ry: i16,
}

impl XusbState {
    /// Los 12 bytes de la estructura, little endian.
    pub fn to_bytes(&self) -> [u8; size::XUSB_STATE] {
        let mut out = [0u8; size::XUSB_STATE];
        out[0..2].copy_from_slice(&self.buttons.to_le_bytes());
        out[2] = self.left_trigger;
        out[3] = self.right_trigger;
        out[4..6].copy_from_slice(&self.thumb_lx.to_le_bytes());
        out[6..8].copy_from_slice(&self.thumb_ly.to_le_bytes());
        out[8..10].copy_from_slice(&self.thumb_rx.to_le_bytes());
        out[10..12].copy_from_slice(&self.thumb_ry.to_le_bytes());
        out
    }
}

/// Un mando HID que se pide al driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PadSpec {
    /// Fabricante.
    pub vendor: u16,
    /// Producto.
    pub product: u16,
    /// Versión.
    pub version: u16,
    /// Descriptor de informes.
    pub descriptor: Vec<u8>,
    /// Informe de reposo (vacío: sin perro guardián).
    pub neutral: Vec<u8>,
    /// Informes de característica que el driver contesta (empiezan por su identificador).
    pub features: Vec<Vec<u8>>,
}

impl PadSpec {
    /// El mando genérico.
    pub fn generic() -> Self {
        Self {
            vendor: 0,
            product: 0,
            version: 0x0100,
            descriptor: GENERIC_DESCRIPTOR.to_vec(),
            neutral: PadReport::neutral().to_bytes().to_vec(),
            features: Vec::new(),
        }
    }
}

fn too_big(what: &str) -> InputError {
    InputError::Backend(format!("mando virtual: {what} no cabe en el driver"))
}

/// Datos de `COROOM_VHID_IOCTL_CREATE` para el mando `pad`.
pub fn create_request(pad: u8, spec: &PadSpec) -> Result<Vec<u8>, InputError> {
    if pad >= DRIVER_PADS {
        return Err(InputError::NoSuchSlot(pad + 1));
    }
    if spec.descriptor.is_empty() || spec.descriptor.len() > hid::MAX_DESCRIPTOR {
        return Err(too_big("el descriptor"));
    }
    if spec.neutral.len() > MAX_REPORT {
        return Err(too_big("el informe de reposo"));
    }
    if spec.features.len() > MAX_FEATURES
        || spec
            .features
            .iter()
            .any(|feature| feature.is_empty() || feature.len() > MAX_REPORT)
    {
        return Err(too_big("algún informe de característica"));
    }
    let mut features = Vec::new();
    for feature in &spec.features {
        features.extend_from_slice(&(feature.len() as u16).to_le_bytes());
        features.extend_from_slice(feature);
    }
    if features.len() > MAX_FEATURE_BYTES + 2 * MAX_FEATURES {
        return Err(too_big("los informes de característica"));
    }
    let mut out = Vec::with_capacity(
        size::CREATE + spec.descriptor.len() + spec.neutral.len() + features.len(),
    );
    out.extend_from_slice(&[pad, 0]);
    out.extend_from_slice(&spec.vendor.to_le_bytes());
    out.extend_from_slice(&spec.product.to_le_bytes());
    out.extend_from_slice(&spec.version.to_le_bytes());
    out.extend_from_slice(&(spec.descriptor.len() as u16).to_le_bytes());
    out.extend_from_slice(&(spec.neutral.len() as u16).to_le_bytes());
    out.extend_from_slice(&(features.len() as u16).to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&spec.descriptor);
    out.extend_from_slice(&spec.neutral);
    out.extend_from_slice(&features);
    Ok(out)
}

/// Datos de `COROOM_VHID_IOCTL_INPUT`.
pub fn input_request(pad: u8, report: &[u8]) -> Result<Vec<u8>, InputError> {
    if pad >= DRIVER_PADS {
        return Err(InputError::NoSuchSlot(pad + 1));
    }
    if report.is_empty() || report.len() > MAX_REPORT {
        return Err(too_big("el informe"));
    }
    let mut out = Vec::with_capacity(size::INPUT + report.len());
    out.extend_from_slice(&[pad, 0]);
    out.extend_from_slice(&(report.len() as u16).to_le_bytes());
    out.extend_from_slice(report);
    Ok(out)
}

/// Una salida del driver (`COROOM_VHID_OUTPUT` y sus datos).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverOutput {
    /// Mando HID (0 a 7).
    pub pad: u8,
    /// [`OUTPUT_REPORT`] u [`OUTPUT_FEATURE`].
    pub kind: u8,
    /// `HID_XFER_PACKET.reportId`.
    pub report_id: u8,
    /// Los bytes, como los pasó Windows.
    pub data: Vec<u8>,
}

/// Lee la respuesta de `COROOM_VHID_IOCTL_TAKE_OUTPUT` (vacía: no había nada).
pub fn parse_output(bytes: &[u8]) -> Result<Option<DriverOutput>, InputError> {
    if bytes.is_empty() {
        return Ok(None);
    }
    let bad = || InputError::Backend("mando virtual: el driver devolvió una salida rota".into());
    if bytes.len() < size::OUTPUT_HEADER {
        return Err(bad());
    }
    let len = usize::from(u16::from_le_bytes([bytes[4], bytes[5]]));
    if len > MAX_REPORT || bytes.len() != size::OUTPUT_HEADER + len || bytes[0] >= DRIVER_PADS {
        return Err(bad());
    }
    Ok(Some(DriverOutput {
        pad: bytes[0],
        kind: bytes[1],
        report_id: bytes[2],
        data: bytes[size::OUTPUT_HEADER..].to_vec(),
    }))
}

/// Un informe de salida en el formato del protocolo de coroom (el de `hidraw` y `hidapi` al
/// escribir): el identificador delante, 0 si el descriptor no los numera. Windows lo pasa con él o
/// sin él según el caso.
pub fn normalize_output(summary: &HidSummary, report_id: u8, data: &[u8]) -> Vec<u8> {
    let has_prefix = if summary.report_ids {
        report_id != 0 && data.first() == Some(&report_id)
    } else {
        data.len() == summary.max_output + 1 && data.first() == Some(&0)
    };
    if has_prefix {
        data.to_vec()
    } else {
        let id = if summary.report_ids { report_id } else { 0 };
        let mut out = Vec::with_capacity(data.len() + 1);
        out.push(id);
        out.extend_from_slice(data);
        out
    }
}

/// Lo que responde `COROOM_VHID_IOCTL_GET_INFO`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverInfo {
    /// Versión del protocolo del driver.
    pub protocol: u32,
    /// [`KIND_CONTROL`] o [`KIND_XUSB`].
    pub kind: u8,
    /// Ranura de un Xbox 360 (0 a 3).
    pub slot: u8,
    /// Si está en uso.
    pub active: u8,
}

impl DriverInfo {
    /// Lee `COROOM_VHID_INFO` (little endian, sin relleno).
    pub fn parse(bytes: &[u8]) -> Result<Self, InputError> {
        if bytes.len() != size::INFO {
            return Err(InputError::GamepadUnavailable(format!(
                "el driver coroomvhid respondió a la consulta de versión con {} bytes: instala el \
                 driver que acompaña a esta versión de coroom (drivers/windows-vhid/README.md)",
                bytes.len()
            )));
        }
        Ok(Self {
            protocol: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            kind: bytes[4],
            slot: bytes[5],
            active: bytes[6],
        })
    }

    /// Comprueba que este cliente puede hablar con ese driver.
    pub fn check(&self) -> Result<(), InputError> {
        if self.protocol != PROTOCOL_VERSION {
            return Err(InputError::GamepadUnavailable(format!(
                "el driver coroomvhid instalado habla la versión {} del protocolo y esta versión \
                 de coroom necesita la {PROTOCOL_VERSION}: instala el driver que acompaña a esta \
                 versión (drivers/windows-vhid/README.md)",
                self.protocol
            )));
        }
        let valid = match self.kind {
            KIND_CONTROL => true,
            KIND_XUSB => self.slot < XUSB_SLOTS,
            _ => false,
        };
        if !valid {
            return Err(InputError::GamepadUnavailable(format!(
                "el driver coroomvhid dice ser de la clase {} con la ranura {}: reinstala el \
                 driver",
                self.kind, self.slot
            )));
        }
        Ok(())
    }
}

/// Operación con el driver, para los mensajes de error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverOp {
    /// Abrir el dispositivo de control.
    Open,
    /// Consultar la versión.
    Info,
    /// Crear un mando.
    Create,
    /// Mandar un informe o un estado.
    Report,
    /// Quitar un mando.
    Destroy,
    /// Recoger las salidas.
    Output,
}

/// Error en español de una operación fallida, a partir del código de Win32 en que Windows
/// convierte el `NTSTATUS` del driver (`slot`: ranura del cable).
pub fn driver_error(
    op: DriverOp,
    slot: Option<u8>,
    win32: Option<u32>,
    detail: &str,
) -> InputError {
    let what = match (op, slot) {
        (DriverOp::Open, _) => "abrir el driver".to_owned(),
        (DriverOp::Info, _) => "consultar la versión del driver".to_owned(),
        (DriverOp::Create, Some(slot)) => format!("crear el mando de la ranura {slot}"),
        (DriverOp::Report, Some(slot)) => format!("mandar el estado del mando de la ranura {slot}"),
        (DriverOp::Destroy, Some(slot)) => format!("quitar el mando de la ranura {slot}"),
        (DriverOp::Output, _) => "recoger la vibración de los mandos".to_owned(),
        (_, None) => "hablar con el driver".to_owned(),
    };
    let why = match win32 {
        // ERROR_ACCESS_DENIED: el dispositivo de control es exclusivo, o el SDDL del INF.
        Some(5) => {
            "lo está usando otro programa (¿otro coroom abierto como anfitrión?) o Windows no te \
             deja: solo pueden SYSTEM, los administradores y el usuario con la sesión iniciada"
        }
        // ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_DEVICE_NOT_CONNECTED,
        // ERROR_DEVICE_REMOVED
        Some(2 | 3 | 1167 | 1617) => {
            "el dispositivo ya no está (¿se ha desactivado o desinstalado el driver?)"
        }
        // ERROR_BUSY (STATUS_DEVICE_BUSY): otro programa activó ese Xbox 360.
        Some(170) => "ese mando de Xbox 360 lo usa otro programa",
        // STATUS_INVALID_PARAMETER
        Some(87) => "el driver rechazó los datos (fuera de rango)",
        // STATUS_INVALID_BUFFER_SIZE, ERROR_INVALID_USER_BUFFER
        Some(1784) => "tamaño de datos inesperado: el driver y coroom no son de la misma versión",
        // STATUS_NOT_IMPLEMENTED, STATUS_INVALID_DEVICE_REQUEST
        Some(1 | 120) => {
            "el driver no conoce esa orden: el driver y coroom no son de la misma versión"
        }
        // ERROR_NOT_SUPPORTED (STATUS_NOT_SUPPORTED): VHF no pudo con ese descriptor.
        Some(50) => "Windows no aceptó el descriptor del mando",
        _ => "",
    };
    let why = if why.is_empty() {
        detail.to_owned()
    } else {
        format!("{why} ({detail})")
    };
    InputError::Backend(format!("mando virtual: no se pudo {what}: {why}"))
}

/// El driver no está: no hay dispositivo de control de coroom.
pub fn not_installed_error() -> InputError {
    InputError::GamepadUnavailable(format!(
        "el driver coroomvhid no está instalado o está desactivado: {DRIVER_HELP}"
    ))
}

/// Lo que [`VhidGamepads`] necesita del driver (mandos HID de 0 a 7 y Xbox 360 de 0 a 3). En
/// Windows es `DriverHandle`; los tests usan uno falso.
pub trait VhidDevice: Send + 'static {
    /// Crea (o sustituye) el mando HID `pad`.
    fn create(&mut self, pad: u8, spec: &PadSpec) -> Result<(), InputError>;
    /// Lo quita.
    fn destroy(&mut self, pad: u8) -> Result<(), InputError>;
    /// Le manda un informe de entrada.
    fn input(&mut self, pad: u8, report: &[u8]) -> Result<(), InputError>;
    /// La salida más antigua que espera, si hay.
    fn take_output(&mut self) -> Result<Option<DriverOutput>, InputError>;
    /// Los Xbox 360 instalados (0 a 3).
    fn xusb_slots(&self) -> Vec<u8>;
    /// Activa un Xbox 360 con esa identidad (los juegos empiezan a verlo).
    fn xusb_activate(&mut self, slot: u8, vendor: u16, product: u16) -> Result<(), InputError>;
    /// Lo desactiva.
    fn xusb_deactivate(&mut self, slot: u8) -> Result<(), InputError>;
    /// Su estado.
    fn xusb_state(&mut self, slot: u8, state: &XusbState) -> Result<(), InputError>;
}

/// Qué dispositivo del driver lleva una ranura.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Un Xbox 360 (0 a 3).
    Xusb(u8),
    /// Un mando HID (0 a 7).
    Hid(u8),
}

/// Lo que se manda por un dispositivo: el informe genérico o el estado de un Xbox 360.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// Informe del mando genérico.
    Hid([u8; REPORT_LEN]),
    /// Estado de un Xbox 360.
    Xusb([u8; size::XUSB_STATE]),
}

impl Wire {
    /// Botones y cruceta: lo que no espera su turno.
    fn digital(&self) -> &[u8] {
        match self {
            Wire::Hid(bytes) => &bytes[offset::BUTTONS..offset::BUTTONS + 2],
            Wire::Xusb(bytes) => &bytes[0..2],
        }
    }

    fn hat(&self) -> Option<u8> {
        match self {
            Wire::Hid(bytes) => Some(bytes[offset::HAT]),
            Wire::Xusb(_) => None,
        }
    }
}

/// Cuándo mandar los informes de un mando convertido: en cada cambio, los ejes como mucho cada
/// [`REPORT_INTERVAL`] (lo que llega antes espera y se sustituye por lo más nuevo), los botones y
/// la cruceta al momento; sin cambios, el último otra vez cada [`KEEPALIVE_INTERVAL`].
#[derive(Debug, Clone, Default)]
pub struct ReportPacer {
    sent: Option<Wire>,
    sent_at: Option<Instant>,
    pending: Option<Wire>,
}

impl ReportPacer {
    /// Nuevo estado del mando a las `now`: devuelve el informe si hay que mandarlo ya (y hay que
    /// llamar a [`ReportPacer::sent`] si se mandó); si no, queda pendiente.
    pub fn offer(&mut self, report: Wire, now: Instant) -> Option<Wire> {
        if self.sent == Some(report) {
            self.pending = None;
            return None;
        }
        let urgent = self.sent.is_none_or(|sent| {
            sent.digital() != report.digital()
                || sent.hat() != report.hat()
                || std::mem::discriminant(&sent) != std::mem::discriminant(&report)
        });
        let due = self
            .sent_at
            .is_none_or(|at| now.saturating_duration_since(at) >= REPORT_INTERVAL);
        if urgent || due {
            self.pending = None;
            Some(report)
        } else {
            self.pending = Some(report);
            None
        }
    }

    /// Anota que `report` salió a las `now`.
    pub fn sent(&mut self, report: Wire, now: Instant) {
        self.sent = Some(report);
        self.sent_at = Some(now);
    }

    /// Cuándo toca mandar lo pendiente, si hay algo.
    pub fn deadline(&self) -> Option<Instant> {
        self.pending?;
        Some(
            self.sent_at
                .map_or_else(Instant::now, |at| at + REPORT_INTERVAL),
        )
    }

    /// Lo pendiente, si ya le toca a las `now`.
    pub fn take_due(&mut self, now: Instant) -> Option<Wire> {
        if self.deadline().is_some_and(|deadline| deadline <= now) {
            self.pending.take()
        } else {
            None
        }
    }

    /// Cuándo toca repetir el último informe (sin nada pendiente), con `keepalive` de intervalo.
    pub fn keepalive_deadline(&self, keepalive: Duration) -> Option<Instant> {
        if self.pending.is_some() {
            return None;
        }
        self.sent?;
        self.sent_at.map(|at| at + keepalive)
    }

    /// El último informe, si ya toca repetirlo a las `now` (y hay que llamar a
    /// [`ReportPacer::sent`] si se mandó).
    pub fn take_keepalive(&self, now: Instant, keepalive: Duration) -> Option<Wire> {
        if self
            .keepalive_deadline(keepalive)
            .is_some_and(|deadline| deadline <= now)
        {
            self.sent
        } else {
            None
        }
    }
}

/// El mando tal cual de una ranura: HID con su descriptor (los eventos convertidos se ignoran) o
/// un Xbox por cable con su identidad (por el camino de los convertidos).
#[derive(Debug, Clone)]
enum Raw {
    Hid(HidSummary),
    XInput,
}

#[derive(Debug)]
struct Pad {
    target: Target,
    report: PadReport,
    pacer: ReportPacer,
    owner: Option<GuestId>,
    raw: Option<Raw>,
}

impl Pad {
    fn new(target: Target) -> Self {
        Self {
            target,
            report: PadReport::neutral(),
            pacer: ReportPacer::default(),
            owner: None,
            raw: None,
        }
    }

    fn wire(&self) -> Wire {
        match self.target {
            Target::Xusb(_) => Wire::Xusb(self.report.to_xusb().to_bytes()),
            Target::Hid(_) => Wire::Hid(self.report.to_bytes()),
        }
    }

    /// Lo lleva el hilo de ritmo (los HID tal cual, no: van sin tocar).
    fn paced(&self) -> bool {
        !matches!(self.raw, Some(Raw::Hid(_)))
    }
}

struct State<D> {
    device: D,
    /// Por ranura del cable (1 a 8).
    pads: BTreeMap<u8, Pad>,
    /// Cada cuánto se repite el estado ([`KEEPALIVE_INTERVAL`]; los tests lo alargan).
    keepalive: Duration,
    stop: bool,
}

struct Shared<D> {
    state: Mutex<State<D>>,
    wake: Condvar,
}

fn lock<D>(shared: &Shared<D>) -> MutexGuard<'_, State<D>> {
    shared.state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Manda `wire` por el dispositivo de `target`.
fn send<D: VhidDevice>(device: &mut D, target: Target, wire: &Wire) -> Result<(), InputError> {
    match (target, wire) {
        (Target::Xusb(slot), Wire::Xusb(bytes)) => {
            let state = XusbState {
                buttons: u16::from_le_bytes([bytes[0], bytes[1]]),
                left_trigger: bytes[2],
                right_trigger: bytes[3],
                thumb_lx: i16::from_le_bytes([bytes[4], bytes[5]]),
                thumb_ly: i16::from_le_bytes([bytes[6], bytes[7]]),
                thumb_rx: i16::from_le_bytes([bytes[8], bytes[9]]),
                thumb_ry: i16::from_le_bytes([bytes[10], bytes[11]]),
            };
            device.xusb_state(slot, &state)
        }
        (Target::Hid(pad), Wire::Hid(bytes)) => device.input(pad, bytes),
        // El destino cambió entre medias: lo manda el siguiente informe.
        _ => Ok(()),
    }
}

impl<D: VhidDevice> State<D> {
    /// Manda el estado de `slot` si toca (o lo deja pendiente).
    fn push(&mut self, slot: u8, now: Instant) -> Result<(), InputError> {
        let State { device, pads, .. } = self;
        let Some(pad) = pads.get_mut(&slot) else {
            return Err(InputError::NoSuchSlot(slot));
        };
        if !pad.paced() {
            return Ok(());
        }
        if let Some(report) = pad.pacer.offer(pad.wire(), now) {
            send(device, pad.target, &report)?;
            pad.pacer.sent(report, now);
        }
        Ok(())
    }

    /// Manda lo pendiente que ya toca y repite los estados que llevan [`State::keepalive`] sin
    /// salir; devuelve cuándo vence lo siguiente.
    fn flush_due(&mut self, now: Instant) -> Option<Instant> {
        let State {
            device,
            pads,
            keepalive,
            ..
        } = self;
        for (slot, pad) in pads.iter_mut().filter(|(_, pad)| pad.paced()) {
            let due = pad
                .pacer
                .take_due(now)
                .or_else(|| pad.pacer.take_keepalive(now, *keepalive));
            if let Some(report) = due {
                match send(device, pad.target, &report) {
                    Ok(()) => pad.pacer.sent(report, now),
                    Err(err) => tracing::warn!("mando virtual de la ranura {slot}: {err}"),
                }
            }
        }
        pads.values()
            .filter(|pad| pad.paced())
            .filter_map(|pad| {
                pad.pacer
                    .deadline()
                    .or_else(|| pad.pacer.keepalive_deadline(*keepalive))
            })
            .min()
    }

    /// El primer Xbox 360 instalado que no lleva ninguna ranura.
    fn free_xusb(&self) -> Option<u8> {
        self.device.xusb_slots().into_iter().find(|slot| {
            !self
                .pads
                .values()
                .any(|pad| pad.target == Target::Xusb(*slot))
        })
    }

    /// Prepara un dispositivo para la ranura `slot`: un Xbox 360 libre con `identity` o, si no
    /// queda ninguno (y `identity` es la del convertido), el mando genérico.
    fn open_target(&mut self, slot: u8, identity: (u16, u16)) -> Result<Target, InputError> {
        if let Some(xusb) = self.free_xusb() {
            self.device.xusb_activate(xusb, identity.0, identity.1)?;
            return Ok(Target::Xusb(xusb));
        }
        if identity != (XBOX360_VENDOR, XBOX360_PRODUCT) {
            return Err(InputError::Unsupported(
                "no queda ningún mando de Xbox 360 libre en el anfitrión".to_owned(),
            ));
        }
        let pad = driver_pad(slot)?;
        self.device.create(pad, &PadSpec::generic())?;
        Ok(Target::Hid(pad))
    }

    fn close_target(&mut self, target: Target) -> Result<(), InputError> {
        match target {
            Target::Xusb(slot) => self.device.xusb_deactivate(slot),
            Target::Hid(pad) => self.device.destroy(pad),
        }
    }

    /// Da a `slot` (que ya no tiene dispositivo) uno convertido nuevo, en reposo.
    fn reopen_converted(&mut self, slot: u8, owner: Option<GuestId>) -> Result<(), InputError> {
        let target = self.open_target(slot, (XBOX360_VENDOR, XBOX360_PRODUCT))?;
        let mut pad = Pad::new(target);
        pad.owner = owner;
        self.pads.insert(slot, pad);
        self.push(slot, Instant::now())
    }
}

/// Mandos virtuales por el driver `coroomvhid`: el de cada ranura se crea al asignarla y se quita
/// al liberarla.
pub struct VhidGamepads<D: VhidDevice> {
    shared: Arc<Shared<D>>,
    flusher: Option<JoinHandle<()>>,
}

impl<D: VhidDevice> fmt::Debug for VhidGamepads<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VhidGamepads")
            .field("slots", &self.assigned_slots())
            .finish_non_exhaustive()
    }
}

impl<D: VhidDevice> VhidGamepads<D> {
    /// Mandos sobre `device`, con el hilo que manda los informes de ejes pendientes y repite el
    /// estado cada [`KEEPALIVE_INTERVAL`].
    pub fn with_device(device: D) -> Result<Self, InputError> {
        Self::with_keepalive(device, KEEPALIVE_INTERVAL)
    }

    fn with_keepalive(device: D, keepalive: Duration) -> Result<Self, InputError> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                device,
                pads: BTreeMap::new(),
                keepalive,
                stop: false,
            }),
            wake: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        let flusher = std::thread::Builder::new()
            .name("coroom-vhid".to_owned())
            .spawn(move || flush_loop(&worker))
            .map_err(|err| {
                InputError::Backend(format!(
                    "mando virtual: no se pudo crear el hilo de informes: {err}"
                ))
            })?;
        Ok(Self {
            shared,
            flusher: Some(flusher),
        })
    }

    /// Crea el mando convertido de `slot` (1 a 8) en reposo: un Xbox 360 si queda alguno libre
    /// y, si no, el genérico. Si ya existe, no hace nada.
    pub fn assign(&mut self, slot: u8) -> Result<(), InputError> {
        driver_pad(slot)?;
        let mut state = lock(&self.shared);
        if state.pads.contains_key(&slot) {
            return Ok(());
        }
        let target = state.open_target(slot, (XBOX360_VENDOR, XBOX360_PRODUCT))?;
        state.pads.insert(slot, Pad::new(target));
        // El primer informe deja el mando en un estado conocido (sticks al centro, nada pulsado).
        let result = state.push(slot, Instant::now());
        if result.is_err() {
            state.pads.remove(&slot);
            let _ = state.close_target(target);
        }
        // El hilo de informes empieza a repetir el estado de este mando.
        self.shared.wake.notify_one();
        result
    }

    /// Quita el mando de `slot`.
    pub fn release(&mut self, slot: u8) -> Result<(), InputError> {
        let mut state = lock(&self.shared);
        let Some(pad) = state.pads.remove(&slot) else {
            return Err(InputError::NoSuchSlot(slot));
        };
        state.close_target(pad.target)
    }

    /// Sustituye el mando de `slot` por uno idéntico al del invitado: HID con su descriptor, o
    /// un Xbox 360 con su VID/PID si es un mando de Xbox por cable. Si no se puede, el de la
    /// ranura sigue como estaba (convertido) y se devuelve por qué.
    pub fn connect_raw(&mut self, slot: u8, device: &GamepadDevice) -> Result<(), InputError> {
        let mut state = lock(&self.shared);
        let Some(current) = state.pads.get(&slot).map(|pad| pad.target) else {
            return Err(InputError::NoSuchSlot(slot));
        };
        if let GamepadTransport::Converted { reason } = &device.transport {
            return Err(InputError::Unsupported(reason.clone()));
        }
        let owner = state.pads.get(&slot).and_then(|pad| pad.owner);
        let summary = match &device.transport {
            GamepadTransport::Hid { descriptor, .. } => Some(
                hid::parse(descriptor).map_err(|err| InputError::Unsupported(err.to_string()))?,
            ),
            _ => None,
        };
        // El convertido se suelta primero: su Xbox 360 queda libre y, con el genérico, el HID
        // ocupa el mismo número.
        state.pads.remove(&slot);
        state.close_target(current)?;
        match &device.transport {
            GamepadTransport::Hid {
                descriptor,
                features,
            } => {
                let pad = driver_pad(slot)?;
                let spec = PadSpec {
                    vendor: device.vendor,
                    product: device.product,
                    version: device.version,
                    descriptor: descriptor.clone(),
                    neutral: Vec::new(),
                    features: features.iter().map(|(_, data)| data.clone()).collect(),
                };
                if let Err(err) = state.device.create(pad, &spec) {
                    state.reopen_converted(slot, owner)?;
                    return Err(err);
                }
                let mut raw = Pad::new(Target::Hid(pad));
                raw.owner = owner;
                raw.raw = summary.map(Raw::Hid);
                state.pads.insert(slot, raw);
                Ok(())
            }
            GamepadTransport::XInput => {
                let target = match state.open_target(slot, (device.vendor, device.product)) {
                    Ok(target) => target,
                    Err(err) => {
                        state.reopen_converted(slot, owner)?;
                        return Err(err);
                    }
                };
                let mut raw = Pad::new(target);
                raw.owner = owner;
                raw.raw = Some(Raw::XInput);
                state.pads.insert(slot, raw);
                let result = state.push(slot, Instant::now());
                self.shared.wake.notify_one();
                result
            }
            GamepadTransport::Converted { .. } => unreachable!("rechazado arriba"),
        }
    }

    /// Un informe tal cual del invitado para el mando HID de `slot`.
    pub fn raw_report(&mut self, slot: u8, data: &[u8]) -> Result<(), InputError> {
        let mut state = lock(&self.shared);
        let target = match state.pads.get(&slot) {
            Some(Pad {
                target,
                raw: Some(Raw::Hid(_)),
                ..
            }) => *target,
            _ => return Err(InputError::NoSuchSlot(slot)),
        };
        match target {
            Target::Hid(pad) => state.device.input(pad, data),
            Target::Xusb(_) => Ok(()),
        }
    }

    /// Vuelve al mando convertido en `slot` (si iba tal cual).
    pub fn disconnect_raw(&mut self, slot: u8) -> Result<(), InputError> {
        let mut state = lock(&self.shared);
        let Some(pad) = state.pads.get(&slot) else {
            return Err(InputError::NoSuchSlot(slot));
        };
        if pad.raw.is_none() {
            return Ok(());
        }
        let (target, owner) = (pad.target, pad.owner);
        state.pads.remove(&slot);
        state.close_target(target)?;
        let result = state.reopen_converted(slot, owner);
        self.shared.wake.notify_one();
        result
    }

    /// Los informes de salida (vibración, luces) que el sistema mandó a los mandos tal cual desde
    /// la última llamada, por ranura y en el formato del protocolo de coroom.
    pub fn take_outputs(&mut self) -> Vec<(u8, Vec<u8>)> {
        let mut state = lock(&self.shared);
        let mut out = Vec::new();
        // Una tanda acotada por llamada: el driver guarda como mucho 64.
        for _ in 0..64 {
            let output = match state.device.take_output() {
                Ok(Some(output)) => output,
                Ok(None) => break,
                Err(err) => {
                    tracing::debug!("mando virtual: {err}");
                    break;
                }
            };
            if output.kind != OUTPUT_REPORT {
                continue;
            }
            let found = state.pads.iter().find_map(|(slot, pad)| match &pad.raw {
                Some(Raw::Hid(summary)) if pad.target == Target::Hid(output.pad) => {
                    Some((*slot, summary.clone()))
                }
                _ => None,
            });
            if let Some((slot, summary)) = found {
                out.push((
                    slot,
                    normalize_output(&summary, output.report_id, &output.data),
                ));
            }
        }
        out
    }

    /// Aplica un evento de mando de `guest` (la sesión ya comprobó que la ranura es suya).
    pub fn inject(&mut self, event: &InputEvent, guest: GuestId) -> Result<(), InputError> {
        let slot = match *event {
            InputEvent::GamepadButton { slot, .. }
            | InputEvent::GamepadAxis { slot, .. }
            | InputEvent::GamepadConnected { slot, .. }
            | InputEvent::GamepadDisconnected { slot } => slot,
            _ => return Ok(()),
        };
        let mut state = lock(&self.shared);
        let Some(pad) = state.pads.get_mut(&slot) else {
            return Err(InputError::NoSuchSlot(slot));
        };
        pad.owner = Some(guest);
        match event {
            InputEvent::GamepadConnected { .. } => return Ok(()),
            InputEvent::GamepadDisconnected { .. } => pad.report = PadReport::neutral(),
            _ => pad.report.apply(event),
        }
        let result = state.push(slot, Instant::now());
        self.wake_if_pending(&state);
        result
    }

    /// Devuelve al reposo los mandos convertidos que usa `guest`.
    pub fn release_guest(&mut self, guest: GuestId) -> Result<(), InputError> {
        let mut state = lock(&self.shared);
        let slots: Vec<u8> = state
            .pads
            .iter_mut()
            .filter(|(_, pad)| pad.owner == Some(guest))
            .map(|(slot, pad)| {
                pad.report = PadReport::neutral();
                *slot
            })
            .collect();
        let now = Instant::now();
        let mut result = Ok(());
        for slot in slots {
            result = result.and(state.push(slot, now));
        }
        self.wake_if_pending(&state);
        result
    }

    /// Despierta al hilo de informes si queda algo pendiente (con un plazo que quizá no conoce).
    fn wake_if_pending(&self, state: &State<D>) {
        if state
            .pads
            .values()
            .any(|pad| pad.pacer.deadline().is_some())
        {
            self.shared.wake.notify_one();
        }
    }

    /// Ranuras con mando.
    pub fn assigned_slots(&self) -> Vec<u8> {
        lock(&self.shared).pads.keys().copied().collect()
    }

    /// Dispositivo de cada ranura.
    pub fn targets(&self) -> BTreeMap<u8, Target> {
        lock(&self.shared)
            .pads
            .iter()
            .map(|(slot, pad)| (*slot, pad.target))
            .collect()
    }
}

impl<D: VhidDevice> Drop for VhidGamepads<D> {
    fn drop(&mut self) {
        lock(&self.shared).stop = true;
        self.shared.wake.notify_one();
        if let Some(flusher) = self.flusher.take() {
            let _ = flusher.join();
        }
        // Al soltar el dispositivo se cierran sus handles: el driver quita los mandos HID y
        // desactiva los Xbox 360.
    }
}

fn flush_loop<D: VhidDevice>(shared: &Shared<D>) {
    let mut state = lock(shared);
    loop {
        if state.stop {
            return;
        }
        let next = state.flush_due(Instant::now());
        state = match next {
            None => shared
                .wake
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner),
            Some(deadline) => {
                let timeout = deadline.saturating_duration_since(Instant::now());
                shared
                    .wake
                    .wait_timeout(state, timeout)
                    .map_or_else(|err| err.into_inner().0, |(guard, _)| guard)
            }
        };
    }
}

#[cfg(target_os = "windows")]
pub use driver::{DriverHandle, open, status};

/// Acceso al driver real (solo Windows). El `unsafe` es el de las llamadas a cfgmgr32,
/// `CreateFileW` y `DeviceIoControl` (`// SAFETY:` en cada una).
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod driver {
    use std::collections::BTreeMap;

    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_List_SizeW,
        CM_Get_Device_Interface_ListW, CR_BUFFER_SMALL, CR_SUCCESS,
    };
    use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::core::{GUID, PCWSTR};

    use super::{
        DriverInfo, DriverOp, DriverOutput, INTERFACE_GUID, KIND_CONTROL, KIND_XUSB, MAX_REPORT,
        PROTOCOL_VERSION, PadSpec, VhidDevice, VhidGamepads, XusbState, create_request,
        driver_error, input_request, ioctl, not_installed_error, parse_output, size,
    };
    use crate::InputError;

    /// Código de Win32 de un error de `windows` (`HRESULT_FROM_WIN32`).
    fn win32_code(err: &windows::core::Error) -> Option<u32> {
        let code = err.code().0 as u32;
        (code & 0xFFFF_0000 == 0x8007_0000).then_some(code & 0xFFFF)
    }

    /// Handle de fichero que se cierra al soltarlo.
    #[derive(Debug)]
    struct OwnedHandle(HANDLE);

    // SAFETY: un HANDLE de fichero de Windows se puede usar desde cualquier hilo; aquí solo lo
    // usa quien tenga el `Mutex` de `VhidGamepads`, y se cierra una vez en `Drop`.
    unsafe impl Send for OwnedHandle {}

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: el handle lo abrió `open_path` y solo se cierra aquí, una vez.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    /// Abre un dispositivo por la ruta de su interfaz (`path` acaba en NUL).
    fn open_path(path: &[u16]) -> Result<OwnedHandle, windows::core::Error> {
        // SAFETY: `path` acaba en NUL y vive durante la llamada; los demás argumentos son valores
        // o `None` (sin atributos de seguridad ni plantilla). Síncrono (sin OVERLAPPED).
        unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
        .map(OwnedHandle)
    }

    /// `DeviceIoControl` síncrono: devuelve los bytes escritos en `output`.
    fn control(
        handle: &OwnedHandle,
        code: u32,
        input: &[u8],
        output: &mut [u8],
    ) -> Result<usize, windows::core::Error> {
        let mut returned = 0u32;
        // SAFETY: el handle está abierto y es síncrono; `input` y `output` son slices que viven
        // durante la llamada, con sus longitudes exactas (o `None` si están vacíos), y `returned`
        // es local.
        unsafe {
            DeviceIoControl(
                handle.0,
                code,
                (!input.is_empty()).then_some(input.as_ptr().cast()),
                input.len() as u32,
                (!output.is_empty()).then_some(output.as_mut_ptr().cast()),
                output.len() as u32,
                Some(&mut returned),
                None,
            )
        }?;
        Ok(returned as usize)
    }

    /// Rutas (con su NUL) de las interfaces del driver presentes.
    fn interfaces() -> Result<Vec<Vec<u16>>, InputError> {
        let guid = GUID::from_u128(INTERFACE_GUID);
        // La lista puede crecer entre las dos llamadas (CR_BUFFER_SMALL): se reintenta.
        for _ in 0..4 {
            let mut len = 0u32;
            // SAFETY: `len` y `guid` son locales y viven durante la llamada; sin identificador de
            // dispositivo (todas las interfaces de la clase).
            let result = unsafe {
                CM_Get_Device_Interface_List_SizeW(
                    &mut len,
                    &guid,
                    PCWSTR::null(),
                    CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
                )
            };
            if result != CR_SUCCESS {
                return Err(InputError::Backend(format!(
                    "mando virtual: no se pudo listar los dispositivos del driver (CONFIGRET {})",
                    result.0
                )));
            }
            let mut buffer = vec![0u16; len as usize];
            // SAFETY: `buffer` tiene los `len` caracteres que pide la API y vive durante la
            // llamada; `guid` también.
            let result = unsafe {
                CM_Get_Device_Interface_ListW(
                    &guid,
                    PCWSTR::null(),
                    &mut buffer,
                    CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
                )
            };
            if result == CR_BUFFER_SMALL {
                continue;
            }
            if result != CR_SUCCESS {
                return Err(InputError::Backend(format!(
                    "mando virtual: no se pudo listar los dispositivos del driver (CONFIGRET {})",
                    result.0
                )));
            }
            // REG_MULTI_SZ: rutas separadas por NUL y acabadas en dos.
            return Ok(buffer
                .split(|&unit| unit == 0)
                .filter(|path| !path.is_empty())
                .map(|path| path.iter().copied().chain(Some(0)).collect())
                .collect());
        }
        Err(InputError::Backend(
            "mando virtual: la lista de dispositivos cambiaba sin parar".to_owned(),
        ))
    }

    fn info(handle: &OwnedHandle) -> Result<DriverInfo, InputError> {
        let mut out = [0u8; size::INFO];
        let len = control(handle, ioctl::GET_INFO, &[], &mut out)
            .map_err(|err| driver_error(DriverOp::Info, None, win32_code(&err), &err.message()))?;
        let info = DriverInfo::parse(&out[..len])?;
        info.check()?;
        Ok(info)
    }

    /// El dispositivo de control y los Xbox 360 del driver, abiertos.
    #[derive(Debug)]
    pub struct DriverHandle {
        control: OwnedHandle,
        xusb: BTreeMap<u8, OwnedHandle>,
    }

    impl DriverHandle {
        /// Abre el dispositivo de control (exclusivo) y los Xbox 360 que haya.
        pub fn open() -> Result<Self, InputError> {
            let mut control = None;
            let mut xusb = BTreeMap::new();
            let mut problem = None;
            for path in interfaces()? {
                let handle = match open_path(&path) {
                    Ok(handle) => handle,
                    Err(err) => {
                        problem = Some(driver_error(
                            DriverOp::Open,
                            None,
                            win32_code(&err),
                            &err.message(),
                        ));
                        continue;
                    }
                };
                match info(&handle) {
                    Ok(info) if info.kind == KIND_CONTROL => control = Some(handle),
                    Ok(info) if info.kind == KIND_XUSB => {
                        xusb.insert(info.slot, handle);
                    }
                    Ok(_) => {}
                    Err(err) => problem = Some(err),
                }
            }
            match control {
                Some(control) => Ok(Self { control, xusb }),
                None => Err(problem.unwrap_or_else(not_installed_error)),
            }
        }

        fn xusb(&self, slot: u8) -> Result<&OwnedHandle, InputError> {
            self.xusb
                .get(&slot)
                .ok_or_else(|| InputError::NoSuchSlot(slot + 1))
        }
    }

    impl VhidDevice for DriverHandle {
        fn create(&mut self, pad: u8, spec: &PadSpec) -> Result<(), InputError> {
            let request = create_request(pad, spec)?;
            control(&self.control, ioctl::CREATE, &request, &mut []).map_err(|err| {
                driver_error(
                    DriverOp::Create,
                    Some(pad + 1),
                    win32_code(&err),
                    &err.message(),
                )
            })?;
            Ok(())
        }

        fn destroy(&mut self, pad: u8) -> Result<(), InputError> {
            control(&self.control, ioctl::DESTROY, &[pad], &mut []).map_err(|err| {
                driver_error(
                    DriverOp::Destroy,
                    Some(pad + 1),
                    win32_code(&err),
                    &err.message(),
                )
            })?;
            Ok(())
        }

        fn input(&mut self, pad: u8, report: &[u8]) -> Result<(), InputError> {
            let request = input_request(pad, report)?;
            control(&self.control, ioctl::INPUT, &request, &mut []).map_err(|err| {
                driver_error(
                    DriverOp::Report,
                    Some(pad + 1),
                    win32_code(&err),
                    &err.message(),
                )
            })?;
            Ok(())
        }

        fn take_output(&mut self) -> Result<Option<DriverOutput>, InputError> {
            let mut out = vec![0u8; size::OUTPUT_HEADER + MAX_REPORT];
            let len = control(&self.control, ioctl::TAKE_OUTPUT, &[], &mut out).map_err(|err| {
                driver_error(DriverOp::Output, None, win32_code(&err), &err.message())
            })?;
            parse_output(&out[..len])
        }

        fn xusb_slots(&self) -> Vec<u8> {
            self.xusb.keys().copied().collect()
        }

        fn xusb_activate(&mut self, slot: u8, vendor: u16, product: u16) -> Result<(), InputError> {
            let mut id = [0u8; size::XUSB_ID];
            id[0..2].copy_from_slice(&vendor.to_le_bytes());
            id[2..4].copy_from_slice(&product.to_le_bytes());
            control(self.xusb(slot)?, ioctl::XUSB_ACTIVATE, &id, &mut []).map_err(|err| {
                driver_error(DriverOp::Create, None, win32_code(&err), &err.message())
            })?;
            Ok(())
        }

        fn xusb_deactivate(&mut self, slot: u8) -> Result<(), InputError> {
            control(self.xusb(slot)?, ioctl::XUSB_DEACTIVATE, &[], &mut []).map_err(|err| {
                driver_error(DriverOp::Destroy, None, win32_code(&err), &err.message())
            })?;
            Ok(())
        }

        fn xusb_state(&mut self, slot: u8, state: &XusbState) -> Result<(), InputError> {
            control(
                self.xusb(slot)?,
                ioctl::XUSB_STATE,
                &state.to_bytes(),
                &mut [],
            )
            .map_err(|err| {
                driver_error(DriverOp::Report, None, win32_code(&err), &err.message())
            })?;
            Ok(())
        }
    }

    /// Abre el driver para [`VhidGamepads`].
    pub fn open() -> Result<VhidGamepads<DriverHandle>, InputError> {
        let device = DriverHandle::open()?;
        tracing::info!(
            "mando virtual: driver coroomvhid, protocolo {PROTOCOL_VERSION}, Xbox 360 {:?}",
            device.xusb_slots()
        );
        VhidGamepads::with_device(device)
    }

    /// Estado del driver para `coroom doctor`.
    pub fn status() -> String {
        match DriverHandle::open() {
            Ok(device) => format!(
                "mandos: driver coroomvhid (protocolo {PROTOCOL_VERSION}): mandos tal cual (HID) \
                 y {} mandos de Xbox 360",
                device.xusb_slots().len()
            ),
            Err(err) => format!("mandos: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESCRIPTOR_SOURCE: &str =
        include_str!("../../../../drivers/windows-vhid/report_descriptor.inc");
    const HEADER_SOURCE: &str = include_str!("../../../../drivers/windows-vhid/coroom_vhid.h");
    const INF_SOURCE: &str = include_str!("../../../../drivers/windows-vhid/coroomvhid.inf");
    const DEVICES_SOURCE: &str = include_str!("../../../../drivers/windows-vhid/devices.ps1");
    const DRIVER_SOURCE: &str = include_str!("../../../../drivers/windows-vhid/driver.h");
    const INSTALL_SOURCE: &str = include_str!("../../../../drivers/windows-vhid/install.ps1");

    const ONE: GuestId = GuestId(1);
    /// Sin repeticiones durante un test (las cuentas de informes son exactas).
    const NO_KEEPALIVE: Duration = Duration::from_secs(3600);
    const TWO: GuestId = GuestId(2);

    /// Quita los comentarios `//` y `/* */` de un texto en C.
    fn strip_c_comments(source: &str) -> String {
        let mut out = String::with_capacity(source.len());
        let mut rest = source;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix("//") {
                rest = after.find('\n').map_or("", |end| &after[end..]);
            } else if let Some(after) = rest.strip_prefix("/*") {
                let end = after.find("*/").expect("comentario sin cerrar");
                rest = &after[end + 2..];
                out.push(' ');
            } else {
                let ch = rest.chars().next().unwrap();
                out.push(ch);
                rest = &rest[ch.len_utf8()..];
            }
        }
        out
    }

    /// Los bytes del mando, `report_descriptor.inc`.
    fn descriptor() -> Vec<u8> {
        descriptor_bytes(DESCRIPTOR_SOURCE)
    }

    /// Los bytes de un `.inc` de descriptor (solo `0x..`, comas y comentarios).
    fn descriptor_bytes(source: &str) -> Vec<u8> {
        strip_c_comments(source)
            .split(',')
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(|token| {
                let hex = token
                    .strip_prefix("0x")
                    .or_else(|| token.strip_prefix("0X"))
                    .unwrap_or_else(|| panic!("«{token}» no es un byte hexadecimal"));
                assert_eq!(hex.len(), 2, "«{token}» debe tener dos cifras");
                u8::from_str_radix(hex, 16).unwrap()
            })
            .collect()
    }

    /// Un campo del descriptor (un elemento de `Input`, `Output` o `Feature` por `Report Count`).
    #[derive(Debug, Clone, Copy)]
    struct Field {
        report_id: u8,
        /// Bit desde el principio del informe, con el identificador.
        bit: usize,
        size: usize,
        usage_page: u16,
        usage: Option<u16>,
        logical_min: i32,
        logical_max: i32,
        flags: u32,
    }

    impl Field {
        fn constant(&self) -> bool {
            self.flags & 1 != 0
        }
    }

    #[derive(Debug, Default)]
    struct Parsed {
        /// Campos de entrada.
        fields: Vec<Field>,
        /// Campos de salida.
        outputs: Vec<Field>,
        /// Campos de característica.
        features: Vec<Field>,
        /// Uso de cada colección de aplicación de más fuera (página, uso), en orden.
        applications: Vec<(u16, u16)>,
        max_depth: usize,
    }

    /// Analizador de descriptores HID (elementos cortos, HID 1.11 §6.2.2), lo justo para este.
    fn parse(descriptor: &[u8]) -> Parsed {
        let mut parsed = Parsed::default();
        let (mut usage_page, mut logical_min, mut logical_max) = (0u16, 0i32, 0i32);
        let (mut report_size, mut report_count, mut report_id) = (0usize, 0usize, 0u8);
        let mut usages: Vec<u16> = Vec::new();
        let (mut usage_min, mut usage_max) = (None::<u16>, None::<u16>);
        // Bits ocupados por cada (tipo de informe, identificador).
        let mut bits: BTreeMap<(u8, u8), usize> = BTreeMap::new();
        let mut depth = 0usize;
        let mut at = 0;
        while at < descriptor.len() {
            let prefix = descriptor[at];
            assert_ne!(prefix, 0xFE, "elemento largo en {at}");
            let size = [0, 1, 2, 4][usize::from(prefix & 3)];
            assert!(
                at + 1 + size <= descriptor.len(),
                "elemento cortado en {at}"
            );
            let data = &descriptor[at + 1..at + 1 + size];
            let unsigned = data
                .iter()
                .rev()
                .fold(0u32, |acc, byte| (acc << 8) | u32::from(*byte));
            let signed = match size {
                0 => 0,
                1 => i32::from(data[0] as i8),
                2 => i32::from(i16::from_le_bytes([data[0], data[1]])),
                _ => unsigned as i32,
            };
            let kind = (prefix >> 2) & 3;
            let tag = prefix >> 4;
            match (kind, tag) {
                // Main
                (0, 0x8 | 0x9 | 0xB) => {
                    let id_bits = if report_id == 0 { 0 } else { 8 };
                    let cursor = bits.entry((tag, report_id)).or_insert(id_bits);
                    for index in 0..report_count {
                        let usage = if !usages.is_empty() {
                            Some(usages[index.min(usages.len() - 1)])
                        } else {
                            usage_min.map(|min| {
                                let usage = min + index as u16;
                                assert!(usage <= usage_max.expect("Usage Maximum"));
                                usage
                            })
                        };
                        let list = match tag {
                            0x8 => &mut parsed.fields,
                            0x9 => &mut parsed.outputs,
                            _ => &mut parsed.features,
                        };
                        list.push(Field {
                            report_id,
                            bit: *cursor,
                            size: report_size,
                            usage_page,
                            usage,
                            logical_min,
                            logical_max,
                            flags: unsigned,
                        });
                        *cursor += report_size;
                    }
                }
                (0, 0xA) => {
                    if depth == 0 {
                        assert_eq!(unsigned, 1, "la colección de fuera es de aplicación");
                        parsed.applications.push((usage_page, usages[0]));
                    }
                    depth += 1;
                    parsed.max_depth = parsed.max_depth.max(depth);
                }
                (0, 0xC) => depth = depth.checked_sub(1).expect("End Collection de más"),
                // Global
                (1, 0x0) => usage_page = unsigned as u16,
                (1, 0x1) => logical_min = signed,
                (1, 0x2) => {
                    // Logical Maximum es con signo solo si Logical Minimum es negativo.
                    logical_max = if logical_min < 0 {
                        signed
                    } else {
                        unsigned as i32
                    }
                }
                (1, 0x3..=0x6) => {} // Physical Min/Max, Unit Exponent, Unit
                (1, 0x7) => report_size = unsigned as usize,
                (1, 0x8) => {
                    report_id = unsigned as u8;
                    assert_ne!(report_id, 0);
                }
                (1, 0x9) => report_count = unsigned as usize,
                // Local
                (2, 0x0) => usages.push(unsigned as u16),
                (2, 0x1) => usage_min = Some(unsigned as u16),
                (2, 0x2) => usage_max = Some(unsigned as u16),
                other => panic!("elemento HID no esperado {other:?} en {at}"),
            }
            if kind == 0 {
                usages.clear();
                usage_min = None;
                usage_max = None;
            }
            at += 1 + size;
        }
        assert_eq!(depth, 0, "colecciones sin cerrar");
        parsed
    }

    fn data_fields(parsed: &Parsed) -> Vec<Field> {
        parsed
            .fields
            .iter()
            .filter(|field| !field.constant())
            .copied()
            .collect()
    }

    fn field(parsed: &Parsed, page: u16, usage: u16) -> Field {
        let found: Vec<Field> = data_fields(parsed)
            .into_iter()
            .filter(|field| field.usage_page == page && field.usage == Some(usage))
            .collect();
        assert_eq!(found.len(), 1, "uso {page:#x}:{usage:#x}");
        found[0]
    }

    const GENERIC_DESKTOP: u16 = 0x01;
    const BUTTON_PAGE: u16 = 0x09;
    const USAGE_X: u16 = 0x30;
    const USAGE_Y: u16 = 0x31;
    const USAGE_Z: u16 = 0x32;
    const USAGE_RX: u16 = 0x33;
    const USAGE_RY: u16 = 0x34;
    const USAGE_RZ: u16 = 0x35;
    const USAGE_HAT: u16 = 0x39;

    /// Escribe `value` en el campo, bit a bit, como lo leería el sistema.
    fn put(report: &mut [u8; REPORT_LEN], field: Field, value: i32) {
        assert!(
            (field.logical_min..=field.logical_max).contains(&value)
                || (field.usage == Some(USAGE_HAT) && value == i32::from(HAT_NULL)),
            "{value} fuera del rango de {field:?}"
        );
        for bit in 0..field.size {
            let at = field.bit + bit;
            let mask = 1u8 << (at % 8);
            if (value >> bit) & 1 == 1 {
                report[at / 8] |= mask;
            } else {
                report[at / 8] &= !mask;
            }
        }
    }

    #[test]
    fn the_descriptor_is_a_gamepad_with_16_buttons_6_axes_and_a_hat() {
        let bytes = descriptor();
        assert_eq!(bytes.len(), 85, "longitud del descriptor");
        let parsed = parse(&bytes);
        assert_eq!(
            parsed.applications,
            vec![(GENERIC_DESKTOP, 0x05)],
            "Game Pad"
        );
        assert_eq!(parsed.max_depth, 1);
        assert!(
            parsed.outputs.is_empty() && parsed.features.is_empty(),
            "el mando no tiene informes de salida (vibración) ni de característica"
        );
        assert!(
            parsed
                .fields
                .iter()
                .all(|field| field.report_id == REPORT_ID)
        );
        let total: usize = parsed.fields.iter().map(|field| field.size).sum();
        assert_eq!(
            8 + total,
            REPORT_LEN * 8,
            "el informe ocupa REPORT_LEN bytes"
        );

        let buttons: Vec<Field> = data_fields(&parsed)
            .into_iter()
            .filter(|field| field.usage_page == BUTTON_PAGE)
            .collect();
        assert_eq!(buttons.len(), 16);
        for (index, button) in buttons.iter().enumerate() {
            assert_eq!(button.usage, Some(index as u16 + 1));
            assert_eq!(
                (button.size, button.logical_min, button.logical_max),
                (1, 0, 1)
            );
            assert_eq!(
                button.bit,
                offset::BUTTONS * 8 + index,
                "botón {}",
                index + 1
            );
        }

        for (usage, at, min) in [
            (USAGE_X, offset::X, -32767),
            (USAGE_Y, offset::Y, -32767),
            (USAGE_RX, offset::RX, -32767),
            (USAGE_RY, offset::RY, -32767),
            (USAGE_Z, offset::Z, 0),
            (USAGE_RZ, offset::RZ, 0),
        ] {
            let axis = field(&parsed, GENERIC_DESKTOP, usage);
            assert_eq!(axis.size, 16, "eje {usage:#x}");
            assert_eq!(axis.bit, at * 8, "eje {usage:#x}");
            assert_eq!(
                (axis.logical_min, axis.logical_max),
                (min, i32::from(AXIS_MAX))
            );
        }
        let axes = data_fields(&parsed)
            .iter()
            .filter(|field| field.usage_page == GENERIC_DESKTOP && field.size == 16)
            .count();
        assert_eq!(axes, 6);

        let hat = field(&parsed, GENERIC_DESKTOP, USAGE_HAT);
        assert_eq!((hat.size, hat.logical_min, hat.logical_max), (4, 0, 7));
        assert_eq!(hat.bit, offset::HAT * 8);
        assert_ne!(hat.flags & 0x40, 0, "estado nulo");
        assert!(
            i32::from(HAT_NULL) > hat.logical_max,
            "el nulo está fuera del rango"
        );
        let padding: usize = parsed
            .fields
            .iter()
            .filter(|field| field.constant())
            .map(|field| field.size)
            .sum();
        assert_eq!(padding, 4);
    }
    #[test]
    fn the_report_follows_the_descriptor() {
        let parsed = parse(&descriptor());
        let neutral = PadReport::neutral().to_bytes();
        assert_eq!(
            neutral,
            [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, HAT_NULL],
            "reposo: sticks al centro, gatillos a 0 y hat nulo"
        );

        // Cada botón enciende el bit del botón HID que le toca y nada más.
        for button in GamepadButton::ALL {
            let mut report = PadReport::neutral();
            report.set_button(button, true);
            let mut expected = neutral;
            match button_bit(button) {
                Some(bit) => {
                    let usage = u16::from(bit) + 1;
                    put(&mut expected, field(&parsed, BUTTON_PAGE, usage), 1);
                }
                None => {
                    let hat = match button {
                        GamepadButton::DPadUp => 0,
                        GamepadButton::DPadRight => 2,
                        GamepadButton::DPadDown => 4,
                        _ => 6,
                    };
                    put(
                        &mut expected,
                        field(&parsed, GENERIC_DESKTOP, USAGE_HAT),
                        hat,
                    );
                }
            }
            assert_eq!(report.to_bytes(), expected, "{button:?}");
        }

        // Vectores fijos (byte 0: id; 1-2: botones; 3-14: ejes; 15: hat).
        let mut report = PadReport::neutral();
        report.set_button(GamepadButton::A, true);
        assert_eq!(
            report.to_bytes()[1..3],
            [0x01, 0x00],
            "A = botón 1 = bit 0 del byte 1"
        );
        report.set_button(GamepadButton::Guide, true);
        assert_eq!(
            report.to_bytes()[1..3],
            [0x01, 0x04],
            "Guide = botón 11 = bit 2 del byte 2"
        );
        report.set_axis(GamepadAxis::LeftX, i16::MAX);
        report.set_axis(GamepadAxis::LeftY, i16::MAX);
        report.set_axis(GamepadAxis::RightY, i16::MIN);
        report.set_axis(GamepadAxis::LeftTrigger, i16::MAX);
        report.set_axis(GamepadAxis::RightTrigger, -5);
        report.set_button(GamepadButton::DPadUp, true);
        report.set_button(GamepadButton::DPadRight, true);
        assert_eq!(
            report.to_bytes(),
            [
                0x01, // id
                0x01, 0x04, // A y Guide
                0xFF, 0x7F, // X = 32767 (derecha)
                0x01, 0x80, // Y = -32767 (arriba en el cable, arriba en HID es negativo)
                0x00, 0x00, // Rx
                0xFF, 0x7F, // Ry = 32767 (abajo: -32768 del cable, recortado)
                0xFF, 0x7F, // Z = 32767 (LT a fondo)
                0x00, 0x00, // Rz = 0 (RT negativo recortado)
                0x01, // hat arriba-derecha
            ]
        );

        // Estados variados: lo empaquetado coincide campo a campo con el descriptor.
        let mut seed = 0x2545_F491_u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..2000 {
            let mut report = PadReport::neutral();
            for button in GamepadButton::ALL {
                report.set_button(button, next() & 1 == 1);
            }
            for axis in GamepadAxis::ALL {
                report.set_axis(axis, next() as i16);
            }
            let mut expected = [0u8; REPORT_LEN];
            expected[0] = REPORT_ID;
            for button in GamepadButton::ALL {
                if let Some(bit) = button_bit(button) {
                    let pressed = report.buttons & (1 << bit) != 0;
                    let usage = u16::from(bit) + 1;
                    put(
                        &mut expected,
                        field(&parsed, BUTTON_PAGE, usage),
                        i32::from(pressed),
                    );
                }
            }
            for axis in GamepadAxis::ALL {
                let value = i32::from(report.axes[axis_index(axis)]);
                let (usage, wire) = match axis {
                    GamepadAxis::LeftX => (USAGE_X, value.clamp(-32767, 32767)),
                    GamepadAxis::LeftY => (USAGE_Y, -value.clamp(-32767, 32767)),
                    GamepadAxis::RightX => (USAGE_RX, value.clamp(-32767, 32767)),
                    GamepadAxis::RightY => (USAGE_RY, -value.clamp(-32767, 32767)),
                    GamepadAxis::LeftTrigger => (USAGE_Z, value.max(0)),
                    GamepadAxis::RightTrigger => (USAGE_RZ, value.max(0)),
                };
                put(&mut expected, field(&parsed, GENERIC_DESKTOP, usage), wire);
            }
            let hat = field(&parsed, GENERIC_DESKTOP, USAGE_HAT);
            put(&mut expected, hat, i32::from(report.hat()));
            assert_eq!(report.to_bytes(), expected, "{report:?}");
        }
    }

    #[test]
    fn the_hat_has_eight_directions_and_opposites_cancel() {
        use GamepadButton::{DPadDown, DPadLeft, DPadRight, DPadUp};
        let cases: [(&[GamepadButton], u8); 12] = [
            (&[], HAT_NULL),
            (&[DPadUp], 0),
            (&[DPadUp, DPadRight], 1),
            (&[DPadRight], 2),
            (&[DPadDown, DPadRight], 3),
            (&[DPadDown], 4),
            (&[DPadDown, DPadLeft], 5),
            (&[DPadLeft], 6),
            (&[DPadUp, DPadLeft], 7),
            (&[DPadUp, DPadDown], HAT_NULL),
            (&[DPadLeft, DPadRight], HAT_NULL),
            (&[DPadUp, DPadDown, DPadLeft], 6),
        ];
        for (pressed, hat) in cases {
            let mut report = PadReport::neutral();
            for button in pressed {
                report.set_button(*button, true);
            }
            assert_eq!(report.hat(), hat, "{pressed:?}");
        }
    }
    #[test]
    fn the_generic_descriptor_is_the_one_of_the_driver_folder() {
        assert_eq!(
            GENERIC_DESCRIPTOR.to_vec(),
            descriptor(),
            "GENERIC_DESCRIPTOR = report_descriptor.inc"
        );
        let summary = hid::parse(&GENERIC_DESCRIPTOR).unwrap();
        assert!(summary.has_controller() && !summary.has_keyboard_or_mouse());
        assert_eq!(summary.max_input, REPORT_LEN);
        let spec = PadSpec::generic();
        assert_eq!(spec.neutral, PadReport::neutral().to_bytes().to_vec());
    }

    /// `#define NOMBRE VALOR` de la cabecera del driver.
    fn header_defines() -> BTreeMap<String, String> {
        strip_c_comments(HEADER_SOURCE)
            .lines()
            .filter_map(|line| line.trim().strip_prefix("#define "))
            .filter_map(|rest| {
                let (name, value) = rest.trim().split_once(char::is_whitespace)?;
                Some((name.to_owned(), value.trim().to_owned()))
            })
            .collect()
    }

    fn header_number(defines: &BTreeMap<String, String>, name: &str) -> u32 {
        let value = defines
            .get(name)
            .unwrap_or_else(|| panic!("falta {name} en coroom_vhid.h"));
        match value.strip_prefix("0x") {
            Some(hex) => u32::from_str_radix(hex, 16).unwrap(),
            None => value.parse().unwrap_or_else(|_| panic!("{name} = {value}")),
        }
    }

    /// Texto de un literal `L"..."` de C con sus escapes `\\`.
    fn header_string(defines: &BTreeMap<String, String>, name: &str) -> String {
        let value = &defines[name];
        let inner = value
            .strip_prefix("L\"")
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or_else(|| panic!("{name} no es un literal L\"...\""));
        inner.replace("\\\\", "\\")
    }

    /// `CTL_CODE(FILE_DEVICE_UNKNOWN, function, METHOD_BUFFERED, FILE_READ_DATA |
    /// FILE_WRITE_DATA)`.
    const fn ctl_code(function: u32) -> u32 {
        (0x22 << 16) | (3 << 14) | (function << 2)
    }

    #[test]
    fn the_constants_match_the_driver_header() {
        let defines = header_defines();
        let number = |name: &str| header_number(&defines, name);
        assert_eq!(number("COROOM_VHID_PROTOCOL_VERSION"), PROTOCOL_VERSION);
        assert_eq!(number("COROOM_VHID_MAX_PADS"), u32::from(DRIVER_PADS));
        assert_eq!(number("COROOM_VHID_XUSB_SLOTS"), u32::from(XUSB_SLOTS));
        assert_eq!(
            number("COROOM_VHID_MAX_DESCRIPTOR") as usize,
            hid::MAX_DESCRIPTOR
        );
        assert_eq!(number("COROOM_VHID_MAX_REPORT") as usize, MAX_REPORT);
        assert_eq!(MAX_REPORT, hid::MAX_REPORT_BYTES);
        assert_eq!(number("COROOM_VHID_MAX_FEATURES") as usize, MAX_FEATURES);
        assert_eq!(MAX_FEATURES, coroom_proto::input::MAX_FEATURES);
        assert_eq!(
            number("COROOM_VHID_MAX_FEATURE_BYTES") as usize,
            MAX_FEATURE_BYTES
        );
        assert_eq!(MAX_FEATURE_BYTES, coroom_proto::input::MAX_FEATURE_BYTES);
        assert_eq!(
            Duration::from_millis(u64::from(number("COROOM_VHID_WATCHDOG_MS"))),
            WATCHDOG
        );
        assert!(
            KEEPALIVE_INTERVAL * 3 < WATCHDOG,
            "el estado se repite varias veces antes de que salte el perro guardián"
        );
        for (name, code, function) in [
            ("GET_INFO", ioctl::GET_INFO, 0x900),
            ("CREATE", ioctl::CREATE, 0x901),
            ("DESTROY", ioctl::DESTROY, 0x902),
            ("INPUT", ioctl::INPUT, 0x903),
            ("TAKE_OUTPUT", ioctl::TAKE_OUTPUT, 0x904),
            ("XUSB_ACTIVATE", ioctl::XUSB_ACTIVATE, 0x910),
            ("XUSB_DEACTIVATE", ioctl::XUSB_DEACTIVATE, 0x911),
            ("XUSB_STATE", ioctl::XUSB_STATE, 0x912),
        ] {
            assert_eq!(number(&format!("COROOM_VHID_IOCTL_{name}")), code, "{name}");
            assert_eq!(code, ctl_code(function), "{name}");
        }
        assert_eq!(number("COROOM_VHID_KIND_CONTROL"), u32::from(KIND_CONTROL));
        assert_eq!(number("COROOM_VHID_KIND_XUSB"), u32::from(KIND_XUSB));
        assert_eq!(
            number("COROOM_VHID_OUTPUT_REPORT"),
            u32::from(OUTPUT_REPORT)
        );
        assert_eq!(
            number("COROOM_VHID_OUTPUT_FEATURE"),
            u32::from(OUTPUT_FEATURE)
        );
        for (name, bytes) in [
            ("INFO", size::INFO),
            ("CREATE", size::CREATE),
            ("INPUT", size::INPUT),
            ("OUTPUT_HEADER", size::OUTPUT_HEADER),
            ("XUSB_ID", size::XUSB_ID),
            ("XUSB_STATE", size::XUSB_STATE),
        ] {
            assert_eq!(
                number(&format!("COROOM_VHID_{name}_SIZE")) as usize,
                bytes,
                "{name}"
            );
        }
        assert_eq!(
            number("COROOM_VHID_XBOX360_VENDOR_ID"),
            u32::from(XBOX360_VENDOR)
        );
        assert_eq!(
            number("COROOM_VHID_XBOX360_PRODUCT_ID"),
            u32::from(XBOX360_PRODUCT)
        );
        assert_eq!(
            header_string(&defines, "COROOM_VHID_HARDWARE_ID"),
            HARDWARE_ID
        );
        assert_eq!(
            header_string(&defines, "COROOM_VHID_XUSB_HARDWARE_ID"),
            XUSB_HARDWARE_ID
        );

        // La interfaz: el texto de la cabecera, el DEFINE_GUID de driver.h y la constante.
        let text = header_string(&defines, "COROOM_VHID_INTERFACE_GUID");
        let hex: String = text.chars().filter(char::is_ascii_hexdigit).collect();
        assert_eq!(u128::from_str_radix(&hex, 16).unwrap(), INTERFACE_GUID);
        let define = DRIVER_SOURCE
            .split("DEFINE_GUID(GUID_DEVINTERFACE_COROOM_VHID,")
            .nth(1)
            .and_then(|rest| rest.split(')').next())
            .expect("DEFINE_GUID de la interfaz en driver.h");
        let parts: Vec<u128> = define
            .split(',')
            .map(|part| u128::from_str_radix(part.trim().trim_start_matches("0x"), 16).unwrap())
            .collect();
        assert_eq!(parts.len(), 11);
        let mut guid = (parts[0] << 96) | (parts[1] << 80) | (parts[2] << 64);
        for (index, byte) in parts[3..].iter().enumerate() {
            guid |= byte << (56 - 8 * index);
        }
        assert_eq!(guid, INTERFACE_GUID);

        // El INF: los dos modelos, VHF debajo del de control, nada de modo kernel propio.
        let inf: String = INF_SOURCE
            .lines()
            .map(|line| line.split(';').next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(inf.contains(&format!("%ControlDesc% = CoroomControl, {HARDWARE_ID}")));
        assert!(inf.contains(&format!("%XusbDesc%    = CoroomXusb, {XUSB_HARDWARE_ID}")));
        assert!(inf.contains("Class       = System"));
        assert!(inf.contains("HKR,,\"LowerFilters\",0x00010008,\"vhf\""));
        assert!(inf.contains(&format!("HKR,,\"CoroomKind\",0x00010001,{KIND_CONTROL}")));
        assert!(inf.contains(&format!("HKR,,\"CoroomKind\",0x00010001,{KIND_XUSB}")));
        assert!(inf.contains("ServiceBinary      = %13%\\coroomvhid.dll"));
        assert!(
            !inf.contains(".sys = 1"),
            "el paquete no lleva drivers de kernel"
        );
        assert!(
            INF_SOURCE.contains("\"D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)\""),
            "el de control, solo para SYSTEM, administradores y el usuario interactivo"
        );

        // Los scripts de instalación usan los mismos valores.
        let slot_value = header_string(&defines, "COROOM_VHID_SLOT_VALUE");
        assert!(DEVICES_SOURCE.contains(&format!("$CoroomHardwareId = '{HARDWARE_ID}'")));
        assert!(DEVICES_SOURCE.contains(&format!("$CoroomXusbHardwareId = '{XUSB_HARDWARE_ID}'")));
        assert!(DEVICES_SOURCE.contains(&format!("$CoroomSlotValue = '{slot_value}'")));
        assert!(DEVICES_SOURCE.contains(&format!("$CoroomXusbSlots = {XUSB_SLOTS}")));
        assert!(
            DEVICES_SOURCE
                .contains("$CoroomClassGuid = [Guid]'4d36e97d-e325-11ce-bfc1-08002be10318'"),
            "clase System, la del INF"
        );
        for script in [DEVICES_SOURCE, INSTALL_SOURCE] {
            let lower = script.to_lowercase();
            assert!(
                !lower.contains("bcdedit") && !lower.contains("testsigning"),
                "la instalación no toca la firma de prueba"
            );
        }
    }

    #[test]
    fn requests_follow_the_header_structures() {
        let spec = PadSpec {
            vendor: 0x2DC8,
            product: 0x6012,
            version: 0x0114,
            descriptor: vec![0x05, 0x01, 0x09, 0x05, 0xA1, 0x01, 0xC0],
            neutral: vec![1, 2],
            features: vec![vec![3, 9, 9], vec![4]],
        };
        let request = create_request(5, &spec).unwrap();
        assert_eq!(
            request[..size::CREATE],
            [
                5, 0, // pad
                0xC8, 0x2D, 0x12, 0x60, 0x14, 0x01, // VID, PID, versión
                7, 0, 2, 0, 8, 0, // longitudes: descriptor, reposo, características
                0, 0,
            ]
        );
        assert_eq!(request[size::CREATE..size::CREATE + 7], spec.descriptor);
        assert_eq!(request[size::CREATE + 7..size::CREATE + 9], [1, 2]);
        assert_eq!(request[size::CREATE + 9..], [3, 0, 3, 9, 9, 1, 0, 4]);
        assert!(create_request(8, &spec).is_err(), "ocho mandos, del 0 al 7");
        let mut empty = spec.clone();
        empty.descriptor.clear();
        assert!(create_request(0, &empty).is_err());
        let mut many = spec.clone();
        many.features = vec![vec![1]; MAX_FEATURES + 1];
        assert!(create_request(0, &many).is_err());

        assert_eq!(input_request(2, &[1, 2, 3]).unwrap(), [2, 0, 3, 0, 1, 2, 3]);
        assert!(input_request(2, &[]).is_err());
        assert!(input_request(2, &[0; MAX_REPORT + 1]).is_err());

        assert_eq!(parse_output(&[]), Ok(None));
        assert_eq!(
            parse_output(&[3, OUTPUT_REPORT, 5, 0, 2, 0, 0, 0, 0xAA, 0xBB]),
            Ok(Some(DriverOutput {
                pad: 3,
                kind: OUTPUT_REPORT,
                report_id: 5,
                data: vec![0xAA, 0xBB],
            }))
        );
        assert!(parse_output(&[3, 1, 5, 0, 9, 0, 0, 0, 0xAA]).is_err());
        assert!(parse_output(&[8, 1, 5, 0, 0, 0, 0, 0]).is_err());

        let info = DriverInfo::parse(&[3, 0, 0, 0, KIND_XUSB, 2, 1, 0]).unwrap();
        assert_eq!(
            info,
            DriverInfo {
                protocol: 3,
                kind: KIND_XUSB,
                slot: 2,
                active: 1
            }
        );
        assert_eq!(info.check(), Ok(()));
        let older = DriverInfo::parse(&[2, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        assert!(
            matches!(older.check(), Err(InputError::GamepadUnavailable(text)) if text.contains("versión 2"))
        );
        assert!(
            DriverInfo::parse(&[3, 0, 0, 0, KIND_XUSB, 4, 0, 0])
                .unwrap()
                .check()
                .is_err()
        );
        assert!(DriverInfo::parse(&[3, 0, 0]).is_err());

        let InputError::GamepadUnavailable(text) = not_installed_error() else {
            panic!()
        };
        assert!(text.contains("drivers/windows-vhid/README.md"), "{text}");
        let InputError::Backend(text) = driver_error(DriverOp::Open, None, Some(5), "x") else {
            panic!()
        };
        assert!(text.contains("otro programa"), "{text}");
        let InputError::Backend(text) = driver_error(DriverOp::Create, Some(3), Some(50), "x")
        else {
            panic!()
        };
        assert!(
            text.contains("ranura 3") && text.contains("descriptor"),
            "{text}"
        );
    }

    #[test]
    fn xinput_state_follows_the_xinput_gamepad_layout() {
        let mut report = PadReport::neutral();
        assert_eq!(report.to_xusb().to_bytes(), [0; size::XUSB_STATE]);
        for button in GamepadButton::ALL {
            let mut one = PadReport::neutral();
            one.set_button(button, true);
            assert_eq!(one.to_xusb().buttons, xinput_bit(button), "{button:?}");
        }
        let bits: u16 = GamepadButton::ALL.iter().map(|b| xinput_bit(*b)).sum();
        assert_eq!(
            bits, 0xF7FF,
            "todos los botones de XInput, cada uno en su bit"
        );

        report.set_button(GamepadButton::A, true);
        report.set_button(GamepadButton::DPadLeft, true);
        report.set_axis(GamepadAxis::LeftX, -100);
        report.set_axis(GamepadAxis::LeftY, i16::MAX);
        report.set_axis(GamepadAxis::RightY, i16::MIN);
        report.set_axis(GamepadAxis::LeftTrigger, i16::MAX);
        report.set_axis(GamepadAxis::RightTrigger, -3);
        assert_eq!(
            report.to_xusb().to_bytes(),
            [
                0x04, 0x10, // A y cruceta izquierda
                0xFF, 0x00, // LT a fondo, RT negativo recortado a 0
                0x9C, 0xFF, // LX = -100
                0xFF, 0x7F, // LY = 32767 (arriba, como en el cable)
                0x00, 0x00, // RX
                0x00, 0x80, // RY = -32768
            ]
        );
    }

    #[test]
    fn outputs_get_their_report_id_as_hidraw_writes_them() {
        let numbered = HidSummary {
            report_ids: true,
            max_output: 48,
            ..HidSummary::default()
        };
        assert_eq!(normalize_output(&numbered, 2, &[2, 9, 9]), vec![2, 9, 9]);
        assert_eq!(normalize_output(&numbered, 2, &[9, 9]), vec![2, 9, 9]);
        let plain = HidSummary {
            report_ids: false,
            max_output: 3,
            ..HidSummary::default()
        };
        assert_eq!(normalize_output(&plain, 0, &[7, 8, 9]), vec![0, 7, 8, 9]);
        assert_eq!(normalize_output(&plain, 0, &[0, 7, 8, 9]), vec![0, 7, 8, 9]);
    }

    fn hat_of(wire: Wire) -> [u8; REPORT_LEN] {
        match wire {
            Wire::Hid(bytes) => bytes,
            Wire::Xusb(_) => panic!("se esperaba el informe genérico"),
        }
    }

    #[test]
    fn the_pacer_caps_axes_at_500_hz_but_never_delays_buttons() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut pacer = ReportPacer::default();
        let neutral = PadReport::neutral();
        let mut pad = neutral;
        let hid = |pad: &PadReport| Wire::Hid(pad.to_bytes());

        // El primero sale siempre.
        let first = pacer.offer(hid(&pad), t0).expect("primero");
        pacer.sent(first, t0);
        assert_eq!(
            pacer.offer(hid(&pad), ms(1)),
            None,
            "sin cambios no se manda"
        );
        assert_eq!(pacer.deadline(), None);

        // Ejes antes de 2 ms: esperan, y el más nuevo sustituye al anterior.
        let us = |n: u64| t0 + Duration::from_micros(n);
        pad.set_axis(GamepadAxis::LeftX, 100);
        assert_eq!(pacer.offer(hid(&pad), us(500)), None);
        pad.set_axis(GamepadAxis::LeftX, 200);
        assert_eq!(pacer.offer(hid(&pad), ms(1)), None);
        assert_eq!(pacer.deadline(), Some(ms(2)));
        assert_eq!(pacer.take_due(us(1_500)), None);
        let due = pacer.take_due(ms(2)).expect("vence a los 2 ms");
        assert_eq!(hat_of(due), pad.to_bytes(), "el último valor");
        pacer.sent(due, ms(2));
        assert_eq!(pacer.deadline(), None);

        // Un botón sale al momento aunque no hayan pasado 2 ms, con los ejes al día.
        pad.set_axis(GamepadAxis::LeftX, 300);
        assert_eq!(pacer.offer(hid(&pad), us(2_500)), None);
        pad.set_button(GamepadButton::A, true);
        let now = pacer.offer(hid(&pad), us(2_500)).expect("botón");
        assert_eq!(hat_of(now), pad.to_bytes());
        assert_eq!(pacer.deadline(), None, "lo pendiente iba incluido");
        pacer.sent(now, us(2_500));
        pad.set_button(GamepadButton::A, false);
        assert!(
            pacer.offer(hid(&pad), ms(5)).is_some(),
            "soltar tampoco espera"
        );
        pacer.sent(hid(&pad), ms(5));

        // La cruceta tampoco espera, y en un Xbox 360 es un botón más.
        pad.set_button(GamepadButton::DPadUp, true);
        assert!(pacer.offer(hid(&pad), ms(6)).is_some());
        let mut xusb = ReportPacer::default();
        let mut xpad = PadReport::neutral();
        xusb.sent(Wire::Xusb(xpad.to_xusb().to_bytes()), t0);
        xpad.set_axis(GamepadAxis::RightX, 10);
        assert_eq!(
            xusb.offer(Wire::Xusb(xpad.to_xusb().to_bytes()), ms(1)),
            None
        );
        xpad.set_button(GamepadButton::DPadUp, true);
        assert!(
            xusb.offer(Wire::Xusb(xpad.to_xusb().to_bytes()), ms(1))
                .is_some()
        );

        // Si vuelve al valor ya enviado, lo pendiente se olvida.
        let mut pacer = ReportPacer::default();
        pacer.sent(hid(&neutral), t0);
        let mut moved = neutral;
        moved.set_axis(GamepadAxis::LeftY, 9);
        assert_eq!(pacer.offer(hid(&moved), ms(1)), None);
        assert_eq!(pacer.offer(hid(&neutral), ms(2)), None);
        assert_eq!(pacer.deadline(), None);
    }

    #[test]
    fn the_pacer_repeats_the_last_report_while_nothing_changes() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let keepalive = Duration::from_millis(250);
        let mut pacer = ReportPacer::default();
        assert_eq!(pacer.keepalive_deadline(keepalive), None, "nada enviado");

        let mut pad = PadReport::neutral();
        let first = pacer.offer(Wire::Hid(pad.to_bytes()), t0).unwrap();
        pacer.sent(first, t0);
        assert_eq!(pacer.keepalive_deadline(keepalive), Some(ms(250)));
        assert_eq!(pacer.take_keepalive(ms(249), keepalive), None);
        assert_eq!(pacer.take_keepalive(ms(250), keepalive), Some(first));
        pacer.sent(first, ms(250));
        assert_eq!(pacer.keepalive_deadline(keepalive), Some(ms(500)));

        // Con algo pendiente, manda lo pendiente (que ya es más nuevo), no la repetición.
        pad.set_axis(GamepadAxis::LeftX, 10);
        assert_eq!(pacer.offer(Wire::Hid(pad.to_bytes()), ms(251)), None);
        assert_eq!(pacer.keepalive_deadline(keepalive), None);
        assert_eq!(pacer.take_keepalive(ms(600), keepalive), None);
        assert_eq!(pacer.take_due(ms(600)), Some(Wire::Hid(pad.to_bytes())));
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Create(u8, PadSpec),
        Destroy(u8),
        Input(u8, Vec<u8>),
        Activate(u8, u16, u16),
        Deactivate(u8),
        State(u8, XusbState),
    }

    #[derive(Debug, Clone)]
    struct FakeDevice {
        calls: Arc<Mutex<Vec<Call>>>,
        outputs: Arc<Mutex<Vec<DriverOutput>>>,
        xusb: Vec<u8>,
        reject_create: Arc<Mutex<bool>>,
    }

    impl FakeDevice {
        fn with_xusb(slots: u8) -> Self {
            Self {
                calls: Arc::default(),
                outputs: Arc::default(),
                xusb: (0..slots).collect(),
                reject_create: Arc::default(),
            }
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }

        fn clear(&self) {
            self.calls.lock().unwrap().clear();
        }

        fn wait_for(&self, condition: impl Fn(&[Call]) -> bool) -> bool {
            let deadline = Instant::now() + Duration::from_secs(20);
            while Instant::now() < deadline {
                if condition(&self.calls.lock().unwrap()) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            false
        }

        fn push(&self, call: Call) {
            self.calls.lock().unwrap().push(call);
        }
    }

    impl VhidDevice for FakeDevice {
        fn create(&mut self, pad: u8, spec: &PadSpec) -> Result<(), InputError> {
            assert!(pad < DRIVER_PADS);
            create_request(pad, spec)?;
            if *self.reject_create.lock().unwrap() && spec.vendor != 0 {
                return Err(InputError::Backend("VHF no quiere".to_owned()));
            }
            self.push(Call::Create(pad, spec.clone()));
            Ok(())
        }

        fn destroy(&mut self, pad: u8) -> Result<(), InputError> {
            self.push(Call::Destroy(pad));
            Ok(())
        }

        fn input(&mut self, pad: u8, report: &[u8]) -> Result<(), InputError> {
            input_request(pad, report)?;
            self.push(Call::Input(pad, report.to_vec()));
            Ok(())
        }

        fn take_output(&mut self) -> Result<Option<DriverOutput>, InputError> {
            let mut outputs = self.outputs.lock().unwrap();
            Ok((!outputs.is_empty()).then(|| outputs.remove(0)))
        }

        fn xusb_slots(&self) -> Vec<u8> {
            self.xusb.clone()
        }

        fn xusb_activate(&mut self, slot: u8, vendor: u16, product: u16) -> Result<(), InputError> {
            assert!(self.xusb.contains(&slot));
            self.push(Call::Activate(slot, vendor, product));
            Ok(())
        }

        fn xusb_deactivate(&mut self, slot: u8) -> Result<(), InputError> {
            self.push(Call::Deactivate(slot));
            Ok(())
        }

        fn xusb_state(&mut self, slot: u8, state: &XusbState) -> Result<(), InputError> {
            self.push(Call::State(slot, *state));
            Ok(())
        }
    }

    fn button(slot: u8, button: GamepadButton, pressed: bool) -> InputEvent {
        InputEvent::GamepadButton {
            slot,
            button,
            pressed,
        }
    }

    fn axis(slot: u8, axis: GamepadAxis, value: i16) -> InputEvent {
        InputEvent::GamepadAxis { slot, axis, value }
    }

    fn generic_neutral(pad: u8) -> Call {
        Call::Input(pad, PadReport::neutral().to_bytes().to_vec())
    }

    #[test]
    fn converted_pads_are_xbox_360_while_there_are_free_ones() {
        let device = FakeDevice::with_xusb(2);
        let mut pads = VhidGamepads::with_keepalive(device.clone(), NO_KEEPALIVE).unwrap();
        let rest = XusbState::default();

        pads.assign(2).unwrap();
        pads.assign(2).unwrap();
        assert_eq!(pads.assign(0), Err(InputError::NoSuchSlot(0)));
        assert_eq!(pads.assign(9), Err(InputError::NoSuchSlot(9)));
        pads.assign(5).unwrap();
        pads.assign(7).unwrap();
        assert_eq!(
            device.calls(),
            vec![
                Call::Activate(0, XBOX360_VENDOR, XBOX360_PRODUCT),
                Call::State(0, rest),
                Call::Activate(1, XBOX360_VENDOR, XBOX360_PRODUCT),
                Call::State(1, rest),
                Call::Create(6, PadSpec::generic()),
                generic_neutral(6),
            ],
            "dos Xbox 360 y, sin más libres, el genérico con el número de la ranura"
        );
        assert_eq!(
            pads.targets(),
            BTreeMap::from([
                (2, Target::Xusb(0)),
                (5, Target::Xusb(1)),
                (7, Target::Hid(6))
            ])
        );

        device.clear();
        pads.inject(&button(2, GamepadButton::A, true), ONE)
            .unwrap();
        pads.inject(&button(7, GamepadButton::A, true), ONE)
            .unwrap();
        let mut pressed = PadReport::neutral();
        pressed.set_button(GamepadButton::A, true);
        assert_eq!(
            device.calls(),
            vec![
                Call::State(0, pressed.to_xusb()),
                Call::Input(6, pressed.to_bytes().to_vec())
            ]
        );
        assert_eq!(
            pads.inject(&button(3, GamepadButton::A, true), ONE),
            Err(InputError::NoSuchSlot(3))
        );
        pads.inject(&InputEvent::MouseMove { dx: 1, dy: 1 }, ONE)
            .unwrap();
        assert_eq!(device.calls().len(), 2, "el ratón no manda nada");

        // Una ráfaga de ejes: el último valor llega, sin pasar de un informe por
        // [`REPORT_INTERVAL`] (500 Hz).
        let start = Instant::now();
        for value in 1..=50 {
            pads.inject(&axis(2, GamepadAxis::LeftX, value * 100), ONE)
                .unwrap();
        }
        pressed.set_axis(GamepadAxis::LeftX, 5000);
        let last = Call::State(0, pressed.to_xusb());
        assert!(device.wait_for(|calls| calls.last() == Some(&last)));
        let reports = device.calls().len() - 2;
        let allowed = start.elapsed().as_millis() / REPORT_INTERVAL.as_millis() + 2;
        assert!(
            reports as u128 <= allowed,
            "{reports} informes en {:?}",
            start.elapsed()
        );

        // Desconectar el mando del invitado lo deja en reposo; soltar la ranura libera el Xbox
        // 360, que vuelve a estar libre para la siguiente.
        pads.inject(&InputEvent::GamepadDisconnected { slot: 2 }, ONE)
            .unwrap();
        assert_eq!(device.calls().last(), Some(&Call::State(0, rest)));
        pads.release(2).unwrap();
        assert_eq!(device.calls().last(), Some(&Call::Deactivate(0)));
        assert_eq!(pads.release(2), Err(InputError::NoSuchSlot(2)));
        pads.release(7).unwrap();
        assert_eq!(device.calls().last(), Some(&Call::Destroy(6)));
        pads.assign(8).unwrap();
        assert_eq!(pads.targets()[&8], Target::Xusb(0));

        // Sin Xbox 360 instalados, todos son genéricos.
        let bare = FakeDevice::with_xusb(0);
        let mut pads = VhidGamepads::with_keepalive(bare.clone(), NO_KEEPALIVE).unwrap();
        pads.assign(1).unwrap();
        assert_eq!(
            bare.calls(),
            vec![Call::Create(0, PadSpec::generic()), generic_neutral(0)]
        );
    }

    #[test]
    fn release_guest_rests_only_that_guests_pads() {
        let device = FakeDevice::with_xusb(4);
        let mut pads = VhidGamepads::with_keepalive(device.clone(), NO_KEEPALIVE).unwrap();
        pads.assign(1).unwrap();
        pads.assign(4).unwrap();
        pads.inject(&button(1, GamepadButton::B, true), ONE)
            .unwrap();
        pads.inject(&button(4, GamepadButton::X, true), TWO)
            .unwrap();
        let before = device.calls().len();
        pads.release_guest(ONE).unwrap();
        assert_eq!(
            device.calls()[before..],
            [Call::State(0, XusbState::default())],
            "solo la ranura 1 (el primer Xbox 360) vuelve al reposo"
        );
    }

    #[test]
    fn idle_gamepads_repeat_their_state_for_the_watchdog() {
        let device = FakeDevice::with_xusb(1);
        let mut pads =
            VhidGamepads::with_keepalive(device.clone(), Duration::from_millis(5)).unwrap();
        pads.assign(3).unwrap();
        let repeated = |calls: &[Call]| {
            calls
                .iter()
                .filter(|call| **call == Call::State(0, XusbState::default()))
                .count()
        };
        assert!(
            device.wait_for(|calls| repeated(calls) >= 4),
            "{:?}",
            device.calls()
        );

        // Un mando liberado deja de repetirse.
        pads.release(3).unwrap();
        let after = device.calls().len();
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(device.calls().len(), after);
        assert_eq!(device.calls().last(), Some(&Call::Deactivate(0)));
    }

    /// Un mando HID con informes numerados: 8 botones (informe 1) y una salida de 3 bytes
    /// (informe 5, vibración).
    const RAW_DESCRIPTOR: &[u8] = &[
        0x05, 0x01, 0x09, 0x05, 0xA1, 0x01, 0x85, 0x01, 0x05, 0x09, 0x19, 0x01, 0x29, 0x08, 0x15,
        0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x85, 0x05, 0x06, 0x00, 0xFF, 0x09,
        0x01, 0x75, 0x08, 0x95, 0x03, 0x91, 0x02, 0xC0,
    ];

    fn raw_device(transport: GamepadTransport) -> GamepadDevice {
        GamepadDevice {
            name: "Mando de prueba".to_owned(),
            vendor: 0x2DC8,
            product: 0x6012,
            version: 0x0114,
            bus: 3,
            transport,
        }
    }

    #[test]
    fn raw_hid_pads_replace_the_converted_one_and_come_back() {
        let device = FakeDevice::with_xusb(1);
        let mut pads = VhidGamepads::with_keepalive(device.clone(), NO_KEEPALIVE).unwrap();
        pads.assign(2).unwrap();
        device.clear();

        let hid = raw_device(GamepadTransport::Hid {
            descriptor: RAW_DESCRIPTOR.to_vec(),
            features: vec![(2, vec![2, 7, 7])],
        });
        pads.connect_raw(2, &hid).unwrap();
        assert_eq!(
            device.calls(),
            vec![
                Call::Deactivate(0),
                Call::Create(
                    1,
                    PadSpec {
                        vendor: 0x2DC8,
                        product: 0x6012,
                        version: 0x0114,
                        descriptor: RAW_DESCRIPTOR.to_vec(),
                        neutral: Vec::new(),
                        features: vec![vec![2, 7, 7]],
                    }
                ),
            ],
            "el Xbox 360 se suelta y el mando del invitado ocupa el HID de su ranura"
        );
        assert_eq!(pads.targets()[&2], Target::Hid(1));

        // Sus informes pasan tal cual; los eventos convertidos ya no mandan nada.
        device.clear();
        pads.raw_report(2, &[1, 0x81]).unwrap();
        pads.inject(&button(2, GamepadButton::A, true), ONE)
            .unwrap();
        assert_eq!(device.calls(), vec![Call::Input(1, vec![1, 0x81])]);
        assert_eq!(pads.raw_report(3, &[1, 0]), Err(InputError::NoSuchSlot(3)));

        // La vibración vuelve con su identificador; lo de otros mandos y las características,
        // no.
        device.outputs.lock().unwrap().extend([
            DriverOutput {
                pad: 1,
                kind: OUTPUT_REPORT,
                report_id: 5,
                data: vec![0x40, 0x40, 0],
            },
            DriverOutput {
                pad: 1,
                kind: OUTPUT_FEATURE,
                report_id: 2,
                data: vec![2, 1],
            },
            DriverOutput {
                pad: 6,
                kind: OUTPUT_REPORT,
                report_id: 5,
                data: vec![5, 1, 1, 1],
            },
        ]);
        assert_eq!(pads.take_outputs(), vec![(2, vec![5, 0x40, 0x40, 0])]);
        assert!(pads.take_outputs().is_empty());

        // Vuelta al convertido: se quita el HID y se toma otra vez un Xbox 360, en reposo.
        device.clear();
        pads.disconnect_raw(2).unwrap();
        assert_eq!(
            device.calls(),
            vec![
                Call::Destroy(1),
                Call::Activate(0, XBOX360_VENDOR, XBOX360_PRODUCT),
                Call::State(0, XusbState::default()),
            ]
        );
        pads.disconnect_raw(2).unwrap();
        assert_eq!(device.calls().len(), 3, "ya era convertido");

        // Un descriptor que Windows no acepta deja el convertido como estaba.
        *device.reject_create.lock().unwrap() = true;
        device.clear();
        assert!(pads.connect_raw(2, &hid).is_err());
        assert_eq!(pads.targets()[&2], Target::Xusb(0));
        assert_eq!(
            device.calls(),
            vec![
                Call::Deactivate(0),
                Call::Activate(0, XBOX360_VENDOR, XBOX360_PRODUCT),
                Call::State(0, XusbState::default()),
            ]
        );
        let broken = raw_device(GamepadTransport::Hid {
            descriptor: vec![0xA1, 0x01],
            features: Vec::new(),
        });
        assert!(pads.connect_raw(2, &broken).is_err());
        assert!(
            pads.connect_raw(
                2,
                &raw_device(GamepadTransport::Converted {
                    reason: "sin hidraw".to_owned()
                })
            )
            .is_err()
        );
        assert_eq!(pads.connect_raw(6, &hid), Err(InputError::NoSuchSlot(6)));
    }

    #[test]
    fn wired_xbox_pads_keep_their_identity_on_an_xbox_360() {
        let device = FakeDevice::with_xusb(1);
        let mut pads = VhidGamepads::with_keepalive(device.clone(), NO_KEEPALIVE).unwrap();
        pads.assign(1).unwrap();
        device.clear();
        let mut xbox = raw_device(GamepadTransport::XInput);
        xbox.vendor = 0x045E;
        xbox.product = 0x0B12;
        pads.connect_raw(1, &xbox).unwrap();
        assert_eq!(
            device.calls(),
            vec![
                Call::Deactivate(0),
                Call::Activate(0, 0x045E, 0x0B12),
                Call::State(0, XusbState::default()),
            ]
        );
        // Sus eventos van como los de un convertido.
        pads.inject(&button(1, GamepadButton::Y, true), ONE)
            .unwrap();
        let mut pressed = PadReport::neutral();
        pressed.set_button(GamepadButton::Y, true);
        assert_eq!(
            device.calls().last(),
            Some(&Call::State(0, pressed.to_xusb()))
        );

        // Sin Xbox 360 libre no se puede: sigue el genérico.
        let bare = FakeDevice::with_xusb(0);
        let mut pads = VhidGamepads::with_keepalive(bare.clone(), NO_KEEPALIVE).unwrap();
        pads.assign(1).unwrap();
        assert!(matches!(
            pads.connect_raw(1, &xbox),
            Err(InputError::Unsupported(text)) if text.contains("Xbox 360")
        ));
        assert_eq!(pads.targets()[&1], Target::Hid(0));
    }
}
