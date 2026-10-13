//! Mandos del invitado con `gilrs` en un hilo a 250 Hz.
//!
//! Un solo mando va a la ranura que el anfitrión haya asignado a este invitado
//! ([`GamepadCapture::set_slot`]): el que elija el invitado ([`PadChoice::Pad`]) o, en automático,
//! el último conectado. Los mandos se pueden conectar y desconectar en cualquier momento: si el
//! elegido se desconecta, la ranura se queda sin mando hasta que vuelva (o, en automático, pasa
//! al anterior que siga conectado); la lista para la interfaz está en [`GamepadCapture::pads`]. Conexión y
//! desconexión, y cambio de ranura, se anuncian con `GamepadConnected`/`GamepadDisconnected`.
//!
//! **Convertido**: botones por evento (al momento), ejes a 250 Hz y solo si cambian más de
//! [`AXIS_EPSILON`], con una zona muerta mínima ([`AXIS_DEADZONE`]) además de los filtros de
//! `gilrs`, en el vocabulario de Xbox. Los mandos que la base de SDL no trae para Linux se
//! añaden en [`MAPPINGS`].
//!
//! **Tal cual**: al conectar el mando a la ranura se declara (`PadData::Device`): si es HID y se
//! puede leer en crudo (`hidraw` en Linux, [`crate::hidraw`]; `hidapi` en Windows,
//! [`crate::hidwin`]), con su descriptor y sus informes de característica; si es un Xbox por cable
//! (`xpad` o `xusb22`), como XInput; si no, convertido y por qué.
//! Si el anfitrión lo acepta (`GamepadMode`, [`PadLink::raw`]), el hilo `mando-crudo` manda cada
//! informe tal cual (y el último otra vez cada [`RESEND`] si no hay nuevos, por si se perdió el
//! datagrama que soltaba un botón) y deja de salir lo convertido; los informes de salida del
//! anfitrión (vibración, luces) se escriben en el mando. Sin ranura no se envía nada.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use coroom_proto::input::{GamepadDevice, GamepadTransport, PadData};
use gilrs::{Axis, Button, EventType, GamepadId, Gilrs, GilrsBuilder};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::{GamepadAxis, GamepadButton, GamepadKind, InputError, InputEvent};

/// Correspondencias de SDL que la base de `gilrs` no trae para Linux.
///
/// - 8BitDo Ultimate 2 en modo PC (`2dc8:6012`, `hid-generic`): la de Windows de la base de SDL
///   con los botones y ejes en el orden de evdev (el stick derecho en `ABS_Z`/`ABS_RZ`, los
///   gatillos en `ABS_GAS`/`ABS_BRAKE`), según `evtest`.
pub const MAPPINGS: &str = "\
03000000c82d00001260000011010000,8BitDo Ultimate 2,a:b0,b:b1,x:b3,y:b4,back:b10,start:b11,guide:b12,leftshoulder:b6,rightshoulder:b7,leftstick:b13,rightstick:b14,leftx:a0,lefty:a1,rightx:a2,righty:a3,lefttrigger:a5,righttrigger:a4,dpup:h0.1,dpdown:h0.4,dpleft:h0.8,dpright:h0.2,platform:Linux,
";

/// Cada cuánto se repite el último informe tal cual si no llega otro.
pub const RESEND: Duration = Duration::from_millis(100);

/// Lo que la captura de mandos necesita de la sesión.
pub trait PadLink: Send + Sync {
    /// Envía un evento convertido.
    fn event(&self, event: InputEvent);
    /// Envía datos del mando tal cual.
    fn pad(&self, data: PadData);
    /// Si el anfitrión tiene el mando de `slot` tal cual (`GamepadMode`).
    fn raw(&self, slot: u8) -> bool;
    /// Informes de salida del anfitrión desde la última llamada: ranura y bytes.
    fn outputs(&self) -> Vec<(u8, Vec<u8>)>;
}

