//! [`InputCapture`]: los eventos de la ventana `winit` del invitado de escritorio (en Android no
//! hay `winit`: el tacto va por [`crate::touch`]).

use std::collections::BTreeSet;

use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use super::{Action, Allowed, CaptureMode, VideoRect, WHEEL_PIXELS_PER_NOTCH, whole};
use crate::keymap::evdev_from_physical;
use crate::{InputEvent, MouseButton};

/// Letras de los atajos (siempre con `Ctrl+Alt`); la interfaz deja cambiarlas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShortcutKeys {
    /// Inmersivo / escritorio (`G`).
    pub immersive: KeyCode,
    /// Overlay o panel de conexión (`S`).
    pub overlay: KeyCode,
    /// Chat de la sala (`C`).
    pub chat: KeyCode,
    /// Salir (`Q`).
    pub quit: KeyCode,
    /// Micrófono (`M`).
    pub mic: KeyCode,
    /// Audio del juego (`V`).
    pub game: KeyCode,
    /// Panel de mandos (`J`).
    pub pads: KeyCode,
    /// Soltar el teclado y el ratón (`R`): deja de reenviar hasta el próximo clic en el vídeo.
    pub release: KeyCode,
    /// Apagar o encender el vídeo (`I`).
    pub video: KeyCode,
}

impl Default for ShortcutKeys {
    fn default() -> Self {
        Self {
            immersive: KeyCode::KeyG,
            overlay: KeyCode::KeyS,
            chat: KeyCode::KeyC,
            quit: KeyCode::KeyQ,
            mic: KeyCode::KeyM,
            game: KeyCode::KeyV,
            pads: KeyCode::KeyJ,
            release: KeyCode::KeyR,
            video: KeyCode::KeyI,
        }
    }
}

/// Traductor de eventos de la ventana.
#[derive(Debug, Default)]
pub struct InputCapture {
    mode: CaptureMode,
    allowed: Allowed,
    grabbed: bool,
    /// Reenviando teclado y ratón (desde el clic en el vídeo hasta soltarlo).
    engaged: bool,
    focused: bool,
    ctrl: [bool; 2],
    alt: [bool; 2],
    /// Alt según el sistema (`ModifiersChanged`), que no cuenta AltGr.
    system_alt: bool,
    keys: BTreeSet<u16>,
    buttons: BTreeSet<MouseButton>,
    frame: Option<(u32, u32)>,
    window: (u32, u32),
    /// Última posición del cursor en la ventana (para llevar el del anfitrión al capturar).
    pointer: Option<(f64, f64)>,
    motion_rest: (f64, f64),
    wheel_rest: (f64, f64),
    push_to_talk: Option<KeyCode>,
    talking: bool,
    /// Si el vídeo estaba delante la última vez que lo dijo la ventana ([`Self::set_in_front`]).
    in_front: bool,
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

