//! Tacto del móvil → ratón del anfitrión (el invitado de Android).
//!
//! [`TouchMapper`] recibe los `MotionEvent` de la vista del vídeo tal como los da Android (la
//! acción, los punteros con su identificador y su posición en píxeles de la vista, y
//! `getEventTime()` en ms) y devuelve los [`InputEvent`] que van al anfitrión. No mira el reloj: el
//! tiempo es el de los eventos, así que los tests son deterministas. Lo que se decide sin que llegue
//! otro evento (mantener el dedo quieto) lo decide [`TouchMapper::tick`], que la aplicación llama al
//! llegar [`TouchMapper::next_deadline`] (con `Handler.postAtTime`, que usa el mismo reloj que
//! `getEventTime()`); si no lo llama, se decide igual, tarde, con el siguiente evento. Los umbrales
//! son los del sistema ([`TouchConfig`], de `ViewConfiguration`), no unos propios.
//!
//! - **Panel** ([`TouchMode::Trackpad`], por defecto): un dedo mueve el cursor en relativo
//!   ([`InputEvent::MouseMove`]), con los píxeles de la vista pasados a los del anfitrión (lo que
//!   mide el vídeo en pantalla) y por la sensibilidad. No se mueve hasta pasar de `touch_slop`, y
//!   entonces sale todo lo recorrido desde que se apoyó. Tocar es clic izquierdo; tocar con dos
//!   dedos, clic derecho; arrastrar con dos dedos, la rueda (en 1/120 de muesca, a
//!   [`WHEEL_PIXELS_PER_NOTCH`] píxeles del anfitrión por muesca como el panel táctil del
//!   escritorio; el contenido sigue a los dedos, como en el móvil). Tocar y, antes de
//!   `double_tap_timeout`, volver a apoyar el dedo y arrastrarlo (o dejarlo quieto `tap_timeout`)
//!   arrastra con el izquierdo pulsado hasta levantarlo.
//! - **Directo** ([`TouchMode::Direct`]): el cursor va adonde está el dedo
//!   ([`InputEvent::MouseMoveAbsolute`], descontando las bandas negras como el modo escritorio,
//!   [`VideoRect`]; fuera del vídeo, al borde). Tocar es clic izquierdo; arrastrar, arrastrar con
//!   el izquierdo; mantenerlo quieto `long_press_timeout`, clic derecho (y después solo mueve el
//!   cursor). El izquierdo se pulsa cuando se decide el gesto (al pasar de `touch_slop` o al
//!   levantar el dedo), no al apoyarlo: si no, cada pulsación larga haría un clic izquierdo antes
//!   del derecho. Dos dedos, como en el panel.
//!
//! Tres dedos o más, y lo que quede de un gesto de dos dedos al levantar uno, no hacen nada hasta
//! levantar todos. `ACTION_CANCEL` (el sistema se queda el gesto) suelta lo pulsado sin hacer clic.
//! Los punteros se siguen por su identificador (`getPointerId`), no por su índice, que cambia al
//! levantar otro dedo.

use std::collections::BTreeMap;

use coroom_proto::input::WHEEL_NOTCH;

use crate::capture::{VideoRect, WHEEL_PIXELS_PER_NOTCH, whole};
use crate::{InputEvent, MouseButton};

/// Constantes de `android.view.MotionEvent` que entiende [`TouchAction::from_android`].
pub mod motion_event {
    /// `ACTION_DOWN`: el primer dedo.
    pub const ACTION_DOWN: i32 = 0;
    /// `ACTION_UP`: el último dedo.
    pub const ACTION_UP: i32 = 1;
    /// `ACTION_MOVE`.
    pub const ACTION_MOVE: i32 = 2;
    /// `ACTION_CANCEL`.
    pub const ACTION_CANCEL: i32 = 3;
    /// `ACTION_POINTER_DOWN`: otro dedo más.
    pub const ACTION_POINTER_DOWN: i32 = 5;
    /// `ACTION_POINTER_UP`: se levanta un dedo y quedan otros.
    pub const ACTION_POINTER_UP: i32 = 6;
}

/// Umbrales de los gestos: los de `android.view.ViewConfiguration` del dispositivo (ya escalados
/// a su densidad), que pasa la aplicación.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TouchConfig {
    /// `getScaledTouchSlop()`: lo que puede moverse un dedo (píxeles de la vista) sin dejar de ser
    /// un toque.
    pub touch_slop: f32,
    /// `ViewConfiguration.getTapTimeout()` (ms): en el panel, lo que hay que dejar quieto el dedo
    /// del segundo toque para arrastrar con el izquierdo sin moverlo.
    pub tap_timeout_ms: u64,
    /// `ViewConfiguration.getLongPressTimeout()` (ms): un toque más largo no es clic; en directo,
    /// es el clic derecho.
    pub long_press_timeout_ms: u64,
    /// `ViewConfiguration.getDoubleTapTimeout()` (ms): desde que se levanta un toque, plazo para
    /// apoyar el dedo otra vez y arrastrar con el izquierdo (panel).
    pub double_tap_timeout_ms: u64,
    /// Sensibilidad del panel: multiplica el movimiento del cursor (1 = el dedo recorre lo mismo
    /// que el cursor sobre el vídeo).
    pub sensitivity: f64,
}

impl TouchConfig {
    /// Con los valores de `ViewConfiguration` y sensibilidad 1.
    pub fn new(
        touch_slop: f32,
        tap_timeout_ms: u64,
        long_press_timeout_ms: u64,
        double_tap_timeout_ms: u64,
    ) -> Self {
        Self {
            touch_slop,
            tap_timeout_ms,
            long_press_timeout_ms,
            double_tap_timeout_ms,
            sensitivity: 1.0,
        }
    }
}

/// Cómo se usa el tacto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TouchMode {
    /// Como un panel táctil: el cursor en relativo.
    #[default]
    Trackpad,
    /// El cursor bajo el dedo.
    Direct,
}

/// Un dedo de un `MotionEvent`: `getPointerId(i)`, `getX(i)` y `getY(i)` (píxeles de la vista).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TouchPointer {
    /// Identificador del dedo, estable mientras está apoyado.
    pub id: i32,
    /// Horizontal.
    pub x: f32,
    /// Vertical.
    pub y: f32,
}

/// Qué pasó (`getActionMasked()`, y el dedo de `getActionIndex()` cuando es uno solo).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchAction {
    /// Se apoya el primer dedo.
    Down,
    /// Se apoya otro dedo, con este identificador.
    PointerDown(i32),
    /// Se mueven.
    Move,
    /// Se levanta un dedo, con este identificador, y quedan otros.
    PointerUp(i32),
    /// Se levanta el último.
    Up,
    /// El sistema se queda el gesto: se suelta todo sin clic.
    Cancel,
}

