//! Componentes con movimiento: el estado de un botón cambia con una transición corta y lo que entra
//! o sale se desliza.
//!
//! Como en osu!, el movimiento acompaña a la mano y nunca la espera: el clic vale en el mismo
//! instante, la transición solo cambia cómo se ve. Todo es función del tiempo (la marca del
//! `RedrawRequested` de cada cuadro) y respeta [`Theme::motion`]; mientras algo se mueve se pide
//! el siguiente cuadro (`Shell::request_redraw`) y la ventana lo dibuja al ritmo del compositor.
//!
//! - [`Animated`]: envuelve un botón de `iced` y sigue su estado (normal, encima, pulsado,
//!   desactivado). Al cambiar, el catálogo de estilos mezcla el del estado anterior y el nuevo
//!   ([`crate::style::Blend`]): color al pasar el ratón ([`Token::Hover`]), hundirse al pulsar
//!   ([`Token::Press`]) y volver ([`Token::Quick`]); los que tienen relleno suben 1 px al pasar.
//!   Los botones de icono, además, se encogen al 96 % al pulsarlos; el texto no se escala nunca
//!   (cambiaría el rasterizado de la letra en cada cuadro).
//! - [`Count`]: una cifra cuenta hasta su valor nuevo.
//! - [`ToneFade`]: una insignia que cambia de tono lo hace con un fundido.
//! - [`Shake`]: el campo se sacude cuando aparece un error.
//! - [`Segments`]: el fondo del segmento elegido se desliza hasta el nuevo.
//! - [`Shift`]: dibuja su contenido desplazado (paneles, toasts y filas que entran o salen); el
//!   puntero se traslada con él para que lo que se ve sea lo que se pulsa.

use std::cell::Cell;

use iced_core::widget::operation::Operation;
use iced_core::widget::{Tree, tree};
use iced_core::window;
use iced_core::{
    Clipboard, Event, Layout, Length, Point, Rectangle, Shell, Size, Transformation, Vector,
    layout, mouse, overlay, renderer, touch,
};
use iced_widget::button as iced_button;

use crate::motion::{Instant, Motion, Token};
use crate::style::Blend;
use crate::{Element, Renderer, Theme};

/// Ver el módulo.
pub struct Animated<'a, M> {
    content: Element<'a, M>,
    enabled: bool,
    press_scale: bool,
    lift: bool,
}

impl<'a, M> Animated<'a, M> {
    /// `content` es un botón; `enabled`, si tiene acción (sin ella se ve desactivado).
    pub fn new(content: impl Into<Element<'a, M>>, enabled: bool) -> Self {
        Self {
            content: content.into(),
            enabled,
            press_scale: false,
            lift: false,
        }
    }

    /// Que se encoja al pulsarlo (botones de icono, sin texto).
    pub fn press_scale(mut self, press_scale: bool) -> Self {
        self.press_scale = press_scale;
        self
    }

    /// Que suba 1 px al pasar el ratón ([`crate::style::ButtonClass::lifts`]).
    pub fn lift(mut self, lift: bool) -> Self {
        self.lift = lift;
        self
    }
}

#[derive(Debug, Clone)]
struct State {
    status: iced_button::Status,
    from: iced_button::Status,
    /// Cuándo cambió al estado actual (`None`: siempre estuvo así, sin transición).
    since: Option<Instant>,
    /// La marca del último cuadro.
    now: Option<Instant>,
    /// El botón izquierdo se pulsó encima y todavía no se soltó.
    held: bool,
    /// El movimiento del último dibujo ([`Theme::motion`]).
    motion: Cell<Motion>,
}

impl State {
    fn new(enabled: bool) -> Self {
        let status = if enabled {
            iced_button::Status::Active
        } else {
            iced_button::Status::Disabled
        };
        Self {
            status,
            from: status,
            since: None,
            now: None,
            held: false,
            motion: Cell::new(Motion::FULL),
        }
    }

    fn token(&self) -> Token {
        match (self.from, self.status) {
            (_, iced_button::Status::Pressed) => Token::Press,
            (iced_button::Status::Pressed, _) => Token::Quick,
            _ => Token::Hover,
        }
    }

    /// Avance de la transición en curso, ya con su curva (1 si no hay).
    fn progress(&self) -> f32 {
        let (Some(since), Some(now)) = (self.since, self.now) else {
            return 1.0;
        };
        let token = self.token();
        let duration = self.motion.get().duration(token);
        if duration.is_zero() {
            return 1.0;
        }
        let t = (now.saturating_duration_since(since).as_secs_f32() / duration.as_secs_f32())
            .clamp(0.0, 1.0);
        if t >= 1.0 {
            1.0
        } else {
            token.easing().value(t)
        }
    }

