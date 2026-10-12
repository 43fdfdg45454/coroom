//! Mandos del invitado con `gilrs` en un hilo a 250 Hz.
//!
//! El primer mando conectado es el de la ranura que el anfitrión haya asignado a este invitado
//! ([`GamepadCapture::set_slot`]); los demás se ignoran. Botones por evento (al momento), ejes a
//! 250 Hz y solo si cambian más de [`AXIS_EPSILON`], con una zona muerta mínima
//! ([`AXIS_DEADZONE`]) además de los filtros de `gilrs`. Conexión y desconexión del mando, y
//! cambio de ranura, se anuncian con `GamepadConnected`/`GamepadDisconnected`. Sin ranura no se
//! envía nada.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use gilrs::{Axis, Button, EventType, GamepadId, Gilrs};
use tracing::{debug, warn};

use crate::{GamepadAxis, GamepadButton, GamepadKind, InputError, InputEvent};

/// Periodo del hilo de mandos (250 Hz).
pub const PERIOD: Duration = Duration::from_millis(4);

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

/// Ejes pendientes y enviados: solo salen los que cambian.
#[derive(Debug, Default)]
pub struct AxisFilter {
    pending: BTreeMap<GamepadAxis, i16>,
    sent: BTreeMap<GamepadAxis, i16>,
}

impl AxisFilter {
    /// Anota el último valor de un eje.
    pub fn set(&mut self, axis: GamepadAxis, value: i16) {
        self.pending.insert(axis, value);
    }

    /// Ejes que cambiaron desde el último envío (el reposo, 0, siempre sale si no se había
    /// enviado).
    pub fn take_changed(&mut self) -> Vec<(GamepadAxis, i16)> {
        let mut out = Vec::new();
        for (axis, value) in std::mem::take(&mut self.pending) {
            let last = self.sent.get(&axis).copied().unwrap_or(0);
            let moved = (i32::from(value) - i32::from(last)).abs() >= AXIS_EPSILON;
            if moved || (value == 0 && last != 0) {
                self.sent.insert(axis, value);
                out.push((axis, value));
            }
        }
        out
    }

    /// Olvida lo enviado (el mando cambia de ranura o se desconecta).
    pub fn reset(&mut self) {
        self.pending.clear();
        self.sent.clear();
    }
}

/// Hilo de mandos.
pub struct GamepadCapture {
    stop: Arc<AtomicBool>,
    slot: Arc<AtomicU8>,
    thread: Option<JoinHandle<()>>,
}

impl GamepadCapture {
    /// Arranca el hilo; `out` recibe los eventos (desde ese hilo).
    pub fn spawn(out: Box<dyn Fn(InputEvent) + Send>) -> Result<Self, InputError> {
        let stop = Arc::new(AtomicBool::new(false));
        let slot = Arc::new(AtomicU8::new(0));
        let thread = {
            let (stop, slot) = (Arc::clone(&stop), Arc::clone(&slot));
            thread::Builder::new()
                .name("mandos".into())
                .spawn(move || run(&stop, &slot, out.as_ref()))
                .map_err(|err| InputError::Backend(format!("hilo de mandos: {err}")))?
        };
        Ok(Self {
            stop,
            slot,
            thread: Some(thread),
        })
    }

    /// Ranura asignada por el anfitrión (`None`: sin mando).
    pub fn set_slot(&self, slot: Option<u8>) {
        self.slot.store(slot.unwrap_or(0), Ordering::Release);
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

fn run(stop: &AtomicBool, slot: &AtomicU8, out: &dyn Fn(InputEvent)) {
    let mut gilrs = match Gilrs::new() {
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
    let mut active: Option<GamepadId> = gilrs.gamepads().next().map(|(id, _)| id);
    let mut current = 0u8;
    let mut axes = AxisFilter::default();
    while !stop.load(Ordering::Acquire) {
        let wanted = slot.load(Ordering::Acquire);
        if wanted != current {
            if current != 0 && active.is_some() {
                out(InputEvent::GamepadDisconnected { slot: current });
            }
            current = wanted;
            axes.reset();
            if current != 0 && active.is_some() {
                out(InputEvent::GamepadConnected {
                    slot: current,
                    kind: GamepadKind::Xbox,
                });
            }
        }
        while let Some(event) = gilrs.next_event() {
            match event.event {
                EventType::Connected if active.is_none() => {
                    active = Some(event.id);
                    debug!(name = gilrs.gamepad(event.id).name(), "mando conectado");
                    if current != 0 {
                        out(InputEvent::GamepadConnected {
                            slot: current,
                            kind: GamepadKind::Xbox,
                        });
                    }
                }
                EventType::Disconnected if active == Some(event.id) => {
                    if current != 0 {
                        out(InputEvent::GamepadDisconnected { slot: current });
                    }
                    axes.reset();
                    active = gilrs
                        .gamepads()
                        .map(|(id, _)| id)
                        .find(|id| *id != event.id);
                    if active.is_some() && current != 0 {
                        out(InputEvent::GamepadConnected {
                            slot: current,
                            kind: GamepadKind::Xbox,
                        });
                    }
                }
                _ if current == 0 || active != Some(event.id) => {}
                EventType::ButtonPressed(pressed, _) => {
                    if let Some(button) = button(pressed) {
                        out(InputEvent::GamepadButton {
                            slot: current,
                            button,
                            pressed: true,
                        });
                    }
                }
                EventType::ButtonReleased(released, _) => {
                    if let Some(button) = button(released) {
                        out(InputEvent::GamepadButton {
                            slot: current,
                            button,
                            pressed: false,
                        });
                    }
                }
                EventType::ButtonChanged(Button::LeftTrigger2, value, _) => {
                    axes.set(GamepadAxis::LeftTrigger, trigger_value(value));
                }
                EventType::ButtonChanged(Button::RightTrigger2, value, _) => {
                    axes.set(GamepadAxis::RightTrigger, trigger_value(value));
                }
                EventType::AxisChanged(changed, value, _) => {
                    if let Some(axis) = axis(changed) {
                        axes.set(axis, stick_value(value));
                    }
                }
                _ => {}
            }
        }
        if current != 0 {
            for (axis, value) in axes.take_changed() {
                out(InputEvent::GamepadAxis {
                    slot: current,
                    axis,
                    value,
                });
            }
        }
        thread::sleep(PERIOD);
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
