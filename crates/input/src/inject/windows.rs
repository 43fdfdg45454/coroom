//! Inyección en Windows (feature `windows-sendinput`): teclado y ratón por `SendInput`.
//!
//! - **Teclado** por *scancode* (`KEYEVENTF_SCANCODE`, con `KEYEVENTF_EXTENDEDKEY` en las
//!   extendidas) con la tabla evdev → set 1 de [`crate::keymap`]: la tecla física, sea cual sea la
//!   distribución del anfitrión.
//! - **Ratón**: relativo con `MOUSEEVENTF_MOVE | MOUSEEVENTF_MOVE_NOCOALESCE` (que Windows no junte
//!   los movimientos); absoluto con `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK` sobre la
//!   zona capturada (el monitor o la ventana que comparte WGC, donde esté ahora; la da la fuente
//!   de [`set_pointer_area_source`]) o, sin ella, sobre el monitor principal, expresada en
//!   coordenadas del escritorio virtual; rueda con `MOUSEEVENTF_WHEEL`/`HWHEEL`
//!   (`WHEEL_DELTA` = 120, como el cable).
//! - **Mandos** (feature `windows-vhid`): un mando HID genérico por ranura con el
//!   driver propio sobre Virtual HID Framework ([`super::windows_vhid::VhidGamepads`]), que se
//!   abre al asignar la primera ranura. Sin el driver, [`InputSink::assign_gamepad`] devuelve
//!   [`InputError::GamepadUnavailable`] con la guía de instalación; el mapeo a teclas
//!   (`--gamepad-to-keys`, [`super::GamepadToKeys`]) sigue disponible.
//!
//! El `unsafe` de este módulo es el de llamar a `SendInput` y `GetSystemMetrics` (`// SAFETY:` en
//! cada uno).

#![allow(unsafe_code)]

use std::sync::{PoisonError, RwLock};

use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_MOVE_NOCOALESCE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, XBUTTON1, XBUTTON2,
};

#[cfg(feature = "windows-vhid")]
use super::windows_vhid::{self, DriverHandle, VhidGamepads};
use super::{InputSink, PressedKeys};
use crate::keymap::scancode_from_evdev;
use crate::{GuestId, InputError, InputEvent, MouseButton};

/// Por qué no hay mando virtual en Windows sin el driver.
pub const GAMEPAD_HELP: &str = "Windows no tiene API de usuario para crear un mando virtual: hace \
    falta el driver de mando virtual de coroom (drivers/windows-vhid/README.md). Sin él, \
    `coroom host --gamepad-to-keys` convierte los mandos en teclas con un perfil fijo";

/// Rectángulo de la pantalla en coordenadas del escritorio virtual (píxeles físicos).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointerArea {
    /// Borde izquierdo.
    pub left: i32,
    /// Borde superior.
    pub top: i32,
    /// Ancho.
    pub width: i32,
    /// Alto.
    pub height: i32,
}

/// Fuente de la zona capturada (ver [`set_pointer_area_source`]).
pub type PointerAreaSource = fn() -> Option<PointerArea>;

/// Quién dice qué zona de la pantalla se está capturando.
static AREA_SOURCE: RwLock<Option<PointerAreaSource>> = RwLock::new(None);

/// Fija la fuente de la zona capturada: el ratón absoluto (relativo al fotograma) se lleva a esa
/// zona. Se consulta en cada movimiento, así que puede seguir a una ventana que se mueve. Si da
/// `None` (o no hay fuente), se usa el monitor principal.
pub fn set_pointer_area_source(source: PointerAreaSource) {
    *AREA_SOURCE.write().unwrap_or_else(PoisonError::into_inner) = Some(source);
}

/// Zona capturada según la fuente, si la hay y es válida.
fn captured_area() -> Option<PointerArea> {
    let source = (*AREA_SOURCE.read().unwrap_or_else(PoisonError::into_inner))?;
    source().filter(|area| area.width > 0 && area.height > 0)
}

/// Estado de la entrada del anfitrión para `coroom doctor`.
pub fn status() -> Vec<String> {
    vec![
        "teclado y ratón: SendInput (scancodes y ratón sin agrupar)".to_owned(),
        pads_status(),
    ]
}

