//! Guion de entrada en JSON para `coroom join --headless --input-script FICHERO`: reproduce una
//! secuencia de eventos sin ventana (tests de extremo a extremo y la puerta de ratón a 1000 Hz).
//!
//! ```json
//! {
//!   "steps": [
//!     { "wait_permissions": { "keyboard_mouse": true, "timeout_ms": 10000 } },
//!     { "event": { "Key": { "code": 30, "pressed": true } } },
//!     { "wait_ms": 20 },
//!     { "event": { "Key": { "code": 30, "pressed": false } } },
//!     { "mouse_moves": { "count": 10000, "hz": 1000, "dx": 1, "dy": 0, "numbered": true } },
//!     { "events": { "hz": 250, "list": [ { "GamepadButton": { "slot": 2, "button": "A", "pressed": true } } ] } },
//!     "release_all"
//!   ]
//! }
//! ```
//!
//! Los eventos son los de [`InputEvent`] tal como los escribe `serde` (`"ReleaseAll"`,
//! `{"MouseMove": {"dx": 1, "dy": 0}}`…). `mouse_moves` con `numbered` pone en `dx` el número de
//! orden (1, 2, 3…) para que el anfitrión pueda comprobar que llegan todos y en orden. Los ritmos
//! se cumplen contra reloj (cada evento a su hora desde el principio del paso, sin acumular el
//! retraso de `sleep`). El guion **no** respeta los permisos por sí mismo: es una herramienta de
//! prueba (también para comprobar que el anfitrión descarta lo no permitido); `wait_permissions`
//! espera a tenerlos.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::capture::Allowed;
use crate::{InputError, InputEvent};

/// Espera por defecto de `wait_permissions`.
pub const DEFAULT_PERMISSION_TIMEOUT_MS: u64 = 10_000;

fn default_timeout() -> u64 {
    DEFAULT_PERMISSION_TIMEOUT_MS
}

/// Un paso del guion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    /// Esperar `n` milisegundos.
    WaitMs(u64),
    /// Esperar a que el anfitrión conceda estos permisos.
    WaitPermissions {
        /// Teclado y ratón.
        #[serde(default)]
        keyboard_mouse: bool,
        /// Una ranura de mando (cualquiera si es `0`).
        #[serde(default)]
        gamepad_slot: Option<u8>,
        /// Espera máxima.
        #[serde(default = "default_timeout")]
        timeout_ms: u64,
    },
    /// Un evento.
    Event(InputEvent),
    /// Una lista de eventos a `hz` por segundo.
    Events {
        /// Eventos.
        list: Vec<InputEvent>,
        /// Ritmo.
        hz: f64,
    },
    /// `count` movimientos relativos a `hz` por segundo.
    MouseMoves {
        /// Cuántos.
        count: u32,
        /// Ritmo.
        hz: f64,
        /// Desplazamiento horizontal de cada uno (o su número de orden con `numbered`).
        dx: i32,
        /// Desplazamiento vertical.
        dy: i32,
        /// `dx` = 1, 2, 3… para comprobar el orden.
        #[serde(default)]
        numbered: bool,
    },
    /// Soltar todo.
    ReleaseAll,
}

/// Guion completo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputScript {
    /// Pasos, en orden.
    pub steps: Vec<Step>,
}

/// Adónde va el guion (lo implementa la sesión del invitado).
pub trait ScriptTarget {
    /// Envía un evento.
    fn send(&self, event: InputEvent);
    /// Permisos que tiene ahora.
    fn allowed(&self) -> Allowed;
    /// Si la sesión terminó (el guion se corta).
    fn stopped(&self) -> bool;
}

/// Resultado de reproducir un guion.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ScriptReport {
    /// Eventos enviados.
    pub events: u64,
    /// Duración.
    pub elapsed: Duration,
    /// Si se cortó porque la sesión terminó.
    pub interrupted: bool,
}

fn period(hz: f64) -> Result<Duration, InputError> {
    if !hz.is_finite() || hz <= 0.0 || hz > 100_000.0 {
        return Err(InputError::Backend(format!(
            "ritmo de guion no válido: {hz} Hz (de 0 a 100000)"
        )));
    }
    Ok(Duration::from_secs_f64(1.0 / hz))
}

/// Espera hasta `deadline` (con `sleep` hasta 1 ms antes y espera activa el resto).
fn sleep_until(deadline: Instant) {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        let left = deadline - now;
        if left > Duration::from_millis(1) {
            std::thread::sleep(left - Duration::from_millis(1));
        } else {
            std::thread::yield_now();
        }
    }
}

impl InputScript {
    /// Lee un guion JSON.
    pub fn from_json(text: &str) -> Result<InputScript, InputError> {
        serde_json::from_str(text)
            .map_err(|err| InputError::Backend(format!("guion de entrada no válido: {err}")))
    }

    /// Lee un guion de un fichero.
    pub fn from_file(path: &std::path::Path) -> Result<InputScript, InputError> {
        let text = std::fs::read_to_string(path).map_err(|err| {
            InputError::Backend(format!("no se pudo leer {}: {err}", path.display()))
        })?;
        Self::from_json(&text)
    }

