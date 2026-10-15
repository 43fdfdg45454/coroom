//! Entrada del invitado: eventos, paquetes y su formato en el cable.
//!
//! Todo lo que el invitado pulsa o mueve viaja por **datagramas** de la pista [`Track::Input`], en
//! [`InputPacket`]: los eventos ([`InputEvent`]) en orden y, detrás, el **estado completo**
//! ([`InputState`]: teclas y botones pulsados, movimiento y rueda acumulados, última posición
//! absoluta y el mando de la ranura) tal como queda tras esos eventos. Con el estado en cada
//! paquete, perder un datagrama no deja una tecla pegada ni un clic sin soltar: el anfitrión aplica
//! los eventos cuando la secuencia viene seguida y, si falta alguna, reconcilia su vista con el
//! estado que llega ([`InputState::diff`]); el invitado además repite el estado unas veces tras
//! cada cambio (`coroom_input::send`). Los datagramas más antiguos que el último aplicado se
//! descartan. Así ninguna tecla espera a una retransmisión, como pasaba con un stream fiable.
//!
//! Lo único que sigue yendo por el **stream** fiable de entrada es la declaración de un mando tal
//! cual ([`InputMessage`] con [`PadData::Device`]: su descriptor HID no cabe en un datagrama y no
//! tiene prisa).
//!
//! Los dos se serializan con `postcard` y se cifran con la clave pareja a pareja
//! (`coroom_crypto::input`). Esta crate solo fija el formato en claro que los rodea:
//!
//! | Canal | Bytes en claro | Resto |
//! |---|---|---|
//! | Datagrama | `2` (versión de cabecera de medios), `3` ([`Track::Input`]), `0`, `0`, `key_id` (u32 BE) | sellado de `coroom_crypto::input` |
//! | Stream | al abrirlo, [`STREAM_VERSION`] y `key_id` (u32 BE); después, tramas `longitud (u16 BE) ‖ sellado` | — |
//!
//! Los dos primeros bytes del datagrama son los de [`ObjectHeader`]: así una conexión separa los
//! datagramas de entrada de los de medios mirando solo la pista. `key_id` identifica la clave
//! pareja a pareja (y con ella al invitado) cuando varios invitados llegan por el relay en una sola
//! conexión; no es secreto ni hace falta autenticarlo aparte: con otra clave el sellado no abre.
//!
//! Cada paquete lleva dos marcas para medir por etapas: `sent_us`, la hora de su primer evento en
//! el reloj del anfitrión (el anfitrión obtiene `recibido − capturado`), y `queued_us`, lo que ese
//! evento esperó en el invitado hasta salir (captura → envío).
//!
//! **Teclas**: códigos **evdev** de Linux (`KEY_*`). **Ratón**: botones por nombre, movimiento
//! relativo en unidades del ratón, posición absoluta en `0..=65535` del fotograma del anfitrión y
//! rueda en 1/120 de muesca (positivo = hacia arriba y a la derecha, como `WHEEL_DELTA`).
//! **Mandos**: vocabulario de un mando Xbox; ejes de los sticks en `i16` con positivo = derecha y
//! arriba, gatillos en `0..=i16::MAX`. Las ranuras se numeran desde 1.
//!
//! **Mandos tal cual** ([`PadData`]): el invitado declara su mando físico ([`GamepadDevice`]: nombre,
//! VID/PID y, si es HID, su descriptor y sus informes de característica) por el stream, y manda
//! cada informe de entrada HID sin tocar ([`PadData::Report`]) por datagrama, en el campo `pads`
//! de los paquetes, aparte de los eventos.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::media::{ObjectHeader, Track};

/// Bytes en claro delante del sellado de un datagrama de entrada.
pub const DATAGRAM_HEADER_LEN: usize = 8;

/// Bytes del preámbulo del stream de entrada ([`STREAM_VERSION`] y `key_id`).
pub const STREAM_PREAMBLE_LEN: usize = 5;

/// Versión del formato del stream de entrada.
pub const STREAM_VERSION: u8 = 1;

/// Longitud máxima de una trama sellada del stream.
pub const MAX_STREAM_FRAME: usize = 16 * 1024;

/// Eventos como mucho por paquete o mensaje (el emisor agrupa o suma por encima).
pub const MAX_EVENTS: usize = 64;

/// Teclas pulsadas a la vez que caben en el estado de un paquete.
pub const MAX_PRESSED_KEYS: usize = 32;

/// Ranuras de mando por defecto.
pub const DEFAULT_GAMEPAD_SLOTS: u8 = 4;

/// Ranuras de mando como mucho (`COROOM_MAX_GAMEPADS`).
pub const MAX_GAMEPAD_SLOTS: u8 = 8;

/// Una muesca de la rueda, en las unidades de [`InputEvent::Wheel`].
pub const WHEEL_NOTCH: i32 = 120;

/// Datos de mandos tal cual por paquete o mensaje, como mucho.
pub const MAX_PADS: usize = 16;

/// Caracteres del nombre de un mando, como mucho.
pub const MAX_PAD_NAME: usize = 128;

/// Informes de característica de un mando, como mucho.
pub const MAX_FEATURES: usize = 32;

/// Bytes de todos los informes de característica de un mando, como mucho.
pub const MAX_FEATURE_BYTES: usize = 8 * 1024;

/// Cada cuánto repite el invitado su estado, sin eventos, mientras tiene algo pulsado y no sale
/// ningún otro paquete: una ráfaga de pérdidas que se lleve una liberación y sus repeticiones
/// cortas no deja la tecla pegada más que esto. El anfitrión cuenta con estas repeticiones
/// ([`STALE_AFTER`]).
pub const KEEPALIVE_PRESSED: Duration = Duration::from_millis(100);

/// Cada cuánto repite el invitado su estado cuando no tiene nada pulsado (señal de vida).
pub const KEEPALIVE_IDLE: Duration = Duration::from_millis(500);

/// Repeticiones de [`KEEPALIVE_PRESSED`] seguidas que el anfitrión da por perdidas antes de
/// soltar lo que el invitado tuviera pulsado.
pub const STALE_KEEPALIVES: u32 = 10;

/// Sin ningún paquete de un invitado en este plazo (derivado de [`KEEPALIVE_PRESSED`]), el
/// anfitrión suelta lo que ese invitado tuviera pulsado según su vista y reconcilia con el estado
/// del siguiente paquete que llegue.
pub const STALE_AFTER: Duration = KEEPALIVE_PRESSED.saturating_mul(STALE_KEEPALIVES);