    fn set(&mut self, status: iced_button::Status) {
        if status != self.status {
            self.from = self.status;
            self.status = status;
            self.since = Some(Instant::now());
        }
    }
}

impl<M> iced_core::Widget<M, Theme, Renderer> for Animated<'_, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::new(self.enabled))
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
        let state = tree.state.downcast_mut::<State>();
        let over = cursor.is_over(layout.bounds());
        match event {
            Event::Window(window::Event::RedrawRequested(now)) => {
                state.now = Some(*now);
                if state.progress() < 1.0 {
                    shell.request_redraw();
                }
                return;
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. })
                if over =>
            {
                state.held = true;
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerLifted { .. } | touch::Event::FingerLost { .. }) => {
                state.held = false;
            }
            _ => {}
        }
        let status = if !self.enabled {
            iced_button::Status::Disabled
        } else if state.held && over {
            iced_button::Status::Pressed
        } else if over {
            iced_button::Status::Hovered
        } else {
            iced_button::Status::Active
        };
        let before = state.status;
        state.set(status);
        if state.status != before {
            shell.request_redraw();
        }
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
        let state = tree.state.downcast_ref::<State>();
        // El movimiento que pide la ventana ahora (puede cambiar sin que cambie el estado).
        state.motion.set(theme.motion);
        let progress = state.progress();
        let themed = Theme {
            blend: (progress < 1.0).then_some(Blend {
                from: state.from,
                progress,
            }),
            ..*theme
        };
        let pressed = |status| f32::from(u8::from(status == iced_button::Status::Pressed));
        let depth = pressed(state.from) + (pressed(state.status) - pressed(state.from)) * progress;
        let scale = 1.0 - (1.0 - theme.motion.press_scale()) * depth;
        // Sube un píxel entero (la letra no se emborrona) a mitad de la transición al pasar.
        let hovered = |status| f32::from(u8::from(status == iced_button::Status::Hovered));
        let height = hovered(state.from) + (hovered(state.status) - hovered(state.from)) * progress;
        let lift = if self.lift && !theme.motion.is_off() {
            -height.round()
        } else {
            0.0
        };
        let draw = |renderer: &mut Renderer| {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                &themed,
                style,
                layout,
                cursor,
                viewport,
            );
        };
        if lift != 0.0 {
            iced_core::Renderer::with_translation(renderer, Vector::new(0.0, lift), draw);
        } else if self.press_scale && scale < 1.0 {
            let center = layout.bounds().center();
            let transformation = Transformation::translate(center.x, center.y)
                * Transformation::scale(scale)
                * Transformation::translate(-center.x, -center.y);
            iced_core::Renderer::with_transformation(renderer, transformation, draw);
        } else {
            draw(renderer);
        }
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

impl<'a, M: 'a> From<Animated<'a, M>> for Element<'a, M> {
    fn from(animated: Animated<'a, M>) -> Self {
        Element::new(animated)
    }
}

/// Ver el módulo.
pub struct Shift<'a, M> {
    content: Element<'a, M>,
    offset: Vector,
}

impl<'a, M> Shift<'a, M> {
    /// `content` dibujado `offset` más allá de su sitio (que no cambia: no mueve a los demás).
    pub fn new(content: impl Into<Element<'a, M>>, offset: Vector) -> Self {
        Self {
            content: content.into(),
            offset,
        }
    }
}

/// El puntero visto desde el contenido desplazado `offset`.
fn shifted(cursor: mouse::Cursor, offset: Vector) -> mouse::Cursor {
    match cursor {
        mouse::Cursor::Available(point) => {
            mouse::Cursor::Available(Point::new(point.x - offset.x, point.y - offset.y))
        }
        mouse::Cursor::Levitating(point) => {
            mouse::Cursor::Levitating(Point::new(point.x - offset.x, point.y - offset.y))
        }
        mouse::Cursor::Unavailable => mouse::Cursor::Unavailable,
    }
}

impl<M> iced_core::Widget<M, Theme, Renderer> for Shift<'_, M> {
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
            shifted(cursor, self.offset),
            renderer,
            clipboard,
            shell,
            viewport,
        );
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
        let cursor = shifted(cursor, self.offset);
        let draw = |renderer: &mut Renderer| {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout,
                cursor,
                viewport,
            );
        };
        if self.offset == Vector::ZERO {
            draw(renderer);
        } else {
            iced_core::Renderer::with_translation(renderer, self.offset, draw);
        }
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
            shifted(cursor, self.offset),
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
            translation + self.offset,
        )
    }
}

