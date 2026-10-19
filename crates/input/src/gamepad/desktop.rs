//! El hilo `mandos` con `gilrs` y los mandos en crudo del invitado de escritorio (no en Android:
//! `gilrs` no tiene backend allí; los mandos los pasa la aplicación a [`crate::android`]).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use coroom_proto::input::{GamepadDevice, GamepadTransport, PadData};
use gilrs::ff::{BaseEffect, BaseEffectType, Effect, EffectBuilder};
use gilrs::{Axis, Button, EventType, GamepadId, Gilrs, GilrsBuilder};
use tracing::{debug, warn};

use super::{
    AxisFilter, ConnectedPad, IDLE_POLL, Motors, PERIOD, PadChoice, PadKey, PadKind, PadLink,
    PadOutputs, PadRumble, pick_index, stick_value, trigger_value,
};
#[cfg(target_os = "linux")]
use super::{EvdevMotors, evdev_motors};
use crate::{GamepadAxis, GamepadButton, GamepadKind, InputError, InputEvent, Rumble};

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

/// Hilo de mandos.
pub struct GamepadCapture {
    stop: Arc<AtomicBool>,
    slot: Arc<AtomicU8>,
    paused: Arc<AtomicBool>,
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
        let paused = Arc::new(AtomicBool::new(false));
        let choice = Arc::new(Mutex::new(choice));
        let generation = Arc::new(AtomicU64::new(0));
        let pads = Arc::new(Mutex::new(Vec::new()));
        let thread = {
            let shared = Shared {
                stop: Arc::clone(&stop),
                slot: Arc::clone(&slot),
                paused: Arc::clone(&paused),
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
            paused,
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

    /// En pausa, el mando sigue declarado en su ranura (el anfitrión no lo ve desconectarse,
    /// que a algunos juegos los descoloca) pero no se manda lo que se toca: al pausar, lo
    /// convertido vuelve al reposo (botones sueltos, ejes al centro) y lo tal cual deja de mandar
    /// informes; al reanudar se manda el estado de ese momento.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Release);
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
    paused: Arc<AtomicBool>,
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

/// Lo que tiene tocado un mando: botones pulsados y ejes fuera del centro.
type Held = (Vec<GamepadButton>, Vec<(GamepadAxis, i16)>);

/// Lo que se manda al pausar (`held` es `None`: todo suelto y al centro) o al reanudar con lo
/// que el mando tiene tocado en ese momento (botones pulsados y ejes, el resto al centro).
fn pause_events(slot: u8, held: Option<Held>) -> Vec<InputEvent> {
    let resumed = held.is_some();
    let (buttons, values) = held.unwrap_or_default();
    let released = GamepadButton::ALL
        .into_iter()
        .filter(|button| !resumed || buttons.contains(button))
        .map(|button| InputEvent::GamepadButton {
            slot,
            button,
            pressed: resumed,
        });
    let axes = GamepadAxis::ALL
        .into_iter()
        .map(|axis| InputEvent::GamepadAxis {
            slot,
            axis,
            value: values
                .iter()
                .find(|(held, _)| *held == axis)
                .map_or(0, |(_, value)| *value),
        });
    released.chain(axes).collect()
}

/// Lo que tiene tocado ahora el mando `id`: botones pulsados y ejes fuera del centro.
fn held(gilrs: &Gilrs, id: GamepadId) -> Held {
    let pad = gilrs.gamepad(id);
    let buttons = [
        Button::South,
        Button::East,
        Button::West,
        Button::North,
        Button::LeftTrigger,
        Button::RightTrigger,
        Button::Select,
        Button::Start,
        Button::Mode,
        Button::LeftThumb,
        Button::RightThumb,
        Button::DPadUp,
        Button::DPadDown,
        Button::DPadLeft,
        Button::DPadRight,
    ]
    .into_iter()
    .filter(|held| pad.is_pressed(*held))
    .filter_map(button)
    .collect();
    let mut axes: Vec<(GamepadAxis, i16)> = [
        Axis::LeftStickX,
        Axis::LeftStickY,
        Axis::RightStickX,
        Axis::RightStickY,
    ]
    .into_iter()
    .filter_map(|held| Some((axis(held)?, stick_value(pad.value(held)))))
    .collect();
    for (trigger, held) in [
        (Button::LeftTrigger2, GamepadAxis::LeftTrigger),
        (Button::RightTrigger2, GamepadAxis::RightTrigger),
    ] {
        if let Some(data) = pad.button_data(trigger) {
            axes.push((held, trigger_value(data.value())));
        }
    }
    axes.retain(|(_, value)| *value != 0);
    (buttons, axes)
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

    impl crate::gamepad::OutputWriter for Device {
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

    /// Qué mirar si el mando no vibra.
    pub const RUMBLE_HELP: &str = "en Windows solo vibran así los mandos de Xbox y los \
         compatibles; con otro, prueba «Tal cual» si se puede";

    impl crate::gamepad::OutputWriter for Device {
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

/// Los motores de un mando de `gilrs`: un efecto con los dos motores que se sustituye en cada
/// cambio (`gilrs` lo mezcla y lo manda al mando cada 50 ms; el nuevo empieza a sonar en la misma
/// vuelta en que se suelta el viejo). Sin efecto, los motores se paran.
pub struct GilrsMotors<'a> {
    /// Los mandos.
    pub gilrs: &'a mut Gilrs,
    /// El mando.
    pub id: GamepadId,
    /// El efecto que suena (lo guarda quien llama entre una vez y otra).
    pub effect: &'a mut Option<Effect>,
}

impl Motors for GilrsMotors<'_> {
    fn set(&mut self, rumble: Rumble) -> Result<(), String> {
        if !rumble.is_on() {
            // Al soltarlo, `gilrs` deja de mezclarlo y los motores se paran.
            *self.effect = None;
            return Ok(());
        }
        let motor = |kind| BaseEffect {
            kind,
            ..BaseEffect::default()
        };
        let effect = EffectBuilder::new()
            .add_effect(motor(BaseEffectType::Strong {
                magnitude: rumble.strong,
            }))
            .add_effect(motor(BaseEffectType::Weak {
                magnitude: rumble.weak,
            }))
            .gamepads(&[self.id])
            .finish(self.gilrs)
            .map_err(|err| match err {
                gilrs::ff::Error::FfNotSupported(_) => {
                    "no tiene vibración (force feedback) o no se puede escribir en él".to_owned()
                }
                gilrs::ff::Error::Disconnected(_) => "se desconectó".to_owned(),
                other => other.to_string(),
            })?;
        effect.play().map_err(|err| err.to_string())?;
        *self.effect = Some(effect);
        Ok(())
    }
}

/// Si `gilrs` mueve los motores con su *force feedback*. En Linux no: su servidor manda cada
/// 50 ms el estado, ceros incluidos, a todos los mandos que vibran, y pisaría la vibración que
/// llega por los informes de salida a un mando tal cual; ahí los mueve [`EvdevMotors`].
pub const GILRS_FORCE_FEEDBACK: bool = !cfg!(target_os = "linux");

/// Cómo se abren los mandos del invitado.
fn gilrs_builder() -> GilrsBuilder {
    GilrsBuilder::new()
        .add_mappings(MAPPINGS)
        .with_force_feedback(GILRS_FORCE_FEEDBACK)
}

/// Los motores del mando de la ranura, con lo que los mueve en cada sistema.
struct SlotMotors {
    #[cfg(target_os = "linux")]
    evdev: Option<EvdevMotors>,
    #[cfg(not(target_os = "linux"))]
    effect: Option<Effect>,
}

impl SlotMotors {
    fn new() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            evdev: None,
            #[cfg(not(target_os = "linux"))]
            effect: None,
        }
    }

    /// Pone en el mando `id` lo que pide el anfitrión (si cambió).
    fn deliver(&mut self, state: &mut PadRumble, gilrs: &mut Gilrs, id: GamepadId, rumble: Rumble) {
        #[cfg(target_os = "linux")]
        {
            use gilrs::LinuxGamepadExt;
            let Some(node) = gilrs
                .connected_gamepad(id)
                .map(|pad| pad.devpath().to_owned())
            else {
                // Desconectado: no hay motores que mover (el núcleo ya soltó los efectos).
                return;
            };
            let motors = self.evdev.get_or_insert_with(|| EvdevMotors::new(&node));
            if motors.node() != node {
                // Volvió en otro nodo: el viejo no tiene nada puesto.
                motors.follow(&node);
                state.forget();
            }
            state.deliver(motors, rumble);
        }
        #[cfg(not(target_os = "linux"))]
        state.deliver(
            &mut GilrsMotors {
                gilrs,
                id,
                effect: &mut self.effect,
            },
            rumble,
        );
    }
}

/// Qué mirar si el mando no vibra.
fn rumble_help() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        evdev_motors::RUMBLE_HELP
    }
    #[cfg(target_os = "windows")]
    {
        platform::RUMBLE_HELP
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        "este sistema no hace vibrar ese mando"
    }
}