impl TouchAction {
    /// De `getActionMasked()` y el `getPointerId(getActionIndex())` del evento; `None` para lo
    /// que no es del tacto (`ACTION_HOVER_*`, `ACTION_SCROLL`…).
    pub fn from_android(action_masked: i32, action_pointer_id: i32) -> Option<Self> {
        Some(match action_masked {
            motion_event::ACTION_DOWN => TouchAction::Down,
            motion_event::ACTION_UP => TouchAction::Up,
            motion_event::ACTION_MOVE => TouchAction::Move,
            motion_event::ACTION_CANCEL => TouchAction::Cancel,
            motion_event::ACTION_POINTER_DOWN => TouchAction::PointerDown(action_pointer_id),
            motion_event::ACTION_POINTER_UP => TouchAction::PointerUp(action_pointer_id),
            _ => return None,
        })
    }
}

/// Un dedo apoyado: dónde empezó y dónde está.
#[derive(Debug, Clone, Copy)]
struct Finger {
    start: (f64, f64),
    at: (f64, f64),
}

/// El gesto en curso.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Gesture {
    /// Sin dedos.
    Idle,
    /// Un dedo sin decidir: no ha pasado de `touch_slop`. `after_tap`: apoyado a tiempo después
    /// de un toque (panel: arrastre con el izquierdo).
    Pending {
        id: i32,
        start_ms: u64,
        after_tap: bool,
    },
    /// Un dedo que mueve el cursor desde `last`, con el izquierdo pulsado si `held`.
    Moving {
        id: i32,
        last: (f64, f64),
        held: bool,
    },
    /// Dos dedos sin decidir; `start` es su punto medio.
    TwoPending { start: (f64, f64), start_ms: u64 },
    /// Dos dedos moviendo la rueda desde `last` (su punto medio).
    Scrolling { last: (f64, f64) },
    /// Nada más hasta levantar todos.
    Done,
}

/// Tacto → ratón (ver el módulo).
#[derive(Debug)]
pub struct TouchMapper {
    config: TouchConfig,
    mode: TouchMode,
    view: (u32, u32),
    frame: Option<(u32, u32)>,
    /// El rectángulo del vídeo si lo da la aplicación; si no, centrado en la vista.
    video: Option<VideoRect>,
    fingers: BTreeMap<i32, Finger>,
    gesture: Gesture,
    /// Cuándo se levantó el último toque que hizo clic izquierdo.
    last_tap_ms: Option<u64>,
    /// Si el izquierdo está pulsado en el anfitrión.
    left: bool,
    motion_rest: (f64, f64),
    wheel_rest: (f64, f64),
}

impl TouchMapper {
    /// En modo panel, sin vista ni fotograma todavía.
    pub fn new(config: TouchConfig) -> Self {
        Self {
            config,
            mode: TouchMode::default(),
            view: (0, 0),
            frame: None,
            video: None,
            fingers: BTreeMap::new(),
            gesture: Gesture::Idle,
            last_tap_ms: None,
            left: false,
            motion_rest: (0.0, 0.0),
            wheel_rest: (0.0, 0.0),
        }
    }

    /// Nuevos umbrales o sensibilidad (valen desde el siguiente evento).
    pub fn set_config(&mut self, config: TouchConfig) {
        self.config = config;
    }

    /// Los umbrales y la sensibilidad.
    pub fn config(&self) -> TouchConfig {
        self.config
    }

    /// Modo actual.
    pub fn mode(&self) -> TouchMode {
        self.mode
    }

    /// Cambia de modo: el gesto en curso se abandona y se suelta lo pulsado.
    pub fn set_mode(&mut self, mode: TouchMode) -> Vec<InputEvent> {
        if mode == self.mode {
            return Vec::new();
        }
        let out = self.reset();
        self.mode = mode;
        out
    }

    /// Tamaño de la vista del vídeo, en píxeles.
    pub fn set_view(&mut self, width: u32, height: u32) {
        self.view = (width, height);
    }

    /// Tamaño del fotograma del anfitrión (`HostFrameSize`).
    pub fn set_host_frame(&mut self, width: u32, height: u32) {
        self.frame = Some((width, height));
    }

    /// Dónde dibuja la aplicación el vídeo dentro de la vista (píxeles de la vista), si no es
    /// centrado y sin deformar (con zoom, por ejemplo); `None` vuelve al centrado.
    pub fn set_video_rect(&mut self, rect: Option<VideoRect>) {
        self.video = rect.filter(|rect| rect.width > 0.0 && rect.height > 0.0);
    }

    /// El rectángulo del vídeo en la vista, si se conoce.
    pub fn video_rect(&self) -> Option<VideoRect> {
        self.video.or_else(|| {
            let frame = self.frame?;
            (self.view.0 > 0 && self.view.1 > 0).then(|| VideoRect::fit(frame, self.view))
        })
    }

    /// Píxeles del anfitrión por píxel de la vista (1 sin fotograma o sin vista).
    fn host_scale(&self) -> f64 {
        match (self.video_rect(), self.frame) {
            (Some(rect), Some((width, _))) => f64::from(width.max(1)) / rect.width,
            _ => 1.0,
        }
    }

    /// Suelta lo pulsado y abandona el gesto (se pierde el permiso, la vista deja de verse).
    pub fn reset(&mut self) -> Vec<InputEvent> {
        let mut out = Vec::new();
        self.abort(&mut out);
        out
    }

    /// Cuándo (en el reloj de `getEventTime()`) hay que llamar a [`Self::tick`] para decidir el
    /// gesto en curso si no llega otro evento; `None` si no hace falta.
    pub fn next_deadline(&self) -> Option<u64> {
        let Gesture::Pending {
            start_ms,
            after_tap,
            ..
        } = self.gesture
        else {
            return None;
        };
        match self.mode {
            TouchMode::Trackpad if after_tap => {
                Some(start_ms.saturating_add(self.config.tap_timeout_ms))
            }
            TouchMode::Trackpad => None,
            TouchMode::Direct => Some(start_ms.saturating_add(self.config.long_press_timeout_ms)),
        }
    }

    /// Decide lo que toque a `time_ms` sin que llegue otro evento (mantener el dedo quieto).
    pub fn tick(&mut self, time_ms: u64) -> Vec<InputEvent> {
        let mut out = Vec::new();
        self.expire(time_ms, &mut out);
        out
    }