impl<'a, M: 'a> From<Shift<'a, M>> for Element<'a, M> {
    fn from(shift: Shift<'a, M>) -> Self {
        Element::new(shift)
    }
}

/// Un botón de `iced` que se anima ([`Animated`]): los mismos ajustes que `Button`, y se convierte
/// en [`Element`] ya envuelto.
pub struct AnimatedButton<'a, M> {
    button: iced_button::Button<'a, M, Theme, Renderer>,
    enabled: bool,
    press_scale: bool,
    lift: bool,
}

impl<'a, M: Clone + 'a> AnimatedButton<'a, M> {
    /// Envuelve `button` (sin acción: desactivado).
    pub fn new(button: iced_button::Button<'a, M, Theme, Renderer>) -> Self {
        Self {
            button,
            enabled: false,
            press_scale: false,
            lift: crate::style::ButtonClass::default().lifts(),
        }
    }

    /// Acción al pulsarlo.
    pub fn on_press(self, message: M) -> Self {
        self.on_press_maybe(Some(message))
    }

    /// Acción al pulsarlo, si la hay (si no, se ve desactivado).
    pub fn on_press_maybe(mut self, message: Option<M>) -> Self {
        self.enabled = message.is_some();
        self.button = self.button.on_press_maybe(message);
        self
    }

    /// Ancho.
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.button = self.button.width(width);
        self
    }

    /// Alto.
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.button = self.button.height(height);
        self
    }

    /// Relleno.
    pub fn padding(mut self, padding: impl Into<iced_core::Padding>) -> Self {
        self.button = self.button.padding(padding);
        self
    }

    /// Clase de estilo.
    pub fn class(mut self, class: crate::style::ButtonClass) -> Self {
        self.button = self.button.class(class);
        self.lift = class.lifts();
        self
    }

    /// Que se encoja al pulsarlo (solo para botones de icono: el texto no se escala).
    pub fn press_scale(mut self, press_scale: bool) -> Self {
        self.press_scale = press_scale;
        self
    }
}

impl<'a, M: Clone + 'a> From<AnimatedButton<'a, M>> for Element<'a, M> {
    fn from(button: AnimatedButton<'a, M>) -> Self {
        Animated::new(button.button, button.enabled)
            .press_scale(button.press_scale)
            .lift(button.lift)
            .into()
    }
}

/// Botón animado con `content` (el `button` de `iced` envuelto en [`Animated`]).
pub fn button<'a, M: Clone + 'a>(content: impl Into<Element<'a, M>>) -> AnimatedButton<'a, M> {
    AnimatedButton::new(iced_widget::button(content))
}

/// Su contenido con la altura multiplicada por `factor` (0 a 1) y recortado: lo que se abre crece
/// y lo que se va se pliega, y lo de debajo se desliza con él en lugar de saltar.
pub struct Collapse<'a, M> {
    content: Element<'a, M>,
    factor: f32,
}

impl<'a, M> Collapse<'a, M> {
    /// `content` a `factor` de su altura (1: entero; 0: no ocupa nada ni recibe eventos).
    pub fn new(content: impl Into<Element<'a, M>>, factor: f32) -> Self {
        Self {
            content: content.into(),
            factor: factor.clamp(0.0, 1.0),
        }
    }
}

impl<M> iced_core::Widget<M, Theme, Renderer> for Collapse<'_, M> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        let size = self.content.as_widget().size();
        Size::new(size.width, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let child =
            self.content
                .as_widget_mut()
                .layout(&mut tree.children[0], renderer, &limits.loose());
        let size = child.size();
        let height = if self.factor >= 1.0 {
            size.height
        } else {
            (size.height * self.factor).round()
        };
        layout::Node::with_children(
            limits.resolve(
                Length::Shrink,
                Length::Shrink,
                Size::new(size.width, height),
            ),
            vec![child],
        )
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
        let Some(child) = layout.children().next() else {
            return;
        };
        // Fuera de lo visible no se pulsa nada.
        let cursor = if cursor.is_over(layout.bounds()) {
            cursor
        } else {
            cursor.levitate()
        };
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            child,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
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
        let (Some(child), Some(clip)) = (
            layout.children().next(),
            layout.bounds().intersection(viewport),
        ) else {
            return;
        };
        if self.factor <= 0.0 {
            return;
        }
        let draw = |renderer: &mut Renderer| {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                child,
                cursor,
                &clip,
            );
        };
        if self.factor >= 1.0 {
            draw(renderer);
        } else {
            iced_core::Renderer::with_layer(renderer, clip, draw);
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        if let Some(child) = layout.children().next() {
            self.content
                .as_widget_mut()
                .operate(&mut tree.children[0], child, renderer, operation);
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
        match layout.children().next() {
            Some(child) if cursor.is_over(layout.bounds()) => self
                .content
                .as_widget()
                .mouse_interaction(&tree.children[0], child, cursor, viewport, renderer),
            _ => mouse::Interaction::None,
        }
    }
}