/// Botón del ratón.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MouseButton {
    /// Izquierdo.
    Left,
    /// Derecho.
    Right,
    /// Central.
    Middle,
    /// Lateral trasero.
    Back,
    /// Lateral delantero.
    Forward,
}

impl MouseButton {
    /// Todos los botones.
    pub const ALL: [MouseButton; 5] = [
        MouseButton::Left,
        MouseButton::Right,
        MouseButton::Middle,
        MouseButton::Back,
        MouseButton::Forward,
    ];
}

/// Tipo de mando que emula el anfitrión.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GamepadKind {
    /// Mando compatible con el de Xbox 360.
    Xbox,
}

/// Botón de mando (vocabulario de Xbox; los gatillos son ejes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum GamepadButton {
    /// A (abajo).
    A,
    /// B (derecha).
    B,
    /// X (izquierda).
    X,
    /// Y (arriba).
    Y,
    /// Botón superior izquierdo (LB).
    LeftBumper,
    /// Botón superior derecho (RB).
    RightBumper,
    /// Back / View.
    Back,
    /// Start / Menu.
    Start,
    /// Guide / botón central.
    Guide,
    /// Pulsar el stick izquierdo (LS).
    LeftStick,
    /// Pulsar el stick derecho (RS).
    RightStick,
    /// Cruceta arriba.
    DPadUp,
    /// Cruceta abajo.
    DPadDown,
    /// Cruceta izquierda.
    DPadLeft,
    /// Cruceta derecha.
    DPadRight,
}

impl GamepadButton {
    /// Todos los botones.
    pub const ALL: [GamepadButton; 15] = [
        GamepadButton::A,
        GamepadButton::B,
        GamepadButton::X,
        GamepadButton::Y,
        GamepadButton::LeftBumper,
        GamepadButton::RightBumper,
        GamepadButton::Back,
        GamepadButton::Start,
        GamepadButton::Guide,
        GamepadButton::LeftStick,
        GamepadButton::RightStick,
        GamepadButton::DPadUp,
        GamepadButton::DPadDown,
        GamepadButton::DPadLeft,
        GamepadButton::DPadRight,
    ];
}

/// Eje de mando.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum GamepadAxis {
    /// Stick izquierdo, horizontal (positivo = derecha).
    LeftX,
    /// Stick izquierdo, vertical (positivo = arriba).
    LeftY,
    /// Stick derecho, horizontal.
    RightX,
    /// Stick derecho, vertical.
    RightY,
    /// Gatillo izquierdo (LT), `0..=i16::MAX`.
    LeftTrigger,
    /// Gatillo derecho (RT), `0..=i16::MAX`.
    RightTrigger,
}

impl GamepadAxis {
    /// Todos los ejes.
    pub const ALL: [GamepadAxis; 6] = [
        GamepadAxis::LeftX,
        GamepadAxis::LeftY,
        GamepadAxis::RightX,
        GamepadAxis::RightY,
        GamepadAxis::LeftTrigger,
        GamepadAxis::RightTrigger,
    ];

    /// Indica si es un gatillo (solo valores no negativos).
    pub const fn is_trigger(self) -> bool {
        matches!(self, GamepadAxis::LeftTrigger | GamepadAxis::RightTrigger)
    }
}

/// Evento de entrada independiente de la plataforma.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InputEvent {
    /// Tecla física pulsada o soltada, por código evdev (`KEY_*`).
    Key {
        /// Código evdev.
        code: u16,
        /// `true` al pulsar.
        pressed: bool,
    },
    /// Botón del ratón.
    MouseButton {
        /// Botón.
        button: MouseButton,
        /// `true` al pulsar.
        pressed: bool,
    },
    /// Movimiento relativo (modo inmersivo), en unidades del ratón.
    MouseMove {
        /// Horizontal (positivo = derecha).
        dx: i32,
        /// Vertical (positivo = abajo).
        dy: i32,
    },
    /// Posición absoluta (modo escritorio) en `0..=65535` del fotograma del anfitrión.
    MouseMoveAbsolute {
        /// Horizontal (0 = borde izquierdo).
        x: u16,
        /// Vertical (0 = borde superior).
        y: u16,
    },
    /// Rueda, en 1/120 de muesca ([`WHEEL_NOTCH`]); positivo = arriba y derecha.
    Wheel {
        /// Horizontal.
        dx: i32,
        /// Vertical.
        dy: i32,
    },
    /// El invitado conectó un mando para su ranura.
    GamepadConnected {
        /// Ranura (desde 1).
        slot: u8,
        /// Tipo.
        kind: GamepadKind,
    },
    /// El invitado desconectó el mando de su ranura.
    GamepadDisconnected {
        /// Ranura.
        slot: u8,
    },
    /// Botón de mando.
    GamepadButton {
        /// Ranura.
        slot: u8,
        /// Botón.
        button: GamepadButton,
        /// `true` al pulsar.
        pressed: bool,
    },
    /// Eje de mando.
    GamepadAxis {
        /// Ranura.
        slot: u8,
        /// Eje.
        axis: GamepadAxis,
        /// Valor ([`GamepadAxis`]).
        value: i16,
    },
    /// Soltar todo lo pulsado (al perder el foco, al pausar o al expulsar).
    ReleaseAll,
}

/// Cómo llega el mando físico de un invitado.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GamepadTransport {
    /// HID: el anfitrión crea un dispositivo con este descriptor y le pasa los informes tal cual
    /// ([`PadData::Report`]). `features` son los informes de característica que el invitado leyó
    /// del mando al conectarlo (identificador y bytes, el identificador incluido si lo hay), para
    /// contestar al driver del anfitrión cuando los pida.
    Hid {
        /// Descriptor de informes ([`crate::hid`]).
        descriptor: Vec<u8>,
        /// Informes de característica.
        features: Vec<(u8, Vec<u8>)>,
    },
    /// XInput (un Xbox por cable): los eventos de siempre, del vocabulario de Xbox, con la
    /// identidad (nombre y VID/PID) del mando.
    XInput,
    /// No se puede mandar tal cual (el invitado no puede leerlo en crudo, por ejemplo): va
    /// convertido. El motivo es para el anfitrión.
    Converted {
        /// Por qué, en español.
        reason: String,
    },
}

/// El mando físico de un invitado.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GamepadDevice {
    /// Nombre que da el mando.
    pub name: String,
    /// Fabricante (VID).
    pub vendor: u16,
    /// Producto (PID).
    pub product: u16,
    /// Versión.
    pub version: u16,
    /// Bus por el que va (`BUS_USB` = 3, `BUS_BLUETOOTH` = 5, como en Linux): algunos drivers
    /// esperan informes distintos por USB y por Bluetooth.
    pub bus: u16,
    /// Cómo llega.
    pub transport: GamepadTransport,
}

