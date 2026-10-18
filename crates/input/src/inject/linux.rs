//! Inyección en Linux (feature `linux-portal`): teclado y ratón por **libei** y mandos por
//! **uinput**.
//!
//! **libei** (`reis` 0.7, de ids1024, mantenida): el anfitrión es un *sender* de EI. El socket lo da
//! el portal `RemoteDesktop` (`ConnectToEIS`, versión 2 del portal, xdg-desktop-portal ≥ 1.17):
//!
//! - con la captura PipeWire, de la sesión `RemoteDesktop` que abre la captura justo después de
//!   la suya de `ScreenCast` (ver `coroom_capture::pipewire`), que llega como
//!   [`EisProvider`](super::EisProvider) junto con la zona que se transmite;
//! - si no (fuente sintética), abriendo aquí una sesión `RemoteDesktop` solo de dispositivos.
//!
//! La conexión se abre en un hilo propio la primera vez que hace falta ([`InputSink::prepare`] o el
//! primer evento): hasta que el usuario acepta el diálogo, los eventos se rechazan con
//! [`InputError::NotReady`]. Ese hilo hace el *handshake*, se ata a las capacidades `pointer`,
//! `pointer_absolute`, `keyboard`, `scroll` y `button` del asiento y atiende los dispositivos
//! (`resumed` → `start_emulating`). Los eventos de un paquete salen juntos
//! ([`InputSink::inject_batch`]): una petición por evento, un `frame` por dispositivo tocado y un
//! solo `flush`; igual que un teclado o un ratón reales, que agrupan en una trama lo que pasa a la
//! vez. La posición
//! absoluta se escala a la región del dispositivo absoluto que corresponde al monitor capturado
//! ([`PointerArea::Monitor`]: la de su `mapping_id`, o la que empieza en su posición; si no hay
//! datos, la primera, con un aviso en el registro cuando hay varias). Con una ventana, Wayland no dice dónde
//! está: la posición absoluta se convierte en movimiento relativo escalado a su tamaño
//! ([`PointerArea::Window`]). La rueda va por `scroll_discrete` (1/120 de muesca, positivo hacia
//! abajo en EI).
//!
//! **Texto sin tecla física** ([`InputEvent::Text`]): el teclado de EI anuncia su mapa XKB
//! (`ei_keyboard.keymap`), que se lee al llegar el dispositivo ([`super::xkb::TextKeymap`]; un
//! mapa nuevo llega con un dispositivo nuevo) y la distribución activa llega en
//! `ei_keyboard.modifiers`, con los modificadores bloqueados y enganchados. Cada carácter se
//! teclea con la tecla y los modificadores que lo dan en esa distribución con ese estado y con lo
//! que tienen pulsado los invitados (que no se suelta), o con una tecla muerta y otra según la
//! tabla de composición del anfitrión (la á de la distribución española): se pulsan los
//! modificadores que falten y la tecla, en una trama, y se sueltan en la siguiente. Lo que no se
//! puede dar así se descarta (con una línea de `debug` en el registro).
//!
//! **uinput** (`evdev` 0.13): un dispositivo por ranura, "coroom Gamepad N", con el
//! vendor/product del mando de Xbox 360 (`045e:028e`) y la disposición del driver `xpad` (A, B,
//! X = `BTN_NORTH`, Y = `BTN_WEST`, LB, RB, Back, Start, Guide, LS, RS; sticks `ABS_X/Y/RX/RY` en
//! ±32767 con Y hacia abajo; gatillos `ABS_Z/RZ` en 0..255; cruceta `ABS_HAT0X/Y`), así que SDL,
//! Steam Input y los juegos lo reconocen. Los eventos de mando de un paquete van en **una sola
//! trama** `SYN_REPORT` por ranura: un stick con `X` e `Y` cambiados llega al juego como una
//! muestra, no como dos a medias. Con vibración como la de `xpad`: *force feedback* `FF_RUMBLE`
//! (y `FF_GAIN`) por el protocolo de uinput. El juego sube efectos (`UI_FF_UPLOAD`, que se le
//! contestan desde el hilo `inject` en la siguiente vuelta, cada pocos ms con mandos asignados), los
//! borra (`UI_FF_ERASE`) y los reproduce o para (`EV_FF` con el id y las repeticiones);
//! [`UinputRumble`] lo convierte en el estado de los dos motores, con la duración de cada efecto,
//! y [`InputSink::take_rumble`] da los cambios para el invitado de la ranura. Si `/dev/uinput` no
//! es accesible, el error explica la regla udev ([`UINPUT_HELP`]).

use std::collections::{BTreeMap, BTreeSet};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventSummary, EventType, FFEffectCode,
    FFEffectKind, InputId, KeyCode, UInputCode,
};
use reis::ei;
use reis::event::{DeviceCapability, EiEvent};
use tracing::{debug, warn};

use super::uhid::UhidPad;
use super::xkb::{KeyboardState, TextKeymap, Typing};
use super::{EisConnection, EisProvider, InputSink, PadTracker, PointerArea, PressedKeys};
use crate::keymap::evdev as codes;
use crate::{
    EventClass, GamepadAxis, GamepadButton, GuestId, InputError, InputEvent, MouseButton, Rumble,
};
use coroom_proto::input::{GamepadDevice, GamepadTransport};

/// Qué hacer si `/dev/uinput` no es accesible.
pub const UINPUT_HELP: &str = "no se puede abrir /dev/uinput para crear el mando virtual. Instala \
    la regla udev de coroom y vuelve a entrar en la sesión:\n  echo 'KERNEL==\"uinput\", \
    SUBSYSTEM==\"misc\", TAG+=\"uaccess\", OPTIONS+=\"static_node=uinput\"' | sudo tee \
    /etc/udev/rules.d/60-coroom-uinput.rules && sudo udevadm control --reload && sudo udevadm \
    trigger\n(en Flatpak hace falta además --device=all)";

/// Vendor del mando que emula uinput (Microsoft).
pub const GAMEPAD_VENDOR: u16 = 0x045e;
/// Product del mando que emula uinput (Xbox 360).
pub const GAMEPAD_PRODUCT: u16 = 0x028e;

/// Espera máxima de los dispositivos de EI tras aceptar el portal.
const EI_DEVICE_WAIT: Duration = Duration::from_secs(5);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Microsegundos de `CLOCK_MONOTONIC`, el reloj de los `frame` de EI.
fn monotonic_us() -> u64 {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    (now.tv_sec as u64).saturating_mul(1_000_000) + (now.tv_nsec as u64) / 1000
}

/// Escribe `events` o ninguno: `target` resuelve el destino de cada uno (`None`: no va por aquí)
/// antes de que `queue` escriba el primero, así un destino que falta no deja a medias lo anterior.
/// Devuelve los destinos tocados, sin repetir y en el orden en que se tocaron.
fn all_or_nothing<D: PartialEq + Copy>(
    events: &[InputEvent],
    target: impl FnMut(&InputEvent) -> Result<Option<D>, InputError>,
    mut queue: impl FnMut(D, &InputEvent),
) -> Result<Vec<D>, InputError> {
    let targets = events.iter().map(target).collect::<Result<Vec<_>, _>>()?;
    let mut touched = Vec::with_capacity(4);
    for (device, event) in targets.into_iter().zip(events) {
        let Some(device) = device else {
            continue;
        };
        queue(device, event);
        if !touched.contains(&device) {
            touched.push(device);
        }
    }
    Ok(touched)
}

// --- libei ------------------------------------------------------------------------------------

/// Un dispositivo de EI del compositor.
struct EiDevice {
    device: reis::event::Device,
    /// Si está activo (`resumed`).
    resumed: bool,
    /// Del mapa XKB que anuncia el teclado, qué tecla da cada carácter ([`InputEvent::Text`]).
    text: Option<TextKeymap>,
    /// Distribución activa y modificadores bloqueados y enganchados del teclado
    /// (`ei_keyboard.modifiers`).
    keyboard: KeyboardState,
}

impl EiDevice {
    fn new(device: reis::event::Device) -> Self {
        let text = device.keymap().and_then(|keymap| {
            if keymap.type_ != ei::keyboard::KeymapType::Xkb {
                return None;
            }
            TextKeymap::from_fd(&keymap.fd, keymap.size)
                .inspect_err(|err| warn!("EI: el mapa de teclado no sirve para el texto: {err}"))
                .ok()
        });
        if device.has_capability(DeviceCapability::Keyboard) {
            debug!(
                name = ?device.name(),
                groups = text.as_ref().map(TextKeymap::groups),
                "EI: teclado (distribuciones de su mapa XKB)"
            );
        }
        Self {
            device,
            resumed: false,
            text,
            keyboard: KeyboardState::default(),
        }
    }
}

/// Mismo dispositivo de EI (para no repetir su `frame`).
impl PartialEq for EiDevice {
    fn eq(&self, other: &Self) -> bool {
        self.device == other.device
    }
}

#[derive(Default)]
struct EiState {
    devices: Vec<EiDevice>,
    connection: Option<reis::event::Connection>,
    sequence: u32,
    disconnected: Option<String>,
}

/// Conexión EI ya negociada (la usa el hilo `inject`; la atiende el hilo `ei`).
struct EiInput {
    context: ei::Context,
    state: Arc<Mutex<EiState>>,
    /// Qué se transmite.
    area: PointerArea,
    /// Última posición absoluta recibida, para [`PointerArea::Window`].
    last_absolute: Mutex<Option<(u16, u16)>>,
    /// Sesión del portal propia (si no se comparte con la captura): se cierra al soltarla.
    _portal: Option<PortalGuard>,
}

