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
//! ([`PointerArea::Monitor`]; si no hay datos, la primera). Con una ventana, Wayland no dice dónde
//! está: la posición absoluta se convierte en movimiento relativo escalado a su tamaño
//! ([`PointerArea::Window`]). La rueda va por `scroll_discrete` (1/120 de muesca, positivo hacia
//! abajo en EI).
//!
//! **uinput** (`evdev` 0.13): un dispositivo por ranura, "coroom Gamepad N", con el
//! vendor/product del mando de Xbox 360 (`045e:028e`) y la disposición del driver `xpad` (A, B,
//! X = `BTN_NORTH`, Y = `BTN_WEST`, LB, RB, Back, Start, Guide, LS, RS; sticks `ABS_X/Y/RX/RY` en
//! ±32767 con Y hacia abajo; gatillos `ABS_Z/RZ` en 0..255; cruceta `ABS_HAT0X/Y`), así que SDL,
//! Steam Input y los juegos lo reconocen. Los eventos de mando de un paquete van en **una sola
//! trama** `SYN_REPORT` por ranura: un stick con `X` e `Y` cambiados llega al juego como una
//! muestra, no como dos a medias. Sin *force feedback*. Si `/dev/uinput` no es accesible, el error
//! explica la regla udev ([`UINPUT_HELP`]).

use std::collections::{BTreeMap, BTreeSet};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::Duration;

use evdev::uinput::VirtualDevice;
use evdev::{AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventType, InputId, KeyCode};
use reis::ei;
use reis::event::{DeviceCapability, EiEvent};
use tracing::{debug, warn};

use super::uhid::UhidPad;
use super::{EisConnection, EisProvider, InputSink, PadTracker, PointerArea, PressedKeys};
use crate::keymap::evdev as codes;
use crate::{EventClass, GamepadAxis, GamepadButton, GuestId, InputError, InputEvent, MouseButton};
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

#[derive(Default)]
struct EiState {
    devices: Vec<(reis::event::Device, bool)>,
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
    fn inject_many(&self, events: &[InputEvent]) -> Result<(), InputError> {
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
        let touched = all_or_nothing(
            events,
            |event| Self::target(&state, self.area, event),
            |device, event| self.queue(device, event),
        )?;
        if touched.is_empty() {
            return Ok(());
        }
        let time = monotonic_us();
        for device in touched {
            device.device().frame(serial, time);
        }
        self.context
            .flush()
            .map_err(|err| InputError::Backend(format!("no se pudo escribir en EI: {err}")))
    }

    /// El dispositivo de EI al que va `event` (`None` si no va por EI: los mandos), sin
    /// escribir nada. Falla si el compositor no ofrece uno activo con la capacidad que hace falta.
    fn target<'s>(
        state: &'s EiState,
        area: PointerArea,
        event: &InputEvent,
    ) -> Result<Option<&'s reis::event::Device>, InputError> {
        let capability = match *event {
            InputEvent::Key { .. } => DeviceCapability::Keyboard,
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
        state
            .devices
            .iter()
            .find(|(device, resumed)| *resumed && device.has_capability(capability))
            .map(|(device, _)| Some(device))
            .ok_or_else(|| {
                InputError::NotReady(format!(
                    "el compositor no ofrece un dispositivo de EI con {capability:?} activo"
                ))
            })
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
                if let PointerArea::Window { width, height } = self.area {
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
            InputEvent::GamepadConnected { .. }
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
        let monitor = match self.area {
            PointerArea::Monitor { x, y } => Some((x, y)),
            _ => None,
        };
        let regions = device.regions();
        let (ox, oy, w, h) = monitor
            .and_then(|(x, y)| {
                regions.iter().find(|region| {
                    i64::from(region.x) == i64::from(x) && i64::from(region.y) == i64::from(y)
                })
            })
            .or_else(|| regions.first())
            .map(|region| (region.x, region.y, region.width, region.height))
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
                lock(state).devices.push((added.device, false));
            }
            EiEvent::DeviceResumed(resumed) => {
                let mut state = lock(state);
                let sequence = state.sequence;
                state.sequence = state.sequence.wrapping_add(1);
                resumed
                    .device
                    .device()
                    .start_emulating(resumed.serial, sequence);
                for (device, active) in &mut state.devices {
                    if *device == resumed.device {
                        *active = true;
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
                for (device, active) in &mut lock(state).devices {
                    if *device == paused.device {
                        *active = false;
                    }
                }
            }
            EiEvent::DeviceRemoved(removed) => {
                lock(state)
                    .devices
                    .retain(|(device, _)| *device != removed.device);
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
}

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
    builder.build().map_err(uinput_error)
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
    pressed: PressedKeys,
    pad_state: PadTracker,
    /// Eventos de teclado y ratón que hay que escribir de un grupo (reutilizado).
    kbm_scratch: Vec<InputEvent>,
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
            pressed: PressedKeys::default(),
            pad_state: PadTracker::default(),
            kbm_scratch: Vec::with_capacity(64),
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
        for event in run {
            let write = match *event {
                InputEvent::Key { code, pressed } => self.pressed.key(guest, code, pressed),
                InputEvent::MouseButton { button, pressed } => {
                    self.pressed.button(guest, button, pressed)
                }
                _ => true,
            };
            if write {
                self.kbm_scratch.push(*event);
            }
        }
        if self.kbm_scratch.is_empty() {
            // Otro invitado ya tenía pulsado lo mismo: nada que escribir.
            return run.len();
        }
        let scratch = &self.kbm_scratch;
        match self.ei.ready().and_then(|ei| ei.inject_many(scratch)) {
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
            result = ei.inject_many(&self.kbm_scratch);
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
        self.pads.insert(
            slot,
            VirtualPad {
                device,
                dpad: [false; 4],
            },
        );
        Ok(())
    }

    fn release_gamepad(&mut self, slot: u8) -> Result<(), InputError> {
        self.pad_state.forget(slot);
        self.xinput.remove(&slot);
        let raw = self.raw.remove(&slot).is_some();
        match self.pads.remove(&slot) {
            Some(_) => Ok(()),
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
                self.pads.remove(&slot);
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
                self.pads.insert(
                    slot,
                    VirtualPad {
                        device: created,
                        dpad: [false; 4],
                    },
                );
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
            self.pads.insert(
                slot,
                VirtualPad {
                    device,
                    dpad: [false; 4],
                },
            );
        }
        Ok(())
    }

    fn take_outputs(&mut self) -> Vec<(u8, Vec<u8>)> {
        self.raw
            .iter()
            .flat_map(|(slot, pad)| pad.take_outputs().into_iter().map(|data| (*slot, data)))
            .collect()
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
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
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
        let mut pad = VirtualPad {
            device,
            dpad: [false; 4],
        };
        pad.emit(&InputEvent::GamepadButton {
            slot: 3,
            button: GamepadButton::A,
            pressed: true,
        })
        .expect("emitir");
    }
}