/// Datos de un mando tal cual.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PadData {
    /// Stream: el mando físico de la ranura (al conectarlo o al cambiar de ranura).
    Device {
        /// Ranura.
        slot: u8,
        /// El mando.
        device: GamepadDevice,
    },
    /// Datagrama: un informe de entrada HID tal cual, con su identificador si el descriptor los
    /// usa.
    Report {
        /// Ranura.
        slot: u8,
        /// Bytes del informe.
        data: Vec<u8>,
    },
}

impl PadData {
    /// Ranura.
    pub const fn slot(&self) -> u8 {
        match self {
            PadData::Device { slot, .. } | PadData::Report { slot, .. } => *slot,
        }
    }

    /// Comprueba los límites ([`MAX_PAD_NAME`], [`crate::hid::MAX_DESCRIPTOR`],
    /// [`MAX_FEATURES`], [`MAX_FEATURE_BYTES`], [`crate::hid::MAX_REPORT_BYTES`]).
    pub fn check(&self) -> Result<(), InputWireError> {
        let bad = |what: &str| Err(InputWireError::Pad(what.to_owned()));
        match self {
            PadData::Report { data, .. } => {
                if data.is_empty() || data.len() > crate::hid::MAX_REPORT_BYTES {
                    return bad("informe de mando vacío o demasiado grande");
                }
            }
            PadData::Device { device, .. } => {
                if device.name.chars().count() > MAX_PAD_NAME {
                    return bad("nombre de mando demasiado largo");
                }
                match &device.transport {
                    GamepadTransport::Hid {
                        descriptor,
                        features,
                    } => {
                        if descriptor.is_empty() || descriptor.len() > crate::hid::MAX_DESCRIPTOR {
                            return bad("descriptor de mando vacío o demasiado grande");
                        }
                        let total: usize = features.iter().map(|(_, data)| data.len()).sum();
                        if features.len() > MAX_FEATURES
                            || total > MAX_FEATURE_BYTES
                            || features
                                .iter()
                                .any(|(_, data)| data.len() > crate::hid::MAX_REPORT_BYTES)
                        {
                            return bad("demasiados informes de característica");
                        }
                    }
                    GamepadTransport::Converted { reason } => {
                        if reason.chars().count() > 256 {
                            return bad("motivo demasiado largo");
                        }
                    }
                    GamepadTransport::XInput => {}
                }
            }
        }
        Ok(())
    }
}

/// Qué permiso necesita un evento.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventClass {
    /// Teclado y ratón.
    KeyboardMouse,
    /// El mando de esa ranura.
    Gamepad(u8),
    /// Ninguno ([`InputEvent::ReleaseAll`]).
    Control,
}

impl InputEvent {
    /// Indica si es movimiento continuo (va por datagramas): ratón y ejes de mando.
    pub const fn is_continuous(&self) -> bool {
        matches!(
            self,
            InputEvent::MouseMove { .. }
                | InputEvent::MouseMoveAbsolute { .. }
                | InputEvent::GamepadAxis { .. }
        )
    }

    /// Permiso que necesita.
    pub const fn class(&self) -> EventClass {
        match self {
            InputEvent::Key { .. }
            | InputEvent::MouseButton { .. }
            | InputEvent::MouseMove { .. }
            | InputEvent::MouseMoveAbsolute { .. }
            | InputEvent::Wheel { .. } => EventClass::KeyboardMouse,
            InputEvent::GamepadConnected { slot, .. }
            | InputEvent::GamepadDisconnected { slot }
            | InputEvent::GamepadButton { slot, .. }
            | InputEvent::GamepadAxis { slot, .. } => EventClass::Gamepad(*slot),
            InputEvent::ReleaseAll => EventClass::Control,
        }
    }
}

/// El mando de la ranura del invitado dentro de [`InputState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GamepadState {
    /// Ranura (desde 1).
    pub slot: u8,
    /// Tipo.
    pub kind: GamepadKind,
    /// Botones pulsados: un bit por posición en [`GamepadButton::ALL`].
    pub buttons: u16,
    /// Ejes, en el orden de [`GamepadAxis::ALL`].
    pub axes: [i16; 6],
}

impl GamepadState {
    /// Mando recién conectado: nada pulsado, ejes en reposo.
    pub const fn neutral(slot: u8, kind: GamepadKind) -> Self {
        Self {
            slot,
            kind,
            buttons: 0,
            axes: [0; 6],
        }
    }

    /// Si `button` está pulsado.
    pub fn pressed(&self, button: GamepadButton) -> bool {
        self.buttons & button_bit(button) != 0
    }
}

fn button_bit(button: GamepadButton) -> u16 {
    let index = GamepadButton::ALL
        .iter()
        .position(|b| *b == button)
        .unwrap_or(0);
    1 << index
}

fn mouse_bit(button: MouseButton) -> u8 {
    let index = MouseButton::ALL
        .iter()
        .position(|b| *b == button)
        .unwrap_or(0);
    1 << index
}

fn axis_index(axis: GamepadAxis) -> usize {
    GamepadAxis::ALL
        .iter()
        .position(|a| *a == axis)
        .unwrap_or(0)
}

/// Todo lo que el invitado tiene pulsado o movido, tal como queda tras los eventos de un paquete.
/// Lo acumulado (movimiento y rueda) da vuelta en `i32`: lo que importa es la diferencia entre dos
/// estados, no el valor.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct InputState {
    /// Teclas pulsadas (códigos evdev), ordenadas y sin repetir; como mucho
    /// [`MAX_PRESSED_KEYS`] (las demás no se anotan).
    pub keys: Vec<u16>,
    /// Botones del ratón pulsados: un bit por posición en [`MouseButton::ALL`].
    pub buttons: u8,
    /// Movimiento relativo acumulado (suma de todos los `MouseMove`), con vuelta.
    pub motion: (i32, i32),
    /// Rueda acumulada (suma de todos los `Wheel`), con vuelta.
    pub wheel: (i32, i32),
    /// Última posición absoluta enviada.
    pub absolute: Option<(u16, u16)>,
    /// El mando de la ranura, si está conectado.
    pub gamepad: Option<GamepadState>,
}