impl EiInput {
    /// Hace el *handshake* con `fd` y lanza el hilo `ei`. Vuelve cuando hay algún dispositivo
    /// listo o pasa [`EI_DEVICE_WAIT`].
    fn connect(
        fd: OwnedFd,
        area: PointerArea,
        portal: Option<PortalGuard>,
    ) -> Result<EiInput, String> {
        let context = ei::Context::new(UnixStream::from(fd))
            .map_err(|err| format!("socket de EI no válido: {err}"))?;
        let state = Arc::new(Mutex::new(EiState::default()));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let thread_context = context.clone();
        let thread_state = Arc::clone(&state);
        thread::Builder::new()
            .name("ei".into())
            .spawn(move || ei_loop(&thread_context, &thread_state, &ready_tx))
            .map_err(|err| format!("no se pudo crear el hilo de EI: {err}"))?;
        match ready_rx.recv_timeout(EI_DEVICE_WAIT) {
            Ok(Ok(())) => {}
            Ok(Err(err)) => return Err(err),
            Err(_) => warn!("EI: ningún dispositivo listo todavía; se seguirá esperando"),
        }
        Ok(EiInput {
            context,
            state,
            area,
            last_absolute: Mutex::new(None),
            _portal: portal,
        })
    }

    /// Escribe `events` (teclado y ratón, ya filtrados por [`PressedKeys`]) de una vez: una
    /// petición por evento, un `frame` por dispositivo tocado y un solo `flush`. O salen todos o
    /// ninguno: el dispositivo de cada evento se resuelve antes de encolar el primero, así un
    /// dispositivo que no está listo devuelve el error sin dejar nada a medias (quien llama
    /// deshace sus anotaciones de todo el grupo, y una tecla ya escrita quedaría pulsada).
    ///
    /// Cada [`InputEvent::Text`] de `events` se teclea con su `strokes`, en orden.
    fn inject_many(&self, events: &[InputEvent], strokes: &[Typing]) -> Result<(), InputError> {
        let state = lock(&self.state);
        if let Some(reason) = &state.disconnected {
            return Err(InputError::Backend(format!(
                "el compositor cerró la conexión de EI: {reason}"
            )));
        }
        let serial = state
            .connection
            .as_ref()
            .map_or(0, reis::event::Connection::serial);
        let time = monotonic_us();
        let mut strokes = strokes.iter();
        let touched = all_or_nothing(
            events,
            |event| Self::target(&state, &self.area, event),
            |device, event| match event {
                InputEvent::Text { .. } => {
                    if let Some(stroke) = strokes.next() {
                        Self::type_text(&device.device, stroke, serial, time);
                    }
                }
                _ => self.queue(&device.device, event),
            },
        )?;
        if touched.is_empty() {
            return Ok(());
        }
        for device in touched {
            device.device.device().frame(serial, time);
        }
        self.context
            .flush()
            .map_err(|err| InputError::Backend(format!("no se pudo escribir en EI: {err}")))
    }

    /// El dispositivo de EI al que va `event` (`None` si no va por EI: los mandos), sin
    /// escribir nada. Falla si el compositor no ofrece uno activo con la capacidad que hace falta.
    fn target<'s>(
        state: &'s EiState,
        area: &PointerArea,
        event: &InputEvent,
    ) -> Result<Option<&'s EiDevice>, InputError> {
        let capability = match *event {
            InputEvent::Key { .. } | InputEvent::Text { .. } => DeviceCapability::Keyboard,
            InputEvent::MouseButton { .. } => DeviceCapability::Button,
            InputEvent::MouseMove { .. } => DeviceCapability::Pointer,
            InputEvent::MouseMoveAbsolute { .. } => match area {
                PointerArea::Window { .. } => DeviceCapability::Pointer,
                _ => DeviceCapability::PointerAbsolute,
            },
            InputEvent::Wheel { .. } => DeviceCapability::Scroll,
            // Los mandos no van por EI.
            InputEvent::GamepadConnected { .. }
            | InputEvent::GamepadDisconnected { .. }
            | InputEvent::GamepadButton { .. }
            | InputEvent::GamepadAxis { .. }
            | InputEvent::ReleaseAll => return Ok(None),
        };
        Self::device(state, capability).map(Some).ok_or_else(|| {
            InputError::NotReady(format!(
                "el compositor no ofrece un dispositivo de EI con {capability:?} activo"
            ))
        })
    }

    /// El primer dispositivo activo con `capability`.
    fn device(state: &EiState, capability: DeviceCapability) -> Option<&EiDevice> {
        state
            .devices
            .iter()
            .find(|device| device.resumed && device.device.has_capability(capability))
    }

    /// Cómo teclear `ch` con el mapa del teclado de EI (el mismo al que irá:
    /// [`EiInput::target`]) contra su estado (distribución activa y modificadores bloqueados y
    /// enganchados, de `ei_keyboard.modifiers`) y lo que `held` dice que tienen pulsado los
    /// invitados, sin soltar nada de eso. `None` (con una línea en el registro) si no se puede:
    /// sin teclado, sin mapa o sin forma de dar el carácter.
    fn text_typing(&self, ch: char, held: impl Fn(u16) -> bool) -> Option<Typing> {
        let state = lock(&self.state);
        let Some(keyboard) = Self::device(&state, DeviceCapability::Keyboard) else {
            debug!(?ch, "EI: sin teclado activo; el texto se descarta");
            return None;
        };
        let Some(keymap) = &keyboard.text else {
            debug!(
                ?ch,
                "EI: el teclado no anuncia mapa XKB; el texto se descarta"
            );
            return None;
        };
        let typing = keymap.lookup(ch, keyboard.keyboard, held);
        if typing.is_none() {
            debug!(
                ?ch,
                state = ?keyboard.keyboard,
                "EI: el carácter no se puede teclear en la distribución activa con lo pulsado; \
                 se descarta"
            );
        }
        typing
    }

    /// Teclea `typing` en `device`: por cada pulsación, sus modificadores y la tecla en una trama
    /// y la tecla y los modificadores sueltos en orden inverso en la siguiente (la última la
    /// cierra quien llama), como si se pulsara y soltara en un teclado; con tecla muerta, la
    /// muerta y luego la otra.
    fn type_text(device: &reis::event::Device, typing: &Typing, serial: u32, time: u64) {
        let Some(keyboard) = device.interface::<ei::Keyboard>() else {
            return;
        };
        for (index, stroke) in typing.strokes().iter().enumerate() {
            if index > 0 {
                device.device().frame(serial, time);
            }
            for modifier in stroke.modifiers() {
                keyboard.key(u32::from(modifier), ei::keyboard::KeyState::Press);
            }
            keyboard.key(u32::from(stroke.key), ei::keyboard::KeyState::Press);
            device.device().frame(serial, time);
            keyboard.key(u32::from(stroke.key), ei::keyboard::KeyState::Released);
            for modifier in stroke.modifiers().rev() {
                keyboard.key(u32::from(modifier), ei::keyboard::KeyState::Released);
            }
        }
    }

    /// Pone `event` en la cola de `device`, el que le dio [`EiInput::target`] (sin `frame`).
    fn queue(&self, device: &reis::event::Device, event: &InputEvent) {
        match *event {
            InputEvent::Key { code, pressed } => {
                if let Some(keyboard) = device.interface::<ei::Keyboard>() {
                    let state = if pressed {
                        ei::keyboard::KeyState::Press
                    } else {
                        ei::keyboard::KeyState::Released
                    };
                    keyboard.key(u32::from(code), state);
                }
            }
            InputEvent::MouseButton { button, pressed } => {
                let code = match button {
                    MouseButton::Left => codes::BTN_LEFT,
                    MouseButton::Right => codes::BTN_RIGHT,
                    MouseButton::Middle => codes::BTN_MIDDLE,
                    MouseButton::Back => codes::BTN_SIDE,
                    MouseButton::Forward => codes::BTN_EXTRA,
                };
                if let Some(buttons) = device.interface::<ei::Button>() {
                    let state = if pressed {
                        ei::button::ButtonState::Press
                    } else {
                        ei::button::ButtonState::Released
                    };
                    buttons.button(u32::from(code), state);
                }
            }
            InputEvent::MouseMove { dx, dy } => Self::motion(device, dx, dy),
            InputEvent::MouseMoveAbsolute { x, y } => {
                if let PointerArea::Window { width, height } = &self.area {
                    let (width, height) = (*width, *height);
                    let last = lock(&self.last_absolute).replace((x, y));
                    if let Some((dx, dy)) = window_delta(last, (x, y), (width, height)) {
                        Self::motion(device, dx, dy);
                    }
                } else {
                    self.absolute(device, x, y);
                }
            }
            InputEvent::Wheel { dx, dy } => {
                if let Some(scroll) = device.interface::<ei::Scroll>() {
                    // En EI el positivo es hacia abajo; en el cable, hacia arriba.
                    scroll.scroll_discrete(dx, dy.saturating_neg());
                }
            }
            // El texto lo teclea `type_text`, con lo que resolvió `text_typing`.
            InputEvent::Text { .. }
            | InputEvent::GamepadConnected { .. }
            | InputEvent::GamepadDisconnected { .. }
            | InputEvent::GamepadButton { .. }
            | InputEvent::GamepadAxis { .. }
            | InputEvent::ReleaseAll => {}
        }
    }

    fn motion(device: &reis::event::Device, dx: i32, dy: i32) {
        if let Some(pointer) = device.interface::<ei::Pointer>() {
            pointer.motion_relative(dx as f32, dy as f32);
        }
    }

    /// Posición absoluta del invitado (`0..=65535` del fotograma) en la región del monitor
    /// capturado del dispositivo absoluto `device`.
    fn absolute(&self, device: &reis::event::Device, x: u16, y: u16) {
        let Some(pointer) = device.interface::<ei::PointerAbsolute>() else {
            return;
        };
        let regions = device.regions();
        let region = monitor_region(regions, &self.area);
        if region.is_none_or(|(_, exact)| !exact) && regions.len() > 1 {
            // Con varios monitores, la primera región puede no ser la compartida: el puntero
            // caería en otra pantalla. Se avisa una vez para que el registro lo cuente.
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                tracing::warn!(
                    area = ?self.area,
                    regions = regions.len(),
                    "EI: ninguna región coincide con el monitor compartido; se usa la primera"
                );
            });
        }
        let (ox, oy, w, h) = region
            .map(|(region, _)| (region.x, region.y, region.width, region.height))
            .or_else(|| device.dimensions().map(|(w, h)| (0, 0, w, h)))
            .unwrap_or((0, 0, 1920, 1080));
        let scale = |value: u16, size: u32| {
            f64::from(value) / f64::from(u16::MAX) * f64::from(size.saturating_sub(1))
        };
        pointer.motion_absolute(
            (f64::from(ox) + scale(x, w)) as f32,
            (f64::from(oy) + scale(y, h)) as f32,
        );
    }
}