    /// Si se reenvían el teclado y el ratón ahora: hay permiso y el invitado hizo clic en el
    /// vídeo sin soltarlo después (atajo o foco perdido).
    pub fn forwarding(&self) -> bool {
        self.allowed.keyboard_mouse && self.engaged
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

    /// Nuevos permisos; si se pierde el teclado y ratón, suelta lo pulsado y el cursor. Recién
    /// dado, no se reenvía nada hasta el primer clic en el vídeo.
    pub fn set_allowed(&mut self, allowed: Allowed) -> Vec<Action> {
        let mut out = Vec::new();
        if self.allowed.keyboard_mouse && !allowed.keyboard_mouse {
            out.extend(self.release());
            out.extend(self.ungrab());
        }
        if self.allowed.keyboard_mouse != allowed.keyboard_mouse {
            self.engaged = false;
        }
        self.allowed = allowed;
        out
    }

    /// Deja de reenviar el teclado y el ratón (`Ctrl+Alt+R`) sin tocar el permiso: suelta lo
    /// pulsado en el anfitrión y el cursor capturado. Un clic en el vídeo lo vuelve a capturar.
    pub fn release_forwarding(&mut self) -> Vec<Action> {
        if !self.engaged {
            return Vec::new();
        }
        self.engaged = false;
        let mut out = self.release();
        out.extend(self.ungrab());
        out
    }

    /// Suelta en el anfitrión lo pulsado y el cursor capturado, pero sigue reenviando (el
    /// teclado pasa un momento a la interfaz, como al escribir en el chat). Si se estaba
    /// hablando con la tecla de pulsar para hablar, cierra el micrófono: su soltar irá a la
    /// interfaz y no llegaría aquí.
    pub fn release_held(&mut self) -> Vec<Action> {
        let mut out = self.release();
        out.extend(self.stop_talking());
        out.extend(self.ungrab());
        out
    }

    /// Si el vídeo está delante (nada lo tapa: un diálogo, otra pantalla). Al dejar de estarlo,
    /// el teclado y el ratón pasan a la interfaz y lo que se suelte ya no llega aquí: suelta lo
    /// pulsado, el cursor y la tecla de pulsar para hablar ([`Self::release_held`]).
    pub fn set_in_front(&mut self, in_front: bool) -> Vec<Action> {
        let covered = self.in_front && !in_front;
        self.in_front = in_front;
        if covered {
            self.release_held()
        } else {
            Vec::new()
        }
    }

    fn stop_talking(&mut self) -> Option<Action> {
        self.talking.then(|| {
            self.talking = false;
            Action::PushToTalk(false)
        })
    }

    fn ungrab(&mut self) -> Option<Action> {
        self.grabbed.then(|| {
            self.grabbed = false;
            Action::Grab(false)
        })
    }

    /// La tecla de pulsar para hablar con el teclado en la interfaz pero la sala abierta (el
    /// invitado apagó el vídeo): si `physical` es esa tecla, abre o cierra el micrófono como con
    /// el vídeo delante (`Some`, con lo que haya que hacer: la tecla no es de la interfaz); si no,
    /// `None`. Ni atajos ni reenvío al anfitrión. El estado es el mismo que con el vídeo delante:
    /// lo que se pulse con el vídeo apagado se suelta bien al encenderlo, y al revés.
    pub fn push_to_talk_key(
        &mut self,
        physical: PhysicalKey,
        pressed: bool,
        repeat: bool,
    ) -> Option<Vec<Action>> {
        let PhysicalKey::Code(code) = physical else {
            return None;
        };
        if self.push_to_talk != Some(code) {
            return None;
        }
        if repeat || pressed == self.talking {
            return Some(Vec::new());
        }
        self.talking = pressed;
        Some(vec![Action::PushToTalk(pressed)])
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

    /// Ctrl y Alt como los ve el sistema (`WindowEvent::ModifiersChanged`): reajusta lo que esta
    /// captura deduce de las teclas (que puede quedarse atrás si una pulsación llegó con la
    /// ventana sin foco) sin enviar nada al anfitrión. El Alt del sistema no incluye AltGr (ni en
    /// Linux, donde es otro modificador, ni en Windows, donde `winit` lo filtra): con él vale el
    /// Alt derecho de una distribución sin AltGr.
    pub fn set_modifiers(&mut self, ctrl: bool, alt: bool) {
        if !ctrl {
            self.ctrl = [false; 2];
        } else if !self.ctrl.iter().any(|down| *down) {
            self.ctrl[0] = true;
        }
        if !alt {
            self.alt = [false; 2];
        }
        self.system_alt = alt;
    }

    /// Si `Ctrl+Alt` está pulsado para un atajo: algún Ctrl y el Alt izquierdo, o un Alt que el
    /// sistema da por Alt (nunca AltGr).
    fn ctrl_alt(&self) -> bool {
        self.ctrl.iter().any(|down| *down) && (self.alt[0] || self.system_alt)
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
        // Pedir el inmersivo es pedir el ratón para el juego: captura el reenvío.
        if self.mode == CaptureMode::Immersive && self.allowed.keyboard_mouse && self.focused {
            self.engaged = true;
        }
        let grab = self.mode == CaptureMode::Immersive && self.forwarding();
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
        if !self.ctrl_alt() {
            return None;
        }
        let keys = self.shortcuts;
        if code == keys.release {
            Some(self.release_forwarding())
        } else if code == keys.immersive {
            Some(self.toggle_mode())
        } else if code == keys.overlay {
            Some(vec![Action::ToggleOverlay])
        } else if code == keys.chat {
            Some(vec![Action::ToggleChat])
        } else if code == keys.mic {
            Some(vec![Action::ToggleMicMute])
        } else if code == keys.game {
            Some(vec![Action::ToggleGameMute])
        } else if code == keys.pads {
            Some(vec![Action::TogglePads])
        } else if code == keys.video {
            Some(vec![Action::ToggleVideo])
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
            if let Some(actions) = self.push_to_talk_key(physical, pressed, repeat) {
                return actions;
            }
            if code == KeyCode::Escape && pressed && !self.allowed.keyboard_mouse {
                return vec![Action::Quit];
            }
        }
        if !self.forwarding() || repeat {
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
        Some(VideoRect::fit(frame, self.window).absolute(x, y))
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
                self.system_alt = false;
                self.engaged = false;
                let mut out = self.release();
                out.extend(self.stop_talking());
                out.extend(self.ungrab());
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
                self.pointer = Some((position.x, position.y));
                if !self.forwarding() || self.mode != CaptureMode::Desktop {
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
                if !self.engaged {
                    // El clic que captura el reenvío no es del juego (ni su soltar).
                    return if pressed { self.engage() } else { Vec::new() };
                }
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
                if !self.forwarding() {
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

    /// Un clic en el vídeo captura el reenvío: en escritorio, el cursor del anfitrión va adonde
    /// está el del invitado; en inmersivo, se captura el cursor.
    fn engage(&mut self) -> Vec<Action> {
        self.engaged = true;
        match self.mode {
            CaptureMode::Desktop => self
                .pointer
                .and_then(|(x, y)| self.absolute(x, y))
                .map(Action::Send)
                .into_iter()
                .collect(),
            CaptureMode::Immersive => {
                self.grabbed = true;
                vec![Action::Grab(true)]
            }
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
        // El clic que captura el reenvío.
        assert!(sent(&capture.on_window_event(&click(true))).is_empty());
        assert!(capture.on_window_event(&click(false)).is_empty());
        assert!(capture.forwarding());
        capture
    }

    fn press(capture: &mut InputCapture, code: KeyCode, pressed: bool) -> Vec<Action> {
        capture.key(PhysicalKey::Code(code), pressed, false)
    }

    fn release_shortcut(capture: &mut InputCapture) -> Vec<Action> {
        press(capture, KeyCode::ControlLeft, true);
        press(capture, KeyCode::AltLeft, true);
        let actions = press(capture, KeyCode::KeyR, true);
        assert!(
            press(capture, KeyCode::KeyR, false).is_empty(),
            "la R no se envía"
        );
        press(capture, KeyCode::AltLeft, false);
        press(capture, KeyCode::ControlLeft, false);
        actions
    }

    fn moved(capture: &mut InputCapture, x: f64, y: f64) -> Vec<Action> {
        capture.on_window_event(&WindowEvent::CursorMoved {
            device_id: device(),
            position: PhysicalPosition::new(x, y),
        })
    }

    /// `AltGr+Q` es `@` en la distribución latinoamericana: no sale de la sala y llega al juego
    /// como AltGr y Q (el anfitrión lo escribe con su distribución).
    #[test]
    fn altgr_is_not_the_alt_of_the_shortcuts() {
        let q = |capture: &mut InputCapture| press(capture, KeyCode::KeyQ, true);
        let quits = |actions: &[Action]| actions.contains(&Action::Quit);
        let altgr_q = vec![
            InputEvent::Key {
                code: 100,
                pressed: true,
            },
            InputEvent::Key {
                code: 16,
                pressed: true,
            },
        ];
        // Windows: `winit` descarta el Ctrl izquierdo sintético y el sistema no da Ctrl ni Alt.
        let mut windows = allowed();
        windows.set_modifiers(false, false);
        let mut out = sent(&press(&mut windows, KeyCode::AltRight, true));
        let actions = q(&mut windows);
        assert!(!quits(&actions));
        out.extend(sent(&actions));
        assert_eq!(out, altgr_q, "al anfitrión, AltGr y Q sin Ctrl");
        // Si el Ctrl sintético llegara a pasar, tampoco: el Alt es el derecho y el sistema no lo
        // da por Alt.
        let mut leaked = allowed();
        press(&mut leaked, KeyCode::ControlLeft, true);
        press(&mut leaked, KeyCode::AltRight, true);
        leaked.set_modifiers(false, false);
        assert!(!quits(&q(&mut leaked)));
        // Linux: AltGr es `ISO_Level3_Shift`, no Alt; ni con Ctrl pulsado de verdad.
        let mut linux = allowed();
        press(&mut linux, KeyCode::ControlLeft, true);
        linux.set_modifiers(true, false);
        press(&mut linux, KeyCode::AltRight, true);
        assert!(!quits(&q(&mut linux)));
        // Ctrl y Alt izquierdos: el atajo, que no se envía.
        let mut left = allowed();
        press(&mut left, KeyCode::ControlLeft, true);
        press(&mut left, KeyCode::AltLeft, true);
        left.set_modifiers(true, true);
        assert_eq!(
            q(&mut left),
            vec![Action::Send(InputEvent::ReleaseAll), Action::Quit]
        );
        // Distribución sin AltGr (inglesa de EE. UU.): el Alt derecho es Alt y vale.
        let mut us = allowed();
        press(&mut us, KeyCode::ControlRight, true);
        press(&mut us, KeyCode::AltRight, true);
        us.set_modifiers(true, true);
        assert!(quits(&q(&mut us)));
        // Sin noticias del sistema (antes del primer `ModifiersChanged`), solo el izquierdo.
        let mut unknown = allowed();
        press(&mut unknown, KeyCode::ControlLeft, true);
        press(&mut unknown, KeyCode::AltRight, true);
        assert!(!quits(&q(&mut unknown)));
    }

    #[test]
    fn a_new_permission_forwards_nothing_until_a_click_on_the_video() {
        let mut capture = InputCapture::new();
        capture.set_host_frame(1920, 1080);
        capture.set_window_size(1000, 1000);
        let granted = capture.set_allowed(Allowed {
            keyboard_mouse: true,
            gamepad_slot: None,
        });
        assert!(granted.is_empty(), "{granted:?}");
        assert!(!capture.forwarding());
        assert!(press(&mut capture, KeyCode::KeyW, true).is_empty());
        assert!(moved(&mut capture, 500.0, 500.0).is_empty());
        assert!(
            capture
                .on_window_event(&wheel(MouseScrollDelta::LineDelta(0.0, 1.0)))
                .is_empty()
        );
        // El clic captura: el cursor del anfitrión va adonde está el del invitado y el botón no
        // llega al juego (ni al pulsar ni al soltar).
        assert_eq!(
            capture.on_window_event(&click(true)),
            vec![Action::Send(InputEvent::MouseMoveAbsolute {
                x: 32768,
                y: 32768
            })]
        );
        assert!(capture.on_window_event(&click(false)).is_empty());
        assert!(capture.forwarding());
        assert_eq!(
            sent(&press(&mut capture, KeyCode::KeyA, true)),
            vec![InputEvent::Key {
                code: 30,
                pressed: true
            }]
        );
        assert_eq!(
            sent(&capture.on_window_event(&click(true))),
            vec![InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true
            }]
        );
    }

    #[test]
    fn the_release_shortcut_stops_forwarding_and_keeps_the_permission() {
        let mut capture = allowed();
        press(&mut capture, KeyCode::KeyW, true);
        capture.on_window_event(&click(true));
        assert_eq!(
            release_shortcut(&mut capture),
            vec![Action::Send(InputEvent::ReleaseAll)],
            "el anfitrión suelta lo pulsado"
        );
        assert!(!capture.forwarding());
        assert!(capture.allowed().keyboard_mouse, "el permiso sigue");
        assert!(press(&mut capture, KeyCode::KeyW, false).is_empty());
        assert!(capture.on_window_event(&click(false)).is_empty());
        assert!(press(&mut capture, KeyCode::KeyD, true).is_empty());
        assert!(moved(&mut capture, 500.0, 500.0).is_empty());
        assert!(
            release_shortcut(&mut capture).is_empty(),
            "ya estaba suelto"
        );
        // Otro clic lo recupera, sin llegar al juego.
        assert_eq!(
            sent(&capture.on_window_event(&click(true))),
            vec![InputEvent::MouseMoveAbsolute { x: 32768, y: 32768 }]
        );
        assert!(capture.on_window_event(&click(false)).is_empty());
        assert!(capture.forwarding());
        // En inmersivo, soltar también suelta el cursor; el modo se queda.
        assert!(capture.set_immersive(true).contains(&Action::Grab(true)));
        assert_eq!(
            release_shortcut(&mut capture),
            vec![Action::Send(InputEvent::ReleaseAll), Action::Grab(false)]
        );
        assert_eq!(capture.mode(), CaptureMode::Immersive);
        assert!(capture.on_mouse_motion(4.0, 4.0).is_empty());
        assert_eq!(
            capture.on_window_event(&click(true)),
            vec![Action::Grab(true)]
        );
        assert!(capture.on_window_event(&click(false)).is_empty());
        assert!(!capture.on_mouse_motion(4.0, 4.0).is_empty());
    }

    #[test]
    fn revoking_the_permission_while_captured_releases_everything() {
        let mut capture = allowed();
        capture.set_immersive(true);
        press(&mut capture, KeyCode::KeyW, true);
        let revoked = capture.set_allowed(Allowed::default());
        assert_eq!(
            revoked,
            vec![Action::Send(InputEvent::ReleaseAll), Action::Grab(false)]
        );
        assert!(!capture.forwarding());
        assert!(capture.on_mouse_motion(3.0, 3.0).is_empty());
        // Si vuelve el permiso, hace falta otro clic.
        capture.set_allowed(Allowed {
            keyboard_mouse: true,
            gamepad_slot: None,
        });
        assert!(!capture.forwarding());
        assert!(press(&mut capture, KeyCode::KeyW, true).is_empty());
        assert_eq!(
            capture.on_window_event(&click(true)),
            vec![Action::Grab(true)]
        );
        assert!(capture.forwarding());
    }

    #[test]
    fn losing_focus_releases_forwarding_until_the_next_click() {
        let mut capture = allowed();
        press(&mut capture, KeyCode::KeyW, true);
        assert_eq!(
            capture.on_window_event(&WindowEvent::Focused(false)),
            vec![Action::Send(InputEvent::ReleaseAll)]
        );
        assert!(
            capture
                .on_window_event(&WindowEvent::Focused(true))
                .is_empty()
        );
        assert!(!capture.forwarding());
        assert!(press(&mut capture, KeyCode::KeyW, true).is_empty());
        assert!(moved(&mut capture, 500.0, 500.0).is_empty());
        assert!(!sent(&capture.on_window_event(&click(true))).is_empty());
        assert!(capture.forwarding());
    }

    #[test]
    fn releasing_held_keys_keeps_forwarding() {
        let mut capture = allowed();
        capture.set_immersive(true);
        press(&mut capture, KeyCode::KeyW, true);
        assert_eq!(
            capture.release_held(),
            vec![Action::Send(InputEvent::ReleaseAll), Action::Grab(false)]
        );
        assert!(
            capture.forwarding(),
            "escribir en el chat no suelta el reenvío"
        );
        assert!(!sent(&press(&mut capture, KeyCode::KeyW, true)).is_empty());
    }

    #[test]
    fn releasing_held_keys_closes_push_to_talk() {
        let mut capture = allowed();
        capture.set_push_to_talk(Some(KeyCode::F12));
        assert_eq!(
            press(&mut capture, KeyCode::F12, true),
            vec![Action::PushToTalk(true)]
        );
        // El chat toma el teclado con la tecla aún pulsada: su soltar no llegará aquí.
        let released = capture.release_held();
        assert!(
            released.contains(&Action::PushToTalk(false)),
            "{released:?}"
        );
        assert!(
            !capture.release_held().contains(&Action::PushToTalk(false)),
            "solo una vez"
        );
        assert!(capture.forwarding());
        assert_eq!(
            press(&mut capture, KeyCode::F12, true),
            vec![Action::PushToTalk(true)],
            "la siguiente pulsación vuelve a abrir el micrófono"
        );
    }

    /// Con el vídeo apagado el teclado es de la interfaz, pero la tecla de hablar sigue abriendo
    /// y cerrando el micrófono; nada más (ni atajos ni reenvío). Pulsada con el vídeo apagado, se
    /// suelta bien con él encendido.
    #[test]
    fn push_to_talk_works_with_the_video_behind() {
        let mut capture = allowed();
        capture.set_push_to_talk(Some(KeyCode::F12));
        assert!(capture.set_in_front(true).is_empty());
        // Se apaga el vídeo sin hablar: el micrófono no se toca.
        assert!(
            !capture
                .set_in_front(false)
                .contains(&Action::PushToTalk(false))
        );
        let key = |code| PhysicalKey::Code(code);
        assert_eq!(
            capture.push_to_talk_key(key(KeyCode::F12), true, false),
            Some(vec![Action::PushToTalk(true)])
        );
        assert_eq!(
            capture.push_to_talk_key(key(KeyCode::F12), true, true),
            Some(Vec::new()),
            "la repetición no cuenta (y no es de la interfaz)"
        );
        assert_eq!(
            capture.push_to_talk_key(key(KeyCode::KeyW), true, false),
            None
        );
        assert_eq!(
            capture.push_to_talk_key(key(KeyCode::KeyI), true, false),
            None
        );
        // Se enciende el vídeo con la tecla pulsada: el soltar llega a la captura.
        assert!(capture.set_in_front(true).is_empty());
        assert_eq!(
            press(&mut capture, KeyCode::F12, false),
            vec![Action::PushToTalk(false)]
        );
        // Y al revés: pulsada con el vídeo delante, al apagarlo se cierra y el soltar no repite.
        assert_eq!(
            press(&mut capture, KeyCode::F12, true),
            vec![Action::PushToTalk(true)]
        );
        assert!(
            capture
                .set_in_front(false)
                .contains(&Action::PushToTalk(false))
        );
        assert_eq!(
            capture.push_to_talk_key(key(KeyCode::F12), false, false),
            Some(Vec::new())
        );
        capture.set_push_to_talk(None);
        assert_eq!(
            capture.push_to_talk_key(key(KeyCode::F12), true, false),
            None
        );
    }

    #[test]
    fn covering_the_video_releases_keys_cursor_and_push_to_talk() {
        let mut capture = allowed();
        capture.set_push_to_talk(Some(KeyCode::F12));
        assert!(capture.set_in_front(true).is_empty(), "nada que soltar");
        capture.set_immersive(true);
        press(&mut capture, KeyCode::KeyW, true);
        press(&mut capture, KeyCode::F12, true);
        // Se abre un diálogo (la oferta de traspaso) con W y F12 aún pulsadas.
        let covered = capture.set_in_front(false);
        assert_eq!(
            covered,
            vec![
                Action::Send(InputEvent::ReleaseAll),
                Action::PushToTalk(false),
                Action::Grab(false)
            ]
        );
        assert!(capture.set_in_front(false).is_empty(), "solo al taparse");
        assert!(
            capture.set_in_front(true).is_empty(),
            "volver no suelta nada"
        );
        assert!(capture.forwarding(), "el permiso y el reenvío siguen");
    }

    #[test]
    fn the_interface_can_switch_modes_and_rename_shortcuts() {
        let mut capture = allowed();
        let actions = capture.set_immersive(true);
        assert!(actions.contains(&Action::Grab(true)), "{actions:?}");
        assert_eq!(capture.mode(), CaptureMode::Immersive);
        assert!(capture.set_immersive(true).is_empty(), "ya lo estaba");
        assert!(capture.set_immersive(false).contains(&Action::Grab(false)));
        // Ctrl+Alt+K en lugar de Ctrl+Alt+S.
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
            !key(&mut capture, KeyCode::KeyS, true).contains(&Action::ToggleOverlay),
            "la S ya no es el atajo"
        );
        assert_eq!(
            key(&mut capture, KeyCode::KeyC, true),
            vec![Action::ToggleChat]
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
        assert_eq!(
            key(&mut capture, KeyCode::KeyI, true),
            vec![Action::ToggleVideo],
            "el vídeo se apaga en el invitado: no llega al anfitrión"
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
        capture.on_window_event(&WindowEvent::Focused(true));
        capture.on_window_event(&click(true));
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
            key(&mut capture, KeyCode::KeyS, true),
            vec![Action::ToggleOverlay]
        );
        let back = key(&mut capture, KeyCode::KeyG, true);
        assert!(back.contains(&Action::Grab(false)));
        assert_eq!(capture.mode(), CaptureMode::Desktop);
    }

    #[test]
    fn the_system_modifiers_reseed_the_shortcut_state_without_sending() {
        let mut capture = allowed();
        // El sistema dice que Ctrl y Alt están pulsados aunque las teclas no pasaran por aquí.
        capture.set_modifiers(true, true);
        assert_eq!(
            capture.key(PhysicalKey::Code(KeyCode::KeyS), true, false),
            vec![Action::ToggleOverlay]
        );
        // Y que se soltaron: la S es una tecla más.
        capture.set_modifiers(false, false);
        assert_eq!(
            sent(&capture.key(PhysicalKey::Code(KeyCode::KeyS), true, false)),
            vec![InputEvent::Key {
                code: 31,
                pressed: true
            }]
        );
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
        // Distribución sin AltGr: el Alt derecho es Alt para el sistema.
        capture.key(PhysicalKey::Code(KeyCode::ControlRight), true, false);
        capture.key(PhysicalKey::Code(KeyCode::AltRight), true, false);
        capture.set_modifiers(true, true);
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