    /// Reproduce el guion en `target`.
    pub fn play(&self, target: &dyn ScriptTarget) -> Result<ScriptReport, InputError> {
        let start = Instant::now();
        let mut report = ScriptReport::default();
        let send = |event: InputEvent, report: &mut ScriptReport| {
            target.send(event);
            report.events += 1;
        };
        for step in &self.steps {
            if target.stopped() {
                report.interrupted = true;
                break;
            }
            match step {
                Step::WaitMs(ms) => sleep_until(Instant::now() + Duration::from_millis(*ms)),
                Step::WaitPermissions {
                    keyboard_mouse,
                    gamepad_slot,
                    timeout_ms,
                } => {
                    let deadline = Instant::now() + Duration::from_millis(*timeout_ms);
                    loop {
                        let allowed = target.allowed();
                        let kbm = !keyboard_mouse || allowed.keyboard_mouse;
                        let pad = match gamepad_slot {
                            None => true,
                            Some(0) => allowed.gamepad_slot.is_some(),
                            Some(slot) => allowed.gamepad_slot == Some(*slot),
                        };
                        if kbm && pad {
                            break;
                        }
                        if target.stopped() || Instant::now() >= deadline {
                            return Err(InputError::NotReady(format!(
                                "el anfitrión no concedió los permisos del guion en {timeout_ms} \
                                 ms (tiene: {})",
                                allowed.label()
                            )));
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
                Step::Event(event) => send(*event, &mut report),
                Step::ReleaseAll => send(InputEvent::ReleaseAll, &mut report),
                Step::Events { list, hz } => {
                    let period = period(*hz)?;
                    let begin = Instant::now();
                    for (i, event) in list.iter().enumerate() {
                        sleep_until(begin + period * i as u32);
                        send(*event, &mut report);
                    }
                }
                Step::MouseMoves {
                    count,
                    hz,
                    dx,
                    dy,
                    numbered,
                } => {
                    let period = period(*hz)?;
                    let begin = Instant::now();
                    for i in 0..*count {
                        sleep_until(begin + period * i);
                        if i % 256 == 0 && target.stopped() {
                            report.interrupted = true;
                            break;
                        }
                        let dx = if *numbered {
                            i32::try_from(i).map_or(i32::MAX, |i| i.saturating_add(1))
                        } else {
                            *dx
                        };
                        send(InputEvent::MouseMove { dx, dy: *dy }, &mut report);
                    }
                }
            }
        }
        report.elapsed = start.elapsed();
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Collect {
        events: RefCell<Vec<(Instant, InputEvent)>>,
        allowed: Allowed,
    }

    impl ScriptTarget for Collect {
        fn send(&self, event: InputEvent) {
            self.events.borrow_mut().push((Instant::now(), event));
        }
        fn allowed(&self) -> Allowed {
            self.allowed
        }
        fn stopped(&self) -> bool {
            false
        }
    }

    const EXAMPLE: &str = r#"{
        "steps": [
            { "wait_permissions": { "keyboard_mouse": true, "timeout_ms": 100 } },
            { "event": { "Key": { "code": 30, "pressed": true } } },
            { "wait_ms": 5 },
            { "mouse_moves": { "count": 50, "hz": 1000, "dx": 1, "dy": 0, "numbered": true } },
            { "events": { "hz": 500, "list": [ { "GamepadButton": { "slot": 2, "button": "A", "pressed": true } }, "ReleaseAll" ] } },
            "release_all"
        ]
    }"#;

    #[test]
    fn the_documented_example_parses_and_plays() {
        let script = InputScript::from_json(EXAMPLE).expect("guion");
        assert_eq!(script.steps.len(), 6);
        let target = Collect {
            events: RefCell::new(Vec::new()),
            allowed: Allowed {
                keyboard_mouse: true,
                gamepad_slot: None,
            },
        };
        let report = script.play(&target).expect("reproducir");
        assert_eq!(report.events, 1 + 50 + 2 + 1);
        let events = target.events.borrow();
        let moves: Vec<i32> = events
            .iter()
            .filter_map(|(_, event)| match event {
                InputEvent::MouseMove { dx, .. } => Some(*dx),
                _ => None,
            })
            .collect();
        assert_eq!(moves, (1..=50).collect::<Vec<_>>());
        // 50 movimientos a 1000 Hz: unos 49 ms entre el primero y el último.
        let first = events[1].0;
        let last = events[50].0;
        let span = last - first;
        assert!(
            span >= Duration::from_millis(48) && span < Duration::from_millis(80),
            "{span:?}"
        );
        assert_eq!(events.last().map(|(_, e)| *e), Some(InputEvent::ReleaseAll));
    }

    #[test]
    fn waiting_for_permissions_times_out_with_an_explanation() {
        let script = InputScript::from_json(EXAMPLE).expect("guion");
        let target = Collect {
            events: RefCell::new(Vec::new()),
            allowed: Allowed::default(),
        };
        let err = script.play(&target).expect_err("sin permisos");
        assert!(err.to_string().contains("solo mirar"), "{err}");
        assert!(target.events.borrow().is_empty());
    }

    #[test]
    fn bad_scripts_are_rejected() {
        assert!(InputScript::from_json("{}").is_err());
        assert!(InputScript::from_json(r#"{"steps": [{"saltar": 3}]}"#).is_err());
        let script =
            InputScript::from_json(r#"{"steps": [{"events": {"hz": 0, "list": ["ReleaseAll"]}}]}"#)
                .expect("se lee");
        let target = Collect {
            events: RefCell::new(Vec::new()),
            allowed: Allowed::default(),
        };
        assert!(script.play(&target).is_err(), "0 Hz no vale");
    }
}