/// Región de EI del monitor compartido: la de su `mapping_id` (lo que el portal y el compositor
/// acuerdan para casar flujo y región), si no la que empieza donde el portal dice que está el
/// monitor y, sin ninguna de las dos, la primera. El `bool` dice si se encontró la exacta.
fn monitor_region<'r>(
    regions: &'r [reis::event::Region],
    area: &PointerArea,
) -> Option<(&'r reis::event::Region, bool)> {
    let PointerArea::Monitor { x, y, mapping_id } = area else {
        return regions.first().map(|region| (region, false));
    };
    if let Some(id) = mapping_id.as_deref()
        && let Some(region) = regions
            .iter()
            .find(|region| region.mapping_id.as_deref() == Some(id))
    {
        return Some((region, true));
    }
    // Por posición solo es seguro si el portal no dio identificador: con uno que ninguna región
    // conoce, la posición puede ser la (0, 0) que se pone cuando el portal no la da.
    let by_position = regions.iter().find(|region| {
        i64::from(region.x) == i64::from(*x) && i64::from(region.y) == i64::from(*y)
    });
    match by_position {
        Some(region) => Some((region, mapping_id.is_none())),
        None => regions.first().map(|region| (region, false)),
    }
}

/// Movimiento relativo, en píxeles lógicos de una ventana de `size`, entre dos posiciones
/// absolutas del invitado (`0..=65535`). La primera posición solo sirve de referencia.
fn window_delta(last: Option<(u16, u16)>, now: (u16, u16), size: (u32, u32)) -> Option<(i32, i32)> {
    let (lx, ly) = last?;
    let step = |from: u16, to: u16, side: u32| {
        let delta = (f64::from(to) - f64::from(from)) / f64::from(u16::MAX) * f64::from(side);
        delta.round() as i32
    };
    let delta = (step(lx, now.0, size.0), step(ly, now.1, size.1));
    (delta != (0, 0)).then_some(delta)
}

/// Hilo `ei`: *handshake*, asientos y dispositivos.
fn ei_loop(
    context: &ei::Context,
    state: &Mutex<EiState>,
    ready: &mpsc::Sender<Result<(), String>>,
) {
    let (connection, events) =
        match context.handshake_blocking("coroom", ei::handshake::ContextType::Sender) {
            Ok(pair) => pair,
            Err(err) => {
                let _ = ready.send(Err(format!("el handshake de EI falló: {err}")));
                return;
            }
        };
    lock(state).connection = Some(connection.clone());
    let mut announced = false;
    for event in events {
        let event = match event {
            Ok(event) => event,
            Err(err) => {
                lock(state).disconnected = Some(err.to_string());
                break;
            }
        };
        match event {
            EiEvent::SeatAdded(added) => {
                added.seat.bind_capabilities(
                    DeviceCapability::Pointer
                        | DeviceCapability::PointerAbsolute
                        | DeviceCapability::Keyboard
                        | DeviceCapability::Scroll
                        | DeviceCapability::Button,
                );
                let _ = connection.flush();
            }
            EiEvent::DeviceAdded(added) => {
                debug!(name = ?added.device.name(), "EI: dispositivo");
                lock(state).devices.push(EiDevice::new(added.device));
            }
            EiEvent::DeviceResumed(resumed) => {
                let mut state = lock(state);
                let sequence = state.sequence;
                state.sequence = state.sequence.wrapping_add(1);
                resumed
                    .device
                    .device()
                    .start_emulating(resumed.serial, sequence);
                for device in &mut state.devices {
                    if device.device == resumed.device {
                        device.resumed = true;
                    }
                }
                drop(state);
                let _ = connection.flush();
                if !announced {
                    announced = true;
                    let _ = ready.send(Ok(()));
                }
            }
            EiEvent::DevicePaused(paused) => {
                for device in &mut lock(state).devices {
                    if device.device == paused.device {
                        device.resumed = false;
                    }
                }
            }
            EiEvent::KeyboardModifiers(modifiers) => {
                for device in &mut lock(state).devices {
                    if device.device == modifiers.device {
                        device.keyboard = KeyboardState {
                            group: modifiers.group,
                            latched: modifiers.latched,
                            locked: modifiers.locked,
                        };
                    }
                }
            }
            EiEvent::DeviceRemoved(removed) => {
                lock(state)
                    .devices
                    .retain(|device| device.device != removed.device);
            }
            EiEvent::Disconnected(disconnected) => {
                lock(state).disconnected = Some(
                    disconnected
                        .explanation
                        .unwrap_or_else(|| format!("{:?}", disconnected.reason)),
                );
                break;
            }
            _ => {}
        }
    }
    let mut state = lock(state);
    state.devices.clear();
    state
        .disconnected
        .get_or_insert_with(|| "conexión cerrada".to_owned());
}

/// Sesión del portal `RemoteDesktop` propia, abierta en el hilo `portal-entrada`.
struct PortalGuard {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for PortalGuard {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

fn portal_error(err: ashpd::Error) -> String {
    match err {
        ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => {
            "el usuario no concedió el control remoto (portal RemoteDesktop)".to_owned()
        }
        ashpd::Error::PortalNotFound(_) => "no hay portal RemoteDesktop en este escritorio".into(),
        ashpd::Error::RequiresVersion(need, have) => format!(
            "el portal RemoteDesktop es la versión {have} y libei necesita la {need} \
             (xdg-desktop-portal 1.17 o posterior)"
        ),
        ashpd::Error::Zbus(err) => {
            format!("no se pudo hablar con el portal por el bus de sesión D-Bus ({err})")
        }
        other => format!("portal RemoteDesktop: {other}"),
    }
}

/// Abre una sesión `RemoteDesktop` solo de teclado y puntero y devuelve el socket de EI.
fn own_portal() -> Result<(OwnedFd, PortalGuard), String> {
    use ashpd::desktop::PersistMode;
    use ashpd::desktop::remote_desktop::{DeviceType, RemoteDesktop, SelectDevicesOptions};

    let (ready_tx, ready_rx) = mpsc::channel::<Result<OwnedFd, String>>();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    thread::Builder::new()
        .name("portal-entrada".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    let _ = ready_tx.send(Err(format!("runtime del portal: {err}")));
                    return;
                }
            };
            runtime.block_on(async move {
                let opened = async {
                    let proxy = RemoteDesktop::new().await.map_err(portal_error)?;
                    let session = proxy
                        .create_session(Default::default())
                        .await
                        .map_err(portal_error)?;
                    proxy
                        .select_devices(
                            &session,
                            SelectDevicesOptions::default()
                                .set_devices(DeviceType::Keyboard | DeviceType::Pointer)
                                .set_persist_mode(PersistMode::DoNot),
                        )
                        .await
                        .map_err(portal_error)?
                        .response()
                        .map_err(portal_error)?;
                    proxy
                        .start(&session, None, Default::default())
                        .await
                        .map_err(portal_error)?
                        .response()
                        .map_err(portal_error)?;
                    let fd = proxy
                        .connect_to_eis(&session, Default::default())
                        .await
                        .map_err(portal_error)?;
                    Ok::<_, String>((fd, proxy, session))
                };
                match opened.await {
                    Ok((fd, _proxy, session)) => {
                        if ready_tx.send(Ok(fd)).is_ok() {
                            let _ = stop_rx.await;
                        }
                        let _ = session.close().await;
                    }
                    Err(err) => {
                        let _ = ready_tx.send(Err(err));
                    }
                }
            });
        })
        .map_err(|err| format!("no se pudo crear el hilo del portal: {err}"))?;
    let fd = ready_rx
        .recv()
        .map_err(|_| "el hilo del portal terminó sin responder".to_owned())??;
    Ok((
        fd,
        PortalGuard {
            stop: Some(stop_tx),
        },
    ))
}

enum EiBackend {
    Idle(Option<EisProvider>),
    Connecting(mpsc::Receiver<Result<EiInput, String>>),
    Ready(EiInput),
    Failed(String),
}

impl EiBackend {
    fn start(&mut self) {
        let EiBackend::Idle(provider) = self else {
            return;
        };
        let provider = provider.take();
        let (tx, rx) = mpsc::channel();
        let spawned = thread::Builder::new()
            .name("ei-conexión".into())
            .spawn(move || {
                let connected = match provider {
                    Some(provider) => provider()
                        .and_then(|EisConnection { fd, area }| EiInput::connect(fd, area, None)),
                    None => own_portal().and_then(|(fd, guard)| {
                        EiInput::connect(fd, PointerArea::Unknown, Some(guard))
                    }),
                };
                let _ = tx.send(connected);
            });
        *self = match spawned {
            Ok(_) => EiBackend::Connecting(rx),
            Err(err) => EiBackend::Failed(format!("no se pudo crear el hilo de EI: {err}")),
        };
    }