impl InputState {
    /// Avanza el estado con `event`.
    pub fn apply(&mut self, event: &InputEvent) {
        match *event {
            InputEvent::Key { code, pressed } => {
                let at = self.keys.binary_search(&code);
                match (at, pressed) {
                    (Err(at), true) if self.keys.len() < MAX_PRESSED_KEYS => {
                        self.keys.insert(at, code);
                    }
                    (Ok(at), false) => {
                        self.keys.remove(at);
                    }
                    _ => {}
                }
            }
            InputEvent::MouseButton { button, pressed } => {
                if pressed {
                    self.buttons |= mouse_bit(button);
                } else {
                    self.buttons &= !mouse_bit(button);
                }
            }
            InputEvent::MouseMove { dx, dy } => {
                self.motion = (
                    self.motion.0.wrapping_add(dx),
                    self.motion.1.wrapping_add(dy),
                );
            }
            InputEvent::MouseMoveAbsolute { x, y } => self.absolute = Some((x, y)),
            InputEvent::Wheel { dx, dy } => {
                self.wheel = (self.wheel.0.wrapping_add(dx), self.wheel.1.wrapping_add(dy));
            }
            InputEvent::GamepadConnected { slot, kind } => {
                self.gamepad = Some(GamepadState::neutral(slot, kind));
            }
            InputEvent::GamepadDisconnected { slot } => {
                if self.gamepad.is_some_and(|pad| pad.slot == slot) {
                    self.gamepad = None;
                }
            }
            InputEvent::GamepadButton {
                slot,
                button,
                pressed,
            } => {
                if let Some(pad) = self.gamepad.as_mut().filter(|pad| pad.slot == slot) {
                    if pressed {
                        pad.buttons |= button_bit(button);
                    } else {
                        pad.buttons &= !button_bit(button);
                    }
                }
            }
            InputEvent::GamepadAxis { slot, axis, value } => {
                if let Some(pad) = self.gamepad.as_mut().filter(|pad| pad.slot == slot) {
                    pad.axes[axis_index(axis)] = value;
                }
            }
            InputEvent::ReleaseAll => self.release_all(),
        }
    }

    /// Suelta todo: teclas, botones y el mando (que sigue conectado, en reposo).
    pub fn release_all(&mut self) {
        self.keys.clear();
        self.buttons = 0;
        if let Some(pad) = self.gamepad.as_mut() {
            pad.buttons = 0;
            pad.axes = [0; 6];
        }
    }

    /// Si `code` está pulsada.
    pub fn key_pressed(&self, code: u16) -> bool {
        self.keys.binary_search(&code).is_ok()
    }

    /// Si `button` está pulsado.
    pub fn mouse_pressed(&self, button: MouseButton) -> bool {
        self.buttons & mouse_bit(button) != 0
    }

    /// Si hay algo que soltar: alguna tecla o botón pulsado, o el mando fuera del reposo.
    pub fn any_pressed(&self) -> bool {
        !self.keys.is_empty()
            || self.buttons != 0
            || self
                .gamepad
                .is_some_and(|pad| pad.buttons != 0 || pad.axes != [0; 6])
    }

    /// Eventos que llevan de `self` a `target`, para reconciliar cuando se perdieron paquetes:
    /// primero lo que se suelta (teclas, botones del ratón y del mando), después lo que se pulsa,
    /// el movimiento y la rueda que faltan, la posición absoluta y los ejes. Puede superar
    /// [`MAX_EVENTS`]: no se serializa, se aplica en el anfitrión.
    pub fn diff(&self, target: &InputState) -> Vec<InputEvent> {
        let mut out = Vec::new();
        for &code in &self.keys {
            if !target.key_pressed(code) {
                out.push(InputEvent::Key {
                    code,
                    pressed: false,
                });
            }
        }
        for button in MouseButton::ALL {
            if self.mouse_pressed(button) && !target.mouse_pressed(button) {
                out.push(InputEvent::MouseButton {
                    button,
                    pressed: false,
                });
            }
        }
        let same_pad = match (self.gamepad, target.gamepad) {
            (Some(a), Some(b)) => a.slot == b.slot && a.kind == b.kind,
            _ => false,
        };
        if let Some(pad) = self.gamepad {
            if same_pad {
                let target_pad = target.gamepad.expect("mismo mando");
                for button in GamepadButton::ALL {
                    if pad.pressed(button) && !target_pad.pressed(button) {
                        out.push(InputEvent::GamepadButton {
                            slot: pad.slot,
                            button,
                            pressed: false,
                        });
                    }
                }
            } else {
                out.push(InputEvent::GamepadDisconnected { slot: pad.slot });
            }
        }
        for &code in &target.keys {
            if !self.key_pressed(code) {
                out.push(InputEvent::Key {
                    code,
                    pressed: true,
                });
            }
        }
        for button in MouseButton::ALL {
            if target.mouse_pressed(button) && !self.mouse_pressed(button) {
                out.push(InputEvent::MouseButton {
                    button,
                    pressed: true,
                });
            }
        }
        let (dx, dy) = (
            target.motion.0.wrapping_sub(self.motion.0),
            target.motion.1.wrapping_sub(self.motion.1),
        );
        if dx != 0 || dy != 0 {
            out.push(InputEvent::MouseMove { dx, dy });
        }
        if let Some((x, y)) = target.absolute
            && target.absolute != self.absolute
        {
            out.push(InputEvent::MouseMoveAbsolute { x, y });
        }
        let (wx, wy) = (
            target.wheel.0.wrapping_sub(self.wheel.0),
            target.wheel.1.wrapping_sub(self.wheel.1),
        );
        if wx != 0 || wy != 0 {
            out.push(InputEvent::Wheel { dx: wx, dy: wy });
        }
        if let Some(pad) = target.gamepad {
            let before = if same_pad {
                self.gamepad.expect("mismo mando")
            } else {
                out.push(InputEvent::GamepadConnected {
                    slot: pad.slot,
                    kind: pad.kind,
                });
                GamepadState::neutral(pad.slot, pad.kind)
            };
            for button in GamepadButton::ALL {
                if pad.pressed(button) && !before.pressed(button) {
                    out.push(InputEvent::GamepadButton {
                        slot: pad.slot,
                        button,
                        pressed: true,
                    });
                }
            }
            for (index, axis) in GamepadAxis::ALL.iter().enumerate() {
                if pad.axes[index] != before.axes[index] {
                    out.push(InputEvent::GamepadAxis {
                        slot: pad.slot,
                        axis: *axis,
                        value: pad.axes[index],
                    });
                }
            }
        }
        out
    }