/// Un mando físico por lo que se le reconoce: VID, PID y nombre.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PadKey {
    /// Fabricante.
    pub vendor: u16,
    /// Producto.
    pub product: u16,
    /// Nombre.
    pub name: String,
}

/// Qué mando mandar.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PadChoice {
    /// El último conectado (y si se desconecta, el anterior que siga conectado).
    #[default]
    Auto,
    /// Ese mando; si no está conectado, ninguno.
    Pad(PadKey),
}

/// Cómo podría ir tal cual un mando conectado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PadKind {
    /// HID, legible en crudo.
    Hid,
    /// Xbox por cable (XInput).
    XInput,
    /// Solo convertido, y por qué.
    Converted(String),
}

/// Un mando conectado al equipo del invitado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedPad {
    /// Cuál es.
    pub key: PadKey,
    /// Cómo podría ir.
    pub kind: PadKind,
    /// Si es el que va a la ranura.
    pub active: bool,
}

/// Periodo del hilo de mandos (250 Hz).
pub const PERIOD: Duration = Duration::from_millis(4);

/// Zona muerta mínima de los sticks (fracción del recorrido).
pub const AXIS_DEADZONE: f32 = 0.02;

/// Cambio mínimo de un eje para enviarlo (en unidades de `i16`).
pub const AXIS_EPSILON: i32 = 64;

/// Botón de `gilrs` en el cable (los gatillos van como ejes).
pub fn button(button: Button) -> Option<GamepadButton> {
    Some(match button {
        Button::South => GamepadButton::A,
        Button::East => GamepadButton::B,
        Button::West => GamepadButton::X,
        Button::North => GamepadButton::Y,
        Button::LeftTrigger => GamepadButton::LeftBumper,
        Button::RightTrigger => GamepadButton::RightBumper,
        Button::Select => GamepadButton::Back,
        Button::Start => GamepadButton::Start,
        Button::Mode => GamepadButton::Guide,
        Button::LeftThumb => GamepadButton::LeftStick,
        Button::RightThumb => GamepadButton::RightStick,
        Button::DPadUp => GamepadButton::DPadUp,
        Button::DPadDown => GamepadButton::DPadDown,
        Button::DPadLeft => GamepadButton::DPadLeft,
        Button::DPadRight => GamepadButton::DPadRight,
        _ => return None,
    })
}

/// Eje de `gilrs` en el cable.
pub fn axis(axis: Axis) -> Option<GamepadAxis> {
    Some(match axis {
        Axis::LeftStickX => GamepadAxis::LeftX,
        Axis::LeftStickY => GamepadAxis::LeftY,
        Axis::RightStickX => GamepadAxis::RightX,
        Axis::RightStickY => GamepadAxis::RightY,
        _ => return None,
    })
}