    fn ready(&mut self) -> Result<&EiInput, InputError> {
        self.start();
        if let EiBackend::Connecting(rx) = self {
            match rx.try_recv() {
                Ok(Ok(input)) => *self = EiBackend::Ready(input),
                Ok(Err(err)) => *self = EiBackend::Failed(err),
                Err(mpsc::TryRecvError::Empty) => {
                    return Err(InputError::NotReady(
                        "esperando a que el usuario acepte el portal RemoteDesktop".into(),
                    ));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    *self = EiBackend::Failed("el hilo de EI terminó sin responder".into());
                }
            }
        }
        match self {
            EiBackend::Ready(input) => Ok(input),
            EiBackend::Failed(err) => Err(InputError::Unsupported(err.clone())),
            EiBackend::Idle(_) | EiBackend::Connecting(_) => Err(InputError::NotReady(
                "conectando con el portal RemoteDesktop".into(),
            )),
        }
    }
}

/// Estado de la entrada del anfitrión para `coroom doctor`: versión del portal `RemoteDesktop` y
/// acceso a `/dev/uinput`, sin abrir sesión ni pedir permiso.
pub fn status() -> Vec<String> {
    use ashpd::desktop::remote_desktop::{DeviceType, RemoteDesktop};

    let portal = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| err.to_string())
        .and_then(|runtime| {
            runtime.block_on(async {
                let probe = async {
                    let proxy = RemoteDesktop::new().await.map_err(portal_error)?;
                    let devices = proxy.available_device_types().await.unwrap_or_default();
                    Ok::<_, String>((proxy.version(), devices))
                };
                tokio::time::timeout(Duration::from_secs(3), probe)
                    .await
                    .unwrap_or_else(|_| Err("el portal RemoteDesktop no respondió a tiempo".into()))
            })
        });
    let keyboard = match portal {
        Ok((version, devices)) => {
            let mut kinds = Vec::new();
            if devices.contains(DeviceType::Keyboard) {
                kinds.push("teclado");
            }
            if devices.contains(DeviceType::Pointer) {
                kinds.push("puntero");
            }
            format!(
                "teclado y ratón: portal RemoteDesktop versión {version} ({}); libei {}",
                if kinds.is_empty() {
                    "sin dispositivos anunciados".to_owned()
                } else {
                    kinds.join(" y ")
                },
                if version >= 2 {
                    "disponible (ConnectToEIS)"
                } else {
                    "no disponible: hace falta la versión 2 (xdg-desktop-portal 1.17)"
                }
            )
        }
        Err(err) => format!("teclado y ratón: no disponible ({err})"),
    };
    let pads = match std::fs::OpenOptions::new().write(true).open("/dev/uinput") {
        Ok(_) => "mandos: /dev/uinput accesible (uinput, como mandos de Xbox 360)".to_owned(),
        Err(err) => {
            format!("mandos: /dev/uinput no accesible ({err}); instala la regla udev del README")
        }
    };
    vec![keyboard, pads]
}

// --- uinput -----------------------------------------------------------------------------------

/// Mando virtual de una ranura.
struct VirtualPad {
    device: VirtualDevice,
    /// Cruceta: arriba, abajo, izquierda, derecha.
    dpad: [bool; 4],
    /// Los efectos de vibración que le subió el juego.
    rumble: UinputRumble,
    /// Si ya se avisó de que no se puede leer su vibración (una vez por mando).
    ff_warned: bool,
}

/// Efectos de vibración que puede tener subidos a la vez cada mando (los de `ff-memless`, el de
/// `xpad`).
pub const FF_EFFECTS_MAX: u32 = 16;

/// Vueltas de lectura del protocolo de *force feedback* por llamada, como mucho (cada una lee
/// hasta 32 eventos): lo que quede se lee en la siguiente.
const FF_READS: usize = 8;

fn uinput_error(err: std::io::Error) -> InputError {
    match err.kind() {
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound => {
            InputError::Uinput(UINPUT_HELP.to_owned())
        }
        _ => InputError::Uinput(format!("no se pudo crear el mando virtual: {err}")),
    }
}

/// Identidad de un mando uinput: nombre, VID, PID y versión.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PadIdentity {
    /// Nombre.
    pub name: String,
    /// Fabricante.
    pub vendor: u16,
    /// Producto.
    pub product: u16,
    /// Versión.
    pub version: u16,
}

impl PadIdentity {
    /// La de siempre: «coroom Gamepad N» con el VID/PID del mando de Xbox 360.
    pub fn converted(slot: u8) -> Self {
        Self {
            name: format!("coroom Gamepad {slot}"),
            vendor: GAMEPAD_VENDOR,
            product: GAMEPAD_PRODUCT,
            version: 0x0110,
        }
    }
}

/// Crea el dispositivo uinput de la ranura `slot`.
pub fn create_gamepad(slot: u8) -> Result<VirtualDevice, InputError> {
    create_gamepad_as(&PadIdentity::converted(slot))
}

/// Crea un dispositivo uinput con la disposición de `xpad` y la identidad `identity` (la del
/// mando XInput de un invitado, tal cual).
pub fn create_gamepad_as(identity: &PadIdentity) -> Result<VirtualDevice, InputError> {
    let mut keys = AttributeSet::<KeyCode>::new();
    for key in [
        KeyCode::BTN_SOUTH,
        KeyCode::BTN_EAST,
        KeyCode::BTN_NORTH,
        KeyCode::BTN_WEST,
        KeyCode::BTN_TL,
        KeyCode::BTN_TR,
        KeyCode::BTN_SELECT,
        KeyCode::BTN_START,
        KeyCode::BTN_MODE,
        KeyCode::BTN_THUMBL,
        KeyCode::BTN_THUMBR,
    ] {
        keys.insert(key);
    }
    let stick = AbsInfo::new(0, -32768, 32767, 16, 128, 0);
    let trigger = AbsInfo::new(0, 0, 255, 0, 0, 0);
    let hat = AbsInfo::new(0, -1, 1, 0, 0, 0);
    let mut builder = VirtualDevice::builder()
        .map_err(uinput_error)?
        .name(&identity.name)
        .input_id(InputId::new(
            BusType::BUS_USB,
            identity.vendor,
            identity.product,
            identity.version,
        ))
        .with_keys(&keys)
        .map_err(uinput_error)?;
    for (axis, info) in [
        (AbsoluteAxisCode::ABS_X, stick),
        (AbsoluteAxisCode::ABS_Y, stick),
        (AbsoluteAxisCode::ABS_RX, stick),
        (AbsoluteAxisCode::ABS_RY, stick),
        (AbsoluteAxisCode::ABS_Z, trigger),
        (AbsoluteAxisCode::ABS_RZ, trigger),
        (AbsoluteAxisCode::ABS_HAT0X, hat),
        (AbsoluteAxisCode::ABS_HAT0Y, hat),
    ] {
        builder = builder
            .with_absolute_axis(&evdev::UinputAbsSetup::new(axis, info))
            .map_err(uinput_error)?;
    }
    let mut ff = AttributeSet::<FFEffectCode>::new();
    ff.insert(FFEffectCode::FF_RUMBLE);
    ff.insert(FFEffectCode::FF_GAIN);
    builder = builder
        .with_ff(&ff)
        .map_err(uinput_error)?
        .with_ff_effects_max(FF_EFFECTS_MAX);
    builder.build().map_err(uinput_error)
}

// --- Vibración (force feedback de uinput) ------------------------------------------------------

/// Un efecto `FF_RUMBLE` subido por el juego.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RumbleEffect {
    rumble: Rumble,
    /// `replay.length`: lo que dura cada reproducción (cero: hasta que se pare).
    length: Duration,
    /// `replay.delay`: la espera antes de cada reproducción.
    delay: Duration,
}

/// Un efecto que se está reproduciendo: desde cuándo y cuántas veces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Playing {
    since: Instant,
    count: u32,
}

/// El estado de los motores de un mando uinput a partir del protocolo de *force feedback*, sin
/// tocar el dispositivo (lo lee [`VirtualPad::service_ff`]). Como `ff-memless`, el módulo de los
/// mandos reales: los efectos que suenan se suman (saturando) escalados por la ganancia
/// (`FF_GAIN`); cada reproducción espera su `delay` y dura su `length` (cero: hasta que se pare), y
/// se repite las veces que diga `EV_FF`. Parar, borrar o que se acabe la última reproducción deja
/// los motores en lo que sumen los demás (sin ninguno, parados).
#[derive(Debug, Clone)]
pub struct UinputRumble {
    effects: BTreeMap<i16, RumbleEffect>,
    playing: BTreeMap<i16, Playing>,
    gain: u16,
    /// Lo último que se dio con [`UinputRumble::poll`].
    reported: Rumble,
}

impl Default for UinputRumble {
    fn default() -> Self {
        Self {
            effects: BTreeMap::new(),
            playing: BTreeMap::new(),
            gain: u16::MAX,
            reported: Rumble::OFF,
        }
    }
}

impl UinputRumble {
    /// `UI_FF_UPLOAD`: el efecto `id` (nuevo o sustituido) con esos motores y su programación en
    /// ms. Sustituir uno que suena cambia lo que suena sin reiniciarlo.
    pub fn upload(&mut self, id: i16, rumble: Rumble, length_ms: u16, delay_ms: u16) {
        self.effects.insert(
            id,
            RumbleEffect {
                rumble,
                length: Duration::from_millis(u64::from(length_ms)),
                delay: Duration::from_millis(u64::from(delay_ms)),
            },
        );
    }