/// Si el driver de mando virtual está instalado y su versión.
#[cfg(feature = "windows-vhid")]
fn pads_status() -> String {
    windows_vhid::status()
}

#[cfg(not(feature = "windows-vhid"))]
fn pads_status() -> String {
    format!("mandos: compilado sin la feature windows-vhid. {GAMEPAD_HELP}")
}

/// Sumidero de Windows: teclado y ratón por `SendInput`, mandos por el driver `coroomvhid`.
#[derive(Debug, Default)]
pub struct SendInputSink {
    pressed: PressedKeys,
    /// Abierto al asignar la primera ranura (sin el driver, se reintenta en la siguiente).
    #[cfg(feature = "windows-vhid")]
    pads: Option<VhidGamepads<DriverHandle>>,
}

impl SendInputSink {
    /// Sumidero listo (no necesita permisos).
    pub fn new() -> Self {
        Self::default()
    }

    fn send(inputs: &[INPUT]) -> Result<(), InputError> {
        // SAFETY: `inputs` es un slice válido de `INPUT` inicializados y `cbsize` es el tamaño de
        // `INPUT`, como pide la API; `SendInput` solo lee el slice durante la llamada.
        let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize == inputs.len() {
            Ok(())
        } else {
            Err(InputError::Backend(format!(
                "SendInput solo entregó {sent} de {} eventos (¿una ventana con más privilegios \
                 tiene el foco?)",
                inputs.len()
            )))
        }
    }

    fn keyboard(code: u16, pressed: bool) -> Result<INPUT, InputError> {
        let scancode = scancode_from_evdev(code).ok_or_else(|| {
            InputError::Backend(format!(
                "la tecla evdev {code} no tiene scancode de Windows"
            ))
        })?;
        let mut flags = KEYEVENTF_SCANCODE;
        if scancode.extended() {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        if !pressed {
            flags |= KEYEVENTF_KEYUP;
        }
        Ok(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: scancode.code(),
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        })
    }