    /// Un `MotionEvent`: la acción, todos sus dedos (los que se levantan incluidos) y su hora.
    pub fn on_touch(
        &mut self,
        action: TouchAction,
        pointers: &[TouchPointer],
        time_ms: u64,
    ) -> Vec<InputEvent> {
        let mut out = Vec::new();
        // Lo que venció antes de este evento, primero (por si no se llamó a `tick`).
        self.expire(time_ms, &mut out);
        match action {
            TouchAction::Down => {
                if self.gesture != Gesture::Idle || !self.fingers.is_empty() {
                    // Se perdió el `ACTION_UP` del gesto anterior.
                    self.abort(&mut out);
                }
                for pointer in pointers {
                    self.add(pointer);
                }
                let Some(first) = pointers.iter().find(|p| self.fingers.contains_key(&p.id)) else {
                    return out;
                };
                let after_tap = self.mode == TouchMode::Trackpad
                    && self.last_tap_ms.is_some_and(|up| {
                        time_ms.saturating_sub(up) <= self.config.double_tap_timeout_ms
                    });
                self.gesture = Gesture::Pending {
                    id: first.id,
                    start_ms: time_ms,
                    after_tap,
                };
                if self.mode == TouchMode::Direct {
                    let at = self.fingers[&first.id].at;
                    out.extend(self.absolute(at));
                }
            }
            TouchAction::PointerDown(id) => {
                self.update(pointers);
                if let Some(pointer) = pointers.iter().find(|p| p.id == id) {
                    self.add(pointer);
                }
                self.second_finger(id, time_ms);
            }
            TouchAction::Move => {
                self.update(pointers);
                self.advance(&mut out);
            }
            TouchAction::PointerUp(id) => {
                self.update(pointers);
                self.advance(&mut out);
                self.lift(id, time_ms, &mut out);
                self.fingers.remove(&id);
                if self.fingers.is_empty() {
                    self.gesture = Gesture::Idle;
                }
            }
            TouchAction::Up => {
                self.update(pointers);
                self.advance(&mut out);
                let lifted: Vec<i32> = self.fingers.keys().copied().collect();
                for id in lifted {
                    self.lift(id, time_ms, &mut out);
                }
                self.fingers.clear();
                self.gesture = Gesture::Idle;
            }
            TouchAction::Cancel => self.abort(&mut out),
        }
        out
    }

    fn add(&mut self, pointer: &TouchPointer) {
        if let Some(at) = position(pointer) {
            self.fingers.insert(pointer.id, Finger { start: at, at });
        }
    }

    /// Las posiciones nuevas de los dedos que se conocen.
    fn update(&mut self, pointers: &[TouchPointer]) {
        for pointer in pointers {
            if let (Some(finger), Some(at)) = (self.fingers.get_mut(&pointer.id), position(pointer))
            {
                finger.at = at;
            }
        }
    }

    /// Se apoyó el dedo `id` con otros ya apoyados.
    fn second_finger(&mut self, id: i32, time_ms: u64) {
        self.gesture = match self.gesture {
            // Sin `ACTION_DOWN` (se perdió): como si fuera el primero.
            Gesture::Idle if self.fingers.len() == 1 => Gesture::Pending {
                id,
                start_ms: time_ms,
                after_tap: false,
            },
            Gesture::Pending { .. } | Gesture::Moving { held: false, .. }
                if self.fingers.len() == 2 =>
            {
                // Los dos dedos empiezan aquí (el primero pudo moverse antes).
                for finger in self.fingers.values_mut() {
                    finger.start = finger.at;
                }
                Gesture::TwoPending {
                    start: self.centroid(),
                    start_ms: time_ms,
                }
            }
            // Arrastrando con el izquierdo: el otro dedo no cuenta.
            moving @ Gesture::Moving { held: true, .. } => moving,
            _ => Gesture::Done,
        };
    }

    /// Lo que hacen los dedos al moverse.
    fn advance(&mut self, out: &mut Vec<InputEvent>) {
        if let Gesture::Pending { id, after_tap, .. } = self.gesture {
            let Some(finger) = self.fingers.get(&id).copied() else {
                return;
            };
            if !self.beyond_slop(finger.start, finger.at) {
                return;
            }
            let held = match self.mode {
                TouchMode::Trackpad => after_tap,
                TouchMode::Direct => true,
            };
            if held {
                self.press(true, out);
            }
            self.gesture = Gesture::Moving {
                id,
                last: finger.start,
                held,
            };
        }
        if let Gesture::TwoPending { start, .. } = self.gesture {
            if self.beyond_slop(start, self.centroid()) {
                self.gesture = Gesture::Scrolling { last: start };
            } else if self
                .fingers
                .values()
                .any(|finger| self.beyond_slop(finger.start, finger.at))
            {
                // Pellizco o giro: ni clic ni rueda.
                self.gesture = Gesture::Done;
            }
        }
        match self.gesture {
            Gesture::Moving { id, last, held } => {
                let Some(at) = self.fingers.get(&id).map(|finger| finger.at) else {
                    return;
                };
                if at == last {
                    return;
                }
                self.gesture = Gesture::Moving { id, last: at, held };
                match self.mode {
                    TouchMode::Trackpad => {
                        let scale = self.host_scale() * self.config.sensitivity;
                        let dx = whole(&mut self.motion_rest.0, (at.0 - last.0) * scale);
                        let dy = whole(&mut self.motion_rest.1, (at.1 - last.1) * scale);
                        if dx != 0 || dy != 0 {
                            out.push(InputEvent::MouseMove { dx, dy });
                        }
                    }
                    TouchMode::Direct => out.extend(self.absolute(at)),
                }
            }
            Gesture::Scrolling { last } => {
                let at = self.centroid();
                self.gesture = Gesture::Scrolling { last: at };
                let notch = self.host_scale() / WHEEL_PIXELS_PER_NOTCH * f64::from(WHEEL_NOTCH);
                // El contenido sigue a los dedos: bajarlos sube la rueda (positivo) y llevarlos a
                // la derecha la mueve a la izquierda (negativo).
                let dx = whole(&mut self.wheel_rest.0, -(at.0 - last.0) * notch);
                let dy = whole(&mut self.wheel_rest.1, (at.1 - last.1) * notch);
                if dx != 0 || dy != 0 {
                    out.push(InputEvent::Wheel { dx, dy });
                }
            }
            _ => {}
        }
    }