    /// `UI_FF_ERASE`: el efecto `id` deja de existir (y de sonar).
    pub fn erase(&mut self, id: i16) {
        self.effects.remove(&id);
        self.playing.remove(&id);
    }

    /// `EV_FF` con el id de un efecto: lo reproduce `count` veces desde `now` (cero lo para).
    pub fn play(&mut self, id: i16, count: i32, now: Instant) {
        match u32::try_from(count) {
            Ok(count) if count > 0 && self.effects.contains_key(&id) => {
                self.playing.insert(id, Playing { since: now, count });
            }
            _ => {
                self.playing.remove(&id);
            }
        }
    }

    /// `EV_FF` con `FF_GAIN`: la ganancia de todos los efectos (`0xFFFF` es la de partida).
    pub fn set_gain(&mut self, gain: i32) {
        self.gain = u16::try_from(gain.clamp(0, i32::from(u16::MAX))).unwrap_or(u16::MAX);
    }

    /// Lo que suman los efectos que suenan en `now`; quita los que ya acabaron.
    pub fn current(&mut self, now: Instant) -> Rumble {
        let effects = &self.effects;
        self.playing.retain(|id, playing| {
            effects
                .get(id)
                .is_some_and(|effect| Self::phase(effect, playing, now).is_some())
        });
        let gain = u32::from(self.gain);
        let scale = |value: u16| u32::from(value) * gain / u32::from(u16::MAX);
        let (mut strong, mut weak) = (0u32, 0u32);
        for (id, playing) in &self.playing {
            let Some(effect) = self.effects.get(id) else {
                continue;
            };
            if Self::phase(effect, playing, now) == Some(true) {
                strong += scale(effect.rumble.strong);
                weak += scale(effect.rumble.weak);
            }
        }
        let saturate = |value: u32| u16::try_from(value).unwrap_or(u16::MAX);
        Rumble::new(saturate(strong), saturate(weak))
    }

    /// `None` si la reproducción ya acabó; si no, si suena en `now` (`false`: en su `delay`).
    fn phase(effect: &RumbleEffect, playing: &Playing, now: Instant) -> Option<bool> {
        let elapsed = now.saturating_duration_since(playing.since);
        if effect.length.is_zero() {
            // Sin duración: suena desde su `delay` hasta que lo paren.
            return Some(elapsed >= effect.delay);
        }
        let period = effect.length + effect.delay;
        let round = elapsed.as_nanos() / period.as_nanos();
        if round >= u128::from(playing.count) {
            return None;
        }
        let into = elapsed.as_nanos() % period.as_nanos();
        Some(into >= effect.delay.as_nanos())
    }

    /// El estado de los motores en `now` si cambió desde la última vez.
    pub fn poll(&mut self, now: Instant) -> Option<Rumble> {
        let current = self.current(now);
        (current != self.reported).then(|| {
            self.reported = current;
            current
        })
    }

    /// Lo último que se dio con [`UinputRumble::poll`].
    pub fn reported(&self) -> Rumble {
        self.reported
    }
}

fn button_code(button: GamepadButton) -> Option<KeyCode> {
    Some(match button {
        GamepadButton::A => KeyCode::BTN_SOUTH,
        GamepadButton::B => KeyCode::BTN_EAST,
        GamepadButton::X => KeyCode::BTN_NORTH,
        GamepadButton::Y => KeyCode::BTN_WEST,
        GamepadButton::LeftBumper => KeyCode::BTN_TL,
        GamepadButton::RightBumper => KeyCode::BTN_TR,
        GamepadButton::Back => KeyCode::BTN_SELECT,
        GamepadButton::Start => KeyCode::BTN_START,
        GamepadButton::Guide => KeyCode::BTN_MODE,
        GamepadButton::LeftStick => KeyCode::BTN_THUMBL,
        GamepadButton::RightStick => KeyCode::BTN_THUMBR,
        GamepadButton::DPadUp
        | GamepadButton::DPadDown
        | GamepadButton::DPadLeft
        | GamepadButton::DPadRight => return None,
    })
}

/// Valor uinput de un eje del cable (Y de los sticks invertida, gatillos a 0..255).
pub fn axis_value(axis: GamepadAxis, value: i16) -> (AbsoluteAxisCode, i32) {
    let flip = |value: i16| -i32::from(value).clamp(-32767, 32767);
    match axis {
        GamepadAxis::LeftX => (AbsoluteAxisCode::ABS_X, i32::from(value)),
        GamepadAxis::LeftY => (AbsoluteAxisCode::ABS_Y, flip(value)),
        GamepadAxis::RightX => (AbsoluteAxisCode::ABS_RX, i32::from(value)),
        GamepadAxis::RightY => (AbsoluteAxisCode::ABS_RY, flip(value)),
        GamepadAxis::LeftTrigger => (
            AbsoluteAxisCode::ABS_Z,
            i32::from(value.max(0)) * 255 / 32767,
        ),
        GamepadAxis::RightTrigger => (
            AbsoluteAxisCode::ABS_RZ,
            i32::from(value.max(0)) * 255 / 32767,
        ),
    }
}

impl VirtualPad {
    fn new(device: VirtualDevice) -> Self {
        Self {
            device,
            dpad: [false; 4],
            rumble: UinputRumble::default(),
            ff_warned: false,
        }
    }

    /// Atiende sin bloquear lo que el juego mandó al mando (subidas y borrados de efectos, que lo
    /// esperan, y reproducciones) y devuelve el estado de los motores si cambió.
    fn service_ff(&mut self, now: Instant) -> Result<Option<Rumble>, InputError> {
        let failed =
            |err: std::io::Error| InputError::Backend(format!("uinput (vibración): {err}"));
        for _ in 0..FF_READS {
            let mut fds = [rustix::event::PollFd::new(
                &self.device,
                rustix::event::PollFlags::IN,
            )];
            let ready = rustix::event::poll(&mut fds, Some(&rustix::event::Timespec::default()))
                .map_err(|err| failed(err.into()))?;
            if ready == 0 {
                break;
            }
            let events: Vec<evdev::InputEvent> =
                self.device.fetch_events().map_err(failed)?.collect();
            if events.is_empty() {
                break;
            }
            for event in events {
                match event.destructure() {
                    EventSummary::UInput(request, UInputCode::UI_FF_UPLOAD, _) => {
                        let mut upload = self.device.process_ff_upload(request).map_err(failed)?;
                        let effect = upload.effect();
                        match effect.kind {
                            FFEffectKind::Rumble {
                                strong_magnitude,
                                weak_magnitude,
                            } => {
                                self.rumble.upload(
                                    upload.effect_id(),
                                    Rumble::new(strong_magnitude, weak_magnitude),
                                    effect.replay.length,
                                    effect.replay.delay,
                                );
                                upload.set_retval(0);
                            }
                            // El mando solo declara `FF_RUMBLE`: el kernel no deja subir otros.
                            _ => upload.set_retval(-rustix::io::Errno::INVAL.raw_os_error()),
                        }
                        // Al soltar `upload` se contesta al juego (`UI_END_FF_UPLOAD`).
                    }
                    EventSummary::UInput(request, UInputCode::UI_FF_ERASE, _) => {
                        let erase = self.device.process_ff_erase(request).map_err(failed)?;
                        self.rumble
                            .erase(i16::try_from(erase.effect_id()).unwrap_or(-1));
                    }
                    EventSummary::ForceFeedback(_, FFEffectCode::FF_GAIN, value) => {
                        self.rumble.set_gain(value);
                    }
                    EventSummary::ForceFeedback(_, code, value) => {
                        self.rumble
                            .play(i16::try_from(code.0).unwrap_or(-1), value, now);
                    }
                    _ => {}
                }
            }
        }
        Ok(self.rumble.poll(now))
    }

    /// Añade a `out` los eventos evdev de `event` (sin `SYN_REPORT`).
    fn queue(&mut self, event: &InputEvent, out: &mut Vec<evdev::InputEvent>) {
        match *event {
            InputEvent::GamepadButton {
                button, pressed, ..
            } => {
                if let Some(code) = button_code(button) {
                    out.push(evdev::InputEvent::new(
                        EventType::KEY.0,
                        code.0,
                        i32::from(pressed),
                    ));
                    return;
                }
                let index = match button {
                    GamepadButton::DPadUp => 0,
                    GamepadButton::DPadDown => 1,
                    GamepadButton::DPadLeft => 2,
                    _ => 3,
                };
                self.dpad[index] = pressed;
                let [up, down, left, right] = self.dpad.map(i32::from);
                out.push(evdev::InputEvent::new(
                    EventType::ABSOLUTE.0,
                    AbsoluteAxisCode::ABS_HAT0X.0,
                    right - left,
                ));
                out.push(evdev::InputEvent::new(
                    EventType::ABSOLUTE.0,
                    AbsoluteAxisCode::ABS_HAT0Y.0,
                    down - up,
                ));
            }
            InputEvent::GamepadAxis { axis, value, .. } => {
                let (code, value) = axis_value(axis, value);
                out.push(evdev::InputEvent::new(EventType::ABSOLUTE.0, code.0, value));
            }
            _ => {}
        }
    }

    /// Escribe `events` en una sola trama (`evdev` añade el `SYN_REPORT`) y la vacía.
    fn flush(&mut self, events: &mut Vec<evdev::InputEvent>) -> Result<(), InputError> {
        if events.is_empty() {
            return Ok(());
        }
        let result = self
            .device
            .emit(events)
            .map_err(|err| InputError::Backend(format!("uinput: {err}")));
        events.clear();
        result
    }