impl<'a, M: 'a> From<Collapse<'a, M>> for Element<'a, M> {
    fn from(collapse: Collapse<'a, M>) -> Self {
        Element::new(collapse)
    }
}

/// Interruptor cuyo tirador va de un lado al otro con un resorte ([`Token::Panel`]), estirándose
/// con la velocidad, y cuyo raíl cambia de color con él. Como el de `iced`: cambia al pulsarlo y, sin `on_toggle`, se ve
/// desactivado.
pub struct Switch<'a, M> {
    value: bool,
    on_toggle: Option<Box<dyn Fn(bool) -> M + 'a>>,
    size: f32,
}

impl<'a, M> Switch<'a, M> {
    /// Interruptor en `value`, de `size` de alto (y el doble de ancho).
    pub fn new(value: bool, on_toggle: Option<impl Fn(bool) -> M + 'a>, size: f32) -> Self {
        Self {
            value,
            on_toggle: on_toggle.map(|f| Box::new(f) as Box<dyn Fn(bool) -> M + 'a>),
            size,
        }
    }
}

#[derive(Debug)]
struct SwitchState {
    spring: crate::motion::Spring,
    now: Option<Instant>,
    hovered: bool,
    motion: Cell<Motion>,
}

impl SwitchState {
    fn position(&self) -> f32 {
        match self.now {
            Some(now) => self.spring.value(now).clamp(0.0, 1.0),
            None => self.spring.target(),
        }
    }

    /// Cuánto se estira el tirador por la velocidad que lleva (fracción de su ancho).
    fn stretch(&self) -> f32 {
        let Some(now) = self.now else {
            return 0.0;
        };
        (self.spring.velocity(now).abs() * SWITCH_STRETCH_PER_SPEED).min(SWITCH_MAX_STRETCH)
    }
}

/// Estiramiento del tirador por unidad de velocidad (recorridos por segundo).
const SWITCH_STRETCH_PER_SPEED: f32 = 0.04;
/// Lo más que se estira el tirador: un 35 % más ancho.
const SWITCH_MAX_STRETCH: f32 = 0.35;

impl<M> iced_core::Widget<M, Theme, Renderer> for Switch<'_, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<SwitchState>()
    }

    fn state(&self) -> tree::State {
        let motion = Motion::FULL;
        tree::State::new(SwitchState {
            spring: motion.spring(Token::Panel, f32::from(u8::from(self.value))),
            now: None,
            hovered: false,
            motion: Cell::new(motion),
        })
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.size * 2.0), Length::Fixed(self.size))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.resolve(
            Length::Fixed(self.size * 2.0),
            Length::Fixed(self.size),
            Size::ZERO,
        ))
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<SwitchState>();
        let target = f32::from(u8::from(self.value));
        let now = state.now.unwrap_or_else(Instant::now);
        state
            .spring
            .set_settle(state.motion.get().spring_settle(Token::Panel));
        if state.spring.target() != target {
            state.spring.go(target, now);
            shell.request_redraw();
        }
        let over = cursor.is_over(layout.bounds());
        if over != state.hovered {
            state.hovered = over;
            shell.request_redraw();
        }
        match event {
            Event::Window(window::Event::RedrawRequested(now)) => {
                state.now = Some(*now);
                if state.spring.is_animating(*now) {
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. })
                if over =>
            {
                if let Some(on_toggle) = &self.on_toggle {
                    shell.publish(on_toggle(!self.value));
                    shell.capture_event();
                }
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        use iced_core::Renderer as _;
        use iced_widget::toggler::{self, Catalog as _};

        let state = tree.state.downcast_ref::<SwitchState>();
        state.motion.set(theme.motion);
        let position = state.position();
        let status = |is_toggled| {
            if self.on_toggle.is_none() {
                toggler::Status::Disabled { is_toggled }
            } else if state.hovered {
                toggler::Status::Hovered { is_toggled }
            } else {
                toggler::Status::Active { is_toggled }
            }
        };
        let off = theme.style(&(), status(false));
        let on = theme.style(&(), status(true));
        let color = |background: iced_core::Background| match background {
            iced_core::Background::Color(color) => color,
            iced_core::Background::Gradient(_) => iced_core::Color::TRANSPARENT,
        };
        let bounds = layout.bounds();
        renderer.fill_quad(
            renderer::Quad {
                bounds,
                border: iced_core::border::rounded(bounds.height / 2.0),
                ..renderer::Quad::default()
            },
            crate::style::mix(color(off.background), color(on.background), position),
        );
        let padding = (bounds.height * on.padding_ratio).round();
        let side = bounds.height - 2.0 * padding;
        let travel = bounds.width - 2.0 * padding - side;
        // Mientras va de un lado al otro se estira con la velocidad, sin salirse del raíl.
        let extra = side * state.stretch();
        renderer.fill_quad(
            renderer::Quad {
                bounds: Rectangle {
                    x: bounds.x + padding + (travel - extra) * position,
                    y: bounds.y + padding,
                    width: side + extra,
                    height: side,
                },
                border: iced_core::border::rounded(side / 2.0),
                ..renderer::Quad::default()
            },
            crate::style::mix(color(off.foreground), color(on.foreground), position),
        );
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if self.on_toggle.is_some() && cursor.is_over(layout.bounds()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::None
        }
    }
}

