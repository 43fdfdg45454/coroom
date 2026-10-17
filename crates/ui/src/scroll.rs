//! El contenedor con scroll de las páginas: nada de arriba empuja lo que se está mirando, y cada
//! página recuerda su posición.
//!
//! `iced` empareja el estado de los *widgets* por posición en el árbol, así que todas las páginas
//! (Inicio, Compartir, cada sección de Ajustes) comparten el mismo `scrollable` y su posición: una
//! página nueva se abría a la altura en que se dejó la anterior. [`Anchored`] envuelve ese
//! `scrollable` y, en cada *layout*:
//!
//! - **Una posición por página**: con otra `key`, guarda la posición de la que se deja y vuelve a
//!   la que tenía la nueva (arriba si es la primera vez).
//! - **Ancla del scroll** (como `overflow-anchor` de CSS): recuerda el primer bloque de la página
//!   que se ve arriba y dónde estaba; si en el *layout* siguiente lo de encima cambió de alto y el
//!   bloque se movió, desplaza lo mismo para que siga donde estaba. Los bloques son los hijos del
//!   primer nodo con más de uno, bajando desde el contenido (la columna de la página); se reconocen
//!   por su posición, que no cambia porque las capas y ranuras de las páginas están siempre.
//!
//! El estado del `scrollable` es privado; se lee y se mueve con las operaciones de `iced`
//! ([`operation::Scrollable`]), como haría una tarea.

use std::collections::HashMap;

use iced_core::widget::operation::{self, Operation};
use iced_core::widget::{Id, Tree, tree};
use iced_core::{
    Clipboard, Event, Layout, Length, Rectangle, Shell, Size, Vector, layout, mouse, overlay,
    renderer,
};

use crate::{Element, Renderer, Theme};

/// Ver el módulo.
pub struct Anchored<'a, M> {
    key: u64,
    content: Element<'a, M>,
}

impl<'a, M> Anchored<'a, M> {
    /// `content` es un `scrollable` vertical; `key`, la página que muestra.
    pub fn new(key: u64, content: impl Into<Element<'a, M>>) -> Self {
        Self {
            key,
            content: content.into(),
        }
    }
}

#[derive(Debug, Default)]
struct State {
    /// La página del último *layout* y la posición que tenía.
    key: Option<u64>,
    offset: f32,
    /// Posición guardada de cada página que se dejó.
    offsets: HashMap<u64, f32>,
    anchor: Option<Anchor>,
}

/// El bloque que se veía arriba: su camino de hijos desde el contenido y su altura en él.
#[derive(Debug, Clone, PartialEq)]
struct Anchor {
    path: Vec<usize>,
    y: f32,
}

/// Lo que se sabe del `scrollable` tras un *layout*.
#[derive(Debug, Clone, Copy)]
struct Scroll {
    bounds: Rectangle,
    content_bounds: Rectangle,
    offset: f32,
}

/// Lee el estado del `scrollable` de arriba (sin bajar a sus hijos).
#[derive(Default)]
struct Probe(Option<Scroll>);

impl Operation for Probe {
    fn traverse(&mut self, _operate: &mut dyn FnMut(&mut dyn Operation)) {}

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn operation::Scrollable,
    ) {
        self.0.get_or_insert(Scroll {
            bounds,
            content_bounds,
            offset: translation.y,
        });
    }
}

/// Mueve el `scrollable` de arriba: un desplazamiento relativo o a una posición.
enum Move {
    By(f32),
    To(f32),
}

impl Operation for Move {
    fn traverse(&mut self, _operate: &mut dyn FnMut(&mut dyn Operation)) {}

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        _translation: Vector,
        state: &mut dyn operation::Scrollable,
    ) {
        match *self {
            Move::By(dy) => state.scroll_by(
                operation::scrollable::AbsoluteOffset { x: 0.0, y: dy },
                bounds,
                content_bounds,
            ),
            Move::To(y) => state.scroll_to(operation::scrollable::AbsoluteOffset {
                x: None,
                y: Some(y),
            }),
        }
    }
}

/// Los bloques de la página: los hijos del primer nodo con más de uno, bajando desde `content`.
fn blocks<'l>(content: Layout<'l>) -> (Vec<usize>, Vec<Layout<'l>>) {
    let mut path = Vec::new();
    let mut node = content;
    loop {
        let mut children = node.children();
        match (children.next(), children.next()) {
            (Some(only), None) => {
                path.push(0);
                node = only;
            }
            (Some(first), Some(second)) => {
                let mut all = vec![first, second];
                all.extend(children);
                return (path, all);
            }
            (None, _) => return (path, Vec::new()),
        }
    }
}

/// El primer bloque que se ve con el contenido desplazado `offset`.
fn pick_anchor(content: Layout<'_>, offset: f32) -> Option<Anchor> {
    let top = content.bounds().y;
    let (path, blocks) = blocks(content);
    blocks
        .iter()
        .position(|block| {
            let bounds = block.bounds();
            bounds.y + bounds.height - top > offset
        })
        .map(|index| {
            let mut path = path.clone();
            path.push(index);
            Anchor {
                path,
                y: blocks[index].bounds().y - top,
            }
        })
}

/// Dónde está ahora el bloque de `path`, si sigue existiendo.
fn find(content: Layout<'_>, path: &[usize]) -> Option<f32> {
    let top = content.bounds().y;
    let mut node = content;
    for &index in path {
        node = node.children().nth(index)?;
    }
    Some(node.bounds().y - top)
}

impl<M> iced_core::Widget<M, Theme, Renderer> for Anchored<'_, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let node = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        let layout = Layout::new(&node);
        let mut probe = Probe::default();
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, &mut probe);
        let Some(scroll) = probe.0 else {
            return node;
        };
        let Some(content) = layout.children().next() else {
            return node;
        };
        let state = tree.state.downcast_mut::<State>();
        let max = (scroll.content_bounds.height - scroll.bounds.height).max(0.0);
        let mut offset = scroll.offset;
        let mut moved = None;
        if state.key != Some(self.key) {
            // Otra página: se guarda la posición de la que se deja y se vuelve a la de esta.
            if let Some(old) = state.key {
                state.offsets.insert(old, state.offset);
            }
            offset = state
                .offsets
                .get(&self.key)
                .copied()
                .unwrap_or(0.0)
                .min(max);
            moved = Some(Move::To(offset));
            state.key = Some(self.key);
        } else if offset > 0.0
            && let Some(anchor) = &state.anchor
            && let Some(y) = find(content, &anchor.path)
            && (y - anchor.y).abs() >= 0.5
        {
            // Lo de arriba cambió de alto: lo que se miraba sigue en su sitio.
            let target = (offset + y - anchor.y).clamp(0.0, max);
            moved = Some(Move::By(target - offset));
            offset = target;
        }
        if let Some(mut moved) = moved {
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                layout,
                renderer,
                &mut moved,
            );
        }
        let state = tree.state.downcast_mut::<State>();
        state.offset = offset;
        state.anchor = pick_anchor(content, offset);
        node
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, M: 'a> From<Anchored<'a, M>> for Element<'a, M> {
    fn from(anchored: Anchored<'a, M>) -> Self {
        Element::new(anchored)
    }
}
