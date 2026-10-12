//! Mando virtual en Windows (feature `windows-vhid`): cliente del driver
//! `coroomvhid.sys` (`drivers/windows-vhid/`).
//!
//! Windows no tiene API de usuario para crear un dispositivo HID: el driver, sobre Virtual HID
//! Framework, crea hasta [`DRIVER_SLOTS`] mandos HID genéricos bajo demanda y los alimenta con
//! los informes que le manda este módulo por el dispositivo de control [`DEVICE_PATH`]
//! (`IOCTL_COROOM_VHID_*` de `coroom_vhid.h`).
//!
//! - **Formato** (independiente de la plataforma y probado en Linux): el informe de 16 bytes del
//!   descriptor `report_descriptor.inc` ([`PadReport::to_bytes`]), las cargas de cada IOCTL y los
//!   códigos `CTL_CODE`. Los tests leen el descriptor y la cabecera del driver y comprueban que
//!   este módulo dice lo mismo que ellos.
//! - **Ranuras**: las del cable van de 1 a 8 y las del driver de 0 a 7 ([`driver_slot`]).
//! - **Ritmo**: un informe en cada cambio, con tope de 250 Hz ([`REPORT_INTERVAL`]) para los ejes;
//!   los botones y la cruceta salen siempre al momento (una pulsación corta no se pierde). Un hilo
//!   (`coroom-vhid`) manda el último estado de los ejes que esperaba su turno.
//! - [`VhidGamepads`] funciona sobre cualquier [`VhidDevice`]; en Windows, `DriverHandle` abre
//!   el driver (`CreateFileW`), comprueba la versión del protocolo y habla por `DeviceIoControl`.
//!   Al cerrarse el handle (también si `coroom` muere) el driver destruye sus mandos.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::{GamepadAxis, GamepadButton, GuestId, InputError, InputEvent};

/// Versión del protocolo del driver que habla este cliente (`COROOM_VHID_PROTOCOL_VERSION`).
pub const PROTOCOL_VERSION: u32 = 1;

/// Mandos que puede crear el driver (`COROOM_VHID_MAX_SLOTS`).
pub const DRIVER_SLOTS: u8 = 8;

/// Dispositivo de control del driver (`COROOM_VHID_USER_PATH`).
pub const DEVICE_PATH: &str = r"\\.\coroomvhid";

/// Identificador del informe de entrada (`COROOM_VHID_REPORT_ID`).
pub const REPORT_ID: u8 = 1;

/// Bytes del informe, con el identificador (`COROOM_VHID_REPORT_SIZE`).
pub const REPORT_LEN: usize = 16;

/// Bytes de la respuesta de `IOCTL_COROOM_VHID_VERSION` (`COROOM_VHID_VERSION_INFO_SIZE`).
pub const VERSION_INFO_LEN: usize = 8;

/// Bytes de la carga de CREATE y DESTROY (`COROOM_VHID_SLOT_SIZE`).
pub const SLOT_LEN: usize = 1;

/// Bytes de la carga de REPORT (`COROOM_VHID_REPORT_PACKET_SIZE`).
pub const REPORT_PACKET_LEN: usize = 1 + REPORT_LEN;

/// Máximo de los ejes del informe (`COROOM_VHID_AXIS_MAX`); los sticks van de `-AXIS_MAX` a
/// `AXIS_MAX` y los gatillos de 0 a `AXIS_MAX`.
pub const AXIS_MAX: i16 = 32767;

/// Valor del hat en reposo (`COROOM_VHID_HAT_NULL`).
pub const HAT_NULL: u8 = 8;

/// Tope de ritmo de los informes de ejes por mando: 250 Hz.
pub const REPORT_INTERVAL: Duration = Duration::from_millis(4);

/// Posición de cada campo en el informe (`COROOM_VHID_REPORT_OFFSET_*`); los ejes son `i16`
/// little endian.
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

const FILE_DEVICE_UNKNOWN: u32 = 0x22;
const METHOD_BUFFERED: u32 = 0;
const FILE_ANY_ACCESS: u32 = 0;

/// `CTL_CODE` de `winioctl.h`.
pub const fn ctl_code(device_type: u32, function: u32, method: u32, access: u32) -> u32 {
    (device_type << 16) | (access << 14) | (function << 2) | method
}

/// `COROOM_VHID_IOCTL(n)`.
pub const fn coroom_ioctl(n: u32) -> u32 {
    ctl_code(
        FILE_DEVICE_UNKNOWN,
        0x800 + n,
        METHOD_BUFFERED,
        FILE_ANY_ACCESS,
    )
}