    /// Un evento en su propia trama.
    #[cfg(test)]
    fn emit(&mut self, event: &InputEvent) -> Result<(), InputError> {
        let mut events = Vec::with_capacity(2);
        self.queue(event, &mut events);
        self.flush(&mut events)
    }
}

// --- Sumidero ---------------------------------------------------------------------------------

/// Sumidero de Linux: libei para teclado y ratón, uinput para los mandos y UHID para los mandos
/// HID tal cual.
pub struct LinuxSink {
    ei: EiBackend,
    pads: BTreeMap<u8, VirtualPad>,
    /// Mandos HID tal cual (en su ranura no hay `VirtualPad`).
    raw: BTreeMap<u8, UhidPad>,
    /// Ranuras con un `VirtualPad` que lleva la identidad del mando XInput del invitado.
    xinput: BTreeSet<u8>,
    /// Paradas de los motores de mandos que se quitaron o sustituyeron vibrando, para la próxima
    /// [`InputSink::take_rumble`].
    rumble_stops: Vec<(u8, Rumble)>,
    pressed: PressedKeys,
    pad_state: PadTracker,
    /// Eventos de teclado y ratón que hay que escribir de un grupo (reutilizado).
    kbm_scratch: Vec<InputEvent>,
    /// Cómo teclear cada [`InputEvent::Text`] de `kbm_scratch`, en orden (reutilizado).
    text_scratch: Vec<Typing>,
    /// Eventos evdev de un grupo para una ranura (reutilizado).
    evdev_scratch: Vec<evdev::InputEvent>,
}

impl LinuxSink {
    /// Sumidero perezoso: con `eis` usa la sesión del portal de la captura; sin ella abre la suya.
    pub fn new(eis: Option<EisProvider>) -> Self {
        Self {
            ei: EiBackend::Idle(eis),
            pads: BTreeMap::new(),
            raw: BTreeMap::new(),
            xinput: BTreeSet::new(),
            rumble_stops: Vec::new(),
            pressed: PressedKeys::default(),
            pad_state: PadTracker::default(),
            kbm_scratch: Vec::with_capacity(64),
            text_scratch: Vec::new(),
            evdev_scratch: Vec::with_capacity(32),
        }
    }

    /// Un grupo seguido de eventos de teclado y ratón de `guest`: los que cambian algo en el
    /// sistema según [`PressedKeys`] salen en una trama de EI. Devuelve cuántos se aplicaron (todos
    /// o ninguno).
    fn kbm_batch(
        &mut self,
        run: &[InputEvent],
        guest: GuestId,
        errors: &mut Vec<InputError>,
    ) -> usize {
        self.kbm_scratch.clear();
        self.text_scratch.clear();
        // El texto se resuelve con el mapa del teclado de EI, que tiene que estar listo.
        let ei = if run
            .iter()
            .any(|event| matches!(event, InputEvent::Text { .. }))
        {
            match self.ei.ready() {
                Ok(ei) => Some(ei),
                Err(err) => {
                    errors.push(err);
                    return 0;
                }
            }
        } else {
            None
        };
        for event in run {
            let write = match *event {
                InputEvent::Key { code, pressed } => self.pressed.key(guest, code, pressed),
                InputEvent::MouseButton { button, pressed } => {
                    self.pressed.button(guest, button, pressed)
                }
                // Con lo pulsado hasta aquí: una Mayúscula que el invitado pulsó antes en el
                // paquete ya cuenta. Lo que no se puede teclear no se escribe (y cuenta como
                // aplicado: no hay nada que reintentar).
                InputEvent::Text { ch } => {
                    let pressed = &self.pressed;
                    match ei.and_then(|ei| ei.text_typing(ch, |code| pressed.is_held(code))) {
                        Some(stroke) => {
                            self.text_scratch.push(stroke);
                            true
                        }
                        None => false,
                    }
                }
                _ => true,
            };
            if write {
                self.kbm_scratch.push(*event);
            }
        }
        if self.kbm_scratch.is_empty() {
            // Nada que escribir: otro invitado ya tenía pulsado lo mismo, o el texto no se puede
            // teclear.
            return run.len();
        }
        let (scratch, strokes) = (&self.kbm_scratch, &self.text_scratch);
        match self
            .ei
            .ready()
            .and_then(|ei| ei.inject_many(scratch, strokes))
        {
            Ok(()) => run.len(),
            Err(err) => {
                // No llegó al sistema: se deshacen las anotaciones, en orden inverso (una tecla
                // pulsada y soltada en el mismo grupo queda como estaba).
                for event in self.kbm_scratch.iter().rev() {
                    match *event {
                        InputEvent::Key { code, pressed } => {
                            self.pressed.key(guest, code, !pressed);
                        }
                        InputEvent::MouseButton { button, pressed } => {
                            self.pressed.button(guest, button, !pressed);
                        }
                        _ => {}
                    }
                }
                errors.push(err);
                0
            }
        }
    }

    /// Un mando que se va (o se sustituye) con los motores en marcha: el invitado los para.
    fn retire(&mut self, slot: u8, pad: Option<VirtualPad>) {
        if pad.is_some_and(|pad| pad.rumble.reported().is_on()) {
            self.rumble_stops.push((slot, Rumble::OFF));
        }
    }

    /// Un grupo seguido de eventos del mando de `slot`: una sola trama `SYN_REPORT`.
    fn pad_batch(
        &mut self,
        slot: u8,
        run: &[InputEvent],
        guest: GuestId,
        errors: &mut Vec<InputError>,
    ) -> usize {
        // Con el mando tal cual, lo convertido que quede en vuelo no tiene adónde ir.
        if self.raw.contains_key(&slot) {
            return run.len();
        }
        let Self {
            pads,
            pad_state,
            evdev_scratch,
            ..
        } = self;
        let Some(pad) = pads.get_mut(&slot) else {
            errors.push(InputError::NoSuchSlot(slot));
            return 0;
        };
        evdev_scratch.clear();
        for event in run {
            match event {
                InputEvent::GamepadButton { .. } | InputEvent::GamepadAxis { .. } => {
                    pad.queue(event, evdev_scratch);
                }
                InputEvent::GamepadDisconnected { .. } => {
                    for neutral in pad_state.neutral_events(None, Some(slot)) {
                        pad.queue(&neutral, evdev_scratch);
                    }
                }
                _ => {}
            }
        }
        if let Err(err) = pad.flush(evdev_scratch) {
            errors.push(err);
            return 0;
        }
        for event in run {
            if matches!(
                event,
                InputEvent::GamepadButton { .. } | InputEvent::GamepadAxis { .. }
            ) {
                pad_state.note(guest, event);
            }
        }
        run.len()
    }
}

impl InputSink for LinuxSink {
    fn inject(&mut self, event: &InputEvent, guest: GuestId) -> Result<(), InputError> {
        let mut errors = Vec::new();
        if self.inject_batch(std::slice::from_ref(event), guest, &mut errors) == 1 {
            Ok(())
        } else {
            Err(errors
                .pop()
                .unwrap_or_else(|| InputError::Backend("evento no aplicado".to_owned())))
        }
    }

    /// Grupos seguidos de la misma clase (teclado y ratón, o un mando) salen en una trama cada
    /// uno, en el orden del paquete.
    fn inject_batch(
        &mut self,
        events: &[InputEvent],
        guest: GuestId,
        errors: &mut Vec<InputError>,
    ) -> usize {
        let mut applied = 0;
        let mut start = 0;
        while start < events.len() {
            let class = events[start].class();
            let end = start
                + events[start..]
                    .iter()
                    .take_while(|event| event.class() == class)
                    .count();
            let run = &events[start..end];
            applied += match class {
                EventClass::KeyboardMouse => self.kbm_batch(run, guest, errors),
                EventClass::Gamepad(slot) => self.pad_batch(slot, run, guest, errors),
                EventClass::Control => run
                    .iter()
                    .filter(|_| match self.release_all(guest) {
                        Ok(()) => true,
                        Err(err) => {
                            errors.push(err);
                            false
                        }
                    })
                    .count(),
            };
            start = end;
        }
        applied
    }

    fn release_all(&mut self, guest: GuestId) -> Result<(), InputError> {
        let (keys, buttons) = self.pressed.release_guest(guest);
        let mut result = Ok(());
        if (!keys.is_empty() || !buttons.is_empty())
            && let EiBackend::Ready(ei) = &self.ei
        {
            // Todo lo del invitado en una trama.
            self.kbm_scratch.clear();
            self.kbm_scratch
                .extend(keys.into_iter().map(|code| InputEvent::Key {
                    code,
                    pressed: false,
                }));
            self.kbm_scratch
                .extend(buttons.into_iter().map(|button| InputEvent::MouseButton {
                    button,
                    pressed: false,
                }));
            result = ei.inject_many(&self.kbm_scratch, &[]);
        }
        // Y cada mando suyo al reposo en una trama por ranura.
        let neutral = self.pad_state.neutral_events(Some(guest), None);
        let slot_of = |event: &InputEvent| match *event {
            InputEvent::GamepadButton { slot, .. } | InputEvent::GamepadAxis { slot, .. } => {
                Some(slot)
            }
            _ => None,
        };
        let mut index = 0;
        while index < neutral.len() {
            let Some(slot) = slot_of(&neutral[index]) else {
                index += 1;
                continue;
            };
            let end = index
                + neutral[index..]
                    .iter()
                    .take_while(|event| slot_of(event) == Some(slot))
                    .count();
            if let Some(pad) = self.pads.get_mut(&slot) {
                for event in &neutral[index..end] {
                    pad.queue(event, &mut self.evdev_scratch);
                }
                result = result.and(pad.flush(&mut self.evdev_scratch));
            }
            index = end;
        }
        self.evdev_scratch.clear();
        result
    }