impl<'a, M: 'a> From<Switch<'a, M>> for Element<'a, M> {
    fn from(switch: Switch<'a, M>) -> Self {
        Element::new(switch)
    }
}

/// Una fila de segmentos (pestañas, selectores) con el fondo del elegido debajo, que se desliza
/// con un resorte ([`Token::Panel`]) hasta el nuevo al cambiar de elección; con distinto ancho,
/// también cambia de ancho por el camino. Los segmentos no pintan su propio fondo de elegido.
pub struct Segments<'a, M> {
    content: Element<'a, M>,
    selected: Option<usize>,
    thumb: crate::style::ButtonClass,
    stops: Option<Vec<usize>>,
}

impl<'a, M> Segments<'a, M> {
    /// `content` es la fila de segmentos; `selected`, el elegido; `thumb`, el estilo del fondo.
    pub fn new(
        content: impl Into<Element<'a, M>>,
        selected: Option<usize>,
        thumb: crate::style::ButtonClass,
    ) -> Self {
        Self {
            content: content.into(),
            selected,
            thumb,
            stops: None,
        }
    }

    /// Solo algunos hijos son segmentos (`stops`, sus índices en orden): los demás (un hueco que
    /// separa dos grupos, un pie) no cuentan, y `selected` es la posición entre los segmentos. El
    /// fondo va de un segmento al siguiente por encima de lo que hay entre ellos (el riel).
    pub fn stops(mut self, stops: Vec<usize>) -> Self {
        self.stops = Some(stops);
        self
    }

    /// Los rectángulos de los segmentos entre los hijos de `layout`.
    fn segment_bounds(&self, layout: Layout<'_>) -> Vec<Rectangle> {
        let children: Vec<Rectangle> = layout.children().map(|child| child.bounds()).collect();
        match &self.stops {
            None => children,
            Some(stops) => stops
                .iter()
                .filter_map(|index| children.get(*index).copied())
                .collect(),
        }
    }
}

#[derive(Debug)]
struct SegmentsState {
    /// Dónde está el fondo, en segmentos (1,5: a medio camino entre el segundo y el tercero).
    spring: crate::motion::Spring,
    selected: Option<usize>,
    now: Option<Instant>,
    motion: Cell<Motion>,
}

impl SegmentsState {
    fn position(&self) -> f32 {
        match self.now {
            Some(now) => self.spring.value(now),
            None => self.spring.target(),
        }
    }
}

/// El rectángulo del fondo en `position` (en segmentos) entre los de `segments`.
fn thumb_bounds(segments: &[Rectangle], position: f32) -> Option<Rectangle> {
    let last = segments.len().checked_sub(1)?;
    let position = position.clamp(0.0, last as f32);
    // `position` está entre 0 y `last`: el índice entra.
    let index = (position.floor() as usize).min(last);
    let next = (index + 1).min(last);
    let t = position - index as f32;
    let (a, b) = (segments[index], segments[next]);
    let lerp = |a: f32, b: f32| a + (b - a) * t;
    Some(Rectangle {
        x: lerp(a.x, b.x),
        y: lerp(a.y, b.y),
        width: lerp(a.width, b.width),
        height: lerp(a.height, b.height),
    })
}

