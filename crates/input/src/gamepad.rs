//! Mandos del invitado con `gilrs` en un hilo que espera en los propios mandos.
//!
//! El hilo `mandos` se bloquea en `gilrs` (`epoll` sobre los dispositivos en Linux) y despierta con
//! cada evento: un botón sale al momento, con la hora en que el sistema lo leyó
//! ([`PadLink::event_at`]), así que la etapa captura → envío incluye el muestreo. Los ejes se
//! acumulan y salen como mucho a 1000 Hz ([`PERIOD`]). Sin eventos, el hilo mira cada
//! [`IDLE_POLL`] la ranura, la elección y los informes de salida del anfitrión. Qué mando va a la
//! ranura se calcula solo al conectar o desconectar uno o al cambiar la elección, y en el bucle no
//! se reserva memoria.
//!
//! Un solo mando va a la ranura que el anfitrión haya asignado a este invitado
//! ([`GamepadCapture::set_slot`]): el que elija el invitado ([`PadChoice::Pad`]) o, en automático,
//! el último conectado. Los mandos se pueden conectar y desconectar en cualquier momento: si el
//! elegido se desconecta, la ranura se queda sin mando hasta que vuelva (o, en automático, pasa
//! al anterior que siga conectado); la lista para la interfaz está en [`GamepadCapture::pads`]. Conexión y
//! desconexión, y cambio de ranura, se anuncian con `GamepadConnected`/`GamepadDisconnected`.
//!
//! **Convertido**: botones por evento (al momento), ejes a 1000 Hz y solo si cambian más de
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
//! anfitrión (vibración, luces) se escriben en el mando ([`PadOutputs`]) con el tamaño que declara
//! su descriptor, y si el mando no los acepta se avisa una vez. Sin ranura no se envía nada.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use coroom_proto::hid::HidSummary;
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
    /// Envía un evento convertido que ocurrió en `at` (la hora en que el sistema leyó el mando):
    /// la etapa captura → envío se mide desde ahí. Por defecto, como [`PadLink::event`].
    fn event_at(&self, event: InputEvent, at: Instant) {
        let _ = at;
        self.event(event);
    }
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

/// Intervalo mínimo entre dos envíos de ejes (1000 Hz); los botones no esperan.
pub const PERIOD: Duration = Duration::from_millis(1);

/// Sin eventos, cada cuánto mira el hilo la ranura, la elección y las salidas del anfitrión
/// (vibración, luces).
pub const IDLE_POLL: Duration = Duration::from_millis(5);

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

/// Ejes pendientes y enviados: solo salen los que cambian. Sin reservar memoria: un hueco por
/// eje, en el orden de [`GamepadAxis::ALL`].
#[derive(Debug, Default)]
pub struct AxisFilter {
    /// Último valor pendiente de cada eje y cuándo llegó el primero desde el último envío.
    pending: [Option<(i16, Instant)>; 6],
    sent: [i16; 6],
}

fn axis_index(axis: GamepadAxis) -> usize {
    GamepadAxis::ALL
        .iter()
        .position(|a| *a == axis)
        .unwrap_or(0)
}

impl AxisFilter {
    /// Anota el último valor de un eje, leído ahora.
    pub fn set(&mut self, axis: GamepadAxis, value: i16) {
        self.set_at(axis, value, Instant::now());
    }

    /// Anota el último valor de un eje, leído en `at` (se conserva la hora del primero pendiente).
    pub fn set_at(&mut self, axis: GamepadAxis, value: i16, at: Instant) {
        let slot = &mut self.pending[axis_index(axis)];
        let first = slot.map_or(at, |(_, first)| first);
        *slot = Some((value, first));
    }

    /// Si hay algún eje pendiente.
    pub fn has_pending(&self) -> bool {
        self.pending.iter().any(Option::is_some)
    }

    /// Recorre y vacía los ejes que cambiaron desde el último envío (el reposo, 0, siempre sale
    /// si no se había enviado), con la hora en que llegó cada uno.
    pub fn drain_changed(&mut self, mut f: impl FnMut(GamepadAxis, i16, Instant)) {
        for (index, slot) in self.pending.iter_mut().enumerate() {
            let Some((value, at)) = slot.take() else {
                continue;
            };
            let last = self.sent[index];
            let moved = (i32::from(value) - i32::from(last)).abs() >= AXIS_EPSILON;
            if moved || (value == 0 && last != 0) {
                self.sent[index] = value;
                f(GamepadAxis::ALL[index], value, at);
            }
        }
    }