    fn assign_gamepad(&mut self, slot: u8) -> Result<(), InputError> {
        if self.pads.contains_key(&slot) {
            return Ok(());
        }
        let device = create_gamepad(slot)?;
        self.pads.insert(slot, VirtualPad::new(device));
        Ok(())
    }

    fn release_gamepad(&mut self, slot: u8) -> Result<(), InputError> {
        self.pad_state.forget(slot);
        self.xinput.remove(&slot);
        let raw = self.raw.remove(&slot).is_some();
        match self.pads.remove(&slot) {
            Some(pad) => {
                self.retire(slot, Some(pad));
                Ok(())
            }
            None if raw => Ok(()),
            None => Err(InputError::NoSuchSlot(slot)),
        }
    }

    fn connect_raw(&mut self, slot: u8, device: &GamepadDevice) -> Result<(), InputError> {
        if !self.pads.contains_key(&slot) && !self.raw.contains_key(&slot) {
            return Err(InputError::NoSuchSlot(slot));
        }
        match &device.transport {
            GamepadTransport::Hid { .. } => {
                let pad = UhidPad::create(slot, device)?;
                // El convertido se quita solo cuando el tal cual ya existe.
                self.pad_state.forget(slot);
                let old = self.pads.remove(&slot);
                self.retire(slot, old);
                self.xinput.remove(&slot);
                self.raw.insert(slot, pad);
                Ok(())
            }
            GamepadTransport::XInput => {
                let identity = PadIdentity {
                    name: device.name.clone(),
                    vendor: device.vendor,
                    product: device.product,
                    version: device.version,
                };
                let created = create_gamepad_as(&identity)?;
                self.pad_state.forget(slot);
                self.raw.remove(&slot);
                let old = self.pads.insert(slot, VirtualPad::new(created));
                self.retire(slot, old);
                self.xinput.insert(slot);
                Ok(())
            }
            GamepadTransport::Converted { reason } => Err(InputError::Unsupported(reason.clone())),
        }
    }

    fn raw_report(&mut self, slot: u8, data: &[u8]) -> Result<(), InputError> {
        self.raw
            .get_mut(&slot)
            .ok_or(InputError::NoSuchSlot(slot))?
            .input(data)
    }

    fn disconnect_raw(&mut self, slot: u8) -> Result<(), InputError> {
        let was_raw = self.raw.remove(&slot).is_some();
        let was_xinput = self.xinput.remove(&slot);
        if was_raw || was_xinput {
            self.pad_state.forget(slot);
            let device = create_gamepad(slot)?;
            let old = self.pads.insert(slot, VirtualPad::new(device));
            self.retire(slot, old);
        }
        Ok(())
    }

    fn take_outputs(&mut self) -> Vec<(u8, Vec<u8>)> {
        self.raw
            .iter()
            .flat_map(|(slot, pad)| pad.take_outputs().into_iter().map(|data| (*slot, data)))
            .collect()
    }

    fn take_rumble(&mut self) -> Vec<(u8, Rumble)> {
        let now = Instant::now();
        let mut out = std::mem::take(&mut self.rumble_stops);
        for (slot, pad) in &mut self.pads {
            match pad.service_ff(now) {
                Ok(Some(rumble)) => out.push((*slot, rumble)),
                Ok(None) => {}
                Err(err) if !std::mem::replace(&mut pad.ff_warned, true) => warn!(
                    "mando virtual de la ranura {slot}: {err}; su vibración no llega al invitado"
                ),
                Err(err) => debug!(slot, error = %err, "vibración del mando virtual"),
            }
        }
        out
    }

    fn prepare(&mut self) {
        self.ei.start();
    }

    /// La sesión del portal de antes se cierra con su captura (y con ella sus dispositivos, que
    /// sueltan lo pulsado): se conecta ya a la nueva, sin esperar al siguiente evento.
    fn replace_eis(&mut self, eis: EisProvider) {
        self.pressed = PressedKeys::default();
        self.ei = EiBackend::Idle(Some(eis));
        self.ei.start();
    }

    fn describe(&self) -> String {
        "libei (portal RemoteDesktop), uinput y UHID".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(x: u32, width: u32, mapping_id: Option<&str>) -> reis::event::Region {
        reis::event::Region {
            x,
            y: 0,
            width,
            height: 1080,
            scale: 1.0,
            mapping_id: mapping_id.map(str::to_owned),
        }
    }

    #[test]
    fn the_pointer_goes_to_the_shared_monitor_with_two_monitors() {
        // Dos monitores lado a lado; se comparte el de la derecha.
        let regions = [
            region(0, 2560, Some("izquierdo")),
            region(2560, 1920, Some("derecho")),
        ];
        let by_id = PointerArea::Monitor {
            x: 0,
            y: 0,
            mapping_id: Some("derecho".to_owned()),
        };
        let (found, exact) = monitor_region(&regions, &by_id).expect("región");
        assert!(exact);
        assert_eq!(found.x, 2560, "el mapping_id manda sobre la posición");
        let by_position = PointerArea::Monitor {
            x: 2560,
            y: 0,
            mapping_id: None,
        };
        let (found, exact) = monitor_region(&regions, &by_position).expect("región");
        assert!(exact);
        assert_eq!(found.x, 2560);
        // Sin datos que casen, la primera, y se dice que no es exacta (se avisa en el registro).
        let unknown = PointerArea::Monitor {
            x: 5000,
            y: 0,
            mapping_id: Some("otro".to_owned()),
        };
        let (found, exact) = monitor_region(&regions, &unknown).expect("región");
        assert!(!exact);
        assert_eq!(found.x, 0);
        // Un identificador que ninguna región conoce con la (0, 0) puesta a falta de posición:
        // casa por posición, pero no se da por exacta.
        let unmatched = PointerArea::Monitor {
            x: 0,
            y: 0,
            mapping_id: Some("otro".to_owned()),
        };
        let (found, exact) = monitor_region(&regions, &unmatched).expect("región");
        assert!(!exact);
        assert_eq!(found.x, 0);
        assert!(monitor_region(&[], &PointerArea::Unknown).is_none());
    }

    #[test]
    fn window_delta_turns_positions_into_motion() {
        assert_eq!(
            window_delta(None, (100, 100), (800, 600)),
            None,
            "la primera es la referencia"
        );
        assert_eq!(window_delta(Some((5, 5)), (5, 5), (800, 600)), None);
        assert_eq!(
            window_delta(Some((0, 0)), (u16::MAX, u16::MAX / 2), (800, 600)),
            Some((800, 300))
        );
        assert_eq!(
            window_delta(Some((u16::MAX, u16::MAX)), (0, 0), (800, 600)),
            Some((-800, -600))
        );
    }

    /// Si el destino del segundo evento falla, el primero tampoco se escribe: el grupo sale entero
    /// o no sale (quien llama deshace sus anotaciones de todo el grupo).
    #[test]
    fn a_batch_with_a_missing_device_writes_nothing() {
        let events = [
            InputEvent::Key {
                code: codes::KEY_A,
                pressed: true,
            },
            InputEvent::MouseMove { dx: 1, dy: 0 },
            InputEvent::Key {
                code: codes::KEY_A,
                pressed: false,
            },
        ];
        let mut written = Vec::new();
        let failing = all_or_nothing(
            &events,
            |event| match event {
                InputEvent::Key { .. } => Ok(Some("teclado")),
                _ => Err(InputError::NotReady("sin puntero".into())),
            },
            |device, event| written.push((device, *event)),
        );
        assert_eq!(failing, Err(InputError::NotReady("sin puntero".into())));
        assert!(written.is_empty(), "nada se escribió a medias: {written:?}");

        let touched = all_or_nothing(
            &events,
            |event| match event {
                InputEvent::Key { .. } => Ok(Some("teclado")),
                InputEvent::MouseMove { .. } => Ok(Some("puntero")),
                _ => Ok(None),
            },
            |device, event| written.push((device, *event)),
        );
        assert_eq!(touched, Ok(vec!["teclado", "puntero"]), "sin repetir");
        assert_eq!(written.len(), 3);
        assert_eq!(written[2], ("teclado", events[2]));

        // Lo que no va por EI (mandos) no cuenta como destino ni se escribe.
        let touched = all_or_nothing(
            &[InputEvent::ReleaseAll],
            |_| Ok(None::<&str>),
            |_, _| unreachable!(),
        );
        assert_eq!(touched, Ok(Vec::new()));
    }

    #[test]
    fn axes_follow_the_xpad_layout() {
        assert_eq!(
            axis_value(GamepadAxis::LeftY, 32767),
            (AbsoluteAxisCode::ABS_Y, -32767),
            "arriba en el cable es negativo en evdev"
        );
        assert_eq!(
            axis_value(GamepadAxis::LeftY, i16::MIN),
            (AbsoluteAxisCode::ABS_Y, 32767)
        );
        assert_eq!(
            axis_value(GamepadAxis::RightTrigger, i16::MAX),
            (AbsoluteAxisCode::ABS_RZ, 255)
        );
        assert_eq!(
            axis_value(GamepadAxis::LeftTrigger, -5),
            (AbsoluteAxisCode::ABS_Z, 0)
        );
        assert_eq!(button_code(GamepadButton::X), Some(KeyCode::BTN_NORTH));
        assert_eq!(button_code(GamepadButton::DPadUp), None);
    }

    #[test]
    fn the_sink_is_lazy() {
        // Crear el sumidero no abre el portal ni toca /dev/uinput.
        let mut sink = LinuxSink::new(Some(Box::new(|| Err("sin portal en el test".into()))));
        assert_eq!(
            sink.inject(
                &InputEvent::GamepadButton {
                    slot: 1,
                    button: GamepadButton::A,
                    pressed: true
                },
                GuestId(1)
            ),
            Err(InputError::NoSuchSlot(1))
        );
        // Soltar sin haber pulsado nada no necesita EI.
        assert_eq!(sink.release_all(GuestId(1)), Ok(()));
        // Con un proveedor que falla, el primer evento de teclado lo cuenta (tras conectar).
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            match sink.inject(
                &InputEvent::Key {
                    code: 30,
                    pressed: true,
                },
                GuestId(1),
            ) {
                Err(InputError::NotReady(_)) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                other => {
                    assert_eq!(
                        other,
                        Err(InputError::Unsupported("sin portal en el test".into()))
                    );
                    break;
                }
            }
        }
    }

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn a_rumble_effect_plays_for_its_length_and_stops() {
        let t0 = Instant::now();
        let mut ff = UinputRumble::default();
        assert_eq!(ff.poll(t0), None, "sin efectos, parados");
        ff.upload(0, Rumble::new(0x8000, 0x4000), 100, 0);
        assert_eq!(ff.poll(t0), None, "subido no suena hasta reproducirlo");
        ff.play(0, 1, t0);
        assert_eq!(ff.poll(t0), Some(Rumble::new(0x8000, 0x4000)));
        assert_eq!(ff.poll(t0 + 50 * MS), None, "sigue igual");
        // Se acaba su `replay.length`: el juego no manda nada más y los motores se paran.
        assert_eq!(ff.poll(t0 + 100 * MS), Some(Rumble::OFF));
        assert!(ff.playing.is_empty());
        // Otra vez, y parado a mano antes de acabar.
        ff.play(0, 1, t0 + 200 * MS);
        assert_eq!(ff.poll(t0 + 200 * MS), Some(Rumble::new(0x8000, 0x4000)));
        ff.play(0, 0, t0 + 210 * MS);
        assert_eq!(ff.poll(t0 + 210 * MS), Some(Rumble::OFF));
        assert_eq!(ff.reported(), Rumble::OFF);
    }

