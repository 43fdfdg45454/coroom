//! Ranuras: lo que puede aparecer tiene su sitio siempre, y lo que cambia de ancho no mueve lo de
//! alrededor (nada empuja el scroll ni lo que se está mirando).
//!
//! `iced` empareja el estado de cada *widget* (desplazamiento, foco, texto escrito, arrastre de un
//! deslizador) con su posición entre los hijos, no con un nombre: si algo aparece delante de una
//! lista, la lista cambia de posición, `iced` la toma por otra y la deja arriba. Y una fila, una
//! columna o una pila descartan al construirse los hijos de tamaño fijo cero (`Space` de 0×0,
//! [`crate::widget::gap`]`(0.0)`), así que esos no guardan el sitio. Aquí:
//!
//! - [`Reserve`]: ocupa siempre lo que la mayor de sus muestras, medidas por el renderizador al
//!   maquetar con su misma letra (no se suponen métricas), muestre algo o nada: lo que aparece o
//!   cambia dentro no mueve lo de alrededor. Con ella se hacen los números de ancho fijo
//!   ([`crate::widget::figure`]): `iced` no permite pedir cifras tabulares a la letra.
//! - [`OneLine`]: texto en una línea que, si no cabe, se acorta con «…» (medido con el
//!   renderizador, no por caracteres) y enseña el texto completo al pasar el ratón.
//!
//! Las funciones de uso están en [`crate::widget`] (`slot`, `reserve`, `reserved`, `figure`,
//! `one_line`).

use iced_core::alignment::{Horizontal, Vertical};
use iced_core::text::Wrapping;
use iced_core::widget::operation::Operation;
use iced_core::widget::{Tree, tree};
use iced_core::{
    Alignment, Clipboard, Event, Layout, Length, Rectangle, Shell, Size, Vector, layout, mouse,
    overlay, renderer,
};
use iced_widget::Space;

use crate::widget::{Text, with_tooltip};
use crate::{Element, Renderer, Theme};

// --- Ranura con muestras --------------------------------------------------------------------------

/// Ranura que ocupa siempre lo que la mayor de sus muestras (ancho y alto por separado), con lo
/// que muestra (o nada) alineado dentro. Las muestras se maquetan pero no se dibujan, no reciben
/// eventos ni operaciones (foco, `id`); lo mostrado es el primer hijo, así que su estado no cambia
/// de sitio aunque aparezca o desaparezca. Si lo mostrado no cabe en las muestras (no debería), la
/// ranura crece hasta él antes que pisar lo de al lado. Lo mostrado va con `Length::Shrink`.
pub struct Reserve<'a, M> {
    /// `[mostrado, muestras…]`.
    children: Vec<Element<'a, M>>,
    align_x: Alignment,
    align_y: Alignment,
}

impl<'a, M: 'a> Reserve<'a, M> {
    /// Ranura medida para la mayor de `samples`, con `content` (o vacía) a la izquierda y centrada
    /// en alto.
    pub fn new(
        samples: impl IntoIterator<Item = Element<'a, M>>,
        content: Option<Element<'a, M>>,
    ) -> Self {
        let mut children = vec![content.unwrap_or_else(|| Space::new().into())];
        children.extend(samples);
        Self {
            children,
            align_x: Alignment::Start,
            align_y: Alignment::Center,
        }
    }

    /// Alineación horizontal de lo mostrado (los números, a la derecha).
    pub fn align_x(mut self, align: impl Into<Horizontal>) -> Self {
        self.align_x = align.into().into();
        self
    }

    /// Alineación vertical de lo mostrado.
    pub fn align_y(mut self, align: impl Into<Vertical>) -> Self {
        self.align_y = align.into().into();
        self
    }
}

