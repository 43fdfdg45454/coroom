//! Inyección en Windows (feature `windows-sendinput`): teclado y ratón por `SendInput`.
//!
//! - **Teclado** por *scancode* (`KEYEVENTF_SCANCODE`, con `KEYEVENTF_EXTENDEDKEY` en las
//!   extendidas) con la tabla evdev → set 1 de [`crate::keymap`]: la tecla física, sea cual sea la
//!   distribución del anfitrión.
//! - **Ratón**: relativo con `MOUSEEVENTF_MOVE | MOUSEEVENTF_MOVE_NOCOALESCE` (que Windows no junte
//!   los movimientos); absoluto con `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK` sobre la
//!   zona capturada (el monitor o la ventana que comparte WGC, donde esté ahora; la da la fuente
//!   de [`set_pointer_area_source`]) o, sin ella, sobre el monitor principal, expresada en
//!   coordenadas del escritorio virtual, y después `SetCursorPos` al píxel exacto: con dos
//!   monitores, la normalización de `SendInput` dejaba el cursor desplazado, más cuanto más cerca
//!   de una esquina. El hilo que inyecta va en *per-monitor DPI aware v2*, así la zona capturada,
//!   el escritorio virtual y `SetCursorPos` están todos en píxeles físicos. Rueda con
//!   `MOUSEEVENTF_WHEEL`/`HWHEEL` (`WHEEL_DELTA` = 120, como el cable).
//! - **Mandos** (feature `windows-vhid`): con el driver propio de UMDF2
//!   ([`super::windows_vhid::VhidGamepads`]), que se abre al asignar la primera ranura: un Xbox
//!   360 por ranura (el genérico cuando no quedan) y, tal cual
//!   ([`InputSink::connect_raw`]), el mando HID del invitado con su VID/PID y su descriptor o un
//!   Xbox por cable con su identidad. Sin el driver, [`InputSink::assign_gamepad`] devuelve
//!   [`InputError::GamepadUnavailable`] con la guía de instalación; el mapeo a teclas
//!   (`--gamepad-to-keys`, [`super::GamepadToKeys`]) sigue disponible.
//!
//! Los eventos de un paquete salen en **una sola llamada** a `SendInput` ([`InputSink::inject_batch`]):
//! Windows los encola juntos y en orden, sin que otro hilo meta nada entre medias.
//!
//! **Repetición de teclas**: `SendInput` escribe una pulsación y no la repite, así que mantener
//! Retroceso desde el invitado borraría un solo carácter. La captura no manda la repetición del
//! sistema del invitado (sería la cadencia del invitado y gastaría datagramas), así que aquí se
//! sintetiza con el retardo y la cadencia del **anfitrión** (`SPI_GETKEYBOARDDELAY` y
//! `SPI_GETKEYBOARDSPEED`, leídos del sistema en cada pulsación, no fijos) con [`KeyRepeat`]: la
//! última tecla pulsada se repite hasta que se suelta, como en un teclado. La sesión llama a
//! [`InputSink::tick`] cuando toca.
//!
//! El `unsafe` de este módulo es el de llamar a `SendInput`, `GetSystemMetrics` y
//! `SystemParametersInfoW` (`// SAFETY:` en cada uno).

#![allow(unsafe_code)]

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{PoisonError, RwLock};
use std::time::Instant;

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_MOVE_NOCOALESCE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SPI_GETKEYBOARDDELAY,
    SPI_GETKEYBOARDSPEED, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetCursorPos, SystemParametersInfoW,
    XBUTTON1, XBUTTON2,
};

#[cfg(feature = "windows-vhid")]
use super::windows_vhid::{self, DriverHandle, VhidGamepads};
use super::{InputSink, KeyRepeat, PressedKeys};
use crate::keymap::scancode_from_evdev;
use crate::{EventClass, GuestId, InputError, InputEvent, MouseButton};
#[cfg(feature = "windows-vhid")]
use coroom_proto::input::GamepadDevice;

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