impl<M> iced_core::Widget<M, Theme, Renderer> for Segments<'_, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<SegmentsState>()
    }

    fn state(&self) -> tree::State {
        let motion = Motion::FULL;
        tree::State::new(SegmentsState {
            spring: motion.spring(Token::Panel, self.selected.unwrap_or(0) as f32),
            selected: self.selected,
            now: None,
            motion: Cell::new(motion),
        })
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
        let state = tree.state.downcast_mut::<SegmentsState>();
        if state.selected != self.selected {
            let now = state.now.unwrap_or_else(Instant::now);
            state
                .spring
                .set_settle(state.motion.get().spring_settle(Token::Panel));
            match (state.selected, self.selected) {
                // De uno a otro: se desliza.
                (Some(_), Some(to)) => state.spring.go(to as f32, now),
                // Sin elegido antes: aparece en su sitio.
                (None, Some(to)) => {
                    state.spring = state.motion.get().spring(Token::Panel, to as f32);
                }
                (_, None) => {}
            }
            state.selected = self.selected;
            shell.request_redraw();
        }
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            state.now = Some(*now);
            if state.spring.is_animating(*now) {
                shell.request_redraw();
            }
        }
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
        use iced_core::Renderer as _;

        let state = tree.state.downcast_ref::<SegmentsState>();
        state.motion.set(theme.motion);
        if state.selected.is_some() {
            let segments = self.segment_bounds(layout);
            if let Some(bounds) = thumb_bounds(&segments, state.position()) {
                let thumb = theme.button_style(self.thumb, iced_button::Status::Active);
                renderer.fill_quad(
                    renderer::Quad {
                        bounds,
                        border: thumb.border,
                        shadow: thumb.shadow,
                        snap: thumb.snap,
                    },
                    thumb
                        .background
                        .unwrap_or(iced_core::Background::Color(iced_core::Color::TRANSPARENT)),
                );
            }
        }
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

impl<'a, M: 'a> From<Segments<'a, M>> for Element<'a, M> {
    fn from(segments: Segments<'a, M>) -> Self {
        Element::new(segments)
    }
}

/// Su contenido (una insignia o un punto de estado) con los colores de tono que pasan del tono
/// anterior al nuevo con un fundido ([`Token::Panel`]) cuando cambia: un amigo que se conecta, la
/// calidad del enlace que baja. El primer tono no se funde.
pub struct ToneFade<'a, M> {
    content: Element<'a, M>,
    tone: crate::theme::Tone,
}

impl<'a, M> ToneFade<'a, M> {
    /// `content` dibujado con el tono `tone`.
    pub fn new(content: impl Into<Element<'a, M>>, tone: crate::theme::Tone) -> Self {
        Self {
            content: content.into(),
            tone,
        }
    }
}

#[derive(Debug)]
struct ToneFadeState {
    tone: crate::theme::Tone,
    from: crate::theme::Tone,
    since: Option<Instant>,
    now: Option<Instant>,
    motion: Cell<Motion>,
}

impl ToneFadeState {
    /// Avance del fundido, ya con su curva (1 si no hay).
    fn progress(&self) -> f32 {
        let (Some(since), Some(now)) = (self.since, self.now) else {
            return 1.0;
        };
        let duration = self.motion.get().duration(Token::Panel);
        if duration.is_zero() {
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

impl<M> iced_core::Widget<M, Theme, Renderer> for ToneFade<'_, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<ToneFadeState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(ToneFadeState {
            tone: self.tone,
            from: self.tone,
            since: None,
            now: None,
            motion: Cell::new(Motion::FULL),
        })
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
        let state = tree.state.downcast_mut::<ToneFadeState>();
        if state.tone != self.tone {
            // A medio fundido sigue desde el tono que llevaba mayormente a la vista.
            state.from = if state.progress() < 0.5 {
                state.from
            } else {
                state.tone
            };
            state.tone = self.tone;
            state.since = Some(state.now.unwrap_or_else(Instant::now));
            shell.request_redraw();
        }
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            state.now = Some(*now);
            if state.progress() < 1.0 {
                shell.request_redraw();
            }
        }
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
        let state = tree.state.downcast_ref::<ToneFadeState>();
        state.motion.set(theme.motion);
        let progress = state.progress();
        let themed = Theme {
            fade: (progress < 1.0).then_some(crate::style::Fade {
                from: state.from,
                progress,
            }),
            ..*theme
        };
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            &themed,
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

impl<'a, M: 'a> From<ToneFade<'a, M>> for Element<'a, M> {
    fn from(fade: ToneFade<'a, M>) -> Self {
        Element::new(fade)
    }
}

/// Lo que tarda una cifra en contar hasta su valor nuevo.
const COUNT: std::time::Duration = std::time::Duration::from_millis(300);