/// Valor `i16` de un stick (`-1..=1`) con la zona muerta.
pub fn stick_value(value: f32) -> i16 {
    if !value.is_finite() || value.abs() < AXIS_DEADZONE {
        return 0;
    }
    (value.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16
}

/// Valor `i16` de un gatillo (`0..=1`).
pub fn trigger_value(value: f32) -> i16 {
    if !value.is_finite() || value < AXIS_DEADZONE {
        return 0;
    }
    (value.min(1.0) * f32::from(i16::MAX)).round() as i16
}

/// Ejes pendientes y enviados: solo salen los que cambian.
#[derive(Debug, Default)]
pub struct AxisFilter {
    pending: BTreeMap<GamepadAxis, i16>,
    sent: BTreeMap<GamepadAxis, i16>,
}

impl AxisFilter {
    /// Anota el último valor de un eje.
    pub fn set(&mut self, axis: GamepadAxis, value: i16) {
        self.pending.insert(axis, value);
    }

    /// Ejes que cambiaron desde el último envío (el reposo, 0, siempre sale si no se había
    /// enviado).
    pub fn take_changed(&mut self) -> Vec<(GamepadAxis, i16)> {
        let mut out = Vec::new();
        for (axis, value) in std::mem::take(&mut self.pending) {
            let last = self.sent.get(&axis).copied().unwrap_or(0);
            let moved = (i32::from(value) - i32::from(last)).abs() >= AXIS_EPSILON;
            if moved || (value == 0 && last != 0) {
                self.sent.insert(axis, value);
                out.push((axis, value));
            }
        }
        out
    }

    /// Olvida lo enviado (el mando cambia de ranura o se desconecta).
    pub fn reset(&mut self) {
        self.pending.clear();
        self.sent.clear();
    }
}

/// Hilo de mandos.
pub struct GamepadCapture {
    stop: Arc<AtomicBool>,
    slot: Arc<AtomicU8>,
    choice: Arc<Mutex<PadChoice>>,
    pads: Arc<Mutex<Vec<ConnectedPad>>>,
    thread: Option<JoinHandle<()>>,
}

impl GamepadCapture {
    /// Arranca el hilo; los eventos y los datos tal cual salen por `link` (desde ese hilo y desde
    /// el de cada mando en crudo).
    pub fn spawn(link: Arc<dyn PadLink>, choice: PadChoice) -> Result<Self, InputError> {
        let stop = Arc::new(AtomicBool::new(false));
        let slot = Arc::new(AtomicU8::new(0));
        let choice = Arc::new(Mutex::new(choice));
        let pads = Arc::new(Mutex::new(Vec::new()));
        let thread = {
            let shared = Shared {
                stop: Arc::clone(&stop),
                slot: Arc::clone(&slot),
                choice: Arc::clone(&choice),
                pads: Arc::clone(&pads),
                link,
            };
            thread::Builder::new()
                .name("mandos".into())
                .spawn(move || run(&shared))
                .map_err(|err| InputError::Backend(format!("hilo de mandos: {err}")))?
        };
        Ok(Self {
            stop,
            slot,
            choice,
            pads,
            thread: Some(thread),
        })
    }

    /// Ranura asignada por el anfitrión (`None`: sin mando).
    pub fn set_slot(&self, slot: Option<u8>) {
        self.slot.store(slot.unwrap_or(0), Ordering::Release);
    }

    /// Qué mando mandar.
    pub fn set_choice(&self, choice: PadChoice) {
        *self.choice.lock().unwrap_or_else(PoisonError::into_inner) = choice;
    }

    /// Mandos conectados ahora, para la interfaz.
    pub fn pads(&self) -> Vec<ConnectedPad> {
        self.pads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for GamepadCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Shared {
    stop: Arc<AtomicBool>,
    slot: Arc<AtomicU8>,
    choice: Arc<Mutex<PadChoice>>,
    pads: Arc<Mutex<Vec<ConnectedPad>>>,
    link: Arc<dyn PadLink>,
}

fn key_of(gilrs: &Gilrs, id: GamepadId) -> PadKey {
    let pad = gilrs.gamepad(id);
    PadKey {
        vendor: pad.vendor_id().unwrap_or(0),
        product: pad.product_id().unwrap_or(0),
        name: pad.name().to_owned(),
    }
}

/// Índice en `connected` (en orden de conexión) del mando que va a la ranura según `choice`.
fn pick_index(choice: &PadChoice, connected: &[PadKey]) -> Option<usize> {
    match choice {
        PadChoice::Auto => connected.len().checked_sub(1),
        PadChoice::Pad(key) => connected.iter().position(|k| k == key),
    }
}

/// El mando que va a la ranura según `choice`.
fn pick(choice: &PadChoice, connected: &[(GamepadId, PadKey)]) -> Option<GamepadId> {
    let keys: Vec<PadKey> = connected.iter().map(|(_, key)| key.clone()).collect();
    pick_index(choice, &keys).map(|index| connected[index].0)
}

/// El mando en crudo de cada sistema, con la misma forma.
#[cfg(all(target_os = "linux", feature = "linux-portal"))]
mod platform {
    use std::path::Path;

    use crate::InputError;
    use crate::hidraw;
    pub use crate::hidraw::{HIDRAW_HELP as HELP, HidrawInfo as Info};

    /// Por qué no se puede abrir, para la lista de mandos.
    pub const NO_ACCESS: &str = "sin permiso para leerlo en crudo (reglas udev)";

    pub fn find(vendor: u16, product: u16) -> Option<Info> {
        hidraw::find(
            Path::new(hidraw::SYS_HIDRAW),
            Path::new("/dev"),
            vendor,
            product,
            &[],
        )
    }

    pub fn is_xinput(vendor: u16, product: u16) -> bool {
        hidraw::is_xpad(Path::new(hidraw::SYS_INPUT), vendor, product)
    }

    /// El nodo `hidraw` abierto.
    pub type Device = hidraw::HidrawDevice;

    pub fn open(info: &Info) -> Result<Device, InputError> {
        Device::open(&info.node)
    }
}

/// El mando en crudo de cada sistema, con la misma forma.
#[cfg(target_os = "windows")]
mod platform {
    use crate::InputError;
    use crate::hidwin;
    pub use crate::hidwin::{HIDWIN_HELP as HELP, HidwinInfo as Info, find, is_xinput};

    /// Por qué no se puede abrir, para la lista de mandos.
    pub const NO_ACCESS: &str = "no se puede abrir en crudo (¿Steam lo tiene en exclusiva?)";

    /// La colección HID abierta.
    pub type Device = hidwin::HidwinDevice;

    pub fn open(info: &Info) -> Result<Device, InputError> {
        Device::open(info)
    }
}

/// El mando de la ranura mandado tal cual en crudo (hilo `mando-crudo`).
struct RawPad {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    #[cfg(any(
        all(target_os = "linux", feature = "linux-portal"),
        target_os = "windows"
    ))]
    writer: Option<platform::Device>,
    /// Si es HID (lo convertido se calla cuando el anfitrión lo tiene tal cual).
    hid: bool,
}