    /// Comprueba los límites: teclas ordenadas, sin repetir y como mucho [`MAX_PRESSED_KEYS`].
    pub fn check(&self) -> Result<(), InputWireError> {
        if self.keys.len() > MAX_PRESSED_KEYS {
            return Err(InputWireError::TooManyKeys(self.keys.len()));
        }
        if self.keys.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(InputWireError::Postcard(
                "las teclas del estado no van ordenadas".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Error del formato de entrada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputWireError {
    /// Faltan bytes.
    Truncated,
    /// No es un datagrama de entrada (otra versión o pista).
    NotInput,
    /// Versión de stream desconocida.
    UnsupportedVersion(u8),
    /// Una trama del stream anuncia más de [`MAX_STREAM_FRAME`] bytes o está vacía.
    BadFrameLength(usize),
    /// Más de [`MAX_EVENTS`] eventos.
    TooManyEvents(usize),
    /// Más de [`MAX_PRESSED_KEYS`] teclas en el estado.
    TooManyKeys(usize),
    /// Datos de mando tal cual fuera de sus límites ([`PadData::check`]) o más de [`MAX_PADS`].
    Pad(String),
    /// El cuerpo no es un `postcard` válido.
    Postcard(String),
}

impl fmt::Display for InputWireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InputWireError::Truncated => f.write_str("mensaje de entrada truncado"),
            InputWireError::NotInput => f.write_str("no es un datagrama de entrada"),
            InputWireError::UnsupportedVersion(version) => {
                write!(f, "versión del stream de entrada no admitida: {version}")
            }
            InputWireError::BadFrameLength(len) => {
                write!(f, "trama de entrada de {len} bytes no válida")
            }
            InputWireError::TooManyEvents(count) => write!(
                f,
                "{count} eventos en un mensaje de entrada (como mucho {MAX_EVENTS})"
            ),
            InputWireError::TooManyKeys(count) => write!(
                f,
                "{count} teclas pulsadas en el estado (como mucho {MAX_PRESSED_KEYS})"
            ),
            InputWireError::Pad(what) => write!(f, "mando tal cual: {what}"),
            InputWireError::Postcard(err) => write!(f, "mensaje de entrada no válido: {err}"),
        }
    }
}

impl std::error::Error for InputWireError {}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, InputWireError> {
    postcard::to_stdvec(value).map_err(|err| InputWireError::Postcard(err.to_string()))
}

fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, InputWireError> {
    let (value, rest) = postcard::take_from_bytes(bytes)
        .map_err(|err| InputWireError::Postcard(err.to_string()))?;
    if !rest.is_empty() {
        return Err(InputWireError::Postcard(format!(
            "sobran {} bytes tras el mensaje",
            rest.len()
        )));
    }
    Ok(value)
}

/// Paquete de entrada (datagrama): los eventos en orden y el estado completo tras ellos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputPacket {
    /// Secuencia del canal de datagramas (la misma que autentica el sellado).
    pub seq: u32,
    /// Hora del primer evento en µs del reloj del anfitrión (estimado por `Ping`/`Pong`, truncado
    /// a 32 bits); `0` si aún no se conoce el desfase. Sin eventos (una repetición del estado),
    /// la hora de envío.
    pub sent_us: u32,
    /// µs que el primer evento esperó en el invitado hasta salir (captura → envío), saturado.
    pub queued_us: u16,
    /// Eventos, en orden.
    pub events: Vec<InputEvent>,
    /// Estado completo tras `events`.
    pub state: InputState,
    /// Informes de mandos tal cual ([`PadData::Report`]).
    pub pads: Vec<PadData>,
}

/// Mensaje del stream fiable: declaraciones de mandos tal cual ([`PadData::Device`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputMessage {
    /// Secuencia del canal del stream.
    pub seq: u32,
    /// Mandos que se declaran.
    pub pads: Vec<PadData>,
}

fn check_pads(pads: &[PadData]) -> Result<(), InputWireError> {
    if pads.len() > MAX_PADS {
        return Err(InputWireError::Pad(format!(
            "{} datos de mando en un mensaje (como mucho {MAX_PADS})",
            pads.len()
        )));
    }
    pads.iter().try_for_each(PadData::check)
}

impl InputPacket {
    fn check(&self) -> Result<(), InputWireError> {
        if self.events.len() > MAX_EVENTS {
            return Err(InputWireError::TooManyEvents(self.events.len()));
        }
        self.state.check()?;
        check_pads(&self.pads)
    }

    /// Serializa con `postcard`.
    pub fn to_bytes(&self) -> Result<Vec<u8>, InputWireError> {
        self.check()?;
        encode(self)
    }

    /// Inverso de `to_bytes`; rechaza bytes sobrantes y lo que pase de los límites.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, InputWireError> {
        let value: Self = decode(bytes)?;
        value.check()?;
        Ok(value)
    }
}

/// Vista prestada de un [`InputPacket`] para serializarlo sin copiar sus partes: produce
/// exactamente los mismos bytes que [`InputPacket::to_bytes`] (a `postcard` le da igual que una
/// secuencia sea un `Vec` o un slice), así que el anfitrión lo abre con
/// [`InputPacket::from_bytes`].
#[derive(Debug, Clone, Copy, Serialize)]
pub struct InputPacketRef<'a> {
    /// Como [`InputPacket::seq`].
    pub seq: u32,
    /// Como [`InputPacket::sent_us`].
    pub sent_us: u32,
    /// Como [`InputPacket::queued_us`].
    pub queued_us: u16,
    /// Como [`InputPacket::events`].
    pub events: &'a [InputEvent],
    /// Como [`InputPacket::state`].
    pub state: &'a InputState,
    /// Como [`InputPacket::pads`].
    pub pads: &'a [PadData],
}

impl InputPacketRef<'_> {
    fn check(&self) -> Result<(), InputWireError> {
        if self.events.len() > MAX_EVENTS {
            return Err(InputWireError::TooManyEvents(self.events.len()));
        }
        self.state.check()?;
        check_pads(self.pads)
    }

    /// Serializa con `postcard` en `buf` (sin reservar memoria) y devuelve los bytes escritos.
    /// Con un `buf` de [`MAX_PACKET_BYTES`] siempre cabe.
    pub fn write_to<'b>(&self, buf: &'b mut [u8]) -> Result<&'b [u8], InputWireError> {
        self.check()?;
        postcard::to_slice(self, buf)
            .map(|written| &*written)
            .map_err(|err| InputWireError::Postcard(err.to_string()))
    }
}

/// Bytes en claro de un [`InputPacket`] dentro de los límites, como mucho (holgado): [`MAX_EVENTS`]
/// eventos de hasta 11 bytes, el estado con [`MAX_PRESSED_KEYS`] teclas, [`MAX_PADS`] informes con
/// `coroom_input::send::MAX_PAD_DATAGRAM_BYTES` de datos y las cabeceras.
pub const MAX_PACKET_BYTES: usize = 2048;

