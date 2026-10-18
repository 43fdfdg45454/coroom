//! El ratón o el touchpad conectado al teléfono, con el puntero capturado sobre el vídeo
//! (`View.requestPointerCapture`): movimiento relativo, botones y rueda, como el modo inmersivo
//! del escritorio. Y el texto del teclado en pantalla (ñ, á, emoji) que no es ninguna tecla.
//!
//! Aquí solo está la traducción, sin JNI: Android da el movimiento y la rueda en `float` y el
//! protocolo en enteros, así que lo que no llega a una unidad se guarda para el siguiente evento
//! (ningún movimiento lento se pierde); y se recuerda qué botones están pulsados para soltarlos
//! todos al perder la captura ([`Mouse::release`]).

use std::collections::BTreeSet;

use coroom_session::input::{InputEvent, MouseButton};
use coroom_transport::proto::input::WHEEL_NOTCH;

/// `MotionEvent.BUTTON_PRIMARY`.
pub const BUTTON_PRIMARY: i32 = 1;
/// `MotionEvent.BUTTON_SECONDARY`.
pub const BUTTON_SECONDARY: i32 = 2;
/// `MotionEvent.BUTTON_TERTIARY`.
pub const BUTTON_TERTIARY: i32 = 4;
/// `MotionEvent.BUTTON_BACK`.
pub const BUTTON_BACK: i32 = 8;
/// `MotionEvent.BUTTON_FORWARD`.
pub const BUTTON_FORWARD: i32 = 16;

/// El botón del protocolo de un botón de `MotionEvent` (`getActionButton()`); `None` si no es
/// de ratón (los del lápiz, por ejemplo).
pub fn button_from_android(button: i32) -> Option<MouseButton> {
    Some(match button {
        BUTTON_PRIMARY => MouseButton::Left,
        BUTTON_SECONDARY => MouseButton::Right,
        BUTTON_TERTIARY => MouseButton::Middle,
        BUTTON_BACK => MouseButton::Back,
        BUTTON_FORWARD => MouseButton::Forward,
        _ => return None,
    })
}

/// Suma `value` a lo que quedaba en `rest` y devuelve la parte entera; el resto se queda.
fn whole(rest: &mut f64, value: f64) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    *rest += value;
    let out = rest.trunc();
    *rest -= out;
    out.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// El estado del ratón capturado.
#[derive(Debug, Default)]
pub struct Mouse {
    motion_rest: (f64, f64),
    wheel_rest: (f64, f64),
    pressed: BTreeSet<MouseButton>,
}

impl Mouse {
    /// Movimiento relativo en unidades del ratón (`MotionEvent.getX/getY` con
    /// `SOURCE_MOUSE_RELATIVE`, positivo = derecha y abajo).
    pub fn motion(&mut self, dx: f32, dy: f32) -> Option<InputEvent> {
        let dx = whole(&mut self.motion_rest.0, f64::from(dx));
        let dy = whole(&mut self.motion_rest.1, f64::from(dy));
        (dx != 0 || dy != 0).then_some(InputEvent::MouseMove { dx, dy })
    }

    /// Un botón (`getActionButton()`) pulsado o soltado. El primer valor dice si es un botón de
    /// ratón; el evento solo sale si cambia algo (pulsar dos veces no es un segundo clic).
    pub fn button(&mut self, button: i32, down: bool) -> (bool, Option<InputEvent>) {
        let Some(button) = button_from_android(button) else {
            return (false, None);
        };
        let changed = if down {
            self.pressed.insert(button)
        } else {
            self.pressed.remove(&button)
        };
        (
            true,
            changed.then_some(InputEvent::MouseButton {
                button,
                pressed: down,
            }),
        )
    }

    /// La rueda en muescas de Android (`AXIS_HSCROLL`, `AXIS_VSCROLL`: +1 = derecha y arriba por
    /// muesca, con fracciones en los touchpads y las ruedas de alta resolución).
    pub fn wheel(&mut self, horizontal: f32, vertical: f32) -> Option<InputEvent> {
        let notch = f64::from(WHEEL_NOTCH);
        let dx = whole(&mut self.wheel_rest.0, f64::from(horizontal) * notch);
        let dy = whole(&mut self.wheel_rest.1, f64::from(vertical) * notch);
        (dx != 0 || dy != 0).then_some(InputEvent::Wheel { dx, dy })
    }

    /// Suelta los botones pulsados y olvida los restos (se perdió la captura, se abrió un panel,
    /// se sale de la sala).
    pub fn release(&mut self) -> Vec<InputEvent> {
        self.motion_rest = (0.0, 0.0);
        self.wheel_rest = (0.0, 0.0);
        std::mem::take(&mut self.pressed)
            .into_iter()
            .map(|button| InputEvent::MouseButton {
                button,
                pressed: false,
            })
            .collect()
    }
}