/// Versión del protocolo del driver.
pub const IOCTL_VERSION: u32 = coroom_ioctl(0);
/// Crea el mando de una ranura.
pub const IOCTL_CREATE: u32 = coroom_ioctl(1);
/// Destruye el mando de una ranura.
pub const IOCTL_DESTROY: u32 = coroom_ioctl(2);
/// Manda un informe a una ranura.
pub const IOCTL_REPORT: u32 = coroom_ioctl(3);

/// Qué hacer si el driver no está.
pub const DRIVER_HELP: &str = "instala el driver de mando virtual de coroom (guía en \
    drivers/windows-vhid/README.md); mientras, `coroom host --gamepad-to-keys` convierte los \
    mandos en teclas";

/// Ranura del driver (0 a 7) de una ranura del cable (1 a 8).
pub fn driver_slot(slot: u8) -> Result<u8, InputError> {
    if (1..=DRIVER_SLOTS).contains(&slot) {
        Ok(slot - 1)
    } else {
        Err(InputError::NoSuchSlot(slot))
    }
}

/// Bit del botón en el campo de botones (botón HID `bit + 1`); la cruceta va al hat.
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

/// Estado de un mando con los valores del cable (sticks con positivo = derecha y arriba,
/// gatillos en `0..=i16::MAX`); [`PadReport::to_bytes`] lo convierte al informe del driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PadReport {
    buttons: u16,
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

    /// Informe de entrada del driver (`report_descriptor.inc`).
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
}

/// Carga de `IOCTL_COROOM_VHID_CREATE` y `IOCTL_COROOM_VHID_DESTROY`.
pub fn slot_payload(driver_slot: u8) -> [u8; SLOT_LEN] {
    [driver_slot]
}

/// Carga de `IOCTL_COROOM_VHID_REPORT`.
pub fn report_payload(driver_slot: u8, report: &[u8; REPORT_LEN]) -> [u8; REPORT_PACKET_LEN] {
    let mut out = [0u8; REPORT_PACKET_LEN];
    out[0] = driver_slot;
    out[1..].copy_from_slice(report);
    out
}

/// Respuesta de `IOCTL_COROOM_VHID_VERSION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionInfo {
    /// Versión del protocolo del driver.
    pub protocol: u32,
    /// Ranuras del driver.
    pub max_slots: u8,
    /// Bytes del informe.
    pub report_len: u8,
}