impl InputMessage {
    /// Serializa con `postcard`.
    pub fn to_bytes(&self) -> Result<Vec<u8>, InputWireError> {
        check_pads(&self.pads)?;
        encode(self)
    }

    /// Inverso de `to_bytes`; rechaza bytes sobrantes y más de [`MAX_PADS`] mandos.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, InputWireError> {
        let value: Self = decode(bytes)?;
        check_pads(&value.pads)?;
        Ok(value)
    }
}

/// Cabecera en claro de un datagrama de entrada con la clave `key_id`.
pub fn datagram_header(key_id: u32) -> [u8; DATAGRAM_HEADER_LEN] {
    let mut out = [0u8; DATAGRAM_HEADER_LEN];
    out[0] = ObjectHeader::VERSION;
    out[1] = Track::Input.id();
    out[4..].copy_from_slice(&key_id.to_be_bytes());
    out
}

/// Indica si un datagrama es de entrada (sin validar el resto).
pub fn is_input_datagram(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == ObjectHeader::VERSION && bytes[1] == Track::Input.id()
}

/// Separa la clave y el sellado de un datagrama de entrada.
pub fn parse_datagram(bytes: &[u8]) -> Result<(u32, &[u8]), InputWireError> {
    if !is_input_datagram(bytes) {
        return Err(if bytes.len() < 2 {
            InputWireError::Truncated
        } else {
            InputWireError::NotInput
        });
    }
    let Some(header) = bytes.first_chunk::<DATAGRAM_HEADER_LEN>() else {
        return Err(InputWireError::Truncated);
    };
    let key_id = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    Ok((key_id, &bytes[DATAGRAM_HEADER_LEN..]))
}

/// Preámbulo del stream de entrada con la clave `key_id`.
pub fn stream_preamble(key_id: u32) -> [u8; STREAM_PREAMBLE_LEN] {
    let mut out = [0u8; STREAM_PREAMBLE_LEN];
    out[0] = STREAM_VERSION;
    out[1..].copy_from_slice(&key_id.to_be_bytes());
    out
}

/// Lee el preámbulo del stream: la clave.
pub fn parse_stream_preamble(bytes: [u8; STREAM_PREAMBLE_LEN]) -> Result<u32, InputWireError> {
    if bytes[0] != STREAM_VERSION {
        return Err(InputWireError::UnsupportedVersion(bytes[0]));
    }
    Ok(u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]))
}

/// Trama del stream: `longitud (u16 BE) ‖ sellado`.
pub fn stream_frame(sealed: &[u8]) -> Result<Vec<u8>, InputWireError> {
    if sealed.is_empty() || sealed.len() > MAX_STREAM_FRAME {
        return Err(InputWireError::BadFrameLength(sealed.len()));
    }
    let mut out = Vec::with_capacity(2 + sealed.len());
    // Cabe: `MAX_STREAM_FRAME < u16::MAX`.
    out.extend_from_slice(&(sealed.len() as u16).to_be_bytes());
    out.extend_from_slice(sealed);
    Ok(out)
}

/// Recompone las tramas del stream a partir de trozos arbitrarios.
#[derive(Debug, Default)]
pub struct StreamFrames {
    buffer: Vec<u8>,
}

impl StreamFrames {
    /// Búfer vacío.
    pub fn new() -> Self {
        Self::default()
    }

    /// Añade bytes leídos.
    pub fn extend(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Bytes pendientes.
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }

    /// Siguiente sellado completo, si ya llegó.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, InputWireError> {
        let Some(prefix) = self.buffer.first_chunk::<2>() else {
            return Ok(None);
        };
        let len = usize::from(u16::from_be_bytes(*prefix));
        if len == 0 || len > MAX_STREAM_FRAME {
            return Err(InputWireError::BadFrameLength(len));
        }
        if self.buffer.len() < 2 + len {
            return Ok(None);
        }
        let frame = self.buffer[2..2 + len].to_vec();
        self.buffer.drain(..2 + len);
        Ok(Some(frame))
    }
}