/// Retardo y cadencia de la repetición de teclas del sistema (Panel de control → Teclado).
fn system_key_repeat() -> (u32, u32) {
    // Valores de fábrica de Windows si el sistema no contesta: 500 ms y ~30 por segundo.
    let (mut delay, mut speed): (u32, u32) = (1, 31);
    // SAFETY: `SPI_GETKEYBOARDDELAY` y `SPI_GETKEYBOARDSPEED` escriben un `DWORD` en `pvparam`;
    // se les pasan punteros a `u32` locales válidos durante la llamada y `uiparam` se ignora.
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETKEYBOARDDELAY,
            0,
            Some((&raw mut delay).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        let _ = SystemParametersInfoW(
            SPI_GETKEYBOARDSPEED,
            0,
            Some((&raw mut speed).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    (delay, speed)
}

/// Sumidero de Windows: teclado y ratón por `SendInput`, mandos por el driver `coroomvhid`.
pub struct SendInputSink {
    pressed: PressedKeys,
    /// Repetición automática de la última tecla pulsada (`SendInput` no la hace).
    repeat: KeyRepeat,
    /// Los `INPUT` de un grupo (reutilizado).
    inputs: Vec<INPUT>,
    /// Teclas y botones del grupo que se anotaron en `pressed` (para deshacer si falla).
    annotated: Vec<InputEvent>,
    /// Abierto al asignar la primera ranura (sin el driver, se reintenta en la siguiente).
    #[cfg(feature = "windows-vhid")]
    pads: Option<VhidGamepads<DriverHandle>>,
}

impl Default for SendInputSink {
    fn default() -> Self {
        Self::new()
    }
}

impl SendInputSink {
    /// Sumidero listo (no necesita permisos).
    pub fn new() -> Self {
        let (delay, speed) = system_key_repeat();
        Self {
            pressed: PressedKeys::default(),
            repeat: KeyRepeat::from_windows_settings(delay, speed),
            inputs: Vec::with_capacity(64),
            annotated: Vec::with_capacity(16),
            #[cfg(feature = "windows-vhid")]
            pads: None,
        }
    }

    /// Un grupo seguido de eventos de teclado y ratón de `guest` en una sola llamada a
    /// `SendInput`. Devuelve cuántos se aplicaron.
    /// Un grupo de teclado y ratón: se corta después de cada movimiento absoluto, que lleva el
    /// cursor a su píxel exacto ([`place_cursor`]) antes de lo que venga detrás (un clic).
    fn kbm_batch(
        &mut self,
        run: &[InputEvent],
        guest: GuestId,
        errors: &mut Vec<InputError>,
    ) -> usize {
        let mut applied = 0;
        let mut start = 0;
        for (index, event) in run.iter().enumerate() {
            if let InputEvent::MouseMoveAbsolute { x, y } = *event {
                let before = errors.len();
                applied += self.kbm_segment(&run[start..=index], guest, errors);
                if errors.len() == before {
                    place_cursor(x, y);
                }
                start = index + 1;
            }
        }
        if start < run.len() {
            applied += self.kbm_segment(&run[start..], guest, errors);
        }
        applied
    }

    fn kbm_segment(
        &mut self,
        run: &[InputEvent],
        guest: GuestId,
        errors: &mut Vec<InputError>,
    ) -> usize {
        self.inputs.clear();
        self.annotated.clear();
        let mut failed = 0;
        for event in run {
            match *event {
                InputEvent::Key { code, pressed } => {
                    if !self.pressed.key(guest, code, pressed) {
                        continue;
                    }
                    match Self::keyboard(code, pressed) {
                        Ok(input) => {
                            self.inputs.push(input);
                            self.annotated.push(*event);
                        }
                        Err(err) => {
                            // Sin scancode no llega al sistema: se deshace la anotación.
                            self.pressed.key(guest, code, !pressed);
                            errors.push(err);
                            failed += 1;
                        }
                    }
                }
                InputEvent::MouseButton { button, pressed } => {
                    if self.pressed.button(guest, button, pressed) {
                        self.inputs.push(Self::button(button, pressed));
                        self.annotated.push(*event);
                    }
                }
                InputEvent::MouseMove { dx, dy } => self.inputs.push(Self::mouse(
                    dx,
                    dy,
                    0,
                    MOUSEEVENTF_MOVE | MOUSEEVENTF_MOVE_NOCOALESCE,
                )),
                InputEvent::MouseMoveAbsolute { x, y } => self.inputs.push(Self::absolute(x, y)),
                InputEvent::Wheel { dx, dy } => {
                    if dy != 0 {
                        self.inputs
                            .push(Self::mouse(0, 0, dy as u32, MOUSEEVENTF_WHEEL));
                    }
                    if dx != 0 {
                        self.inputs
                            .push(Self::mouse(0, 0, dx as u32, MOUSEEVENTF_HWHEEL));
                    }
                }
                _ => {}
            }
        }
        if self.inputs.is_empty() {
            return run.len() - failed;
        }
        match Self::send(&self.inputs) {
            Ok(()) => {
                let now = Instant::now();
                for event in &self.annotated {
                    if let InputEvent::Key { code, pressed } = *event {
                        if pressed {
                            // La cadencia del anfitrión, por si la cambió.
                            let (delay, speed) = system_key_repeat();
                            let fresh = KeyRepeat::from_windows_settings(delay, speed);
                            let (delay, interval) = fresh.timing();
                            self.repeat.set_timing(delay, interval);
                            self.repeat.pressed(code, now);
                        } else {
                            self.repeat.released(code);
                        }
                    }
                }
                run.len() - failed
            }
            Err(err) => {
                // Nada llegó al sistema: se deshacen las anotaciones en orden inverso.
                for event in self.annotated.iter().rev() {
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
        let (vx, vy, vw, vh) = virtual_desktop();
        let (px, py) = target_pixel(x, y);
        let to_desk = |pixel: i32, origin: i32, virtual_size: i32| {
            (f64::from(pixel - origin) * 65535.0 / f64::from((virtual_size - 1).max(1))).round()
                as i32
        };
        Self::mouse(
            to_desk(px, vx, vw),
            to_desk(py, vy, vh),
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        )
    }
}

/// Pone el hilo en *per-monitor DPI aware v2* (una vez por hilo): `GetSystemMetrics`, el
/// rectángulo de los monitores y `SetCursorPos` en píxeles físicos, como el fotograma de WGC y
/// `DWMWA_EXTENDED_FRAME_BOUNDS`. Sin esto, con un escalado distinto de 100 % (o distinto en cada
/// monitor) unas cosas iban en píxeles lógicos y otras en físicos.
fn per_monitor_aware() {
    thread_local! {
        static AWARE: Cell<bool> = const { Cell::new(false) };
    }
    AWARE.with(|aware| {
        if !aware.get() {
            // SAFETY: cambia el contexto de DPI de este hilo; el valor es una constante del
            // sistema.
            let _ =
                unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
            aware.set(true);
        }
    });
}

/// Origen y tamaño del escritorio virtual en píxeles físicos.
fn virtual_desktop() -> (i32, i32, i32, i32) {
    per_monitor_aware();
    // SAFETY: `GetSystemMetrics` no tiene precondiciones; solo lee métricas del sistema.
    let metric = |index| unsafe { GetSystemMetrics(index) };
    (
        metric(SM_XVIRTUALSCREEN),
        metric(SM_YVIRTUALSCREEN),
        metric(SM_CXVIRTUALSCREEN).max(1),
        metric(SM_CYVIRTUALSCREEN).max(1),
    )
}

/// Píxel del escritorio virtual (físico) que corresponde a `(x, y)` normalizados (`0..=65535`)
/// de la zona capturada, o del monitor principal si no hay.
fn target_pixel(x: u16, y: u16) -> (i32, i32) {
    per_monitor_aware();
    // SAFETY: como en `virtual_desktop`.
    let metric = |index| unsafe { GetSystemMetrics(index) };
    // Sin zona capturada, el monitor principal (su origen es el del escritorio, (0, 0)).
    let area = captured_area().unwrap_or_else(|| PointerArea {
        left: 0,
        top: 0,
        width: metric(SM_CXSCREEN).max(1),
        height: metric(SM_CYSCREEN).max(1),
    });
    let to_pixel = |value: u16, start: i32, size: i32| {
        (f64::from(start) + f64::from(value) / 65535.0 * f64::from(size - 1)).round() as i32
    };
    (
        to_pixel(x, area.left, area.width),
        to_pixel(y, area.top, area.height),
    )
}

/// Lleva el cursor del sistema al píxel exacto de `(x, y)` tras el `SendInput` absoluto (que los
/// juegos con *raw input* necesitan ver). La primera vez que `SendInput` lo dejó en otro sitio,
/// lo anota en el registro con las medidas del escritorio.
fn place_cursor(x: u16, y: u16) {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    let (tx, ty) = target_pixel(x, y);
    let mut landed = POINT::default();
    // SAFETY: `landed` es un `POINT` local válido para escribir.
    if unsafe { GetCursorPos(&mut landed) }.is_ok()
        && ((landed.x - tx).abs() > 1 || (landed.y - ty).abs() > 1)
        && !REPORTED.swap(true, Ordering::Relaxed)
    {
        tracing::info!(
            target = ?(tx, ty),
            landed = ?(landed.x, landed.y),
            desktop = ?virtual_desktop(),
            area = ?captured_area(),
            "ratón absoluto: SendInput dejó el cursor en otro píxel; se corrige con SetCursorPos"
        );
    }
    // SAFETY: `SetCursorPos` admite cualquier coordenada (la recorta al escritorio).
    if let Err(err) = unsafe { SetCursorPos(tx, ty) } {
        tracing::debug!(error = %err, "SetCursorPos");
    }
}

impl InputSink for SendInputSink {
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

    /// Los grupos seguidos de teclado y ratón van en una llamada a `SendInput` cada uno; los de
    /// mando, al driver uno a uno (lleva su propio ritmo).
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
                EventClass::Gamepad(_) => run
                    .iter()
                    .filter(|event| match self.gamepad(event, guest) {
                        Ok(()) => true,
                        Err(err) => {
                            errors.push(err);
                            false
                        }
                    })
                    .count(),
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
        let mut inputs = Vec::with_capacity(keys.len() + buttons.len());
        let mut result = Ok(());
        for code in keys {
            self.repeat.released(code);
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

    /// Repite la última tecla pulsada si toca y dice cuándo volver.
    fn tick(&mut self, now: Instant) -> Option<Instant> {
        if let Some(code) = self.repeat.due(now)
            && Self::keyboard(code, true)
                .and_then(|input| Self::send(&[input]))
                .is_err()
        {
            // El sistema no la acepta ahora (otra ventana con más privilegios): se deja de
            // insistir hasta la siguiente pulsación.
            self.repeat.clear();
        }
        self.repeat.next()
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

    #[cfg(feature = "windows-vhid")]
    fn connect_raw(&mut self, slot: u8, device: &GamepadDevice) -> Result<(), InputError> {
        match &mut self.pads {
            Some(pads) => pads.connect_raw(slot, device),
            None => Err(InputError::NoSuchSlot(slot)),
        }
    }

    #[cfg(feature = "windows-vhid")]
    fn raw_report(&mut self, slot: u8, data: &[u8]) -> Result<(), InputError> {
        match &mut self.pads {
            Some(pads) => pads.raw_report(slot, data),
            None => Err(InputError::NoSuchSlot(slot)),
        }
    }

    #[cfg(feature = "windows-vhid")]
    fn disconnect_raw(&mut self, slot: u8) -> Result<(), InputError> {
        match &mut self.pads {
            Some(pads) => pads.disconnect_raw(slot),
            None => Ok(()),
        }
    }

    #[cfg(feature = "windows-vhid")]
    fn take_outputs(&mut self) -> Vec<(u8, Vec<u8>)> {
        self.pads
            .as_mut()
            .map(VhidGamepads::take_outputs)
            .unwrap_or_default()
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