    /// Ejes que cambiaron desde el último envío (como [`AxisFilter::drain_changed`], en un vector).
    pub fn take_changed(&mut self) -> Vec<(GamepadAxis, i16)> {
        let mut out = Vec::new();
        self.drain_changed(|axis, value, _| out.push((axis, value)));
        out
    }

    /// Olvida lo enviado (el mando cambia de ranura o se desconecta).
    pub fn reset(&mut self) {
        self.pending = [None; 6];
        self.sent = [0; 6];
    }
}

/// Hilo de mandos.
pub struct GamepadCapture {
    stop: Arc<AtomicBool>,
    slot: Arc<AtomicU8>,
    choice: Arc<Mutex<PadChoice>>,
    /// Sube con cada [`GamepadCapture::set_choice`]: el hilo solo mira `choice` cuando cambia.
    generation: Arc<AtomicU64>,
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
        let generation = Arc::new(AtomicU64::new(0));
        let pads = Arc::new(Mutex::new(Vec::new()));
        let thread = {
            let shared = Shared {
                stop: Arc::clone(&stop),
                slot: Arc::clone(&slot),
                choice: Arc::clone(&choice),
                generation: Arc::clone(&generation),
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
            generation,
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
        self.generation.fetch_add(1, Ordering::AcqRel);
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
    generation: Arc<AtomicU64>,
    pads: Arc<Mutex<Vec<ConnectedPad>>>,
    link: Arc<dyn PadLink>,
}

/// El `Instant` que corresponde a la hora `time` de un evento de `gilrs` (la del sistema al
/// leer el mando): ahora menos lo que ha pasado desde entonces.
fn instant_of(time: SystemTime) -> Instant {
    let now = Instant::now();
    match SystemTime::now().duration_since(time) {
        Ok(age) => now.checked_sub(age).unwrap_or(now),
        Err(_) => now,
    }
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
fn pick_index<'a>(
    choice: &PadChoice,
    mut connected: impl ExactSizeIterator<Item = &'a PadKey>,
) -> Option<usize> {
    match choice {
        PadChoice::Auto => connected.len().checked_sub(1),
        PadChoice::Pad(key) => connected.position(|k| k == key),
    }
}

/// El mando que va a la ranura según `choice`.
fn pick(choice: &PadChoice, connected: &[(GamepadId, PadKey)]) -> Option<GamepadId> {
    pick_index(choice, connected.iter().map(|(_, key)| key)).map(|index| connected[index].0)
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

    /// Qué mirar si el mando no acepta los informes de salida.
    pub const WRITE_HELP: &str = "comprueba que tu usuario puede escribir en su /dev/hidraw* \
         (reglas udev) y que ningún otro programa (Steam, por ejemplo) le esté mandando los suyos";

    impl super::OutputWriter for Device {
        fn write_output(&mut self, data: &[u8]) -> Result<(), InputError> {
            Device::write_output(self, data)
        }
    }

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

    /// Qué mirar si el mando no acepta los informes de salida.
    pub const WRITE_HELP: &str = "si Steam está abierto en este equipo, ciérralo (puede tener el \
         mando en uso o mandarle los suyos) y vuelve a conectarlo";

    impl super::OutputWriter for Device {
        fn write_output(&mut self, data: &[u8]) -> Result<(), InputError> {
            Device::write_output(self, data)
        }
    }

    /// La colección HID abierta.
    pub type Device = hidwin::HidwinDevice;

    pub fn open(info: &Info) -> Result<Device, InputError> {
        Device::open(info)
    }
}

/// Dónde escribe el invitado los informes de salida del anfitrión: el mando en crudo (y en los
/// tests, uno de mentira).
pub trait OutputWriter: Send {
    /// Escribe un informe de salida (`data[0]` es su identificador, 0 si no los hay).
    fn write_output(&mut self, data: &[u8]) -> Result<(), InputError>;
}

/// Los informes de salida (vibración, luces) que el anfitrión manda al mando tal cual de una
/// ranura, hasta el mando físico.
pub struct PadOutputs {
    writer: Box<dyn OutputWriter>,
    summary: HidSummary,
    /// Nombre del mando, para el aviso.
    name: String,
    /// Qué mirar si no los acepta.
    help: &'static str,
    /// Escritos bien y fallidos.
    written: u64,
    failed: u64,
    /// Si ya se avisó de que no los acepta (una vez por mando).
    warned: bool,
}

impl std::fmt::Debug for PadOutputs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PadOutputs")
            .field("name", &self.name)
            .field("written", &self.written)
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl PadOutputs {
    /// Para el mando `name` con su descriptor (`summary`): `help` dice qué mirar si falla.
    pub fn new(
        writer: Box<dyn OutputWriter>,
        summary: HidSummary,
        name: &str,
        help: &'static str,
    ) -> Self {
        Self {
            writer,
            summary,
            name: name.to_owned(),
            help,
            written: 0,
            failed: 0,
            warned: false,
        }
    }

    /// Escribe los de `outputs` que son de `slot` (los de otra ranura se descartan: el mando
    /// cambió de ranura entre medias), cada uno con el tamaño que declara el descriptor.
    pub fn deliver(&mut self, slot: u8, outputs: impl IntoIterator<Item = (u8, Vec<u8>)>) {
        for (out_slot, data) in outputs {
            if out_slot != slot || data.is_empty() {
                continue;
            }
            let data = self.summary.fit_output(&data);
            match self.writer.write_output(&data) {
                Ok(()) => {
                    if self.written == 0 {
                        debug!(name = %self.name, report = data[0], "primera salida del anfitrión escrita en el mando");
                    }
                    self.written += 1;
                }
                Err(err) => {
                    self.failed += 1;
                    if !std::mem::replace(&mut self.warned, true) {
                        warn!(
                            error = %err,
                            report = data[0],
                            "el mando «{}» no acepta los informes de salida del anfitrión: su \
                             vibración y sus luces no llegan; {}",
                            self.name,
                            self.help
                        );
                    } else {
                        debug!(error = %err, "informe de salida al mando");
                    }
                }
            }
        }
    }

    /// Informes escritos bien y fallidos.
    pub fn counts(&self) -> (u64, u64) {
        (self.written, self.failed)
    }

    /// Si ya se avisó de que el mando no los acepta.
    pub fn warned(&self) -> bool {
        self.warned
    }
}

/// El mando de la ranura mandado tal cual en crudo (hilo `mando-crudo`).
struct RawPad {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Sus informes de salida (solo si es HID y se pudo abrir).
    outputs: Option<PadOutputs>,
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
        outputs: None,
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
        let outputs = PadOutputs::new(
            Box::new(writer),
            info.summary.clone(),
            &info.name,
            platform::WRITE_HELP,
        );
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
            outputs: Some(outputs),
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
    // Qué mando elige `choice`: se recalcula al conectar o desconectar uno o al cambiar la
    // elección, no en cada vuelta.
    let mut active: Option<GamepadId> = None;
    let mut repick = true;
    let mut seen_generation = shared.generation.load(Ordering::Acquire);
    let mut last_axes: Option<Instant> = None;
    let mut last_outputs = Instant::now();
    while !shared.stop.load(Ordering::Acquire) {
        // Espera en los mandos: un botón despierta al momento; con ejes pendientes, hasta que
        // toque enviarlos; si no, hasta la siguiente mirada a la ranura y las salidas.
        let now = Instant::now();
        let axes_due = last_axes.map_or(now, |at| at + PERIOD);
        let wake = if axes.has_pending() {
            axes_due
        } else {
            now + IDLE_POLL
        };
        let mut next = gilrs.next_event_blocking(Some(wake.saturating_duration_since(now)));
        while let Some(event) = next {
            match event.event {
                EventType::Connected => {
                    let key = key_of(&gilrs, event.id);
                    debug!(name = %key.name, "mando conectado");
                    connected.retain(|(id, _)| *id != event.id);
                    connected.push((event.id, key));
                    publish = true;
                    repick = true;
                }
                EventType::Disconnected => {
                    connected.retain(|(id, _)| *id != event.id);
                    publish = true;
                    repick = true;
                }
                _ => {}
            }
            next = gilrs.next_event();
            let Some((slot, announced_id)) = announced else {
                continue;
            };
            if event.id != announced_id || raw.as_ref().is_some_and(|r| r.hid && link.raw(slot)) {
                continue;
            }
            let at = instant_of(event.time);
            match event.event {
                EventType::ButtonPressed(pressed, _) => {
                    if let Some(button) = button(pressed) {
                        link.event_at(
                            InputEvent::GamepadButton {
                                slot,
                                button,
                                pressed: true,
                            },
                            at,
                        );
                    }
                }
                EventType::ButtonReleased(released, _) => {
                    if let Some(button) = button(released) {
                        link.event_at(
                            InputEvent::GamepadButton {
                                slot,
                                button,
                                pressed: false,
                            },
                            at,
                        );
                    }
                }
                EventType::ButtonChanged(Button::LeftTrigger2, value, _) => {
                    axes.set_at(GamepadAxis::LeftTrigger, trigger_value(value), at);
                }
                EventType::ButtonChanged(Button::RightTrigger2, value, _) => {
                    axes.set_at(GamepadAxis::RightTrigger, trigger_value(value), at);
                }
                EventType::AxisChanged(changed, value, _) => {
                    if let Some(axis) = axis(changed) {
                        axes.set_at(axis, stick_value(value), at);
                    }
                }
                _ => {}
            }
        }

        // Qué mando va a qué ranura ahora.
        let generation = shared.generation.load(Ordering::Acquire);
        if repick || generation != seen_generation {
            repick = false;
            seen_generation = generation;
            let choice = shared.choice.lock().unwrap_or_else(PoisonError::into_inner);
            active = pick(&choice, &connected);
        }
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

        let now = Instant::now();
        // Vibración y luces del anfitrión: solo con un mando tal cual, y no en cada vuelta.
        if let (Some((slot, _)), Some(outputs)) =
            (announced, raw.as_mut().and_then(|raw| raw.outputs.as_mut()))
            && now.saturating_duration_since(last_outputs) >= IDLE_POLL
        {
            last_outputs = now;
            outputs.deliver(slot, link.outputs());
        }

        // Los ejes, como mucho a 1000 Hz, con la hora en que llegó cada uno.
        if axes.has_pending() && now >= axes_due {
            last_axes = Some(now);
            match announced {
                Some((slot, _)) => axes.drain_changed(|axis, value, at| {
                    link.event_at(InputEvent::GamepadAxis { slot, axis, value }, at);
                }),
                None => axes.reset(),
            }
        }
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
        assert!(!filter.has_pending());

        // La hora que sale es la del primer valor pendiente de ese eje (la muestra que empezó el
        // movimiento), aunque después se actualice.
        let t0 = Instant::now();
        filter.set_at(GamepadAxis::RightX, 5000, t0);
        filter.set_at(GamepadAxis::RightX, 6000, t0 + Duration::from_millis(1));
        assert!(filter.has_pending());
        let mut seen = Vec::new();
        filter.drain_changed(|axis, value, at| seen.push((axis, value, at)));
        assert_eq!(seen, vec![(GamepadAxis::RightX, 6000, t0)]);
        filter.reset();
        assert!(!filter.has_pending());
        assert_eq!(filter.take_changed(), vec![]);
    }

    #[test]
    fn event_times_become_instants() {
        let now = Instant::now();
        let at = instant_of(SystemTime::now() - Duration::from_millis(20));
        let after = Instant::now();
        // Funcional: la hora de hace 20 ms queda al menos 20 ms antes de ahora.
        assert!(
            after.saturating_duration_since(at) >= Duration::from_millis(20),
            "{:?}",
            after.saturating_duration_since(at)
        );
        let age = now.saturating_duration_since(at);
        coroom_testkit::timing_assert!(
            age < Duration::from_millis(500),
            "una hora de hace 20 ms sale con {age:?}: el hilo se quedó parado"
        );
        // Una hora futura (reloj ajustado) no se adelanta.
        assert!(instant_of(SystemTime::now() + Duration::from_secs(5)) <= Instant::now());
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
        assert_eq!(pick_index(&PadChoice::Auto, pads.iter()), Some(1));
        assert_eq!(pick_index(&PadChoice::Auto, [].iter()), None);
        let eightbitdo = PadChoice::Pad(pads[0].clone());
        assert_eq!(pick_index(&eightbitdo, pads.iter()), Some(0));
        // El último se desconecta: en automático, el anterior; el elegido sigue.
        let only = keys(&["8BitDo"]);
        assert_eq!(pick_index(&PadChoice::Auto, only.iter()), Some(0));
        assert_eq!(pick_index(&eightbitdo, only.iter()), Some(0));
        // El elegido se desconecta: ninguno, y vuelve al reconectarlo.
        let other = keys(&["DualSense"]);
        assert_eq!(pick_index(&eightbitdo, other.iter()), None);
        assert_eq!(pick_index(&eightbitdo, pads.iter()), Some(0));
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

    /// Un mando de mentira que apunta lo que se le escribe (o falla siempre).
    struct FakeWriter {
        written: Arc<Mutex<Vec<Vec<u8>>>>,
        fail: bool,
    }

    impl OutputWriter for FakeWriter {
        fn write_output(&mut self, data: &[u8]) -> Result<(), InputError> {
            if self.fail {
                return Err(InputError::Backend("hidraw: Broken pipe".into()));
            }
            self.written.lock().unwrap().push(data.to_vec());
            Ok(())
        }
    }

    /// Descriptor de un mando con entrada `0x01` y salida `0x05` de 32 bytes (vibración y luz,
    /// como la de un DualShock 4 por USB).
    fn numbered_pad() -> HidSummary {
        let mut descriptor = vec![0x05, 0x01, 0x09, 0x05, 0xA1, 0x01];
        descriptor.extend([0x85, 0x01, 0x75, 0x08, 0x95, 0x3F, 0x81, 0x02]);
        descriptor.extend([0x06, 0x00, 0xFF, 0x09, 0x02]);
        descriptor.extend([0x85, 0x05, 0x95, 0x1F, 0x91, 0x02]);
        descriptor.push(0xC0);
        coroom_proto::hid::parse(&descriptor).unwrap()
    }

    fn outputs_with(fail: bool) -> (PadOutputs, Arc<Mutex<Vec<Vec<u8>>>>) {
        let written = Arc::new(Mutex::new(Vec::new()));
        let writer = FakeWriter {
            written: Arc::clone(&written),
            fail,
        };
        (
            PadOutputs::new(
                Box::new(writer),
                numbered_pad(),
                "Mando de ejemplo",
                "ayuda",
            ),
            written,
        )
    }

    #[test]
    fn host_outputs_reach_the_pad_of_their_slot_with_their_size() {
        let (mut outputs, written) = outputs_with(false);
        outputs.deliver(
            2,
            vec![
                // De la ranura 2, corto (como lo manda un driver que no rellena): a 32 bytes.
                (2, vec![0x05, 0xFF, 0x04, 0x00, 0x80, 0x40]),
                // De otra ranura (el mando cambió de ranura entre medias): no.
                (3, vec![0x05, 0xFF, 0x04]),
                // Vacío: no.
                (2, vec![]),
                // Rellenado hasta 64 bytes (Windows): recortado a 32.
                (2, {
                    let mut data = vec![0x05, 0xFF, 0x04, 0x00, 0x00, 0x00];
                    data.resize(64, 0);
                    data
                }),
            ],
        );
        let written = written.lock().unwrap();
        assert_eq!(written.len(), 2);
        assert!(written.iter().all(|data| data.len() == 32));
        assert_eq!(&written[0][..6], &[0x05, 0xFF, 0x04, 0x00, 0x80, 0x40]);
        assert!(written[0][6..].iter().all(|b| *b == 0));
        assert_eq!(&written[1][..6], &[0x05, 0xFF, 0x04, 0x00, 0x00, 0x00]);
        assert_eq!(outputs.counts(), (2, 0));
        assert!(!outputs.warned());
    }

    #[test]
    fn a_pad_that_refuses_outputs_is_warned_about_once() {
        let (mut outputs, written) = outputs_with(true);
        outputs.deliver(1, vec![(1, vec![0x05, 0xFF]), (1, vec![0x05, 0x00])]);
        outputs.deliver(1, vec![(1, vec![0x05, 0x10])]);
        assert!(written.lock().unwrap().is_empty());
        assert_eq!(outputs.counts(), (0, 3));
        assert!(outputs.warned(), "se avisa (una vez) de que no llegan");
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
