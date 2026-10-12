//! Foco con el teclado (accesibilidad): `Tab` y `Mayús+Tab` recorren los campos
//! enfocables en el orden de la interfaz y **dan la vuelta** al llegar al final o al principio,
//! como en GNOME y KDE (`focus_next` de `iced` deja la ventana sin foco tras el último campo).
//!
//! Lo usa la ventana (`runner.rs`) y el test del orden de foco (`tests/focus.rs`, con
//! `shots::ShotRenderer::focus_order`), así que el test prueba la lógica real.

use iced_core::widget::Id;
use iced_core::widget::operation::{self, Focusable, Operation, Outcome};
use iced_core::{Rectangle, Renderer};
use iced_runtime::user_interface::UserInterface;

/// Campos enfocables de la interfaz y cuál tiene el foco.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Focus {
    /// Campos enfocables.
    pub total: usize,
    /// El que tiene el foco, si alguno.
    pub focused: Option<usize>,
}

impl Operation for Focus {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn focusable(&mut self, _id: Option<&Id>, _bounds: Rectangle, state: &mut dyn Focusable) {
        if state.is_focused() {
            self.focused = Some(self.total);
        }
        self.total += 1;
    }
}

/// Ejecuta `op` (y las operaciones que encadene) sobre `interface`.
fn run<M, T, R: Renderer>(
    interface: &mut UserInterface<'_, M, T, R>,
    renderer: &R,
    mut op: Box<dyn Operation>,
) {
    loop {
        interface.operate(renderer, op.as_mut());
        match op.finish() {
            Outcome::Chain(next) => op = next,
            Outcome::Some(()) | Outcome::None => return,
        }
    }
}

/// Cuántos campos enfocables hay y cuál tiene el foco.
pub fn focus<M, T, R: Renderer>(interface: &mut UserInterface<'_, M, T, R>, renderer: &R) -> Focus {
    let mut focus = Focus::default();
    interface.operate(renderer, &mut focus);
    focus
}

/// `Tab` (`backwards = false`) o `Mayús+Tab`: el campo siguiente o el anterior, con vuelta.
pub fn move_focus<M, T, R: Renderer>(
    interface: &mut UserInterface<'_, M, T, R>,
    renderer: &R,
    backwards: bool,
) {
    let step = |interface: &mut UserInterface<'_, M, T, R>| {
        let op: Box<dyn Operation> = if backwards {
            Box::new(operation::focusable::focus_previous())
        } else {
            Box::new(operation::focusable::focus_next())
        };
        run(interface, renderer, op);
    };
    step(interface);
    // Del último al primero (o del primero al último), `iced` deja todo sin foco; con nada
    // enfocado, la misma operación enfoca el extremo correcto.
    let after = focus(interface, renderer);
    if after.focused.is_none() && after.total > 0 {
        step(interface);
    }
}
