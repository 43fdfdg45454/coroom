//! Captura en el invitado: eventos de la ventana `winit` → [`InputEvent`].
//!
//! [`InputCapture`] no toca la ventana: devuelve [`Action`] que la ventana aplica (enviar un evento,
//! capturar o soltar el cursor, mostrar u ocultar el overlay, salir), así se prueba sin pantalla.
//!
//! - **Modo escritorio** (por defecto): cursor libre; su posición dentro del vídeo (descontando las
//!   bandas negras) va como [`InputEvent::MouseMoveAbsolute`] en `0..=65535` del fotograma del
//!   anfitrión.
//! - **Modo inmersivo**: la ventana captura el cursor (`CursorGrabMode::Locked`, o `Confined` y
//!   recentrado si el compositor no admite `Locked`) y lo oculta; el movimiento va como
//!   [`InputEvent::MouseMove`] con los deltas crudos de `DeviceEvent::MouseMotion`. Si el cursor se
//!   suelta (foco perdido), un clic en la ventana lo vuelve a capturar (ese clic no se envía).
//! - **Teclado** por tecla física ([`crate::keymap`]); las repeticiones automáticas no se envían
//!   (el sistema del anfitrión repite solo).
//! - **Rueda** en 1/120 de muesca: `LineDelta` × 120 y `PixelDelta` a [`WHEEL_PIXELS_PER_NOTCH`]
//!   píxeles por muesca, guardando los restos.
//! - **Atajos** (no se envían al anfitrión): `Ctrl+Alt+G` alterna inmersivo y escritorio y suelta
//!   la captura; `Ctrl+Alt+O` muestra u oculta el overlay; `Ctrl+Alt+Q` sale;
//!   `Ctrl+Alt+M` silencia o reactiva el micrófono y `Ctrl+Alt+V` el audio del juego. Sin permiso
//!   de teclado, `Esc` también sale.
//! - **Pulsar para hablar** ([`InputCapture::set_push_to_talk`]): la tecla elegida
//!   (`--ptt TECLA`) no va al anfitrión; pulsarla y soltarla dan [`Action::PushToTalk`].
//! - **Foco**: al perderlo se envía [`InputEvent::ReleaseAll`] y se suelta el cursor.
//!
//! Solo se envía lo que el anfitrión permite ([`Allowed`]); sin teclado y ratón, nada.

use std::collections::BTreeSet;

use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use crate::keymap::evdev_from_physical;
use crate::{InputEvent, MouseButton};

/// Píxeles de desplazamiento suave (panel táctil) que cuentan como una muesca de rueda.
pub const WHEEL_PIXELS_PER_NOTCH: f64 = 40.0;

/// Lo que el anfitrión permite a este invitado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Allowed {
    /// Teclado y ratón.
    pub keyboard_mouse: bool,
    /// Ranura de mando.
    pub gamepad_slot: Option<u8>,
}

impl Allowed {
    /// Texto del overlay: "solo mirar", "teclado y ratón", "mando en ranura 2" o los dos.
    pub fn label(&self) -> String {
        match (self.keyboard_mouse, self.gamepad_slot) {
            (false, None) => "solo mirar".to_owned(),
            (true, None) => "teclado y ratón".to_owned(),
            (false, Some(slot)) => format!("mando en ranura {slot}"),
            (true, Some(slot)) => format!("teclado y ratón, mando en ranura {slot}"),
        }
    }
}

/// Modo del ratón.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CaptureMode {
    /// Absoluto, cursor libre.
    #[default]
    Desktop,
    /// Relativo, cursor capturado.
    Immersive,
}

impl CaptureMode {
    /// Nombre para el overlay.
    pub const fn label(self) -> &'static str {
        match self {
            CaptureMode::Desktop => "escritorio",
            CaptureMode::Immersive => "inmersivo",
        }
    }
}