/// Una cifra que cuenta hasta su valor nuevo en 300 ms en lugar de saltar (latencias, el
/// indicador del servidor). `build` dibuja la cifra para un valor; se llama en cada cuadro de la
/// cuenta con el valor intermedio y debe dar siempre la misma forma (una ranura de ancho fijo,
/// como [`crate::widget::figure`]), así que la fila no se mueve mientras cuenta. Con animaciones
/// reducidas o sin ellas, salta.
pub struct Count<'a, M> {
    value: f64,
    build: Box<dyn Fn(f64) -> Element<'a, M> + 'a>,
    content: Element<'a, M>,
    /// El valor que dibuja `content`.
    built: f64,
}

impl<'a, M> Count<'a, M> {
    /// La cifra `value`, dibujada con `build`.
    pub fn new(value: f64, build: impl Fn(f64) -> Element<'a, M> + 'a) -> Self {
        let content = build(value);
        Self {
            value,
            build: Box::new(build),
            content,
            built: value,
        }
    }
}

#[derive(Debug)]
struct CountState {
    from: f64,
    to: f64,
    since: Option<Instant>,
    now: Option<Instant>,
    motion: Cell<Motion>,
}

impl CountState {
    /// Avance de la cuenta, ya con su curva (1 si no hay).
    fn progress(&self) -> f32 {
        let (Some(since), Some(now)) = (self.since, self.now) else {
            return 1.0;
        };
        let motion = self.motion.get();
        let duration = motion.scale(COUNT);
        if motion.is_reduced() || motion.is_off() || duration.is_zero() {
            return 1.0;
        }
        let t = now.saturating_duration_since(since).as_secs_f32() / duration.as_secs_f32();
        if t >= 1.0 {
            1.0
        } else {
            Token::Panel.easing().value(t.max(0.0))
        }
    }

    fn shown(&self) -> f64 {
        self.from + (self.to - self.from) * f64::from(self.progress())
    }
}

impl<M> iced_core::Widget<M, Theme, Renderer> for Count<'_, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<CountState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(CountState {
            from: self.value,
            to: self.value,
            since: None,
            now: None,
            motion: Cell::new(Motion::FULL),
        })
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
        let state = tree.state.downcast_mut::<CountState>();
        if state.to != self.value {
            // Un valor nuevo: cuenta desde el que se ve ahora.
            state.from = state.shown();
            state.to = self.value;
            state.since = state.now;
        }
        let shown = state.shown();
        if shown != self.built {
            self.content = (self.build)(shown);
            self.built = shown;
            tree.children[0].diff(&self.content);
        }
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
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
        let state = tree.state.downcast_mut::<CountState>();
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            state.now = Some(*now);
            if state.to != self.value || state.since.is_some() && state.progress() < 1.0 {
                shell.invalidate_layout();
                shell.request_redraw();
            } else if state.since.is_some() {
                // Terminó: el último *layout* deja el valor exacto.
                state.since = None;
                shell.invalidate_layout();
            }
        }
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
        tree.state
            .downcast_ref::<CountState>()
            .motion
            .set(theme.motion);
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
}

impl<'a, M: 'a> From<Count<'a, M>> for Element<'a, M> {
    fn from(count: Count<'a, M>) -> Self {
        Element::new(count)
    }
}

/// Lo que dura el sacudón de un campo con error.
const SHAKE: std::time::Duration = std::time::Duration::from_millis(240);
/// Oscilaciones del sacudón.
const SHAKE_CYCLES: f32 = 3.0;
/// Cuánto se aparta el campo al sacudirse, en px.
const SHAKE_DISTANCE: f32 = 4.0;

/// Desplazamiento horizontal del sacudón a `t` (0 a 1) de su duración: tres oscilaciones que se
/// apagan, en px enteros (la letra no se emborrona entre píxeles).
fn shake_offset(t: f32) -> f32 {
    if !(0.0..1.0).contains(&t) {
        return 0.0;
    }
    (SHAKE_DISTANCE * (1.0 - t * t) * (std::f32::consts::TAU * SHAKE_CYCLES * t).sin()).round()
}

/// Su contenido, que se sacude de lado a lado (240 ms) cuando aparece un error o cambia por otro
/// (`error`: algo que lo identifique, como un *hash* del texto). Sin movimiento, no se sacude.
pub struct Shake<'a, M> {
    content: Element<'a, M>,
    error: Option<u64>,
}

impl<'a, M> Shake<'a, M> {
    /// `content` con el error `error` (`None`: sin error).
    pub fn new(content: impl Into<Element<'a, M>>, error: Option<u64>) -> Self {
        Self {
            content: content.into(),
            error,
        }
    }
}