/// Lo que deben tener los motores del mando de la ranura `slot`: lo que pide el anfitrión, salvo
/// si va tal cual por HID (su vibración llega en sus informes de salida y no se le escribe nada).
fn slot_rumble(link: &dyn PadLink, slot: u8, raw: Option<&RawPad>) -> Rumble {
    if raw.is_some_and(|raw| raw.hid && link.raw(slot)) {
        Rumble::OFF
    } else {
        link.rumble(slot)
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
fn declare(link: &Arc<dyn PadLink>, slot: u8, key: &PadKey, paused: &Arc<AtomicBool>) -> RawPad {
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
            let (stop, link, paused) = (Arc::clone(&stop), Arc::clone(link), Arc::clone(paused));
            thread::Builder::new()
                .name("mando-crudo".into())
                .spawn(move || raw_loop(reader, info, slot, &link, &stop, &paused))
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
        let _ = paused;
        link.pad(device(GamepadTransport::Converted {
            reason: "el invitado todavía no manda mandos tal cual desde este sistema".into(),
        }));
        idle(false)
    }
}

/// Hilo `mando-crudo`: lee los informes de característica, declara el mando y pasa cada informe
/// de entrada tal cual mientras el anfitrión lo tenga así y no esté en pausa
/// ([`GamepadCapture::set_paused`]: el anfitrión se queda con el último; al reanudar va el de
/// ese momento).
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
    paused: &AtomicBool,
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
                if link.raw(slot) && !paused.load(Ordering::Acquire) {
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
        // Lo último, cada `RESEND` sin informes nuevos (también al reanudar, a más tardar en un
        // `RESEND`).
        if since.elapsed() >= RESEND
            && let Some(data) = &last
            && link.raw(slot)
            && !paused.load(Ordering::Acquire)
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
    let mut gilrs = match gilrs_builder().build() {
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
    // La vibración del mando de la ranura: su estado y lo que mueve sus motores.
    let mut rumble: Option<PadRumble> = None;
    let mut motors = SlotMotors::new();
    let mut last_rumble = Instant::now();
    // Si lo que se toca se está callando ([`GamepadCapture::set_paused`]).
    let mut paused = false;
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
            if paused
                || event.id != announced_id
                || raw.as_ref().is_some_and(|r| r.hid && link.raw(slot))
            {
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
            if let Some((old, old_id)) = announced {
                link.event(InputEvent::GamepadDisconnected { slot: old });
                // El mando deja la ranura: sin motores en marcha.
                if let Some(mut state) = rumble.take() {
                    motors.deliver(&mut state, &mut gilrs, old_id, Rumble::OFF);
                }
                motors = SlotMotors::new();
            }
            raw = None;
            axes.reset();
            if let Some((slot, id)) = wanted {
                link.event(InputEvent::GamepadConnected {
                    slot,
                    kind: GamepadKind::Xbox,
                });
                let key = key_of(&gilrs, id);
                raw = Some(declare(link, slot, &key, &shared.paused));
                rumble = Some(PadRumble::new(&key.name, rumble_help()));
            }
            announced = wanted;
            publish = true;
        }

        // Pausa: el mando sigue en la ranura, en reposo; al reanudar, lo que se tenga tocado.
        let pause = shared.paused.load(Ordering::Acquire);
        if pause != paused {
            paused = pause;
            axes.reset();
            if let Some((slot, id)) = announced
                && !raw.as_ref().is_some_and(|r| r.hid && link.raw(slot))
            {
                let now = (!paused).then(|| held(&gilrs, id));
                for event in pause_events(slot, now) {
                    link.event(event);
                }
            }
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
        // Y la de los que no van tal cual por HID, al mismo ritmo: llega como estado, así que un
        // mando que entra en la ranura con el juego vibrando también vibra. A uno tal cual no se
        // le escribe nada (su vibración va en sus informes de salida): si pasa a tal cual
        // vibrando, se para y ya.
        if let (Some((slot, id)), Some(state)) = (announced, rumble.as_mut())
            && now.saturating_duration_since(last_rumble) >= IDLE_POLL
        {
            last_rumble = now;
            let wanted = slot_rumble(link.as_ref(), slot, raw.as_ref());
            motors.deliver(state, &mut gilrs, id, wanted);
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
    // Al terminar, los motores parados: el efecto se suelta (y se borra) antes que `gilrs`.
    drop(motors);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gamepad::tests::FakeMotors;

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

    /// Al pausar, el mando vuelve al reposo sin desconectarse; al reanudar, se manda lo que se
    /// tiene tocado en ese momento y el resto al centro.
    #[test]
    fn pausing_rests_the_pad_and_resuming_sends_what_is_held() {
        let paused = pause_events(2, None);
        assert!(
            !paused
                .iter()
                .any(|event| matches!(event, InputEvent::GamepadDisconnected { .. })),
            "no se desconecta"
        );
        for button in GamepadButton::ALL {
            assert!(paused.contains(&InputEvent::GamepadButton {
                slot: 2,
                button,
                pressed: false
            }));
        }
        for axis in GamepadAxis::ALL {
            assert!(paused.contains(&InputEvent::GamepadAxis {
                slot: 2,
                axis,
                value: 0
            }));
        }
        let resumed = pause_events(
            2,
            Some((vec![GamepadButton::A], vec![(GamepadAxis::LeftX, 12_000)])),
        );
        let pressed: Vec<_> = resumed
            .iter()
            .filter(|event| matches!(event, InputEvent::GamepadButton { .. }))
            .collect();
        assert_eq!(
            pressed,
            [&InputEvent::GamepadButton {
                slot: 2,
                button: GamepadButton::A,
                pressed: true
            }]
        );
        assert!(resumed.contains(&InputEvent::GamepadAxis {
            slot: 2,
            axis: GamepadAxis::LeftX,
            value: 12_000
        }));
        assert!(resumed.contains(&InputEvent::GamepadAxis {
            slot: 2,
            axis: GamepadAxis::RightY,
            value: 0
        }));
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

    /// Un `PadLink` que solo responde a si el mando va tal cual y qué vibración pide el anfitrión.
    struct FakeLink {
        raw: AtomicBool,
        rumble: Rumble,
    }

    impl PadLink for FakeLink {
        fn event(&self, _: InputEvent) {}
        fn pad(&self, _: PadData) {}
        fn raw(&self, _: u8) -> bool {
            self.raw.load(Ordering::Acquire)
        }
        fn outputs(&self) -> Vec<(u8, Vec<u8>)> {
            Vec::new()
        }
        fn rumble(&self, _: u8) -> Rumble {
            self.rumble
        }
    }

    fn raw_pad(hid: bool) -> RawPad {
        RawPad {
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
            outputs: None,
            hid,
        }
    }

    #[test]
    fn a_tal_cual_pad_never_gets_the_converted_rumble() {
        let link = FakeLink {
            raw: AtomicBool::new(false),
            rumble: Rumble::new(0xFFFF, 0x8000),
        };
        // Convertido (sin interfaz HID en crudo) o Xbox por cable: lo del anfitrión.
        assert_eq!(slot_rumble(&link, 1, None), link.rumble);
        assert_eq!(slot_rumble(&link, 1, Some(&raw_pad(false))), link.rumble);
        // HID que el anfitrión no aceptó tal cual: va convertido.
        let hid = raw_pad(true);
        assert_eq!(slot_rumble(&link, 1, Some(&hid)), link.rumble);

        let mut motors = FakeMotors::default();
        let mut rumble = PadRumble::new("Mando de prueba", "nada que mirar");
        rumble.deliver(&mut motors, slot_rumble(&link, 1, Some(&hid)));
        // Pasa a tal cual vibrando: se para una vez y no se le vuelve a escribir.
        link.raw.store(true, Ordering::Release);
        for _ in 0..3 {
            rumble.deliver(&mut motors, slot_rumble(&link, 1, Some(&hid)));
        }
        assert_eq!(motors.set, vec![link.rumble, Rumble::OFF]);
        // Un mando que entra ya tal cual no recibe nada.
        let mut fresh = FakeMotors::default();
        let mut other = PadRumble::new("Otro mando", "nada que mirar");
        for _ in 0..3 {
            other.deliver(&mut fresh, slot_rumble(&link, 2, Some(&hid)));
        }
        assert!(fresh.set.is_empty(), "{:?}", fresh.set);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn gilrs_never_drives_the_motors_on_linux() {
        // Su servidor de *force feedback* mandaría ceros cada 50 ms a todos los mandos que
        // vibran, también a los tal cual.
        const { assert!(!GILRS_FORCE_FEEDBACK) };
        let _builder = gilrs_builder();
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