/// Letras de los atajos (siempre con `Ctrl+Alt`); la interfaz deja cambiarlas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShortcutKeys {
    /// Inmersivo / escritorio (`G`).
    pub immersive: KeyCode,
    /// Overlay o panel de estadísticas (`O`).
    pub overlay: KeyCode,
    /// Salir (`Q`).
    pub quit: KeyCode,
    /// Micrófono (`M`).
    pub mic: KeyCode,
    /// Audio del juego (`V`).
    pub game: KeyCode,
}

impl Default for ShortcutKeys {
    fn default() -> Self {
        Self {
            immersive: KeyCode::KeyG,
            overlay: KeyCode::KeyO,
            quit: KeyCode::KeyQ,
            mic: KeyCode::KeyM,
            game: KeyCode::KeyV,
        }
    }
}

/// Lo que la ventana debe hacer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Enviar el evento al anfitrión.
    Send(InputEvent),
    /// Capturar (`true`) o soltar el cursor.
    Grab(bool),
    /// Mostrar u ocultar el overlay.
    ToggleOverlay,
    /// Cerrar la ventana.
    Quit,
    /// Silenciar o reactivar el micrófono (`Ctrl+Alt+M`).
    ToggleMicMute,
    /// Silenciar o reactivar el audio del juego (`Ctrl+Alt+V`).
    ToggleGameMute,
    /// La tecla de pulsar para hablar se pulsó (`true`) o se soltó.
    PushToTalk(bool),
}

/// Traductor de eventos de la ventana.
#[derive(Debug, Default)]
pub struct InputCapture {
    mode: CaptureMode,
    allowed: Allowed,
    grabbed: bool,
    focused: bool,
    ctrl: [bool; 2],
    alt: [bool; 2],
    keys: BTreeSet<u16>,
    buttons: BTreeSet<MouseButton>,
    frame: Option<(u32, u32)>,
    window: (u32, u32),
    motion_rest: (f64, f64),
    wheel_rest: (f64, f64),
    push_to_talk: Option<KeyCode>,
    talking: bool,
    shortcuts: ShortcutKeys,
}

/// Botón de `winit` en el cable.
pub fn mouse_button(button: winit::event::MouseButton) -> Option<MouseButton> {
    Some(match button {
        winit::event::MouseButton::Left => MouseButton::Left,
        winit::event::MouseButton::Right => MouseButton::Right,
        winit::event::MouseButton::Middle => MouseButton::Middle,
        winit::event::MouseButton::Back => MouseButton::Back,
        winit::event::MouseButton::Forward => MouseButton::Forward,
        winit::event::MouseButton::Other(_) => return None,
    })
}