impl VersionInfo {
    /// Lee la respuesta (`COROOM_VHID_VERSION_INFO`, little endian y sin relleno).
    pub fn parse(bytes: &[u8]) -> Result<Self, InputError> {
        if bytes.len() < VERSION_INFO_LEN {
            return Err(InputError::GamepadUnavailable(format!(
                "el driver coroomvhid respondió a la consulta de versión con {} bytes en vez de \
                 {VERSION_INFO_LEN}",
                bytes.len()
            )));
        }
        Ok(Self {
            protocol: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            max_slots: bytes[4],
            report_len: bytes[5],
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
        if self.max_slots < DRIVER_SLOTS || usize::from(self.report_len) != REPORT_LEN {
            return Err(InputError::GamepadUnavailable(format!(
                "el driver coroomvhid dice tener {} ranuras e informes de {} bytes (se esperaban \
                 {DRIVER_SLOTS} y {REPORT_LEN}): reinstala el driver",
                self.max_slots, self.report_len
            )));
        }
        Ok(())
    }
}

/// Operación del driver, para los mensajes de error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverOp {
    /// `IOCTL_COROOM_VHID_VERSION`.
    Version,
    /// `IOCTL_COROOM_VHID_CREATE`.
    Create,
    /// `IOCTL_COROOM_VHID_DESTROY`.
    Destroy,
    /// `IOCTL_COROOM_VHID_REPORT`.
    Report,
}

/// Error en español de un IOCTL fallido, a partir del código de Win32 en que Windows convierte
/// el `NTSTATUS` del driver (`slot`: ranura del cable).
pub fn driver_error(
    op: DriverOp,
    slot: Option<u8>,
    win32: Option<u32>,
    detail: &str,
) -> InputError {
    let what = match (op, slot) {
        (DriverOp::Version, _) => "consultar la versión del driver".to_owned(),
        (DriverOp::Create, Some(slot)) => format!("crear el mando de la ranura {slot}"),
        (DriverOp::Destroy, Some(slot)) => format!("destruir el mando de la ranura {slot}"),
        (DriverOp::Report, Some(slot)) => format!("mandar el estado del mando de la ranura {slot}"),
        (_, None) => "hablar con el driver".to_owned(),
    };
    let why = match win32 {
        // STATUS_DEVICE_BUSY
        Some(170) => "la ranura la usa otro programa (¿otro coroom abierto como anfitrión?)",
        // STATUS_ACCESS_DENIED
        Some(5) => "la ranura es de otro programa",
        // STATUS_INVALID_DEVICE_STATE
        Some(22) => "el driver no tiene creado ese mando",
        // STATUS_INVALID_PARAMETER
        Some(87) => "el driver rechazó la ranura o el informe (fuera de rango)",
        // STATUS_INVALID_BUFFER_SIZE
        Some(1784) => "tamaño de datos inesperado: el driver y coroom no son de la misma versión",
        // STATUS_DEVICE_REMOVED
        Some(1617) => "el dispositivo raíz del driver se está desinstalando o desactivando",
        // STATUS_FILE_CLOSED
        Some(6) => "el handle del driver se está cerrando",
        // STATUS_INVALID_DEVICE_REQUEST
        Some(1) => "el driver no conoce esa orden: el driver y coroom no son de la misma versión",
        _ => "",
    };
    let why = if why.is_empty() {
        detail.to_owned()
    } else {
        format!("{why} ({detail})")
    };
    InputError::Backend(format!("mando virtual: no se pudo {what}: {why}"))
}

/// Error en español al abrir [`DEVICE_PATH`] (`win32`: el código de `CreateFileW`).
pub fn open_error(win32: Option<u32>, detail: &str) -> InputError {
    InputError::GamepadUnavailable(match win32 {
        // ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND
        Some(2 | 3) => format!(
            "el driver coroomvhid no está instalado o su dispositivo raíz está desactivado: \
             {DRIVER_HELP}"
        ),
        // ERROR_ACCESS_DENIED
        Some(5) => format!(
            "Windows no deja abrir {DEVICE_PATH}: solo pueden usar el mando virtual los \
             administradores y el usuario con la sesión iniciada en este equipo ({detail})"
        ),
        _ => format!("no se pudo abrir {DEVICE_PATH} ({detail}): {DRIVER_HELP}"),
    })
}

/// Lo que [`VhidGamepads`] necesita del driver (ranuras del driver, de 0 a 7). En Windows es
/// `DriverHandle`; los tests usan uno falso.
pub trait VhidDevice: Send + 'static {
    /// `IOCTL_COROOM_VHID_CREATE`.
    fn create(&mut self, driver_slot: u8) -> Result<(), InputError>;
    /// `IOCTL_COROOM_VHID_DESTROY`.
    fn destroy(&mut self, driver_slot: u8) -> Result<(), InputError>;
    /// `IOCTL_COROOM_VHID_REPORT`.
    fn report(&mut self, driver_slot: u8, report: &[u8; REPORT_LEN]) -> Result<(), InputError>;
}

/// Cuándo mandar los informes de un mando: en cada cambio, los ejes como mucho cada
/// [`REPORT_INTERVAL`] (lo que llega antes espera y se sustituye por lo más nuevo), los botones y
/// la cruceta al momento.
#[derive(Debug, Clone, Default)]
pub struct ReportPacer {
    sent: Option<[u8; REPORT_LEN]>,
    sent_at: Option<Instant>,
    pending: Option<[u8; REPORT_LEN]>,
}

/// Indica si dos informes difieren en botones o hat (cambios de estado, que no esperan).
fn state_changed(a: &[u8; REPORT_LEN], b: &[u8; REPORT_LEN]) -> bool {
    a[offset::BUTTONS..offset::BUTTONS + 2] != b[offset::BUTTONS..offset::BUTTONS + 2]
        || a[offset::HAT] != b[offset::HAT]
}