impl Drop for RawPad {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl RawPad {
    fn write(&mut self, data: &[u8]) {
        #[cfg(any(
            all(target_os = "linux", feature = "linux-portal"),
            target_os = "windows"
        ))]
        if let Some(writer) = &mut self.writer
            && let Err(err) = writer.write_output(data)
        {
            debug!(error = %err, "informe de salida al mando");
        }
        #[cfg(not(any(
            all(target_os = "linux", feature = "linux-portal"),
            target_os = "windows"
        )))]
        let _ = data;
    }
}

/// Declara el mando de la ranura y, si es HID legible, arranca su hilo en crudo.
fn declare(link: &Arc<dyn PadLink>, slot: u8, key: &PadKey) -> RawPad {
    let device = |transport| PadData::Device {
        slot,
        device: GamepadDevice {
            name: key.name.clone(),
            vendor: key.vendor,
            product: key.product,
            version: 0,
            bus: 3,
            transport,
        },
    };
    let idle = |hid| RawPad {
        stop: Arc::new(AtomicBool::new(false)),
        thread: None,
        #[cfg(any(
            all(target_os = "linux", feature = "linux-portal"),
            target_os = "windows"
        ))]
        writer: None,
        hid,
    };
    #[cfg(any(
        all(target_os = "linux", feature = "linux-portal"),
        target_os = "windows"
    ))]
    {
        let Some(info) = platform::find(key.vendor, key.product) else {
            let transport = if platform::is_xinput(key.vendor, key.product) {
                GamepadTransport::XInput
            } else {
                GamepadTransport::Converted {
                    reason: "el mando no tiene interfaz HID en crudo en el equipo del invitado"
                        .into(),
                }
            };
            link.pad(device(transport));
            return idle(false);
        };
        let opened = platform::open(&info).and_then(|reader| {
            let writer = reader.try_clone()?;
            Ok((reader, writer))
        });
        let (reader, writer) = match opened {
            Ok(pair) => pair,
            Err(err) => {
                warn!(error = %err, "{}", platform::HELP);
                link.pad(device(GamepadTransport::Converted {
                    reason: "el invitado no tiene permiso para leer el mando en crudo".into(),
                }));
                return idle(false);
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (stop, link) = (Arc::clone(&stop), Arc::clone(link));
            thread::Builder::new()
                .name("mando-crudo".into())
                .spawn(move || raw_loop(reader, info, slot, &link, &stop))
                .ok()
        };
        RawPad {
            stop,
            thread,
            writer: Some(writer),
            hid: true,
        }
    }
    #[cfg(not(any(
        all(target_os = "linux", feature = "linux-portal"),
        target_os = "windows"
    )))]
    {
        link.pad(device(GamepadTransport::Converted {
            reason: "el invitado todavía no manda mandos tal cual desde este sistema".into(),
        }));
        idle(false)
    }
}