    /// Se levanta el dedo `id` (ya con su última posición).
    fn lift(&mut self, id: i32, time_ms: u64, out: &mut Vec<InputEvent>) {
        match self.gesture {
            Gesture::Pending {
                id: pending,
                start_ms,
                ..
            } if pending == id => {
                // No pasó de `touch_slop` (sería `Moving`) ni, en directo, del toque largo (lo
                // habría decidido `expire`).
                if time_ms.saturating_sub(start_ms) < self.config.long_press_timeout_ms {
                    self.press(true, out);
                    self.press(false, out);
                    self.last_tap_ms = Some(time_ms);
                }
                self.gesture = Gesture::Done;
            }
            Gesture::Moving {
                id: moving, held, ..
            } if moving == id => {
                if held {
                    self.press(false, out);
                }
                self.gesture = Gesture::Done;
            }
            Gesture::TwoPending { start_ms, .. } => {
                if time_ms.saturating_sub(start_ms) < self.config.long_press_timeout_ms {
                    out.push(InputEvent::MouseButton {
                        button: MouseButton::Right,
                        pressed: true,
                    });
                    out.push(InputEvent::MouseButton {
                        button: MouseButton::Right,
                        pressed: false,
                    });
                }
                self.gesture = Gesture::Done;
            }
            Gesture::Scrolling { .. } => self.gesture = Gesture::Done,
            // Un dedo que no era del gesto.
            _ => {}
        }
    }

    /// Lo que vence a `time_ms` con el dedo quieto.
    fn expire(&mut self, time_ms: u64, out: &mut Vec<InputEvent>) {
        let Some(deadline) = self.next_deadline() else {
            return;
        };
        let Gesture::Pending { id, .. } = self.gesture else {
            return;
        };
        if time_ms < deadline {
            return;
        }
        let Some(start) = self.fingers.get(&id).map(|finger| finger.start) else {
            return;
        };
        match self.mode {
            // Segundo toque quieto: arrastre con el izquierdo.
            TouchMode::Trackpad => {
                self.press(true, out);
                self.gesture = Gesture::Moving {
                    id,
                    last: start,
                    held: true,
                };
            }
            // Toque largo: clic derecho, y el dedo ya solo mueve el cursor.
            TouchMode::Direct => {
                out.push(InputEvent::MouseButton {
                    button: MouseButton::Right,
                    pressed: true,
                });
                out.push(InputEvent::MouseButton {
                    button: MouseButton::Right,
                    pressed: false,
                });
                self.gesture = Gesture::Moving {
                    id,
                    last: start,
                    held: false,
                };
            }
        }
    }

    /// Suelta el izquierdo si estaba pulsado y olvida el gesto (sin clic).
    fn abort(&mut self, out: &mut Vec<InputEvent>) {
        self.press(false, out);
        self.fingers.clear();
        self.gesture = Gesture::Idle;
        self.last_tap_ms = None;
        self.motion_rest = (0.0, 0.0);
        self.wheel_rest = (0.0, 0.0);
    }

    /// Pulsa o suelta el izquierdo, si cambia.
    fn press(&mut self, pressed: bool, out: &mut Vec<InputEvent>) {
        if self.left != pressed {
            self.left = pressed;
            out.push(InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed,
            });
        }
    }

    fn absolute(&self, at: (f64, f64)) -> Option<InputEvent> {
        self.video_rect().map(|rect| rect.absolute(at.0, at.1))
    }

    fn beyond_slop(&self, from: (f64, f64), to: (f64, f64)) -> bool {
        let slop = f64::from(self.config.touch_slop);
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        dx * dx + dy * dy > slop * slop
    }

    /// Punto medio de los dedos apoyados.
    fn centroid(&self) -> (f64, f64) {
        let count = self.fingers.len().max(1) as f64;
        let (x, y) = self.fingers.values().fold((0.0, 0.0), |(x, y), finger| {
            (x + finger.at.0, y + finger.at.1)
        });
        (x / count, y / count)
    }
}