impl<M> iced_core::Widget<M, Theme, Renderer> for Reserve<'_, M> {
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let loose = limits.loose();
        let mut nodes: Vec<layout::Node> = self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .map(|(child, tree)| child.as_widget_mut().layout(tree, renderer, &loose))
            .collect();
        let intrinsic = nodes
            .iter()
            .fold(Size::ZERO, |size, node| size.max(node.size()));
        let size = limits.resolve(Length::Shrink, Length::Shrink, intrinsic);
        nodes[0].align_mut(self.align_x, self.align_y, size);
        layout::Node::with_children(size, nodes)
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
        if let Some(shown) = layout.children().next() {
            self.children[0].as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                shown,
                cursor,
                viewport,
            );
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        if let Some(shown) = layout.children().next() {
            self.children[0].as_widget_mut().operate(
                &mut tree.children[0],
                shown,
                renderer,
                operation,
            );
        }
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
        if let Some(shown) = layout.children().next() {
            self.children[0].as_widget_mut().update(
                &mut tree.children[0],
                event,
                shown,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
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
        layout
            .children()
            .next()
            .map_or(mouse::Interaction::None, |shown| {
                self.children[0].as_widget().mouse_interaction(
                    &tree.children[0],
                    shown,
                    cursor,
                    viewport,
                    renderer,
                )
            })
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        let shown = layout.children().next()?;
        self.children[0].as_widget_mut().overlay(
            &mut tree.children[0],
            shown,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, M: 'a> From<Reserve<'a, M>> for Element<'a, M> {
    fn from(reserve: Reserve<'a, M>) -> Self {
        Element::new(reserve)
    }
}

// --- Texto en una línea ---------------------------------------------------------------------------

/// Cómo se escribe el texto de [`OneLine`] (tamaño, letra, clase): se usa para lo mostrado y para
/// medir, así que las dos cosas van con la misma letra.
type Style<'a> = Box<dyn Fn(String) -> Text<'a> + 'a>;

/// Texto en una línea: si no cabe en el ancho que le dan, se acorta con «…» y el texto completo
/// sale al pasar el ratón. El corte se mide con el renderizador (el ancho de verdad de cada
/// prueba, con la letra del texto) y se recuerda mientras no cambien el texto ni el ancho.
pub struct OneLine<'a, M> {
    full: String,
    style: Style<'a>,
    width: Length,
    /// Lo que se dibuja: el texto entero, o el cortado con su texto emergente. Se decide al
    /// maquetar, que es cuando se sabe el ancho.
    inner: Option<Element<'a, M>>,
}

/// El último corte medido.
#[derive(Debug, Default)]
struct Cut {
    /// Texto y ancho para los que se midió.
    key: Option<(String, u32)>,
    /// El texto cortado con «…», o `None` si cabe entero.
    shown: Option<String>,
}

impl<'a, M: 'a> OneLine<'a, M> {
    /// `full` escrito con `style`.
    pub fn new(full: impl Into<String>, style: impl Fn(String) -> Text<'a> + 'a) -> Self {
        Self {
            full: full.into(),
            style: Box::new(style),
            width: Length::Shrink,
            inner: None,
        }
    }

    /// Ancho (`Shrink`: el del texto, hasta lo que le dejen; `Fill`: todo lo que le dejen).
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    fn text(&self, content: String) -> Text<'a> {
        (self.style)(content).wrapping(Wrapping::None)
    }

    /// Ancho de `content` en una línea, sin límite.
    fn measure(&self, content: String, renderer: &Renderer, height: f32) -> f32 {
        let mut element: Element<'a, M> = self.text(content).into();
        let mut tree = Tree::new(&element);
        let free = layout::Limits::new(Size::ZERO, Size::new(f32::INFINITY, height));
        element
            .as_widget_mut()
            .layout(&mut tree, renderer, &free)
            .size()
            .width
    }

    /// El texto más largo que cabe en `max` terminado en «…» (o solo «…»).
    fn cut(&self, max: f32, renderer: &Renderer, height: f32) -> Option<String> {
        if !max.is_finite() || self.measure(self.full.clone(), renderer, height) <= max + 0.5 {
            return None;
        }
        let chars: Vec<char> = self.full.chars().collect();
        let with = |kept: usize| {
            let prefix: String = chars[..kept].iter().collect();
            format!("{}…", prefix.trim_end())
        };
        // El mayor `kept` que cabe; con cero, «…» solo.
        let (mut low, mut high) = (0, chars.len().saturating_sub(1));
        while low < high {
            let middle = (low + high).div_ceil(2);
            if self.measure(with(middle), renderer, height) <= max + 0.5 {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        Some(with(low))
    }
}

impl<'a, M: 'a> iced_core::Widget<M, Theme, Renderer> for OneLine<'a, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Cut>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Cut::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::empty()]
    }

    fn diff(&self, _tree: &mut Tree) {
        // Lo mostrado se decide al maquetar: allí se casa con su árbol.
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let limits = limits.width(self.width).height(Length::Shrink);
        let max = limits.max();
        let key = (self.full.clone(), max.width.to_bits());
        let cut = tree.state.downcast_mut::<Cut>();
        if cut.key.as_ref() != Some(&key) {
            let shown = self.cut(max.width, renderer, max.height);
            let cut = tree.state.downcast_mut::<Cut>();
            cut.shown = shown;
            cut.key = Some(key);
        }
        let shown = tree.state.downcast_ref::<Cut>().shown.clone();
        let mut inner: Element<'a, M> = match shown {
            Some(shown) => with_tooltip(self.text(shown), self.full.clone()),
            None => self.text(self.full.clone()).into(),
        };
        if tree.children.is_empty() {
            tree.children.push(Tree::empty());
        }
        tree.children[0].diff(inner.as_widget());
        let node = inner
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, &limits);
        self.inner = Some(inner);
        let size = limits.resolve(self.width, Length::Shrink, node.size());
        layout::Node::with_children(size, vec![node])
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
        if let (Some(inner), Some(shown)) = (&self.inner, layout.children().next()) {
            inner.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                shown,
                cursor,
                viewport,
            );
        }
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
        if let (Some(inner), Some(shown)) = (&mut self.inner, layout.children().next()) {
            inner.as_widget_mut().update(
                &mut tree.children[0],
                event,
                shown,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
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
        match (&self.inner, layout.children().next()) {
            (Some(inner), Some(shown)) => inner.as_widget().mouse_interaction(
                &tree.children[0],
                shown,
                cursor,
                viewport,
                renderer,
            ),
            _ => mouse::Interaction::None,
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        let shown = layout.children().next()?;
        self.inner.as_mut()?.as_widget_mut().overlay(
            &mut tree.children[0],
            shown,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, M: 'a> From<OneLine<'a, M>> for Element<'a, M> {
    fn from(line: OneLine<'a, M>) -> Self {
        Element::new(line)
    }
}
