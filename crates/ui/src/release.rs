//! Desplegables que eligen al **soltar** el botón, como los menús de GTK y de Qt: el `pick_list` de
//! `iced` 0.14 elige en cuanto se pulsa sobre una opción, y un clic que se arrepiente (pulsar,
//! apartarse y soltar fuera) ya ha hecho la acción. [`release_to_pick`] envuelve el desplegable y le
//! pasa al menú la pulsación solo cuando el botón se suelta sobre una opción. La barra de
//! desplazamiento de un menú largo recibe la pulsación al momento, para poder arrastrarla.
//!
//! Al abrirse, el menú sale del botón: crece del 96 al 100 % y se descubre de arriba abajo, así que
//! sus opciones aparecen escalonadas ([`Token::Panel`]; sin movimiento con animaciones reducidas).
//! Se puede elegir desde el primer cuadro: la animación solo cambia cómo se ve.

use iced_core::layout::{self, Layout};
use iced_core::widget::tree::{self, Tree};
use iced_core::{
    Clipboard, Event, Length, Rectangle, Shell, Size, Vector, Widget, mouse, overlay, renderer,
};

use std::cell::Cell;

use crate::motion::{Instant, Motion, Token};
use crate::{Element, Renderer, Theme};

/// De dónde crece el menú: 96 % de su tamaño.
const MENU_START_SCALE: f32 = 0.96;

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
    /// Cuándo se abrió el menú desplegado.
    opened_at: Option<Instant>,
    /// La marca del último cuadro.
    now: Option<Instant>,
    /// El movimiento del último dibujo ([`Theme::motion`]).
    motion: Cell<Motion>,
}

impl State {
    /// Cuánto se abrió el menú, ya con su curva (1: del todo).
    fn opening(&self) -> f32 {
        let (Some(since), Some(now)) = (self.opened_at, self.now) else {
            return 1.0;
        };
        let motion = self.motion.get();
        let duration = motion.duration(Token::Panel);
        if motion.is_reduced() || duration.is_zero() {
            return 1.0;
        }
        let t = now.saturating_duration_since(since).as_secs_f32() / duration.as_secs_f32();
        if t >= 1.0 {
            1.0
        } else {
            Token::Panel.easing().value(t.max(0.0))
        }
    }
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
        let state = tree.state.downcast_mut::<State>();
        match event {
            Event::Window(iced_core::window::Event::RedrawRequested(now)) => state.now = Some(*now),
            // El desplegable se abre al pulsarlo: el cuadro siguiente empieza a desplegar el menú
            // (y desde ahí el propio menú pide los demás).
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(iced_core::touch::Event::FingerPressed { .. })
                if !state.open && cursor.is_over(layout.bounds()) =>
            {
                shell.request_redraw();
            }
            _ => {}
        }
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
        let opened = state.open;
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
        state.open = true;
        if !opened {
            // Un menú nuevo: una pulsación del anterior (cerrado con el botón aún pulsado) no
            // cuenta para este.
            state.armed = false;
            state.opened_at = Some(state.now.unwrap_or_else(Instant::now));
        }
        Some(overlay::Element::new(Box::new(Release { inner, state })))
    }
}

/// El menú desplegado, con la pulsación retenida hasta que se suelta el botón.
struct Release<'a, Message> {
    inner: overlay::Element<'a, Message, Theme, Renderer>,
    state: &'a mut State,
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
        self.state.motion.set(theme.motion);
        let opening = self.state.opening();
        let draw = |renderer: &mut Renderer| {
            self.inner
                .as_overlay()
                .draw(renderer, theme, style, layout, cursor);
        };
        if opening >= 1.0 {
            draw(renderer);
            return;
        }
        // Crece desde el borde de arriba (el del botón) y se descubre hacia abajo.
        let bounds = layout.bounds();
        let scale = MENU_START_SCALE + (1.0 - MENU_START_SCALE) * opening;
        let origin = iced_core::Point::new(bounds.center_x(), bounds.y);
        let transformation = iced_core::Transformation::translate(origin.x, origin.y)
            * iced_core::Transformation::scale(scale)
            * iced_core::Transformation::translate(-origin.x, -origin.y);
        // La sombra del menú sobresale: el recorte deja margen a los lados y abajo.
        let margin = 16.0;
        let clip = Rectangle {
            x: bounds.x - margin,
            y: bounds.y - margin,
            width: bounds.width + 2.0 * margin,
            height: (bounds.height + margin) * opening + margin,
        };
        iced_core::Renderer::with_layer(renderer, clip, |renderer| {
            iced_core::Renderer::with_transformation(renderer, transformation, draw);
        });
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
            Event::Window(iced_core::window::Event::RedrawRequested(now)) => {
                self.state.now = Some(*now);
                if self.state.opening() < 1.0 {
                    shell.request_redraw();
                }
                self.inner
                    .as_overlay_mut()
                    .update(event, layout, cursor, renderer, clipboard, shell);
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) if over => {
                if self.over_option(layout, cursor, renderer) {
                    // Se retiene: el menú sigue abierto y la página de debajo no la recibe.
                    self.state.armed = true;
                } else {
                    // La barra de desplazamiento (o el borde): el menú la recibe ya.
                    self.inner
                        .as_overlay_mut()
                        .update(event, layout, cursor, renderer, clipboard, shell);
                }
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if std::mem::take(&mut self.state.armed)
                    && self.over_option(layout, cursor, renderer)
                {
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
