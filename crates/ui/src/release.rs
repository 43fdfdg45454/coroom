//! Desplegables que eligen al **soltar** el botón, como los menús de GTK y de Qt: el `pick_list` de
//! `iced` 0.14 elige en cuanto se pulsa sobre una opción, y un clic que se arrepiente (pulsar,
//! apartarse y soltar fuera) ya ha hecho la acción. [`release_to_pick`] envuelve el desplegable y le
//! pasa al menú la pulsación solo cuando el botón se suelta sobre una opción. La barra de
//! desplazamiento de un menú largo recibe la pulsación al momento, para poder arrastrarla.

use iced_core::layout::{self, Layout};
use iced_core::widget::tree::{self, Tree};
use iced_core::{
    Clipboard, Event, Length, Rectangle, Shell, Size, Vector, Widget, mouse, overlay, renderer,
};

use crate::{Element, Renderer, Theme};

/// Envoltorio de [`release_to_pick`].
pub struct ReleaseToPick<'a, Message> {
    content: Element<'a, Message>,
}

/// Hace que el menú desplegado por `content` (un `pick_list`) elija al soltar el botón izquierdo
/// sobre una opción y no al pulsarlo. Pulsar sobre una opción y soltar fuera no elige nada.
pub fn release_to_pick<'a, Message>(
    content: impl Into<Element<'a, Message>>,
) -> ReleaseToPick<'a, Message> {
    ReleaseToPick {
        content: content.into(),
    }
}

#[derive(Debug, Default)]
struct State {
    /// El botón izquierdo se pulsó sobre una opción del menú y aún no se ha soltado.
    armed: bool,
    /// El menú estaba desplegado la última vez que se pidió (para saber cuándo se abre otro).
    open: bool,
}

impl<Message> Widget<Message, Theme, Renderer> for ReleaseToPick<'_, Message> {
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
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
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
        operation: &mut dyn iced_core::widget::Operation,
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
        shell: &mut Shell<'_, Message>,
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
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let Tree {
            state, children, ..
        } = tree;
        let state = state.downcast_mut::<State>();
        let Some(inner) = self.content.as_widget_mut().overlay(
            &mut children[0],
            layout,
            renderer,
            viewport,
            translation,
        ) else {
            state.open = false;
            return None;
        };
        if !std::mem::replace(&mut state.open, true) {
            // Un menú nuevo: una pulsación del anterior (cerrado con el botón aún pulsado) no
            // cuenta para este.
            state.armed = false;
        }
        Some(overlay::Element::new(Box::new(Release {
            inner,
            armed: &mut state.armed,
        })))
    }
}

/// El menú desplegado, con la pulsación retenida hasta que se suelta el botón.
struct Release<'a, Message> {
    inner: overlay::Element<'a, Message, Theme, Renderer>,
    armed: &'a mut bool,
}

impl<Message> Release<'_, Message> {
    /// Si el cursor está sobre una opción del menú. Lo dice el propio menú de `iced`: su lista
    /// pide el cursor de enlace sobre las opciones, y su `Scrollable` le esconde el cursor cuando
    /// está sobre la barra de desplazamiento (la misma zona en la que agarra la barra al pulsar).
    fn over_option(&self, layout: Layout<'_>, cursor: mouse::Cursor, renderer: &Renderer) -> bool {
        cursor.is_over(layout.bounds())
            && self
                .inner
                .as_overlay()
                .mouse_interaction(layout, cursor, renderer)
                == mouse::Interaction::Pointer
    }
}

impl<Message> overlay::Overlay<Message, Theme, Renderer> for Release<'_, Message> {
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        self.inner.as_overlay_mut().layout(renderer, bounds)
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        self.inner
            .as_overlay()
            .draw(renderer, theme, style, layout, cursor);
    }

    fn operate(
        &mut self,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn iced_core::widget::Operation,
    ) {
        self.inner
            .as_overlay_mut()
            .operate(layout, renderer, operation);
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        let over = cursor.is_over(layout.bounds());
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) if over => {
                if self.over_option(layout, cursor, renderer) {
                    // Se retiene: el menú sigue abierto y la página de debajo no la recibe.
                    *self.armed = true;
                } else {
                    // La barra de desplazamiento (o el borde): el menú la recibe ya.
                    self.inner
                        .as_overlay_mut()
                        .update(event, layout, cursor, renderer, clipboard, shell);
                }
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if std::mem::take(self.armed) && self.over_option(layout, cursor, renderer) {
                    // Soltar sobre una opción la elige: el menú recibe la pulsación que espera.
                    let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
                    self.inner
                        .as_overlay_mut()
                        .update(&press, layout, cursor, renderer, clipboard, shell);
                    shell.capture_event();
                }
                // Y también el soltar: la barra de desplazamiento suelta el arrastre con él.
                self.inner
                    .as_overlay_mut()
                    .update(event, layout, cursor, renderer, clipboard, shell);
            }
            _ => {
                self.inner
                    .as_overlay_mut()
                    .update(event, layout, cursor, renderer, clipboard, shell);
            }
        }
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.inner
            .as_overlay()
            .mouse_interaction(layout, cursor, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        layout: Layout<'b>,
        renderer: &Renderer,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.inner.as_overlay_mut().overlay(layout, renderer)
    }

    fn index(&self) -> f32 {
        self.inner.as_overlay().index()
    }
}

impl<'a, Message: 'a> From<ReleaseToPick<'a, Message>> for Element<'a, Message> {
    fn from(widget: ReleaseToPick<'a, Message>) -> Self {
        Element::new(widget)
    }
}