/// Hilo `mando-crudo`: lee los informes de característica, declara el mando y pasa cada informe
/// de entrada tal cual mientras el anfitrión lo tenga así.
#[cfg(any(
    all(target_os = "linux", feature = "linux-portal"),
    target_os = "windows"
))]
fn raw_loop(
    mut reader: platform::Device,
    info: platform::Info,
    slot: u8,
    link: &Arc<dyn PadLink>,
    stop: &AtomicBool,
) {
    let features = reader.read_features(&info.summary);
    link.pad(PadData::Device {
        slot,
        device: GamepadDevice {
            name: info.name.clone(),
            vendor: info.vendor,
            product: info.product,
            version: info.version,
            bus: info.bus,
            transport: GamepadTransport::Hid {
                descriptor: info.descriptor.clone(),
                features,
            },
        },
    });
    let mut buffer = vec![0u8; coroom_proto::hid::MAX_REPORT_BYTES];
    let mut last: Option<Vec<u8>> = None;
    let mut since = std::time::Instant::now();
    while !stop.load(Ordering::Acquire) {
        let timeout = RESEND.as_millis() as u32;
        match reader.read_report(&mut buffer, timeout) {
            Ok(Some(len)) => {
                let data = buffer[..len].to_vec();
                if link.raw(slot) {
                    link.pad(PadData::Report {
                        slot,
                        data: data.clone(),
                    });
                }
                last = Some(data);
                since = std::time::Instant::now();
            }
            Ok(None) => {}
            Err(err) => {
                debug!(error = %err, "mando en crudo");
                return;
            }
        }
        if since.elapsed() >= RESEND
            && let Some(data) = &last
            && link.raw(slot)
        {
            link.pad(PadData::Report {
                slot,
                data: data.clone(),
            });
            since = std::time::Instant::now();
        }
    }
}

/// Clasifica un mando conectado para la interfaz.
fn kind_of(key: &PadKey) -> PadKind {
    #[cfg(any(
        all(target_os = "linux", feature = "linux-portal"),
        target_os = "windows"
    ))]
    {
        match platform::find(key.vendor, key.product) {
            Some(info) if platform::open(&info).is_ok() => PadKind::Hid,
            Some(_) => PadKind::Converted(platform::NO_ACCESS.to_owned()),
            None if platform::is_xinput(key.vendor, key.product) => PadKind::XInput,
            None => PadKind::Converted("sin interfaz HID en crudo".to_owned()),
        }
    }
    #[cfg(not(any(
        all(target_os = "linux", feature = "linux-portal"),
        target_os = "windows"
    )))]
    {
        let _ = key;
        PadKind::Converted("todavía no desde este sistema".to_owned())
    }
}