impl ReportPacer {
    /// Nuevo estado del mando a las `now`: devuelve el informe si hay que mandarlo ya (y hay que
    /// llamar a [`ReportPacer::sent`] si se mandó); si no, queda pendiente.
    pub fn offer(&mut self, report: [u8; REPORT_LEN], now: Instant) -> Option<[u8; REPORT_LEN]> {
        if self.sent == Some(report) {
            self.pending = None;
            return None;
        }
        let urgent = self.sent.is_none_or(|sent| state_changed(&sent, &report));
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
    pub fn sent(&mut self, report: [u8; REPORT_LEN], now: Instant) {
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
    pub fn take_due(&mut self, now: Instant) -> Option<[u8; REPORT_LEN]> {
        if self.deadline().is_some_and(|deadline| deadline <= now) {
            self.pending.take()
        } else {
            None
        }
    }
}

#[derive(Debug, Default)]
struct Pad {
    report: PadReport,
    pacer: ReportPacer,
    owner: Option<GuestId>,
}

struct State<D> {
    device: D,
    /// Por ranura del cable (1 a 8).
    pads: BTreeMap<u8, Pad>,
    stop: bool,
}

struct Shared<D> {
    state: Mutex<State<D>>,
    wake: Condvar,
}

fn lock<D>(shared: &Shared<D>) -> MutexGuard<'_, State<D>> {
    shared.state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl<D: VhidDevice> State<D> {
    /// Manda el estado de `slot` si toca (o lo deja pendiente).
    fn push(&mut self, slot: u8, now: Instant) -> Result<(), InputError> {
        let Some(pad) = self.pads.get_mut(&slot) else {
            return Err(InputError::NoSuchSlot(slot));
        };
        if let Some(report) = pad.pacer.offer(pad.report.to_bytes(), now) {
            self.device.report(driver_slot(slot)?, &report)?;
            pad.pacer.sent(report, now);
        }
        Ok(())
    }

    /// Manda lo pendiente que ya toca; devuelve cuándo vence lo siguiente.
    fn flush_due(&mut self, now: Instant) -> Option<Instant> {
        let State { device, pads, .. } = self;
        for (slot, pad) in pads.iter_mut() {
            if let Some(report) = pad.pacer.take_due(now) {
                match driver_slot(*slot).and_then(|index| device.report(index, &report)) {
                    Ok(()) => pad.pacer.sent(report, now),
                    Err(err) => tracing::warn!("mando virtual de la ranura {slot}: {err}"),
                }
            }
        }
        pads.values().filter_map(|pad| pad.pacer.deadline()).min()
    }
}

/// Mandos virtuales por el driver `coroomvhid.sys`: un mando HID por ranura, creado al asignarla y
/// destruido al liberarla.
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
    /// Mandos sobre `device`, con el hilo que manda los informes de ejes pendientes.
    pub fn with_device(device: D) -> Result<Self, InputError> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                device,
                pads: BTreeMap::new(),
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

    /// Crea el mando de `slot` (1 a 8) en reposo; si ya existe, no hace nada.
    pub fn assign(&mut self, slot: u8) -> Result<(), InputError> {
        let index = driver_slot(slot)?;
        let mut state = lock(&self.shared);
        if state.pads.contains_key(&slot) {
            return Ok(());
        }
        state.device.create(index)?;
        state.pads.insert(slot, Pad::default());
        // El primer informe deja el mando en un estado conocido (sticks al centro, hat nulo).
        let result = state.push(slot, Instant::now());
        if result.is_err() {
            state.pads.remove(&slot);
            let _ = state.device.destroy(index);
        }
        result
    }

    /// Destruye el mando de `slot`.
    pub fn release(&mut self, slot: u8) -> Result<(), InputError> {
        let index = driver_slot(slot)?;
        let mut state = lock(&self.shared);
        if state.pads.remove(&slot).is_none() {
            return Err(InputError::NoSuchSlot(slot));
        }
        state.device.destroy(index)
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

    /// Devuelve al reposo los mandos que usa `guest`.
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

    /// Ranuras con mando creado.
    pub fn assigned_slots(&self) -> Vec<u8> {
        lock(&self.shared).pads.keys().copied().collect()
    }
}

impl<D: VhidDevice> Drop for VhidGamepads<D> {
    fn drop(&mut self) {
        lock(&self.shared).stop = true;
        self.shared.wake.notify_one();
        if let Some(flusher) = self.flusher.take() {
            let _ = flusher.join();
        }
        // Al soltar el dispositivo se cierra el handle y el driver destruye los mandos.
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

/// Acceso al driver real (solo Windows). El `unsafe` es el de `CreateFileW`, `DeviceIoControl` y
/// `CloseHandle` (`// SAFETY:` en cada uno).
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod driver {
    use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::core::PCWSTR;

    use super::{
        DEVICE_PATH, DriverOp, IOCTL_CREATE, IOCTL_DESTROY, IOCTL_REPORT, IOCTL_VERSION,
        REPORT_LEN, VERSION_INFO_LEN, VersionInfo, VhidDevice, VhidGamepads, driver_error,
        open_error, report_payload, slot_payload,
    };
    use crate::InputError;

    /// Código de Win32 de un error de `windows` (`HRESULT_FROM_WIN32`).
    fn win32_code(err: &windows::core::Error) -> Option<u32> {
        let code = err.code().0 as u32;
        (code & 0xFFFF_0000 == 0x8007_0000).then_some(code & 0xFFFF)
    }

    /// Handle abierto de `\\.\coroomvhid`.
    #[derive(Debug)]
    pub struct DriverHandle {
        handle: HANDLE,
    }

    // SAFETY: un HANDLE de fichero de Windows se puede usar desde cualquier hilo; aquí solo lo
    // usa quien tenga el `Mutex` de `VhidGamepads`, y se cierra una vez en `Drop`.
    unsafe impl Send for DriverHandle {}

    impl DriverHandle {
        /// Abre el driver y comprueba la versión de su protocolo.
        pub fn open() -> Result<(Self, VersionInfo), InputError> {
            let path: Vec<u16> = DEVICE_PATH.encode_utf16().chain(Some(0)).collect();
            // SAFETY: `path` acaba en NUL y vive durante la llamada; los demás argumentos son
            // valores o `None` (sin atributos de seguridad ni plantilla).
            let handle = unsafe {
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
            .map_err(|err| open_error(win32_code(&err), &err.message()))?;
            let device = Self { handle };
            let mut out = [0u8; VERSION_INFO_LEN];
            let returned = device.ioctl(IOCTL_VERSION, &[], &mut out).map_err(|err| {
                driver_error(DriverOp::Version, None, win32_code(&err), &err.message())
            })?;
            let info = VersionInfo::parse(&out[..returned.min(out.len())])?;
            info.check()?;
            Ok((device, info))
        }

        /// `DeviceIoControl` síncrono; devuelve los bytes escritos en `output`.
        fn ioctl(
            &self,
            code: u32,
            input: &[u8],
            output: &mut [u8],
        ) -> Result<usize, windows::core::Error> {
            let mut returned = 0u32;
            // SAFETY: el handle está abierto (se cierra solo en `Drop`) y sin `OVERLAPPED`, así
            // que la llamada es síncrona; los punteros y longitudes son los de `input` y `output`,
            // que viven durante la llamada, y `returned` es un `u32` local.
            unsafe {
                DeviceIoControl(
                    self.handle,
                    code,
                    (!input.is_empty()).then(|| input.as_ptr().cast()),
                    input.len() as u32,
                    (!output.is_empty()).then(|| output.as_mut_ptr().cast()),
                    output.len() as u32,
                    Some(&mut returned),
                    None,
                )
            }?;
            Ok(returned as usize)
        }

        fn command(
            &self,
            op: DriverOp,
            code: u32,
            driver_slot: u8,
            input: &[u8],
        ) -> Result<(), InputError> {
            self.ioctl(code, input, &mut []).map(|_| ()).map_err(|err| {
                driver_error(op, Some(driver_slot + 1), win32_code(&err), &err.message())
            })
        }
    }

    impl Drop for DriverHandle {
        fn drop(&mut self) {
            // SAFETY: el handle lo abrió `open` y solo se cierra aquí, una vez.
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }

    impl VhidDevice for DriverHandle {
        fn create(&mut self, driver_slot: u8) -> Result<(), InputError> {
            self.command(
                DriverOp::Create,
                IOCTL_CREATE,
                driver_slot,
                &slot_payload(driver_slot),
            )
        }

        fn destroy(&mut self, driver_slot: u8) -> Result<(), InputError> {
            self.command(
                DriverOp::Destroy,
                IOCTL_DESTROY,
                driver_slot,
                &slot_payload(driver_slot),
            )
        }

        fn report(&mut self, driver_slot: u8, report: &[u8; REPORT_LEN]) -> Result<(), InputError> {
            self.command(
                DriverOp::Report,
                IOCTL_REPORT,
                driver_slot,
                &report_payload(driver_slot, report),
            )
        }
    }

    /// Abre el driver para [`VhidGamepads`].
    pub fn open() -> Result<VhidGamepads<DriverHandle>, InputError> {
        let (device, info) = DriverHandle::open()?;
        tracing::info!(
            "mando virtual: driver coroomvhid, protocolo {}, {} ranuras",
            info.protocol,
            info.max_slots
        );
        VhidGamepads::with_device(device)
    }

    /// Estado del driver para `coroom doctor`.
    pub fn status() -> String {
        match DriverHandle::open() {
            Ok((_, info)) => format!(
                "mandos: driver coroomvhid (protocolo {}, hasta {} mandos HID genéricos)",
                info.protocol, info.max_slots
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

    const ONE: GuestId = GuestId(1);
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

    /// Los bytes de `report_descriptor.inc` (solo `0x..`, comas y comentarios).
    fn descriptor() -> Vec<u8> {
        strip_c_comments(DESCRIPTOR_SOURCE)
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

    /// Un campo de entrada del descriptor (un elemento de `Input` por `Report Count`).
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
        fields: Vec<Field>,
        /// Uso de la colección de aplicación de más fuera (página, uso).
        application: Option<(u16, u16)>,
        max_depth: usize,
        output_or_feature: usize,
    }

    /// Analizador de descriptores HID (elementos cortos, HID 1.11 §6.2.2), lo justo para este.
    fn parse(descriptor: &[u8]) -> Parsed {
        let mut parsed = Parsed::default();
        let (mut usage_page, mut logical_min, mut logical_max) = (0u16, 0i32, 0i32);
        let (mut report_size, mut report_count, mut report_id) = (0usize, 0usize, 0u8);
        let mut usages: Vec<u16> = Vec::new();
        let (mut usage_min, mut usage_max) = (None::<u16>, None::<u16>);
        let mut bits: BTreeMap<u8, usize> = BTreeMap::new();
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
                (0, 0x8) => {
                    let id_bits = if report_id == 0 { 0 } else { 8 };
                    let cursor = bits.entry(report_id).or_insert(id_bits);
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
                        parsed.fields.push(Field {
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
                (0, 0x9 | 0xB) => parsed.output_or_feature += 1,
                (0, 0xA) => {
                    if depth == 0 {
                        assert_eq!(unsigned, 1, "la colección de fuera es de aplicación");
                        parsed.application = Some((usage_page, usages[0]));
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
            parsed.application,
            Some((GENERIC_DESKTOP, 0x05)),
            "Game Pad"
        );
        assert_eq!(parsed.max_depth, 1);
        assert_eq!(
            parsed.output_or_feature, 0,
            "sin informes de salida (vibración)"
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

    #[test]
    fn the_constants_match_the_driver_header() {
        let defines = header_defines();
        let number = |name: &str| header_number(&defines, name);
        assert_eq!(number("COROOM_VHID_PROTOCOL_VERSION"), PROTOCOL_VERSION);
        assert_eq!(number("COROOM_VHID_MAX_SLOTS"), u32::from(DRIVER_SLOTS));
        assert_eq!(number("COROOM_VHID_REPORT_ID"), u32::from(REPORT_ID));
        assert_eq!(number("COROOM_VHID_REPORT_SIZE") as usize, REPORT_LEN);
        assert_eq!(
            number("COROOM_VHID_VERSION_INFO_SIZE") as usize,
            VERSION_INFO_LEN
        );
        assert_eq!(number("COROOM_VHID_SLOT_SIZE") as usize, SLOT_LEN);
        assert_eq!(
            number("COROOM_VHID_REPORT_PACKET_SIZE") as usize,
            REPORT_PACKET_LEN
        );
        assert_eq!(number("COROOM_VHID_HAT_NULL"), u32::from(HAT_NULL));
        assert_eq!(number("COROOM_VHID_AXIS_MAX"), AXIS_MAX as u32);
        for (name, at) in [
            ("BUTTONS", offset::BUTTONS),
            ("X", offset::X),
            ("Y", offset::Y),
            ("RX", offset::RX),
            ("RY", offset::RY),
            ("Z", offset::Z),
            ("RZ", offset::RZ),
            ("HAT", offset::HAT),
        ] {
            assert_eq!(
                number(&format!("COROOM_VHID_REPORT_OFFSET_{name}")) as usize,
                at,
                "{name}"
            );
        }
        assert_eq!(
            header_string(&defines, "COROOM_VHID_USER_PATH"),
            DEVICE_PATH
        );
        assert_eq!(
            header_string(&defines, "COROOM_VHID_SDDL"),
            "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;IU)",
            "solo SYSTEM, administradores y el usuario interactivo"
        );
        assert_eq!(
            defines["COROOM_VHID_IOCTL(n)"],
            "CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800 + (n), METHOD_BUFFERED, FILE_ANY_ACCESS)"
        );
        for (name, code) in [
            ("IOCTL_COROOM_VHID_VERSION", IOCTL_VERSION),
            ("IOCTL_COROOM_VHID_CREATE", IOCTL_CREATE),
            ("IOCTL_COROOM_VHID_DESTROY", IOCTL_DESTROY),
            ("IOCTL_COROOM_VHID_REPORT", IOCTL_REPORT),
        ] {
            let n: u32 = defines[name]
                .strip_prefix("COROOM_VHID_IOCTL(")
                .and_then(|rest| rest.strip_suffix(')'))
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(coroom_ioctl(n), code, "{name}");
        }
        assert_eq!(
            [IOCTL_VERSION, IOCTL_CREATE, IOCTL_DESTROY, IOCTL_REPORT],
            [0x0022_2000, 0x0022_2004, 0x0022_2008, 0x0022_200C],
            "CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800 + n, METHOD_BUFFERED, FILE_ANY_ACCESS)"
        );
        assert!(INF_SOURCE.contains("Root\\coroomvhid"));
        assert!(INF_SOURCE.contains("HKR,,\"LowerFilters\",0x00010000,\"vhf\""));
    }

    #[test]
    fn payloads_and_versions() {
        assert_eq!(slot_payload(7), [7]);
        let report = PadReport::neutral().to_bytes();
        let payload = report_payload(3, &report);
        assert_eq!(payload[0], 3);
        assert_eq!(payload[1..], report);

        assert_eq!(driver_slot(1), Ok(0));
        assert_eq!(driver_slot(8), Ok(7));
        assert_eq!(driver_slot(0), Err(InputError::NoSuchSlot(0)));
        assert_eq!(driver_slot(9), Err(InputError::NoSuchSlot(9)));

        let info = VersionInfo::parse(&[1, 0, 0, 0, 8, 16, 0, 0, 0xAA]).unwrap();
        assert_eq!(
            info,
            VersionInfo {
                protocol: 1,
                max_slots: 8,
                report_len: 16
            }
        );
        assert_eq!(info.check(), Ok(()));
        assert!(VersionInfo::parse(&[1, 0, 0]).is_err());
        let newer = VersionInfo::parse(&[2, 0, 0, 0, 8, 16, 0, 0]).unwrap();
        assert!(
            matches!(newer.check(), Err(InputError::GamepadUnavailable(text)) if text.contains("versión 2"))
        );
        let small = VersionInfo::parse(&[1, 0, 0, 0, 4, 16, 0, 0]).unwrap();
        assert!(small.check().is_err());

        let InputError::GamepadUnavailable(text) = open_error(Some(2), "x") else {
            panic!()
        };
        assert!(text.contains("drivers/windows-vhid/README.md"), "{text}");
        let InputError::Backend(text) = driver_error(DriverOp::Create, Some(2), Some(170), "x")
        else {
            panic!()
        };
        assert!(
            text.contains("ranura 2") && text.contains("otro programa"),
            "{text}"
        );
    }

    #[test]
    fn the_pacer_caps_axes_at_250_hz_but_never_delays_buttons() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut pacer = ReportPacer::default();
        let neutral = PadReport::neutral();
        let mut pad = neutral;

        // El primero sale siempre.
        let first = pacer.offer(pad.to_bytes(), t0).expect("primero");
        pacer.sent(first, t0);
        assert_eq!(
            pacer.offer(pad.to_bytes(), ms(1)),
            None,
            "sin cambios no se manda"
        );
        assert_eq!(pacer.deadline(), None);

        // Ejes antes de 4 ms: esperan, y el más nuevo sustituye al anterior.
        pad.set_axis(GamepadAxis::LeftX, 100);
        assert_eq!(pacer.offer(pad.to_bytes(), ms(1)), None);
        pad.set_axis(GamepadAxis::LeftX, 200);
        assert_eq!(pacer.offer(pad.to_bytes(), ms(2)), None);
        assert_eq!(pacer.deadline(), Some(ms(4)));
        assert_eq!(pacer.take_due(ms(3)), None);
        let due = pacer.take_due(ms(4)).expect("vence a los 4 ms");
        assert_eq!(due, pad.to_bytes(), "el último valor");
        pacer.sent(due, ms(4));
        assert_eq!(pacer.deadline(), None);

        // Un botón sale al momento aunque no hayan pasado 4 ms, con los ejes al día.
        pad.set_axis(GamepadAxis::LeftX, 300);
        assert_eq!(pacer.offer(pad.to_bytes(), ms(5)), None);
        pad.set_button(GamepadButton::A, true);
        let now = pacer.offer(pad.to_bytes(), ms(5)).expect("botón");
        assert_eq!(now, pad.to_bytes());
        assert_eq!(pacer.deadline(), None, "lo pendiente iba incluido");
        pacer.sent(now, ms(5));
        pad.set_button(GamepadButton::A, false);
        assert!(
            pacer.offer(pad.to_bytes(), ms(5)).is_some(),
            "soltar tampoco espera"
        );
        pacer.sent(pad.to_bytes(), ms(5));

        // Pasados 4 ms, un eje sale al momento.
        pad.set_axis(GamepadAxis::RightTrigger, 5);
        assert!(pacer.offer(pad.to_bytes(), ms(9)).is_some());

        // Si vuelve al valor ya enviado, lo pendiente se olvida.
        let mut pacer = ReportPacer::default();
        pacer.sent(neutral.to_bytes(), t0);
        let mut moved = neutral;
        moved.set_axis(GamepadAxis::LeftY, 9);
        assert_eq!(pacer.offer(moved.to_bytes(), ms(1)), None);
        assert_eq!(pacer.offer(neutral.to_bytes(), ms(2)), None);
        assert_eq!(pacer.deadline(), None);
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Create(u8),
        Destroy(u8),
        Report(u8, [u8; REPORT_LEN]),
    }

    #[derive(Debug, Clone, Default)]
    struct FakeDevice {
        calls: Arc<Mutex<Vec<Call>>>,
    }

    impl FakeDevice {
        fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }

        fn wait_for(&self, condition: impl Fn(&[Call]) -> bool) -> bool {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if condition(&self.calls.lock().unwrap()) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            false
        }
    }

    impl VhidDevice for FakeDevice {
        fn create(&mut self, driver_slot: u8) -> Result<(), InputError> {
            self.calls.lock().unwrap().push(Call::Create(driver_slot));
            Ok(())
        }

        fn destroy(&mut self, driver_slot: u8) -> Result<(), InputError> {
            self.calls.lock().unwrap().push(Call::Destroy(driver_slot));
            Ok(())
        }

        fn report(&mut self, driver_slot: u8, report: &[u8; REPORT_LEN]) -> Result<(), InputError> {
            assert!(driver_slot < DRIVER_SLOTS);
            self.calls
                .lock()
                .unwrap()
                .push(Call::Report(driver_slot, *report));
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

    #[test]
    fn gamepads_are_created_fed_and_destroyed_per_slot() {
        let device = FakeDevice::default();
        let mut pads = VhidGamepads::with_device(device.clone()).unwrap();
        let neutral = PadReport::neutral().to_bytes();

        pads.assign(2).unwrap();
        pads.assign(2).unwrap();
        assert_eq!(pads.assign(0), Err(InputError::NoSuchSlot(0)));
        assert_eq!(pads.assign(9), Err(InputError::NoSuchSlot(9)));
        assert_eq!(
            device.calls(),
            vec![Call::Create(1), Call::Report(1, neutral)],
            "la ranura 2 del cable es la 1 del driver; se crea una vez y en reposo"
        );
        assert_eq!(pads.assigned_slots(), vec![2]);

        pads.inject(&button(2, GamepadButton::A, true), ONE)
            .unwrap();
        let mut expected = PadReport::neutral();
        expected.set_button(GamepadButton::A, true);
        assert_eq!(device.calls()[2], Call::Report(1, expected.to_bytes()));
        assert_eq!(
            pads.inject(&button(3, GamepadButton::A, true), ONE),
            Err(InputError::NoSuchSlot(3))
        );
        pads.inject(&InputEvent::MouseMove { dx: 1, dy: 1 }, ONE)
            .unwrap();
        pads.inject(
            &InputEvent::GamepadConnected {
                slot: 2,
                kind: crate::GamepadKind::Xbox,
            },
            ONE,
        )
        .unwrap();
        assert_eq!(
            device.calls().len(),
            3,
            "ni el ratón ni la conexión mandan informes"
        );

        // Una ráfaga de ejes: el último valor llega, sin pasar de 250 Hz.
        let start = Instant::now();
        for value in 1..=50 {
            pads.inject(&axis(2, GamepadAxis::LeftX, value * 100), ONE)
                .unwrap();
        }
        expected.set_axis(GamepadAxis::LeftX, 5000);
        let last = Call::Report(1, expected.to_bytes());
        assert!(device.wait_for(|calls| calls.last() == Some(&last)));
        let reports = device.calls().len() - 3;
        let allowed = start.elapsed().as_millis() / 4 + 2;
        assert!(
            reports as u128 <= allowed,
            "{reports} informes en {:?}",
            start.elapsed()
        );

        // Desconectar el mando del invitado lo deja en reposo.
        pads.inject(&InputEvent::GamepadDisconnected { slot: 2 }, ONE)
            .unwrap();
        assert_eq!(device.calls().last(), Some(&Call::Report(1, neutral)));

        pads.release(2).unwrap();
        assert_eq!(device.calls().last(), Some(&Call::Destroy(1)));
        assert_eq!(pads.release(2), Err(InputError::NoSuchSlot(2)));
        assert!(pads.assigned_slots().is_empty());
    }

    #[test]
    fn release_guest_rests_only_that_guests_pads() {
        let device = FakeDevice::default();
        let mut pads = VhidGamepads::with_device(device.clone()).unwrap();
        pads.assign(1).unwrap();
        pads.assign(4).unwrap();
        pads.inject(&button(1, GamepadButton::B, true), ONE)
            .unwrap();
        pads.inject(&button(4, GamepadButton::X, true), TWO)
            .unwrap();
        let before = device.calls().len();
        pads.release_guest(ONE).unwrap();
        let calls = device.calls();
        assert_eq!(
            calls[before..],
            [Call::Report(0, PadReport::neutral().to_bytes())],
            "solo la ranura 1 (la 0 del driver) vuelve al reposo"
        );
        drop(pads);
    }
}