/// La posición de un dedo, si es un número.
fn position(pointer: &TouchPointer) -> Option<(f64, f64)> {
    let (x, y) = (f64::from(pointer.x), f64::from(pointer.y));
    (x.is_finite() && y.is_finite()).then_some((x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Umbrales de ejemplo (los de `ViewConfiguration` por defecto en una pantalla de densidad 1).
    fn config() -> TouchConfig {
        TouchConfig::new(8.0, 100, 400, 300)
    }

    /// Vista de 960×540 con un anfitrión de 1920×1080: el vídeo la llena y cada píxel de la vista
    /// son dos del anfitrión.
    fn mapper(mode: TouchMode) -> TouchMapper {
        let mut touch = TouchMapper::new(config());
        touch.set_view(960, 540);
        touch.set_host_frame(1920, 1080);
        assert!(touch.set_mode(mode).is_empty());
        touch
    }

    fn p(id: i32, x: f32, y: f32) -> TouchPointer {
        TouchPointer { id, x, y }
    }

    fn button(button: MouseButton, pressed: bool) -> InputEvent {
        InputEvent::MouseButton { button, pressed }
    }

    fn click(which: MouseButton) -> Vec<InputEvent> {
        vec![button(which, true), button(which, false)]
    }

    fn abs(x: u16, y: u16) -> InputEvent {
        InputEvent::MouseMoveAbsolute { x, y }
    }

    /// Un toque de un dedo de `from` a `to` ms.
    fn tap(touch: &mut TouchMapper, at: (f32, f32), from: u64, to: u64) -> Vec<InputEvent> {
        let mut out = touch.on_touch(TouchAction::Down, &[p(0, at.0, at.1)], from);
        out.extend(touch.on_touch(TouchAction::Up, &[p(0, at.0, at.1)], to));
        out
    }

    #[test]
    fn android_actions_are_understood() {
        assert_eq!(TouchAction::from_android(0, 0), Some(TouchAction::Down));
        assert_eq!(TouchAction::from_android(1, 0), Some(TouchAction::Up));
        assert_eq!(TouchAction::from_android(2, 0), Some(TouchAction::Move));
        assert_eq!(TouchAction::from_android(3, 0), Some(TouchAction::Cancel));
        assert_eq!(
            TouchAction::from_android(5, 7),
            Some(TouchAction::PointerDown(7))
        );
        assert_eq!(
            TouchAction::from_android(6, 7),
            Some(TouchAction::PointerUp(7))
        );
        // `ACTION_HOVER_MOVE` y `ACTION_SCROLL` no son del tacto.
        assert_eq!(TouchAction::from_android(7, 0), None);
        assert_eq!(TouchAction::from_android(8, 0), None);
    }

    #[test]
    fn a_tap_is_a_left_click_without_moving_the_cursor() {
        let mut touch = mapper(TouchMode::Trackpad);
        assert_eq!(
            tap(&mut touch, (100.0, 100.0), 1000, 1080),
            click(MouseButton::Left)
        );
        // Un temblor dentro de `touch_slop` sigue siendo un toque, y no mueve el cursor.
        let mut out = touch.on_touch(TouchAction::Down, &[p(0, 50.0, 50.0)], 5000);
        out.extend(touch.on_touch(TouchAction::Move, &[p(0, 55.0, 53.0)], 5020));
        out.extend(touch.on_touch(TouchAction::Up, &[p(0, 54.0, 55.0)], 5060));
        assert_eq!(out, click(MouseButton::Left));
        assert_eq!(touch.next_deadline(), None);
    }

    #[test]
    fn a_long_still_touch_is_not_a_click_in_the_trackpad() {
        let mut touch = mapper(TouchMode::Trackpad);
        assert!(
            touch
                .on_touch(TouchAction::Down, &[p(0, 10.0, 10.0)], 0)
                .is_empty()
        );
        assert_eq!(touch.next_deadline(), None, "el panel no espera a nada");
        assert!(touch.tick(1000).is_empty());
        assert!(
            touch
                .on_touch(TouchAction::Up, &[p(0, 10.0, 10.0)], 400)
                .is_empty()
        );
    }

    #[test]
    fn one_finger_moves_the_cursor_in_host_pixels() {
        let mut touch = mapper(TouchMode::Trackpad);
        assert!(
            touch
                .on_touch(TouchAction::Down, &[p(0, 100.0, 100.0)], 0)
                .is_empty()
        );
        // Dentro de `touch_slop`, nada; al pasarlo sale todo lo recorrido (×2: 960 → 1920).
        assert!(
            touch
                .on_touch(TouchAction::Move, &[p(0, 105.0, 100.0)], 10)
                .is_empty()
        );
        assert_eq!(
            touch.on_touch(TouchAction::Move, &[p(0, 110.0, 97.0)], 20),
            vec![InputEvent::MouseMove { dx: 20, dy: -6 }]
        );
        // Los restos de píxel se guardan: dos cuartos de píxel de la vista son un píxel.
        assert!(
            touch
                .on_touch(TouchAction::Move, &[p(0, 110.25, 97.0)], 30)
                .is_empty()
        );
        assert_eq!(
            touch.on_touch(TouchAction::Move, &[p(0, 110.5, 97.0)], 40),
            vec![InputEvent::MouseMove { dx: 1, dy: 0 }]
        );
        assert!(
            touch
                .on_touch(TouchAction::Up, &[p(0, 110.5, 97.0)], 50)
                .is_empty()
        );

        // Con la sensibilidad, más.
        touch.set_config(TouchConfig {
            sensitivity: 1.5,
            ..config()
        });
        touch.on_touch(TouchAction::Down, &[p(0, 0.0, 0.0)], 1000);
        assert_eq!(
            touch.on_touch(TouchAction::Move, &[p(0, 10.0, 0.0)], 1010),
            vec![InputEvent::MouseMove { dx: 30, dy: 0 }]
        );
    }

    #[test]
    fn letterboxing_changes_the_scale() {
        // Vista de 1000×1000 con un anfitrión de 1920×1080: el vídeo mide 1000×562,5 y cada
        // píxel de la vista son 1,92 del anfitrión.
        let mut touch = TouchMapper::new(config());
        touch.set_view(1000, 1000);
        touch.set_host_frame(1920, 1080);
        touch.on_touch(TouchAction::Down, &[p(0, 0.0, 0.0)], 0);
        assert_eq!(
            touch.on_touch(TouchAction::Move, &[p(0, 100.0, 0.0)], 10),
            vec![InputEvent::MouseMove { dx: 192, dy: 0 }]
        );
        // Sin fotograma, píxel a píxel.
        let mut bare = TouchMapper::new(config());
        bare.on_touch(TouchAction::Down, &[p(0, 0.0, 0.0)], 0);
        assert_eq!(
            bare.on_touch(TouchAction::Move, &[p(0, 0.0, 30.0)], 10),
            vec![InputEvent::MouseMove { dx: 0, dy: 30 }]
        );
    }

    #[test]
    fn a_two_finger_tap_is_a_right_click() {
        let mut touch = mapper(TouchMode::Trackpad);
        let mut out = touch.on_touch(TouchAction::Down, &[p(0, 100.0, 100.0)], 0);
        out.extend(touch.on_touch(
            TouchAction::PointerDown(1),
            &[p(0, 100.0, 100.0), p(1, 200.0, 100.0)],
            30,
        ));
        out.extend(touch.on_touch(
            TouchAction::Move,
            &[p(0, 102.0, 101.0), p(1, 199.0, 102.0)],
            60,
        ));
        out.extend(touch.on_touch(
            TouchAction::PointerUp(0),
            &[p(0, 102.0, 101.0), p(1, 199.0, 102.0)],
            90,
        ));
        out.extend(touch.on_touch(TouchAction::Up, &[p(1, 199.0, 102.0)], 100));
        assert_eq!(out, click(MouseButton::Right), "un solo clic derecho");
        // Y lo siguiente no cuenta como segundo toque de un arrastre.
        assert_eq!(
            tap(&mut touch, (10.0, 10.0), 200, 250),
            click(MouseButton::Left)
        );
    }

    #[test]
    fn two_fingers_dragged_are_the_wheel_and_the_content_follows_them() {
        let mut touch = mapper(TouchMode::Trackpad);
        touch.on_touch(TouchAction::Down, &[p(0, 100.0, 100.0)], 0);
        touch.on_touch(
            TouchAction::PointerDown(1),
            &[p(0, 100.0, 100.0), p(1, 200.0, 100.0)],
            10,
        );
        // Bajan 10 píxeles de la vista (20 del anfitrión, media muesca): rueda arriba.
        assert_eq!(
            touch.on_touch(
                TouchAction::Move,
                &[p(0, 100.0, 110.0), p(1, 200.0, 110.0)],
                20
            ),
            vec![InputEvent::Wheel { dx: 0, dy: 60 }]
        );
        // A la derecha: rueda a la izquierda.
        assert_eq!(
            touch.on_touch(
                TouchAction::Move,
                &[p(0, 120.0, 110.0), p(1, 220.0, 110.0)],
                30
            ),
            vec![InputEvent::Wheel { dx: -120, dy: 0 }]
        );
        // Al levantar uno se acaba: el que queda no mueve el cursor ni la rueda.
        assert!(
            touch
                .on_touch(
                    TouchAction::PointerUp(1),
                    &[p(0, 120.0, 110.0), p(1, 220.0, 110.0)],
                    40
                )
                .is_empty()
        );
        assert!(
            touch
                .on_touch(TouchAction::Move, &[p(0, 300.0, 300.0)], 50)
                .is_empty()
        );
        assert!(
            touch
                .on_touch(TouchAction::Up, &[p(0, 300.0, 300.0)], 60)
                .is_empty()
        );
    }

    #[test]
    fn a_pinch_is_neither_a_click_nor_the_wheel() {
        let mut touch = mapper(TouchMode::Trackpad);
        let mut out = touch.on_touch(TouchAction::Down, &[p(0, 100.0, 100.0)], 0);
        out.extend(touch.on_touch(
            TouchAction::PointerDown(1),
            &[p(0, 100.0, 100.0), p(1, 200.0, 100.0)],
            10,
        ));
        // Se separan: el punto medio no se mueve.
        out.extend(touch.on_touch(
            TouchAction::Move,
            &[p(0, 80.0, 100.0), p(1, 220.0, 100.0)],
            20,
        ));
        out.extend(touch.on_touch(
            TouchAction::PointerUp(0),
            &[p(0, 80.0, 100.0), p(1, 220.0, 100.0)],
            30,
        ));
        out.extend(touch.on_touch(TouchAction::Up, &[p(1, 220.0, 100.0)], 40));
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn pointer_ids_are_followed_whatever_their_order() {
        let mut touch = mapper(TouchMode::Trackpad);
        // Los identificadores no son 0 y 1 ni van en orden: el gesto los sigue por su nombre.
        let mut out = touch.on_touch(TouchAction::Down, &[p(3, 100.0, 100.0)], 0);
        out.extend(touch.on_touch(
            TouchAction::PointerDown(0),
            &[p(0, 200.0, 100.0), p(3, 100.0, 100.0)],
            10,
        ));
        out.extend(touch.on_touch(
            TouchAction::Move,
            &[p(3, 101.0, 100.0), p(0, 201.0, 100.0)],
            20,
        ));
        out.extend(touch.on_touch(
            TouchAction::Move,
            &[p(0, 201.0, 101.0), p(3, 101.0, 101.0)],
            30,
        ));
        // Uno que no se conoce se ignora.
        out.extend(touch.on_touch(TouchAction::Move, &[p(9, 900.0, 900.0)], 35));
        out.extend(touch.on_touch(
            TouchAction::PointerUp(3),
            &[p(0, 201.0, 101.0), p(3, 101.0, 101.0)],
            40,
        ));
        out.extend(touch.on_touch(TouchAction::Up, &[p(0, 201.0, 101.0)], 50));
        assert_eq!(out, click(MouseButton::Right));
    }

    #[test]
    fn three_fingers_do_nothing() {
        let mut touch = mapper(TouchMode::Trackpad);
        let mut out = touch.on_touch(TouchAction::Down, &[p(0, 0.0, 0.0)], 0);
        let two = [p(0, 0.0, 0.0), p(1, 50.0, 0.0)];
        out.extend(touch.on_touch(TouchAction::PointerDown(1), &two, 10));
        let three = [p(0, 0.0, 0.0), p(1, 50.0, 0.0), p(2, 100.0, 0.0)];
        out.extend(touch.on_touch(TouchAction::PointerDown(2), &three, 20));
        let moved = [p(0, 0.0, 90.0), p(1, 50.0, 90.0), p(2, 100.0, 90.0)];
        out.extend(touch.on_touch(TouchAction::Move, &moved, 30));
        out.extend(touch.on_touch(TouchAction::PointerUp(2), &moved, 40));
        out.extend(touch.on_touch(TouchAction::PointerUp(1), &moved[..2], 50));
        out.extend(touch.on_touch(TouchAction::Up, &moved[..1], 60));
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn tap_then_drag_holds_the_left_button() {
        let mut touch = mapper(TouchMode::Trackpad);
        assert_eq!(
            tap(&mut touch, (100.0, 100.0), 0, 60),
            click(MouseButton::Left)
        );
        // Vuelve a tiempo (200 ms después de levantarlo, el plazo es 300) y arrastra.
        assert!(
            touch
                .on_touch(TouchAction::Down, &[p(0, 100.0, 100.0)], 260)
                .is_empty()
        );
        assert_eq!(touch.next_deadline(), Some(360));
        assert_eq!(
            touch.on_touch(TouchAction::Move, &[p(0, 120.0, 100.0)], 280),
            vec![
                button(MouseButton::Left, true),
                InputEvent::MouseMove { dx: 40, dy: 0 }
            ]
        );
        assert_eq!(touch.next_deadline(), None);
        assert_eq!(
            touch.on_touch(TouchAction::Move, &[p(0, 120.0, 110.0)], 300),
            vec![InputEvent::MouseMove { dx: 0, dy: 20 }]
        );
        assert_eq!(
            touch.on_touch(TouchAction::Up, &[p(0, 120.0, 110.0)], 320),
            vec![button(MouseButton::Left, false)]
        );
    }

    #[test]
    fn tap_then_hold_still_holds_the_left_button_on_tick() {
        let mut touch = mapper(TouchMode::Trackpad);
        tap(&mut touch, (100.0, 100.0), 0, 60);
        touch.on_touch(TouchAction::Down, &[p(0, 100.0, 100.0)], 100);
        assert!(touch.tick(199).is_empty(), "antes de `tap_timeout`");
        assert_eq!(touch.tick(200), vec![button(MouseButton::Left, true)]);
        assert!(touch.tick(250).is_empty());
        // Lo que se mueva desde ahí, con el botón pulsado, desde donde se apoyó.
        assert_eq!(
            touch.on_touch(TouchAction::Move, &[p(0, 103.0, 100.0)], 260),
            vec![InputEvent::MouseMove { dx: 6, dy: 0 }]
        );
        assert_eq!(
            touch.on_touch(TouchAction::Up, &[p(0, 103.0, 100.0)], 300),
            vec![button(MouseButton::Left, false)]
        );
    }

    #[test]
    fn a_quick_second_tap_is_a_second_click() {
        let mut touch = mapper(TouchMode::Trackpad);
        let mut out = tap(&mut touch, (100.0, 100.0), 0, 60);
        out.extend(tap(&mut touch, (100.0, 100.0), 150, 200));
        let mut double = click(MouseButton::Left);
        double.extend(click(MouseButton::Left));
        assert_eq!(out, double, "doble clic");
        // Pasado el plazo, un dedo vuelve a mover sin botón.
        touch.on_touch(TouchAction::Down, &[p(0, 100.0, 100.0)], 501);
        assert_eq!(touch.next_deadline(), None);
        assert_eq!(
            touch.on_touch(TouchAction::Move, &[p(0, 110.0, 100.0)], 520),
            vec![InputEvent::MouseMove { dx: 20, dy: 0 }]
        );
        assert!(
            touch
                .on_touch(TouchAction::Up, &[p(0, 110.0, 100.0)], 540)
                .is_empty()
        );
    }

    #[test]
    fn direct_touches_go_to_the_finger_inside_the_letterbox() {
        // Vista cuadrada de 1000×1000 con un anfitrión 16:9: bandas de 218,75 arriba y abajo.
        let mut touch = TouchMapper::new(config());
        touch.set_view(1000, 1000);
        touch.set_host_frame(1920, 1080);
        touch.set_mode(TouchMode::Direct);
        let rect = touch.video_rect().expect("rectángulo");
        assert_eq!(
            (rect.x, rect.y, rect.width, rect.height),
            (0.0, 218.75, 1000.0, 562.5)
        );

        // El centro va al centro; el toque es clic izquierdo al levantar el dedo.
        let mut out = touch.on_touch(TouchAction::Down, &[p(0, 500.0, 500.0)], 0);
        assert_eq!(out, vec![abs(32768, 32768)]);
        out = touch.on_touch(TouchAction::Up, &[p(0, 500.0, 500.0)], 50);
        assert_eq!(out, click(MouseButton::Left));

        // En las bandas, al borde del vídeo.
        let edges = [
            ((500.0, 100.0), abs(32768, 0)),
            ((500.0, 218.75), abs(32768, 0)),
            ((0.0, 900.0), abs(0, 65535)),
            ((1000.0, 781.25), abs(65535, 65535)),
            ((-50.0, 500.0), abs(0, 32768)),
        ];
        for (index, ((x, y), expected)) in edges.into_iter().enumerate() {
            let at = 1000 + index as u64 * 1000;
            let out = touch.on_touch(TouchAction::Down, &[p(0, x, y)], at);
            assert_eq!(out, vec![expected], "({x}, {y})");
            touch.on_touch(TouchAction::Up, &[p(0, x, y)], at + 10);
        }
    }

    #[test]
    fn a_video_rect_from_the_app_wins() {
        let mut touch = mapper(TouchMode::Direct);
        // La aplicación dibuja el vídeo con zoom: 1920×1080 desde (-480, -270).
        touch.set_video_rect(Some(VideoRect {
            x: -480.0,
            y: -270.0,
            width: 1920.0,
            height: 1080.0,
        }));
        assert_eq!(
            touch.on_touch(TouchAction::Down, &[p(0, 480.0, 270.0)], 0),
            vec![abs(32768, 32768)]
        );
        touch.on_touch(TouchAction::Up, &[p(0, 480.0, 270.0)], 10);
        // Y en el panel, un píxel de la vista es uno del anfitrión.
        touch.set_mode(TouchMode::Trackpad);
        touch.on_touch(TouchAction::Down, &[p(0, 0.0, 0.0)], 100);
        assert_eq!(
            touch.on_touch(TouchAction::Move, &[p(0, 10.0, 0.0)], 110),
            vec![InputEvent::MouseMove { dx: 10, dy: 0 }]
        );
        // Vacío no vale: vuelve al centrado.
        touch.set_video_rect(Some(VideoRect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 10.0,
        }));
        assert_eq!(
            touch.video_rect(),
            Some(VideoRect::fit((1920, 1080), (960, 540)))
        );
    }

    #[test]
    fn direct_drag_holds_the_left_button() {
        let mut touch = mapper(TouchMode::Direct);
        assert_eq!(
            touch.on_touch(TouchAction::Down, &[p(0, 480.0, 270.0)], 0),
            vec![abs(32768, 32768)]
        );
        assert!(
            touch
                .on_touch(TouchAction::Move, &[p(0, 484.0, 270.0)], 10)
                .is_empty()
        );
        assert_eq!(
            touch.on_touch(TouchAction::Move, &[p(0, 960.0, 270.0)], 20),
            vec![button(MouseButton::Left, true), abs(65535, 32768)]
        );
        assert_eq!(touch.next_deadline(), None, "ya no es un toque largo");
        assert!(touch.tick(1000).is_empty());
        assert_eq!(
            touch.on_touch(TouchAction::Move, &[p(0, 0.0, 0.0)], 30),
            vec![abs(0, 0)]
        );
        assert_eq!(
            touch.on_touch(TouchAction::Up, &[p(0, 0.0, 0.0)], 40),
            vec![button(MouseButton::Left, false)]
        );
    }

    #[test]
    fn a_direct_long_press_is_a_right_click() {
        let mut touch = mapper(TouchMode::Direct);
        touch.on_touch(TouchAction::Down, &[p(0, 480.0, 270.0)], 1000);
        assert_eq!(touch.next_deadline(), Some(1400));
        assert!(touch.tick(1399).is_empty());
        assert_eq!(touch.tick(1400), click(MouseButton::Right));
        assert_eq!(touch.next_deadline(), None);
        // Después el dedo solo mueve el cursor, y levantarlo no hace clic.
        assert_eq!(
            touch.on_touch(TouchAction::Move, &[p(0, 0.0, 0.0)], 1500),
            vec![abs(0, 0)]
        );
        assert!(
            touch
                .on_touch(TouchAction::Up, &[p(0, 0.0, 0.0)], 1600)
                .is_empty()
        );

        // Sin `tick`, el siguiente evento lo decide igual (y no hay clic izquierdo).
        touch.on_touch(TouchAction::Down, &[p(0, 480.0, 270.0)], 5000);
        assert_eq!(
            touch.on_touch(TouchAction::Up, &[p(0, 480.0, 270.0)], 5600),
            click(MouseButton::Right)
        );
    }

    #[test]
    fn direct_without_a_frame_still_clicks() {
        let mut touch = TouchMapper::new(config());
        touch.set_mode(TouchMode::Direct);
        assert_eq!(touch.video_rect(), None);
        assert_eq!(
            tap(&mut touch, (10.0, 10.0), 0, 50),
            click(MouseButton::Left)
        );
    }

    #[test]
    fn two_fingers_in_direct_mode_are_a_right_click_without_a_left_one() {
        let mut touch = mapper(TouchMode::Direct);
        let mut out = touch.on_touch(TouchAction::Down, &[p(0, 480.0, 270.0)], 0);
        assert_eq!(out, vec![abs(32768, 32768)]);
        let two = [p(0, 480.0, 270.0), p(1, 600.0, 270.0)];
        out = touch.on_touch(TouchAction::PointerDown(1), &two, 20);
        out.extend(touch.on_touch(TouchAction::PointerUp(1), &two, 80));
        out.extend(touch.on_touch(TouchAction::Up, &two[..1], 90));
        assert_eq!(out, click(MouseButton::Right));
        // Con dos dedos, el toque largo no corre.
        touch.on_touch(TouchAction::Down, &[p(0, 480.0, 270.0)], 1000);
        touch.on_touch(TouchAction::PointerDown(1), &two, 1010);
        assert_eq!(touch.next_deadline(), None);
        assert!(touch.tick(2000).is_empty());
    }

    #[test]
    fn a_second_finger_while_dragging_is_ignored() {
        let mut touch = mapper(TouchMode::Direct);
        touch.on_touch(TouchAction::Down, &[p(0, 0.0, 0.0)], 0);
        touch.on_touch(TouchAction::Move, &[p(0, 96.0, 0.0)], 10);
        let two = [p(0, 96.0, 0.0), p(1, 500.0, 500.0)];
        assert!(
            touch
                .on_touch(TouchAction::PointerDown(1), &two, 20)
                .is_empty()
        );
        // El segundo se mueve: nada; el primero, el cursor.
        assert!(
            touch
                .on_touch(TouchAction::Move, &[p(0, 96.0, 0.0), p(1, 0.0, 0.0)], 30)
                .is_empty()
        );
        assert_eq!(
            touch.on_touch(TouchAction::Move, &[p(0, 192.0, 0.0), p(1, 0.0, 0.0)], 40),
            vec![abs(13107, 0)]
        );
        // Se levanta el que arrastra: se suelta, y el otro ya no hace nada.
        assert_eq!(
            touch.on_touch(
                TouchAction::PointerUp(0),
                &[p(0, 192.0, 0.0), p(1, 0.0, 0.0)],
                50
            ),
            vec![button(MouseButton::Left, false)]
        );
        assert!(
            touch
                .on_touch(TouchAction::Move, &[p(1, 300.0, 0.0)], 60)
                .is_empty()
        );
        assert!(
            touch
                .on_touch(TouchAction::Up, &[p(1, 300.0, 0.0)], 70)
                .is_empty()
        );
    }

    #[test]
    fn cancel_releases_without_clicking() {
        let mut touch = mapper(TouchMode::Direct);
        touch.on_touch(TouchAction::Down, &[p(0, 0.0, 0.0)], 0);
        touch.on_touch(TouchAction::Move, &[p(0, 96.0, 0.0)], 10);
        assert_eq!(
            touch.on_touch(TouchAction::Cancel, &[p(0, 96.0, 0.0)], 20),
            vec![button(MouseButton::Left, false)]
        );
        // Un toque cancelado sin decidir no hace nada.
        touch.on_touch(TouchAction::Down, &[p(0, 0.0, 0.0)], 100);
        assert!(touch.on_touch(TouchAction::Cancel, &[], 110).is_empty());
        assert_eq!(touch.next_deadline(), None);
        // Y en el panel, tras cancelar, el siguiente no es un segundo toque.
        touch.set_mode(TouchMode::Trackpad);
        tap(&mut touch, (0.0, 0.0), 200, 220);
        touch.on_touch(TouchAction::Down, &[p(0, 0.0, 0.0)], 250);
        touch.on_touch(TouchAction::Cancel, &[], 260);
        touch.on_touch(TouchAction::Down, &[p(0, 0.0, 0.0)], 270);
        assert_eq!(touch.next_deadline(), None);
    }

    #[test]
    fn a_lost_up_releases_before_the_next_gesture() {
        let mut touch = mapper(TouchMode::Direct);
        touch.on_touch(TouchAction::Down, &[p(0, 0.0, 0.0)], 0);
        touch.on_touch(TouchAction::Move, &[p(0, 96.0, 0.0)], 10);
        // Llega otro `ACTION_DOWN` sin el `ACTION_UP`: se suelta el izquierdo y empieza otro.
        assert_eq!(
            touch.on_touch(TouchAction::Down, &[p(5, 480.0, 270.0)], 20),
            vec![button(MouseButton::Left, false), abs(32768, 32768)]
        );
        assert_eq!(
            touch.on_touch(TouchAction::Up, &[p(5, 480.0, 270.0)], 30),
            click(MouseButton::Left)
        );
        // Y un `ACTION_POINTER_DOWN` sin `ACTION_DOWN` empieza el gesto.
        touch.on_touch(TouchAction::PointerDown(2), &[p(2, 0.0, 0.0)], 100);
        assert_eq!(
            touch.on_touch(TouchAction::Up, &[p(2, 0.0, 0.0)], 120),
            click(MouseButton::Left)
        );
    }

    #[test]
    fn changing_mode_or_resetting_releases_the_button() {
        let mut touch = mapper(TouchMode::Trackpad);
        tap(&mut touch, (0.0, 0.0), 0, 20);
        touch.on_touch(TouchAction::Down, &[p(0, 0.0, 0.0)], 50);
        assert_eq!(touch.tick(150), vec![button(MouseButton::Left, true)]);
        assert_eq!(
            touch.set_mode(TouchMode::Direct),
            vec![button(MouseButton::Left, false)]
        );
        assert!(touch.set_mode(TouchMode::Direct).is_empty());
        assert_eq!(touch.mode(), TouchMode::Direct);
        // Lo que quede del gesto ya no hace nada.
        assert!(
            touch
                .on_touch(TouchAction::Move, &[p(0, 400.0, 0.0)], 160)
                .is_empty()
        );
        assert!(
            touch
                .on_touch(TouchAction::Up, &[p(0, 400.0, 0.0)], 170)
                .is_empty()
        );

        touch.on_touch(TouchAction::Down, &[p(0, 0.0, 0.0)], 300);
        touch.on_touch(TouchAction::Move, &[p(0, 96.0, 0.0)], 310);
        assert_eq!(touch.reset(), vec![button(MouseButton::Left, false)]);
        assert!(touch.reset().is_empty());
    }

    #[test]
    fn invalid_positions_are_ignored() {
        let mut touch = mapper(TouchMode::Trackpad);
        touch.on_touch(TouchAction::Down, &[p(0, 0.0, 0.0)], 0);
        assert!(
            touch
                .on_touch(TouchAction::Move, &[p(0, f32::NAN, 0.0)], 10)
                .is_empty()
        );
        assert_eq!(
            touch.on_touch(TouchAction::Move, &[p(0, 10.0, 0.0)], 20),
            vec![InputEvent::MouseMove { dx: 20, dy: 0 }]
        );
        // Un dedo que se apoya sin posición válida no empieza nada.
        let mut other = mapper(TouchMode::Direct);
        assert!(
            other
                .on_touch(TouchAction::Down, &[p(0, f32::INFINITY, 0.0)], 0)
                .is_empty()
        );
        assert_eq!(other.next_deadline(), None);
        assert!(
            other
                .on_touch(TouchAction::Up, &[p(0, 0.0, 0.0)], 10)
                .is_empty()
        );
    }
}