    fn mouse(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: data,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn button(button: MouseButton, pressed: bool) -> INPUT {
        let (flags, data) = match (button, pressed) {
            (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
            (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, u32::from(XBUTTON1)),
            (MouseButton::Back, false) => (MOUSEEVENTF_XUP, u32::from(XBUTTON1)),
            (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, u32::from(XBUTTON2)),
            (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, u32::from(XBUTTON2)),
        };
        Self::mouse(0, 0, data, flags)
    }

    /// Posición normalizada de la zona capturada (o del monitor principal) en coordenadas
    /// normalizadas del escritorio virtual (`0..=65535`).
    fn absolute(x: u16, y: u16) -> INPUT {
        // SAFETY: `GetSystemMetrics` no tiene precondiciones; solo lee métricas del sistema.
        let metric = |index| unsafe { GetSystemMetrics(index) };
        let (vx, vy) = (metric(SM_XVIRTUALSCREEN), metric(SM_YVIRTUALSCREEN));
        let (vw, vh) = (
            metric(SM_CXVIRTUALSCREEN).max(1),
            metric(SM_CYVIRTUALSCREEN).max(1),
        );
        // Sin zona capturada, el monitor principal (su origen es el del escritorio, (0, 0)).
        let area = captured_area().unwrap_or_else(|| PointerArea {
            left: 0,
            top: 0,
            width: metric(SM_CXSCREEN).max(1),
            height: metric(SM_CYSCREEN).max(1),
        });
        let to_desk = |value: u16, start: i32, size: i32, origin: i32, virtual_size: i32| {
            // Píxel de la zona → coordenada normalizada del escritorio virtual.
            let pixel = f64::from(start) + f64::from(value) / 65535.0 * f64::from(size - 1);
            ((pixel - f64::from(origin)) * 65535.0 / f64::from((virtual_size - 1).max(1))).round()
                as i32
        };
        Self::mouse(
            to_desk(x, area.left, area.width, vx, vw),
            to_desk(y, area.top, area.height, vy, vh),
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        )
    }
}

impl InputSink for SendInputSink {
    fn inject(&mut self, event: &InputEvent, guest: GuestId) -> Result<(), InputError> {
        match *event {
            InputEvent::Key { code, pressed } => {
                if self.pressed.key(guest, code, pressed)
                    && let Err(err) =
                        Self::keyboard(code, pressed).and_then(|input| Self::send(&[input]))
                {
                    // No llegó al sistema: se deshace la anotación.
                    self.pressed.key(guest, code, !pressed);
                    return Err(err);
                }
                Ok(())
            }
            InputEvent::MouseButton { button, pressed } => {
                if self.pressed.button(guest, button, pressed)
                    && let Err(err) = Self::send(&[Self::button(button, pressed)])
                {
                    self.pressed.button(guest, button, !pressed);
                    return Err(err);
                }
                Ok(())
            }
            InputEvent::MouseMove { dx, dy } => Self::send(&[Self::mouse(
                dx,
                dy,
                0,
                MOUSEEVENTF_MOVE | MOUSEEVENTF_MOVE_NOCOALESCE,
            )]),
            InputEvent::MouseMoveAbsolute { x, y } => Self::send(&[Self::absolute(x, y)]),
            InputEvent::Wheel { dx, dy } => {
                let mut inputs = Vec::with_capacity(2);
                if dy != 0 {
                    inputs.push(Self::mouse(0, 0, dy as u32, MOUSEEVENTF_WHEEL));
                }
                if dx != 0 {
                    inputs.push(Self::mouse(0, 0, dx as u32, MOUSEEVENTF_HWHEEL));
                }
                if inputs.is_empty() {
                    Ok(())
                } else {
                    Self::send(&inputs)
                }
            }
            InputEvent::GamepadConnected { .. }
            | InputEvent::GamepadDisconnected { .. }
            | InputEvent::GamepadButton { .. }
            | InputEvent::GamepadAxis { .. } => self.gamepad(event, guest),
            InputEvent::ReleaseAll => self.release_all(guest),
        }
    }

    fn release_all(&mut self, guest: GuestId) -> Result<(), InputError> {
        let (keys, buttons) = self.pressed.release_guest(guest);
        let mut inputs = Vec::with_capacity(keys.len() + buttons.len());
        let mut result = Ok(());
        for code in keys {
            match Self::keyboard(code, false) {
                Ok(input) => inputs.push(input),
                Err(err) => result = Err(err),
            }
        }
        for button in buttons {
            inputs.push(Self::button(button, false));
        }
        if !inputs.is_empty() {
            result = result.and(Self::send(&inputs));
        }
        #[cfg(feature = "windows-vhid")]
        if let Some(pads) = &mut self.pads {
            result = result.and(pads.release_guest(guest));
        }
        result
    }

    fn assign_gamepad(&mut self, slot: u8) -> Result<(), InputError> {
        #[cfg(feature = "windows-vhid")]
        {
            let pads = match &mut self.pads {
                Some(pads) => pads,
                none => none.insert(windows_vhid::open()?),
            };
            pads.assign(slot)
        }
        #[cfg(not(feature = "windows-vhid"))]
        {
            let _ = slot;
            Err(InputError::GamepadUnavailable(GAMEPAD_HELP.to_owned()))
        }
    }

    fn release_gamepad(&mut self, slot: u8) -> Result<(), InputError> {
        #[cfg(feature = "windows-vhid")]
        if let Some(pads) = &mut self.pads {
            return pads.release(slot);
        }
        // Sin driver abierto no se creó ningún mando.
        let _ = slot;
        Ok(())
    }

    fn describe(&self) -> String {
        #[cfg(feature = "windows-vhid")]
        if self.pads.is_some() {
            return "SendInput y mandos virtuales (driver coroomvhid)".to_owned();
        }
        "SendInput".to_owned()
    }
}

impl SendInputSink {
    /// Un evento de mando: al driver si está abierto.
    fn gamepad(&mut self, event: &InputEvent, guest: GuestId) -> Result<(), InputError> {
        #[cfg(feature = "windows-vhid")]
        if let Some(pads) = &mut self.pads {
            return pads.inject(event, guest);
        }
        let _ = (event, guest);
        Err(InputError::GamepadUnavailable(GAMEPAD_HELP.to_owned()))
    }
}