    #[test]
    fn an_endless_effect_waits_its_delay_and_plays_until_stopped_or_erased() {
        let t0 = Instant::now();
        let mut ff = UinputRumble::default();
        ff.upload(3, Rumble::new(0xFFFF, 0), 0, 20);
        ff.play(3, 1, t0);
        assert_eq!(ff.poll(t0 + 10 * MS), None, "en su delay, parados");
        assert_eq!(ff.poll(t0 + 20 * MS), Some(Rumble::new(0xFFFF, 0)));
        assert_eq!(
            ff.poll(t0 + 60_000 * MS),
            None,
            "sin duración, hasta que lo paren"
        );
        // Sustituirlo mientras suena cambia lo que suena sin reiniciar la espera.
        ff.upload(3, Rumble::new(0x1000, 0x2000), 0, 20);
        assert_eq!(ff.poll(t0 + 60_001 * MS), Some(Rumble::new(0x1000, 0x2000)));
        // Borrarlo (el juego cerró el mando) lo para.
        ff.erase(3);
        assert_eq!(ff.poll(t0 + 60_002 * MS), Some(Rumble::OFF));
        // Reproducir un efecto que no existe no hace nada.
        ff.play(3, 1, t0 + 60_003 * MS);
        assert_eq!(ff.poll(t0 + 60_003 * MS), None);
    }

    #[test]
    fn repeated_effects_pause_in_their_delay_and_end_after_the_count() {
        let t0 = Instant::now();
        let mut ff = UinputRumble::default();
        ff.upload(1, Rumble::new(0x3000, 0x3000), 50, 10);
        ff.play(1, 2, t0);
        let on = Rumble::new(0x3000, 0x3000);
        assert_eq!(ff.current(t0 + 5 * MS), Rumble::OFF);
        assert_eq!(ff.current(t0 + 10 * MS), on);
        assert_eq!(ff.current(t0 + 59 * MS), on);
        assert_eq!(ff.current(t0 + 65 * MS), Rumble::OFF, "segundo delay");
        assert_eq!(ff.current(t0 + 70 * MS), on, "segunda vez");
        assert_eq!(ff.current(t0 + 119 * MS), on);
        assert_eq!(ff.current(t0 + 120 * MS), Rumble::OFF, "acabaron las dos");
        assert!(ff.playing.is_empty());
    }

    #[test]
    fn effects_add_up_saturating_and_follow_the_gain() {
        let t0 = Instant::now();
        let mut ff = UinputRumble::default();
        ff.upload(0, Rumble::new(0xC000, 0x1000), 0, 0);
        ff.upload(1, Rumble::new(0x8000, 0x2000), 0, 0);
        ff.play(0, 1, t0);
        ff.play(1, 1, t0);
        assert_eq!(ff.poll(t0), Some(Rumble::new(0xFFFF, 0x3000)));
        // `FF_GAIN` a la mitad escala cada efecto antes de sumarlos.
        ff.set_gain(0x8000);
        let half = |value: u32| (value * 0x8000 / 0xFFFF) as u16;
        assert_eq!(
            ff.poll(t0),
            Some(Rumble::new(
                half(0xC000) + half(0x8000),
                half(0x1000) + half(0x2000)
            ))
        );
        ff.set_gain(-5);
        assert_eq!(
            ff.poll(t0),
            Some(Rumble::OFF),
            "ganancia cero (y fuera de rango)"
        );
        ff.set_gain(i32::MAX);
        ff.play(0, 0, t0);
        assert_eq!(
            ff.poll(t0),
            Some(Rumble::new(0x8000, 0x2000)),
            "queda el otro"
        );
    }

    /// Crea un mando uinput real, le mueve los motores como el invitado ([`EvdevMotors`], en otro
    /// hilo: la subida espera a que el mando la conteste) y comprueba que el mando lo convierte en
    /// los motores.
    #[test]
    #[ignore = "necesita acceso a /dev/uinput (regla udev de coroom)"]
    fn a_real_uinput_gamepad_tells_the_rumble_a_game_plays() {
        use crate::gamepad::{EvdevMotors, Motors};
        let mut device = create_gamepad(4).expect("mando uinput");
        let node = device
            .enumerate_dev_nodes_blocking()
            .expect("nodos")
            .filter_map(Result::ok)
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("event"))
            })
            .expect("nodo /dev/input/event*");
        let mut pad = VirtualPad::new(device);
        let (step, next) = mpsc::channel::<()>();
        // El «juego» es el invitado moviendo los motores de su mando: el mismo camino de ida y
        // vuelta de un mando convertido.
        let game = thread::spawn(move || {
            let mut motors = EvdevMotors::new(&node);
            motors.set(Rumble::new(0xC000, 0x2000)).expect("vibrar");
            next.recv().expect("paso");
            motors.set(Rumble::OFF).expect("parar");
            next.recv().expect("paso");
            // Soltarlo lo borra (`UI_FF_ERASE`, que también espera respuesta).
            drop(motors);
        });
        let wait_for = |pad: &mut VirtualPad, wanted: Rumble| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if pad.service_ff(Instant::now()).expect("leer") == Some(wanted) {
                    return;
                }
                assert!(Instant::now() < deadline, "no llegó {wanted:?}");
                thread::sleep(MS);
            }
        };
        wait_for(&mut pad, Rumble::new(0xC000, 0x2000));
        step.send(()).expect("paso");
        wait_for(&mut pad, Rumble::OFF);
        step.send(()).expect("paso");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !game.is_finished() {
            pad.service_ff(Instant::now()).expect("leer");
            assert!(Instant::now() < deadline, "el borrado no terminó");
            thread::sleep(MS);
        }
        game.join().expect("hilo del juego");
        assert!(pad.rumble.effects.is_empty(), "borrado");
    }

    /// Crea un mando uinput real y comprueba con `evdev` que se presenta como un mando de Xbox 360.
    #[test]
    #[ignore = "necesita acceso a /dev/uinput (regla udev de coroom)"]
    fn a_real_uinput_gamepad_looks_like_an_xbox_controller() {
        let mut device = create_gamepad(3).expect("mando uinput");
        let node = device
            .enumerate_dev_nodes_blocking()
            .expect("nodos")
            .filter_map(Result::ok)
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("event"))
            })
            .expect("nodo /dev/input/event*");
        let opened = evdev::Device::open(&node).expect("abrir el nodo");
        assert_eq!(opened.name(), Some("coroom Gamepad 3"));
        assert_eq!(opened.input_id().vendor(), GAMEPAD_VENDOR);
        assert_eq!(opened.input_id().product(), GAMEPAD_PRODUCT);
        let keys = opened.supported_keys().expect("botones");
        assert!(keys.contains(KeyCode::BTN_SOUTH) && keys.contains(KeyCode::BTN_MODE));
        let axes = opened.supported_absolute_axes().expect("ejes");
        assert!(
            axes.contains(AbsoluteAxisCode::ABS_RZ) && axes.contains(AbsoluteAxisCode::ABS_HAT0Y)
        );
        let ff = opened.supported_ff().expect("vibración");
        assert!(ff.contains(FFEffectCode::FF_RUMBLE) && ff.contains(FFEffectCode::FF_GAIN));
        let mut pad = VirtualPad::new(device);
        pad.emit(&InputEvent::GamepadButton {
            slot: 3,
            button: GamepadButton::A,
            pressed: true,
        })
        .expect("emitir");
    }
}