/// Parte entera de `rest + value`, guardando el resto.
fn whole(rest: &mut f64, value: f64) -> i32 {
    *rest += value;
    let out = rest.trunc();
    *rest -= out;
    out.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

impl InputCapture {
    /// Captura en modo escritorio, sin permisos, con la ventana enfocada.
    pub fn new() -> Self {
        Self {
            focused: true,
            ..Self::default()
        }
    }

    /// Modo actual.
    pub fn mode(&self) -> CaptureMode {
        self.mode
    }

    /// Si el cursor está capturado.
    pub fn grabbed(&self) -> bool {
        self.grabbed
    }

    /// Permisos actuales.
    pub fn allowed(&self) -> Allowed {
        self.allowed
    }

    /// Texto del overlay: permisos y modo.
    pub fn status_label(&self) -> String {
        if self.allowed.keyboard_mouse {
            format!("{} · modo {}", self.allowed.label(), self.mode.label())
        } else {
            self.allowed.label()
        }
    }

    /// Tamaño del fotograma del anfitrión (`HostFrameSize`).
    pub fn set_host_frame(&mut self, width: u32, height: u32) {
        self.frame = Some((width, height));
    }

    /// Tamaño de la ventana en píxeles físicos.
    pub fn set_window_size(&mut self, width: u32, height: u32) {
        self.window = (width, height);
    }

    /// Nuevos permisos; si se pierde el teclado y ratón, suelta lo pulsado y el cursor.
    pub fn set_allowed(&mut self, allowed: Allowed) -> Vec<Action> {
        let mut out = Vec::new();
        if self.allowed.keyboard_mouse && !allowed.keyboard_mouse {
            out.extend(self.release());
            if self.grabbed {
                self.grabbed = false;
                out.push(Action::Grab(false));
            }
        }
        self.allowed = allowed;
        out
    }

    /// Tecla de pulsar para hablar (`None`: activación por voz).
    pub fn set_push_to_talk(&mut self, key: Option<KeyCode>) {
        self.push_to_talk = key;
        self.talking = false;
    }

    /// Letras de los atajos.
    pub fn set_shortcuts(&mut self, shortcuts: ShortcutKeys) {
        self.shortcuts = shortcuts;
    }

    /// Pasa al modo inmersivo (`true`) o al de escritorio, como `Ctrl+Alt+G` (desde un botón de
    /// la interfaz).
    pub fn set_immersive(&mut self, immersive: bool) -> Vec<Action> {
        let wanted = if immersive {
            CaptureMode::Immersive
        } else {
            CaptureMode::Desktop
        };
        if wanted == self.mode {
            return Vec::new();
        }
        self.toggle_mode()
    }

    fn toggle_mode(&mut self) -> Vec<Action> {
        let mut out = self.release();
        self.mode = match self.mode {
            CaptureMode::Desktop => CaptureMode::Immersive,
            CaptureMode::Immersive => CaptureMode::Desktop,
        };
        let grab = self.mode == CaptureMode::Immersive && self.allowed.keyboard_mouse;
        if grab != self.grabbed {
            self.grabbed = grab;
            out.push(Action::Grab(grab));
        }
        out
    }

    /// La ventana no pudo capturar el cursor: se vuelve al modo escritorio.
    pub fn grab_failed(&mut self) {
        self.grabbed = false;
        self.mode = CaptureMode::Desktop;
    }

    fn release(&mut self) -> Vec<Action> {
        let had = !self.keys.is_empty() || !self.buttons.is_empty();
        self.keys.clear();
        self.buttons.clear();
        if had || self.allowed.keyboard_mouse {
            vec![Action::Send(InputEvent::ReleaseAll)]
        } else {
            Vec::new()
        }
    }

    fn shortcut(&mut self, code: KeyCode) -> Option<Vec<Action>> {
        if !(self.ctrl.iter().any(|d| *d) && self.alt.iter().any(|d| *d)) {
            return None;
        }
        let keys = self.shortcuts;
        if code == keys.immersive {
            Some(self.toggle_mode())
        } else if code == keys.overlay {
            Some(vec![Action::ToggleOverlay])
        } else if code == keys.mic {
            Some(vec![Action::ToggleMicMute])
        } else if code == keys.game {
            Some(vec![Action::ToggleGameMute])
        } else if code == keys.quit {
            let mut out = self.release();
            out.push(Action::Quit);
            Some(out)
        } else {
            None
        }
    }

    fn key(&mut self, physical: PhysicalKey, pressed: bool, repeat: bool) -> Vec<Action> {
        if let PhysicalKey::Code(code) = physical {
            match code {
                KeyCode::ControlLeft => self.ctrl[0] = pressed,
                KeyCode::ControlRight => self.ctrl[1] = pressed,
                KeyCode::AltLeft => self.alt[0] = pressed,
                KeyCode::AltRight => self.alt[1] = pressed,
                _ => {}
            }
            if pressed
                && !repeat
                && let Some(actions) = self.shortcut(code)
            {
                return actions;
            }
            if self.push_to_talk == Some(code) {
                if repeat || pressed == self.talking {
                    return Vec::new();
                }
                self.talking = pressed;
                return vec![Action::PushToTalk(pressed)];
            }
            if code == KeyCode::Escape && pressed && !self.allowed.keyboard_mouse {
                return vec![Action::Quit];
            }
        }
        if !self.allowed.keyboard_mouse || repeat {
            return Vec::new();
        }
        let Some(code) = evdev_from_physical(physical) else {
            return Vec::new();
        };
        let changed = if pressed {
            self.keys.insert(code)
        } else {
            self.keys.remove(&code)
        };
        if changed {
            vec![Action::Send(InputEvent::Key { code, pressed })]
        } else {
            Vec::new()
        }
    }

    /// Posición normalizada dentro del vídeo (sin las bandas), si se conoce el fotograma.
    fn absolute(&self, x: f64, y: f64) -> Option<InputEvent> {
        let frame = self.frame?;
        let (fw, fh) = (f64::from(frame.0.max(1)), f64::from(frame.1.max(1)));
        let (tw, th) = (
            f64::from(self.window.0.max(1)),
            f64::from(self.window.1.max(1)),
        );
        let scale = (tw / fw).min(th / fh);
        let (w, h) = (fw * scale, fh * scale);
        let (ox, oy) = ((tw - w) / 2.0, (th - h) / 2.0);
        let norm = |value: f64, origin: f64, size: f64| {
            (((value - origin) / size).clamp(0.0, 1.0) * 65535.0).round() as u16
        };
        Some(InputEvent::MouseMoveAbsolute {
            x: norm(x, ox, w),
            y: norm(y, oy, h),
        })
    }

    /// Traduce un evento de la ventana.
    pub fn on_window_event(&mut self, event: &WindowEvent) -> Vec<Action> {
        match event {
            WindowEvent::Focused(focused) => {
                self.focused = *focused;
                if *focused {
                    return Vec::new();
                }
                self.ctrl = [false; 2];
                self.alt = [false; 2];
                let mut out = self.release();
                if self.talking {
                    self.talking = false;
                    out.push(Action::PushToTalk(false));
                }
                if self.grabbed {
                    self.grabbed = false;
                    out.push(Action::Grab(false));
                }
                out
            }
            WindowEvent::Resized(size) => {
                self.set_window_size(size.width, size.height);
                Vec::new()
            }
            WindowEvent::KeyboardInput { event, .. } => self.key(
                event.physical_key,
                event.state == ElementState::Pressed,
                event.repeat,
            ),
            WindowEvent::CursorMoved { position, .. } => {
                if !self.allowed.keyboard_mouse || self.mode != CaptureMode::Desktop {
                    return Vec::new();
                }
                self.absolute(position.x, position.y)
                    .map(Action::Send)
                    .into_iter()
                    .collect()
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if !self.allowed.keyboard_mouse {
                    return Vec::new();
                }
                let pressed = *state == ElementState::Pressed;
                if self.mode == CaptureMode::Immersive && !self.grabbed {
                    // Un clic vuelve a capturar el cursor; ese clic no es del juego.
                    if pressed {
                        self.grabbed = true;
                        return vec![Action::Grab(true)];
                    }
                    return Vec::new();
                }
                let Some(button) = mouse_button(*button) else {
                    return Vec::new();
                };
                let changed = if pressed {
                    self.buttons.insert(button)
                } else {
                    self.buttons.remove(&button)
                };
                if changed {
                    vec![Action::Send(InputEvent::MouseButton { button, pressed })]
                } else {
                    Vec::new()
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if !self.allowed.keyboard_mouse {
                    return Vec::new();
                }
                let (x, y) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (
                        f64::from(*x) * f64::from(coroom_proto::input::WHEEL_NOTCH),
                        f64::from(*y) * f64::from(coroom_proto::input::WHEEL_NOTCH),
                    ),
                    MouseScrollDelta::PixelDelta(position) => (
                        position.x / WHEEL_PIXELS_PER_NOTCH
                            * f64::from(coroom_proto::input::WHEEL_NOTCH),
                        position.y / WHEEL_PIXELS_PER_NOTCH
                            * f64::from(coroom_proto::input::WHEEL_NOTCH),
                    ),
                };
                let dx = whole(&mut self.wheel_rest.0, x);
                let dy = whole(&mut self.wheel_rest.1, y);
                if dx == 0 && dy == 0 {
                    Vec::new()
                } else {
                    vec![Action::Send(InputEvent::Wheel { dx, dy })]
                }
            }
            _ => Vec::new(),
        }
    }

    /// Deltas crudos del ratón (`DeviceEvent::MouseMotion`): solo en inmersivo con el cursor
    /// capturado.
    pub fn on_mouse_motion(&mut self, dx: f64, dy: f64) -> Vec<Action> {
        if !self.allowed.keyboard_mouse
            || self.mode != CaptureMode::Immersive
            || !self.grabbed
            || !self.focused
        {
            return Vec::new();
        }
        let dx = whole(&mut self.motion_rest.0, dx);
        let dy = whole(&mut self.motion_rest.1, dy);
        if dx == 0 && dy == 0 {
            Vec::new()
        } else {
            vec![Action::Send(InputEvent::MouseMove { dx, dy })]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::dpi::{PhysicalPosition, PhysicalSize};
    use winit::event::DeviceId;

    fn device() -> DeviceId {
        DeviceId::dummy()
    }

    fn wheel(delta: MouseScrollDelta) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: device(),
            delta,
            phase: winit::event::TouchPhase::Moved,
        }
    }

    fn click(pressed: bool) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: device(),
            state: if pressed {
                ElementState::Pressed
            } else {
                ElementState::Released
            },
            button: winit::event::MouseButton::Left,
        }
    }

    fn sent(actions: &[Action]) -> Vec<InputEvent> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Send(event) => Some(*event),
                _ => None,
            })
            .collect()
    }

    fn allowed() -> InputCapture {
        let mut capture = InputCapture::new();
        capture.set_allowed(Allowed {
            keyboard_mouse: true,
            gamepad_slot: None,
        });
        capture.set_host_frame(1920, 1080);
        capture.set_window_size(1000, 1000);
        capture
    }

    #[test]
    fn the_interface_can_switch_modes_and_rename_shortcuts() {
        let mut capture = allowed();
        let actions = capture.set_immersive(true);
        assert!(actions.contains(&Action::Grab(true)), "{actions:?}");
        assert_eq!(capture.mode(), CaptureMode::Immersive);
        assert!(capture.set_immersive(true).is_empty(), "ya lo estaba");
        assert!(capture.set_immersive(false).contains(&Action::Grab(false)));
        // Ctrl+Alt+K en lugar de Ctrl+Alt+O.
        capture.set_shortcuts(ShortcutKeys {
            overlay: KeyCode::KeyK,
            ..ShortcutKeys::default()
        });
        let key = |capture: &mut InputCapture, code: KeyCode, pressed: bool| {
            capture.key(PhysicalKey::Code(code), pressed, false)
        };
        key(&mut capture, KeyCode::ControlLeft, true);
        key(&mut capture, KeyCode::AltLeft, true);
        assert_eq!(
            key(&mut capture, KeyCode::KeyK, true),
            vec![Action::ToggleOverlay]
        );
        assert!(
            !key(&mut capture, KeyCode::KeyO, true).contains(&Action::ToggleOverlay),
            "la O ya no es el atajo"
        );
    }

    #[test]
    fn nothing_is_sent_without_permission() {
        let mut capture = InputCapture::new();
        assert!(capture.on_window_event(&click(true)).is_empty());
        assert!(capture.on_mouse_motion(5.0, 5.0).is_empty());
        assert_eq!(capture.status_label(), "solo mirar");
        assert_eq!(
            Allowed {
                keyboard_mouse: false,
                gamepad_slot: Some(2)
            }
            .label(),
            "mando en ranura 2"
        );
    }

    #[test]
    fn desktop_mode_sends_positions_inside_the_video() {
        let mut capture = allowed();
        // 1920×1080 en 1000×1000: vídeo de 1000×562,5 centrado (bandas de 218,75 px).
        let moved = |capture: &mut InputCapture, x: f64, y: f64| {
            sent(&capture.on_window_event(&WindowEvent::CursorMoved {
                device_id: device(),
                position: PhysicalPosition::new(x, y),
            }))
        };
        assert_eq!(
            moved(&mut capture, 0.0, 218.75),
            vec![InputEvent::MouseMoveAbsolute { x: 0, y: 0 }]
        );
        assert_eq!(
            moved(&mut capture, 1000.0, 1000.0),
            vec![InputEvent::MouseMoveAbsolute { x: 65535, y: 65535 }]
        );
        assert_eq!(
            moved(&mut capture, 500.0, 500.0),
            vec![InputEvent::MouseMoveAbsolute { x: 32768, y: 32768 }]
        );
        assert!(
            capture.on_mouse_motion(3.0, 4.0).is_empty(),
            "solo en inmersivo"
        );
    }

    #[test]
    fn audio_shortcuts_and_push_to_talk_stay_local() {
        let mut capture = allowed();
        let key = |capture: &mut InputCapture, code: KeyCode, pressed: bool| {
            capture.key(PhysicalKey::Code(code), pressed, false)
        };
        key(&mut capture, KeyCode::ControlLeft, true);
        key(&mut capture, KeyCode::AltLeft, true);
        assert_eq!(
            key(&mut capture, KeyCode::KeyM, true),
            vec![Action::ToggleMicMute]
        );
        assert_eq!(
            key(&mut capture, KeyCode::KeyV, true),
            vec![Action::ToggleGameMute]
        );
        key(&mut capture, KeyCode::AltLeft, false);
        key(&mut capture, KeyCode::ControlLeft, false);
        capture.set_push_to_talk(Some(KeyCode::F12));
        assert_eq!(
            key(&mut capture, KeyCode::F12, true),
            vec![Action::PushToTalk(true)]
        );
        assert!(
            capture
                .key(PhysicalKey::Code(KeyCode::F12), true, true)
                .is_empty(),
            "la repetición no cuenta"
        );
        assert_eq!(
            key(&mut capture, KeyCode::F12, false),
            vec![Action::PushToTalk(false)]
        );
        key(&mut capture, KeyCode::F12, true);
        let lost = capture.on_window_event(&WindowEvent::Focused(false));
        assert!(lost.contains(&Action::PushToTalk(false)), "{lost:?}");
        // Sin pulsar para hablar, la tecla va al anfitrión como cualquier otra.
        capture.set_push_to_talk(None);
        assert!(!sent(&key(&mut capture, KeyCode::F12, true)).is_empty());
    }

    #[test]
    fn the_toggle_shortcut_switches_to_immersive_and_back() {
        let mut capture = allowed();
        let key = |capture: &mut InputCapture, code: KeyCode, pressed: bool| {
            capture.key(PhysicalKey::Code(code), pressed, false)
        };
        assert_eq!(
            sent(&key(&mut capture, KeyCode::ControlLeft, true)),
            vec![InputEvent::Key {
                code: 29,
                pressed: true
            }]
        );
        key(&mut capture, KeyCode::AltLeft, true);
        let actions = key(&mut capture, KeyCode::KeyG, true);
        assert_eq!(
            actions,
            vec![Action::Send(InputEvent::ReleaseAll), Action::Grab(true)]
        );
        assert_eq!(capture.mode(), CaptureMode::Immersive);
        assert!(
            key(&mut capture, KeyCode::KeyG, false).is_empty(),
            "G no se envía"
        );
        assert_eq!(
            sent(&capture.on_mouse_motion(2.6, -1.2)),
            vec![InputEvent::MouseMove { dx: 2, dy: -1 }]
        );
        assert_eq!(
            sent(&capture.on_mouse_motion(0.6, -0.9)),
            vec![InputEvent::MouseMove { dx: 1, dy: -1 }],
            "los restos se acumulan"
        );
        assert_eq!(
            key(&mut capture, KeyCode::KeyO, true),
            vec![Action::ToggleOverlay]
        );
        let back = key(&mut capture, KeyCode::KeyG, true);
        assert!(back.contains(&Action::Grab(false)));
        assert_eq!(capture.mode(), CaptureMode::Desktop);
    }

    #[test]
    fn losing_focus_releases_everything() {
        let mut capture = allowed();
        capture.key(PhysicalKey::Code(KeyCode::KeyW), true, false);
        assert!(
            capture
                .key(PhysicalKey::Code(KeyCode::KeyW), true, true)
                .is_empty(),
            "repetición"
        );
        capture.on_window_event(&click(true));
        let actions = capture.on_window_event(&WindowEvent::Focused(false));
        assert_eq!(actions, vec![Action::Send(InputEvent::ReleaseAll)]);
        // Soltar después no manda nada (ya se soltó todo).
        assert!(
            capture
                .key(PhysicalKey::Code(KeyCode::KeyW), false, false)
                .is_empty()
        );
    }

    #[test]
    fn an_immersive_click_regrabs_without_reaching_the_game() {
        let mut capture = allowed();
        capture.key(PhysicalKey::Code(KeyCode::ControlRight), true, false);
        capture.key(PhysicalKey::Code(KeyCode::AltRight), true, false);
        capture.key(PhysicalKey::Code(KeyCode::KeyG), true, false);
        capture.on_window_event(&WindowEvent::Focused(false));
        assert!(!capture.grabbed());
        capture.on_window_event(&WindowEvent::Focused(true));
        assert_eq!(
            capture.on_window_event(&click(true)),
            vec![Action::Grab(true)]
        );
        assert!(capture.on_window_event(&click(false)).is_empty());
        assert_eq!(
            sent(&capture.on_window_event(&click(true))),
            vec![InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true
            }]
        );
    }

    #[test]
    fn the_wheel_goes_in_120ths() {
        let mut capture = allowed();
        assert_eq!(
            sent(&capture.on_window_event(&wheel(MouseScrollDelta::LineDelta(0.0, 1.0)))),
            vec![InputEvent::Wheel { dx: 0, dy: 120 }]
        );
        assert_eq!(
            sent(
                &capture.on_window_event(&wheel(MouseScrollDelta::PixelDelta(
                    PhysicalPosition::new(0.0, 10.0)
                )))
            ),
            vec![InputEvent::Wheel { dx: 0, dy: 30 }]
        );
        assert!(
            capture
                .on_window_event(&wheel(MouseScrollDelta::PixelDelta(PhysicalPosition::new(
                    0.1, 0.1
                ))))
                .is_empty()
        );
    }

    #[test]
    fn escape_quits_only_when_watching() {
        let mut watching = InputCapture::new();
        assert_eq!(
            watching.key(PhysicalKey::Code(KeyCode::Escape), true, false),
            vec![Action::Quit]
        );
        let mut playing = allowed();
        assert_eq!(
            sent(&playing.key(PhysicalKey::Code(KeyCode::Escape), true, false)),
            vec![InputEvent::Key {
                code: 1,
                pressed: true
            }]
        );
        // Revocar el permiso suelta lo pulsado.
        let actions = playing.set_allowed(Allowed::default());
        assert_eq!(actions, vec![Action::Send(InputEvent::ReleaseAll)]);
        let _ = playing.on_window_event(&WindowEvent::Resized(PhysicalSize::new(10, 10)));
    }
}