fn run(shared: &Shared) {
    let link = &shared.link;
    let mut gilrs = match GilrsBuilder::new().add_mappings(MAPPINGS).build() {
        Ok(gilrs) => gilrs,
        Err(gilrs::Error::NotImplemented(dummy)) => {
            warn!("los mandos no están disponibles en esta plataforma");
            dummy
        }
        Err(err) => {
            warn!(error = %err, "no se pudieron leer los mandos");
            return;
        }
    };
    let mut connected: Vec<(GamepadId, PadKey)> = gilrs
        .gamepads()
        .map(|(id, _)| (id, key_of(&gilrs, id)))
        .collect();
    let mut kinds: BTreeMap<PadKey, PadKind> = BTreeMap::new();
    let mut publish = true;
    // Lo que se anunció al anfitrión: ranura y mando.
    let mut announced: Option<(u8, GamepadId)> = None;
    let mut raw: Option<RawPad> = None;
    let mut axes = AxisFilter::default();
    while !shared.stop.load(Ordering::Acquire) {
        while let Some(event) = gilrs.next_event() {
            match event.event {
                EventType::Connected => {
                    let key = key_of(&gilrs, event.id);
                    debug!(name = %key.name, "mando conectado");
                    connected.retain(|(id, _)| *id != event.id);
                    connected.push((event.id, key));
                    publish = true;
                }
                EventType::Disconnected => {
                    connected.retain(|(id, _)| *id != event.id);
                    publish = true;
                }
                _ => {}
            }
            let Some((slot, active)) = announced else {
                continue;
            };
            if event.id != active || raw.as_ref().is_some_and(|r| r.hid && link.raw(slot)) {
                continue;
            }
            match event.event {
                EventType::ButtonPressed(pressed, _) => {
                    if let Some(button) = button(pressed) {
                        link.event(InputEvent::GamepadButton {
                            slot,
                            button,
                            pressed: true,
                        });
                    }
                }
                EventType::ButtonReleased(released, _) => {
                    if let Some(button) = button(released) {
                        link.event(InputEvent::GamepadButton {
                            slot,
                            button,
                            pressed: false,
                        });
                    }
                }
                EventType::ButtonChanged(Button::LeftTrigger2, value, _) => {
                    axes.set(GamepadAxis::LeftTrigger, trigger_value(value));
                }
                EventType::ButtonChanged(Button::RightTrigger2, value, _) => {
                    axes.set(GamepadAxis::RightTrigger, trigger_value(value));
                }
                EventType::AxisChanged(changed, value, _) => {
                    if let Some(axis) = axis(changed) {
                        axes.set(axis, stick_value(value));
                    }
                }
                _ => {}
            }
        }

        // Qué mando va a qué ranura ahora.
        let choice = shared
            .choice
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let active = pick(&choice, &connected);
        let slot = shared.slot.load(Ordering::Acquire);
        let wanted = active.filter(|_| slot != 0).map(|id| (slot, id));
        if wanted != announced {
            if let Some((old, _)) = announced {
                link.event(InputEvent::GamepadDisconnected { slot: old });
            }
            raw = None;
            axes.reset();
            if let Some((slot, id)) = wanted {
                link.event(InputEvent::GamepadConnected {
                    slot,
                    kind: GamepadKind::Xbox,
                });
                let key = key_of(&gilrs, id);
                raw = Some(declare(link, slot, &key));
            }
            announced = wanted;
            publish = true;
        }

        if publish {
            publish = false;
            let active = announced.map(|(_, id)| id);
            let list: Vec<ConnectedPad> = connected
                .iter()
                .map(|(id, key)| ConnectedPad {
                    key: key.clone(),
                    kind: kinds
                        .entry(key.clone())
                        .or_insert_with(|| kind_of(key))
                        .clone(),
                    active: Some(*id) == active,
                })
                .collect();
            *shared.pads.lock().unwrap_or_else(PoisonError::into_inner) = list;
        }

        // Vibración y luces del anfitrión.
        for (out_slot, data) in link.outputs() {
            if let (Some((slot, _)), Some(raw)) = (announced, raw.as_mut())
                && out_slot == slot
            {
                raw.write(&data);
            }
        }

        if let Some((slot, _)) = announced {
            for (axis, value) in axes.take_changed() {
                link.event(InputEvent::GamepadAxis { slot, axis, value });
            }
        }
        thread::sleep(PERIOD);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_have_a_minimal_deadzone() {
        assert_eq!(stick_value(0.01), 0);
        assert_eq!(stick_value(-1.5), -i16::MAX);
        assert_eq!(stick_value(1.0), i16::MAX);
        assert_eq!(stick_value(f32::NAN), 0);
        assert_eq!(trigger_value(0.5), 16384);
        assert_eq!(trigger_value(-0.5), 0);
    }

    #[test]
    fn axes_are_sent_only_when_they_change() {
        let mut filter = AxisFilter::default();
        filter.set(GamepadAxis::LeftX, 10);
        assert!(filter.take_changed().is_empty(), "por debajo del umbral");
        filter.set(GamepadAxis::LeftX, 1000);
        filter.set(GamepadAxis::RightY, -2000);
        assert_eq!(
            filter.take_changed(),
            vec![(GamepadAxis::LeftX, 1000), (GamepadAxis::RightY, -2000)]
        );
        filter.set(GamepadAxis::LeftX, 1010);
        assert!(filter.take_changed().is_empty());
        filter.set(GamepadAxis::LeftX, 0);
        assert_eq!(filter.take_changed(), vec![(GamepadAxis::LeftX, 0)]);
    }

    #[test]
    fn the_choice_picks_the_pad_and_follows_hotplug() {
        // Sin `gilrs` no hay GamepadId reales: se comprueba con la lógica de elección sobre
        // identificadores ficticios a través de `pick_index`.
        let keys = |names: &[&str]| -> Vec<PadKey> {
            names
                .iter()
                .map(|name| PadKey {
                    vendor: 1,
                    product: 2,
                    name: (*name).to_owned(),
                })
                .collect()
        };
        // En orden de conexión: en automático va el último conectado.
        let pads = keys(&["8BitDo", "DualSense"]);
        assert_eq!(pick_index(&PadChoice::Auto, &pads), Some(1));
        assert_eq!(pick_index(&PadChoice::Auto, &[]), None);
        let eightbitdo = PadChoice::Pad(pads[0].clone());
        assert_eq!(pick_index(&eightbitdo, &pads), Some(0));
        // El último se desconecta: en automático, el anterior; el elegido sigue.
        let only = keys(&["8BitDo"]);
        assert_eq!(pick_index(&PadChoice::Auto, &only), Some(0));
        assert_eq!(pick_index(&eightbitdo, &only), Some(0));
        // El elegido se desconecta: ninguno, y vuelve al reconectarlo.
        let other = keys(&["DualSense"]);
        assert_eq!(pick_index(&eightbitdo, &other), None);
        assert_eq!(pick_index(&eightbitdo, &pads), Some(0));
    }

    #[test]
    fn the_8bitdo_ultimate_2_mapping_is_valid_for_linux() {
        let mapping = MAPPINGS
            .lines()
            .find(|line| line.contains("8BitDo Ultimate 2"))
            .unwrap();
        assert!(mapping.starts_with("03000000c82d00001260000011010000,"));
        assert!(
            mapping.contains("rightx:a2,righty:a3"),
            "stick derecho en ABS_Z/ABS_RZ"
        );
        assert!(
            mapping.contains("lefttrigger:a5,righttrigger:a4"),
            "gatillos en BRAKE/GAS"
        );
        assert!(mapping.ends_with("platform:Linux,"));
    }

    #[test]
    fn the_xbox_vocabulary_is_mapped() {
        assert_eq!(button(Button::West), Some(GamepadButton::X));
        assert_eq!(button(Button::LeftTrigger), Some(GamepadButton::LeftBumper));
        assert_eq!(button(Button::LeftTrigger2), None, "LT va como eje");
        assert_eq!(axis(Axis::LeftStickY), Some(GamepadAxis::LeftY));
        assert_eq!(axis(Axis::DPadX), None);
    }
}