/// El texto del teclado en pantalla como eventos, un carácter cada uno; los de control (saltos de
/// línea, tabuladores, borrar) no son texto y se quedan fuera.
pub fn text_events(text: &str) -> impl Iterator<Item = InputEvent> + '_ {
    text.chars()
        .filter(|ch| !ch.is_control())
        .map(|ch| InputEvent::Text { ch })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_motion_adds_up_instead_of_being_lost() {
        let mut mouse = Mouse::default();
        assert_eq!(mouse.motion(0.4, -0.4), None);
        assert_eq!(mouse.motion(0.4, -0.4), None);
        assert_eq!(
            mouse.motion(0.4, -0.4),
            Some(InputEvent::MouseMove { dx: 1, dy: -1 })
        );
        assert_eq!(
            mouse.motion(2.5, 3.0),
            Some(InputEvent::MouseMove { dx: 2, dy: 2 })
        );
        // Quedaban 0,7 y 0,8: medio píxel más completa uno en x y ninguno en y.
        assert_eq!(
            mouse.motion(0.5, 0.0),
            Some(InputEvent::MouseMove { dx: 1, dy: 0 })
        );
        assert_eq!(mouse.motion(f32::NAN, f32::INFINITY), None);
    }

    #[test]
    fn android_buttons_map_to_mouse_buttons_once() {
        assert_eq!(button_from_android(BUTTON_PRIMARY), Some(MouseButton::Left));
        assert_eq!(
            button_from_android(BUTTON_SECONDARY),
            Some(MouseButton::Right)
        );
        assert_eq!(
            button_from_android(BUTTON_TERTIARY),
            Some(MouseButton::Middle)
        );
        assert_eq!(button_from_android(BUTTON_BACK), Some(MouseButton::Back));
        assert_eq!(
            button_from_android(BUTTON_FORWARD),
            Some(MouseButton::Forward)
        );
        // BUTTON_STYLUS_PRIMARY no es de ratón.
        assert_eq!(button_from_android(32), None);

        let mut mouse = Mouse::default();
        let press = InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        };
        assert_eq!(mouse.button(BUTTON_PRIMARY, true), (true, Some(press)));
        assert_eq!(
            mouse.button(BUTTON_PRIMARY, true),
            (true, None),
            "ya estaba pulsado"
        );
        assert_eq!(
            mouse.button(BUTTON_PRIMARY, false),
            (
                true,
                Some(InputEvent::MouseButton {
                    button: MouseButton::Left,
                    pressed: false,
                })
            )
        );
        assert_eq!(mouse.button(BUTTON_PRIMARY, false), (true, None));
        assert_eq!(mouse.button(64, true), (false, None));
    }

    #[test]
    fn the_wheel_goes_in_120ths_of_a_notch_up_and_right_positive() {
        let mut mouse = Mouse::default();
        assert_eq!(
            mouse.wheel(0.0, 1.0),
            Some(InputEvent::Wheel { dx: 0, dy: 120 })
        );
        assert_eq!(
            mouse.wheel(-1.0, -2.0),
            Some(InputEvent::Wheel { dx: -120, dy: -240 })
        );
        // Un touchpad da fracciones pequeñas: se suman hasta llegar a una unidad.
        assert_eq!(mouse.wheel(0.0, 0.004), None);
        assert_eq!(
            mouse.wheel(0.0, 0.006),
            Some(InputEvent::Wheel { dx: 0, dy: 1 })
        );
    }

    #[test]
    fn releasing_lets_go_of_every_pressed_button_and_forgets_the_rest() {
        let mut mouse = Mouse::default();
        let _ = mouse.button(BUTTON_PRIMARY, true);
        let _ = mouse.button(BUTTON_BACK, true);
        let _ = mouse.motion(0.9, 0.0);
        let _ = mouse.wheel(0.0, 0.005);
        let released = mouse.release();
        assert_eq!(
            released,
            vec![
                InputEvent::MouseButton {
                    button: MouseButton::Left,
                    pressed: false,
                },
                InputEvent::MouseButton {
                    button: MouseButton::Back,
                    pressed: false,
                },
            ]
        );
        assert!(mouse.release().is_empty(), "nada que soltar dos veces");
        assert_eq!(mouse.motion(0.2, 0.0), None, "el resto se olvidó");
        assert_eq!(mouse.wheel(0.0, 0.005), None);
    }

    #[test]
    fn text_is_sent_one_character_at_a_time_without_control_characters() {
        let events: Vec<_> = text_events("ñá\n😀\t\u{8}b").collect();
        assert_eq!(
            events,
            vec![
                InputEvent::Text { ch: 'ñ' },
                InputEvent::Text { ch: 'á' },
                InputEvent::Text { ch: '😀' },
                InputEvent::Text { ch: 'b' },
            ]
        );
        assert_eq!(text_events("").count(), 0);
    }
}