#[derive(Debug)]
struct ShakeState {
    error: Option<u64>,
    since: Option<Instant>,
    now: Option<Instant>,
    motion: Cell<Motion>,
}

impl ShakeState {
    fn offset(&self) -> f32 {
        let (Some(since), Some(now)) = (self.since, self.now) else {
            return 0.0;
        };
        if self.motion.get().is_reduced() || self.motion.get().is_off() {
            return 0.0;
        }
        shake_offset(now.saturating_duration_since(since).as_secs_f32() / SHAKE.as_secs_f32())
    }

    fn is_shaking(&self) -> bool {
        match (self.since, self.now) {
            (Some(since), Some(now)) => now.saturating_duration_since(since) < SHAKE,
            _ => false,
        }
    }
}

impl<M> iced_core::Widget<M, Theme, Renderer> for Shake<'_, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<ShakeState>()
    }

    fn state(&self) -> tree::State {
        // Un error que ya estaba al aparecer el campo no lo sacude.
        tree::State::new(ShakeState {
            error: self.error,
            since: None,
            now: None,
            motion: Cell::new(Motion::FULL),
        })
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
        let state = tree.state.downcast_mut::<ShakeState>();
        if state.error != self.error {
            if self.error.is_some() {
                state.since = Some(state.now.unwrap_or_else(Instant::now));
                shell.request_redraw();
            }
            state.error = self.error;
        }
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            state.now = Some(*now);
            if state.is_shaking() {
                shell.request_redraw();
            }
        }
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
        let state = tree.state.downcast_ref::<ShakeState>();
        state.motion.set(theme.motion);
        let offset = state.offset();
        let draw = |renderer: &mut Renderer| {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout,
                cursor,
                viewport,
            );
        };
        if offset == 0.0 {
            draw(renderer);
        } else {
            iced_core::Renderer::with_translation(renderer, Vector::new(offset, 0.0), draw);
        }
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

impl<'a, M: 'a> From<Shake<'a, M>> for Element<'a, M> {
    fn from(shake: Shake<'a, M>) -> Self {
        Element::new(shake)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, width: f32) -> Rectangle {
        Rectangle {
            x,
            y: 0.0,
            width,
            height: 20.0,
        }
    }

    #[test]
    fn thumb_slides_between_segments_and_takes_their_width() {
        let segments = [rect(0.0, 40.0), rect(42.0, 80.0), rect(124.0, 60.0)];
        assert_eq!(thumb_bounds(&segments, 1.0), Some(segments[1]));
        let halfway = thumb_bounds(&segments, 0.5).unwrap();
        assert_eq!(halfway.x, 21.0);
        assert_eq!(halfway.width, 60.0);
        // Fuera de la fila se queda en el borde; sin segmentos, no hay fondo.
        assert_eq!(thumb_bounds(&segments, 7.0), Some(segments[2]));
        assert_eq!(thumb_bounds(&segments, -1.0), Some(segments[0]));
        assert_eq!(thumb_bounds(&[], 0.0), None);
    }

    #[test]
    fn a_count_goes_from_the_old_value_to_the_new_one_and_stops_there() {
        let start = Instant::now();
        let state = |motion: Motion, after_ms: u64| CountState {
            from: 10.0,
            to: 50.0,
            since: Some(start),
            now: Some(start + std::time::Duration::from_millis(after_ms)),
            motion: Cell::new(motion),
        };
        assert_eq!(state(Motion::FULL, 0).shown(), 10.0);
        let halfway = state(Motion::FULL, 150).shown();
        assert!(halfway > 10.0 && halfway < 50.0, "{halfway}");
        assert_eq!(state(Motion::FULL, 300).shown(), 50.0);
        // Sin animaciones (o con la entrada tomada) salta.
        assert_eq!(state(Motion::OFF, 0).shown(), 50.0);
    }

    #[test]
    fn shake_swings_three_times_and_settles_in_place() {
        assert_eq!(shake_offset(0.0), 0.0);
        assert_eq!(shake_offset(1.0), 0.0);
        let samples: Vec<f32> = (0..240).map(|ms| shake_offset(ms as f32 / 240.0)).collect();
        assert!(
            samples
                .iter()
                .all(|x| x.abs() <= SHAKE_DISTANCE && x.fract() == 0.0)
        );
        // Va a un lado y al otro: tres veces a la derecha y tres a la izquierda.
        let mut swings = 0;
        let mut side = 0.0_f32;
        for x in samples.into_iter().filter(|x| *x != 0.0) {
            if x.signum() != side {
                side = x.signum();
                swings += 1;
            }
        }
        assert_eq!(swings, 6);
    }
}