/// `true` si la secuencia `a` es posterior a `b` con aritmética circular de 32 bits.
pub const fn seq_after(a: u32, b: u32) -> bool {
    a != b && a.wrapping_sub(b) < 1 << 31
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn slot() -> impl Strategy<Value = u8> {
        1..=MAX_GAMEPAD_SLOTS
    }

    fn event() -> impl Strategy<Value = InputEvent> {
        prop_oneof![
            (any::<u16>(), any::<bool>())
                .prop_map(|(code, pressed)| InputEvent::Key { code, pressed }),
            (
                prop::sample::select(MouseButton::ALL.to_vec()),
                any::<bool>()
            )
                .prop_map(|(button, pressed)| InputEvent::MouseButton { button, pressed }),
            (any::<i32>(), any::<i32>()).prop_map(|(dx, dy)| InputEvent::MouseMove { dx, dy }),
            (any::<u16>(), any::<u16>()).prop_map(|(x, y)| InputEvent::MouseMoveAbsolute { x, y }),
            (any::<i32>(), any::<i32>()).prop_map(|(dx, dy)| InputEvent::Wheel { dx, dy }),
            slot().prop_map(|slot| InputEvent::GamepadConnected {
                slot,
                kind: GamepadKind::Xbox
            }),
            slot().prop_map(|slot| InputEvent::GamepadDisconnected { slot }),
            (
                slot(),
                prop::sample::select(GamepadButton::ALL.to_vec()),
                any::<bool>()
            )
                .prop_map(|(slot, button, pressed)| InputEvent::GamepadButton {
                    slot,
                    button,
                    pressed
                }),
            (
                slot(),
                prop::sample::select(GamepadAxis::ALL.to_vec()),
                any::<i16>()
            )
                .prop_map(|(slot, axis, value)| InputEvent::GamepadAxis {
                    slot,
                    axis,
                    value
                }),
            Just(InputEvent::ReleaseAll),
        ]
    }

    fn events() -> impl Strategy<Value = Vec<InputEvent>> {
        prop::collection::vec(event(), 0..=MAX_EVENTS)
    }

    fn pad() -> impl Strategy<Value = PadData> {
        let bytes = |max: usize| prop::collection::vec(any::<u8>(), 1..max);
        let transport = prop_oneof![
            (
                bytes(300),
                prop::collection::vec((any::<u8>(), bytes(80)), 0..4)
            )
                .prop_map(|(descriptor, features)| GamepadTransport::Hid {
                    descriptor,
                    features
                }),
            Just(GamepadTransport::XInput),
            "[a-z ]{0,40}".prop_map(|reason| GamepadTransport::Converted { reason }),
        ];
        prop_oneof![
            (slot(), bytes(80)).prop_map(|(slot, data)| PadData::Report { slot, data }),
            (slot(), "[A-Za-z0-9 ]{0,60}", any::<[u16; 4]>(), transport).prop_map(
                |(slot, name, [vendor, product, version, bus], transport)| {
                    PadData::Device {
                        slot,
                        device: GamepadDevice {
                            name,
                            vendor,
                            product,
                            version,
                            bus,
                            transport,
                        },
                    }
                }
            ),
        ]
    }

    fn pads() -> impl Strategy<Value = Vec<PadData>> {
        prop::collection::vec(pad(), 0..4)
    }

    /// Un estado cualquiera: el que resulta de aplicar eventos al vacío.
    fn state() -> impl Strategy<Value = InputState> {
        events().prop_map(|events| {
            let mut state = InputState::default();
            for event in &events {
                state.apply(event);
            }
            state
        })
    }

    proptest! {
        #[test]
        fn packets_round_trip(seq in any::<u32>(), sent_us in any::<u32>(), queued_us in any::<u16>(), events in events(), state in state(), pads in pads()) {
            let packet = InputPacket { seq, sent_us, queued_us, events, state, pads };
            let bytes = packet.to_bytes().expect("serializar");
            prop_assert_eq!(InputPacket::from_bytes(&bytes), Ok(packet));
        }

        #[test]
        fn messages_round_trip(seq in any::<u32>(), pads in pads()) {
            let message = InputMessage { seq, pads };
            let bytes = message.to_bytes().expect("serializar");
            prop_assert_eq!(InputMessage::from_bytes(&bytes), Ok(message.clone()));
            if bytes.len() > 1 {
                prop_assert!(InputMessage::from_bytes(&bytes[..bytes.len() - 1]).is_err());
            }
        }

        /// Aplicar a un estado los eventos de su diferencia con otro lo deja igual que el otro:
        /// así el anfitrión reconcilia lo que se perdió.
        #[test]
        fn applying_the_diff_reaches_the_target(from in state(), to in state()) {
            let mut state = from.clone();
            for event in from.diff(&to) {
                state.apply(&event);
            }
            // La posición absoluta no se «desconoce»: sin una nueva, queda la que había.
            let mut expected = to.clone();
            if expected.absolute.is_none() {
                expected.absolute = from.absolute;
            }
            prop_assert_eq!(state, expected);
            prop_assert!(from.diff(&from).is_empty(), "sin cambios no hay eventos");
        }

        /// Un estado mantiene las teclas ordenadas y sin repetir, y nunca más de las que caben.
        #[test]
        fn states_stay_within_limits(state in state()) {
            prop_assert!(state.check().is_ok());
            prop_assert!(state.keys.len() <= MAX_PRESSED_KEYS);
        }

        #[test]
        fn events_round_trip_alone(event in event()) {
            let bytes = postcard::to_stdvec(&event).expect("serializar");
            prop_assert_eq!(decode::<InputEvent>(&bytes), Ok(event));
        }

        #[test]
        fn garbage_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..128)) {
            let _ = InputPacket::from_bytes(&bytes);
            let _ = InputMessage::from_bytes(&bytes);
            let _ = parse_datagram(&bytes);
            let mut frames = StreamFrames::new();
            frames.extend(&bytes);
            let _ = frames.next_frame();
        }

        #[test]
        fn datagram_headers_round_trip(key_id in any::<u32>(), body in prop::collection::vec(any::<u8>(), 0..64)) {
            let mut datagram = datagram_header(key_id).to_vec();
            datagram.extend_from_slice(&body);
            prop_assert!(is_input_datagram(&datagram));
            prop_assert_eq!(parse_datagram(&datagram), Ok((key_id, &body[..])));
        }

        #[test]
        fn stream_frames_survive_chunking(
            frames in prop::collection::vec(prop::collection::vec(any::<u8>(), 1..200), 1..10),
            chunk in 1usize..50,
        ) {
            let mut stream = Vec::new();
            for frame in &frames {
                stream.extend(stream_frame(frame).expect("trama"));
            }
            let mut buffer = StreamFrames::new();
            let mut out = Vec::new();
            for part in stream.chunks(chunk) {
                buffer.extend(part);
                while let Some(frame) = buffer.next_frame().expect("trama") {
                    out.push(frame);
                }
            }
            prop_assert_eq!(out, frames);
            prop_assert_eq!(buffer.pending(), 0);
        }
    }

    #[test]
    fn too_many_events_are_rejected() {
        let packet = InputPacket {
            seq: 1,
            sent_us: 0,
            queued_us: 0,
            events: vec![InputEvent::ReleaseAll; MAX_EVENTS + 1],
            state: InputState::default(),
            pads: Vec::new(),
        };
        assert_eq!(
            packet.to_bytes(),
            Err(InputWireError::TooManyEvents(MAX_EVENTS + 1))
        );
        let raw = postcard::to_stdvec(&packet).expect("serializar");
        assert!(matches!(
            InputPacket::from_bytes(&raw),
            Err(InputWireError::TooManyEvents(_))
        ));
    }

    /// La vista prestada produce los mismos bytes que el paquete con sus `Vec`, y el peor caso
    /// dentro de los límites cabe en [`MAX_PACKET_BYTES`].
    #[test]
    fn the_borrowed_view_serializes_like_the_packet() {
        let state = InputState {
            keys: (0..MAX_PRESSED_KEYS as u16).map(|k| 300 + k * 7).collect(),
            buttons: 0x1f,
            motion: (i32::MIN, i32::MAX),
            wheel: (-120_000, 120_000),
            absolute: Some((u16::MAX, u16::MAX)),
            gamepad: Some(GamepadState {
                slot: 8,
                kind: GamepadKind::Xbox,
                buttons: u16::MAX,
                axes: [i16::MIN, i16::MAX, i16::MIN, i16::MAX, i16::MAX, i16::MIN],
            }),
        };
        let packet = InputPacket {
            seq: u32::MAX,
            sent_us: u32::MAX,
            queued_us: u16::MAX,
            events: vec![
                InputEvent::MouseMove {
                    dx: i32::MIN,
                    dy: i32::MAX
                };
                MAX_EVENTS
            ],
            state,
            // 800 bytes de informes (lo que admite `coroom_input::send` por datagrama).
            pads: vec![
                PadData::Report {
                    slot: 1,
                    data: vec![0xff; 50],
                };
                MAX_PADS
            ],
        };
        let view = InputPacketRef {
            seq: packet.seq,
            sent_us: packet.sent_us,
            queued_us: packet.queued_us,
            events: &packet.events,
            state: &packet.state,
            pads: &packet.pads,
        };
        let mut buf = [0u8; MAX_PACKET_BYTES];
        let written = view.write_to(&mut buf).expect("cabe");
        assert_eq!(written, packet.to_bytes().unwrap().as_slice());
        assert_eq!(InputPacket::from_bytes(written).unwrap(), packet);
        assert!(written.len() < MAX_PACKET_BYTES, "{} bytes", written.len());

        let extra = vec![InputEvent::ReleaseAll; MAX_EVENTS + 1];
        let too_many = InputPacketRef {
            events: &extra,
            ..view
        };
        assert_eq!(
            too_many.write_to(&mut buf).err(),
            Some(InputWireError::TooManyEvents(MAX_EVENTS + 1))
        );
        let mut small = [0u8; 8];
        assert!(matches!(
            view.write_to(&mut small),
            Err(InputWireError::Postcard(_))
        ));
    }

    #[test]
    fn the_stale_threshold_follows_the_keepalive() {
        assert_eq!(STALE_AFTER, KEEPALIVE_PRESSED * STALE_KEEPALIVES);
        assert!(KEEPALIVE_PRESSED < KEEPALIVE_IDLE);
        let mut state = InputState::default();
        assert!(!state.any_pressed());
        state.apply(&InputEvent::GamepadConnected {
            slot: 1,
            kind: GamepadKind::Xbox,
        });
        assert!(
            !state.any_pressed(),
            "un mando en reposo no es nada pulsado"
        );
        state.apply(&InputEvent::GamepadAxis {
            slot: 1,
            axis: GamepadAxis::LeftX,
            value: 5,
        });
        assert!(state.any_pressed());
        state.release_all();
        assert!(!state.any_pressed());
        state.apply(&InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        });
        assert!(state.any_pressed());
    }

    #[test]
    fn pad_data_out_of_bounds_is_rejected() {
        let report = |len: usize| PadData::Report {
            slot: 1,
            data: vec![0; len],
        };
        assert!(report(64).check().is_ok());
        assert!(report(0).check().is_err());
        assert!(report(crate::hid::MAX_REPORT_BYTES + 1).check().is_err());
        let device = |transport| PadData::Device {
            slot: 1,
            device: GamepadDevice {
                name: "8BitDo Ultimate 2".into(),
                vendor: 0x2dc8,
                product: 0x6012,
                version: 0x0111,
                bus: 3,
                transport,
            },
        };
        assert!(
            device(GamepadTransport::Hid {
                descriptor: vec![0; 200],
                features: vec![(5, vec![0; 41])]
            })
            .check()
            .is_ok()
        );
        assert!(
            device(GamepadTransport::Hid {
                descriptor: vec![0; crate::hid::MAX_DESCRIPTOR + 1],
                features: Vec::new()
            })
            .check()
            .is_err()
        );
        assert!(
            device(GamepadTransport::Hid {
                descriptor: vec![0; 10],
                features: vec![(1, vec![0; 400]); MAX_FEATURES + 1]
            })
            .check()
            .is_err()
        );
        // Con la trama de stream más grande posible, cabe.
        let big = InputMessage {
            seq: 1,
            pads: vec![device(GamepadTransport::Hid {
                descriptor: vec![0xAB; crate::hid::MAX_DESCRIPTOR],
                features: vec![(1, vec![0xCD; 256]); MAX_FEATURE_BYTES / 256],
            })],
        };
        let bytes = big.to_bytes().expect("cabe");
        assert!(bytes.len() + 64 <= MAX_STREAM_FRAME, "{}", bytes.len());
        let too_many = InputPacket {
            seq: 1,
            sent_us: 0,
            queued_us: 0,
            events: Vec::new(),
            state: InputState::default(),
            pads: vec![report(8); MAX_PADS + 1],
        };
        assert!(matches!(too_many.to_bytes(), Err(InputWireError::Pad(_))));
    }

    #[test]
    fn input_datagrams_are_told_apart_from_media() {
        let media = ObjectHeader {
            track: Track::Video,
            flags: ObjectHeader::LAST_OBJECT,
            group: 1,
            object: 0,
            object_count: 1,
            payload_len: 0,
            epoch_hint: 0,
        };
        assert!(!is_input_datagram(&media.to_bytes()));
        assert_eq!(
            parse_datagram(&media.to_bytes()),
            Err(InputWireError::NotInput)
        );
        assert!(is_input_datagram(&datagram_header(7)));
        assert_eq!(
            parse_datagram(&datagram_header(7)[..5]),
            Err(InputWireError::Truncated)
        );
    }

    #[test]
    fn stream_preamble_and_frames_are_checked() {
        assert_eq!(
            parse_stream_preamble(stream_preamble(0xdead_beef)),
            Ok(0xdead_beef)
        );
        assert_eq!(
            parse_stream_preamble([9, 0, 0, 0, 0]),
            Err(InputWireError::UnsupportedVersion(9))
        );
        assert!(stream_frame(&[]).is_err());
        assert!(stream_frame(&vec![0; MAX_STREAM_FRAME + 1]).is_err());
        let mut frames = StreamFrames::new();
        frames.extend(&[0, 0]);
        assert_eq!(frames.next_frame(), Err(InputWireError::BadFrameLength(0)));
    }

    #[test]
    fn classes_follow_the_permissions() {
        assert_eq!(
            InputEvent::Key {
                code: 30,
                pressed: true
            }
            .class(),
            EventClass::KeyboardMouse
        );
        assert_eq!(
            InputEvent::GamepadAxis {
                slot: 2,
                axis: GamepadAxis::LeftX,
                value: 5
            }
            .class(),
            EventClass::Gamepad(2)
        );
        assert_eq!(InputEvent::ReleaseAll.class(), EventClass::Control);
        assert!(InputEvent::MouseMove { dx: 1, dy: 1 }.is_continuous());
        assert!(!InputEvent::Wheel { dx: 0, dy: 120 }.is_continuous());
    }

    #[test]
    fn sequences_wrap_around() {
        assert!(seq_after(1, 0));
        assert!(!seq_after(0, 1));
        assert!(!seq_after(5, 5));
        assert!(seq_after(0, u32::MAX));
        assert!(seq_after(10, u32::MAX - 10));
    }
}
