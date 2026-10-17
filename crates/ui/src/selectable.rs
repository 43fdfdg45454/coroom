//! Texto que se puede seleccionar con el ratón y copiar, con enlaces clicables: el de los mensajes
//! del chat. `iced` 0.14 no lo trae (su `rich_text` solo se lee), así que este *widget* coloca un
//! párrafo por línea del texto y hace él mismo la selección:
//!
//! - arrastrar con el botón izquierdo selecciona; doble clic, la palabra; triple clic, la línea;
//!   mayúsculas y clic amplía la selección;
//! - Ctrl+C o el botón derecho copian lo seleccionado (el derecho, sin selección, sigue al padre);
//! - un clic sin arrastrar sobre un enlace lo abre; un clic en otro sitio quita la selección.
//!
//! La selección se dibuja con el color de selección de los campos de texto ([`Selectable::selection`]).

use std::ops::Range;

use iced_core::Renderer as _;
use iced_core::layout::{self, Layout};
use iced_core::mouse::{self, Click};
use iced_core::text::{
    Alignment, LineHeight, Paragraph, Renderer as _, Shaping, Span, Text, Wrapping,
};
use iced_core::widget::text::Catalog as _;
use iced_core::widget::tree::{self, Tree};
use iced_core::{
    Clipboard, Color, Element, Event, Font, Length, Pixels, Point, Rectangle, Shell, Size, Vector,
    Widget, alignment, keyboard, renderer,
};

use crate::style::TextClass;
use crate::{Renderer, Theme};

type Para = <Renderer as iced_core::text::Renderer>::Paragraph;

/// Texto seleccionable. Se crea con [`selectable`].
pub struct Selectable<'a, Message> {
    content: &'a str,
    links: Vec<(Range<usize>, String)>,
    size: Option<Pixels>,
    font: Option<Font>,
    line_height: LineHeight,
    wrapping: Wrapping,
    width: Length,
    class: TextClass,
    link_color: Option<Color>,
    selection: Option<Color>,
    on_link: Option<Box<dyn Fn(String) -> Message + 'a>>,
    on_copy: Option<Box<dyn Fn(String) -> Message + 'a>>,
}

/// Texto seleccionable con el contenido dado (sin enlaces; se añaden con [`Selectable::links`]).
pub fn selectable<'a, Message>(content: &'a str) -> Selectable<'a, Message> {
    Selectable {
        content,
        links: Vec::new(),
        size: None,
        font: None,
        line_height: LineHeight::default(),
        wrapping: Wrapping::WordOrGlyph,
        width: Length::Fill,
        class: TextClass::Inherit,
        link_color: None,
        selection: None,
        on_link: None,
        on_copy: None,
    }
}

impl<'a, Message> Selectable<'a, Message> {
    /// Trozos del contenido que son enlaces (rango en bytes y dirección), sin solaparse.
    #[must_use]
    pub fn links(mut self, links: impl IntoIterator<Item = (Range<usize>, String)>) -> Self {
        self.links = links
            .into_iter()
            .filter(|(range, _)| {
                range.start < range.end
                    && self.content.is_char_boundary(range.start)
                    && self.content.is_char_boundary(range.end)
                    && range.end <= self.content.len()
            })
            .collect();
        self.links.sort_by_key(|(range, _)| range.start);
        self
    }

    /// Tamaño del texto.
    #[must_use]
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = Some(size.into());
        self
    }

    /// Fuente (por defecto, la del renderizador).
    #[must_use]
    pub fn font(mut self, font: Font) -> Self {
        self.font = Some(font);
        self
    }

    /// Cómo se parten las líneas largas (por defecto, por palabras y si no cabe, por letras).
    #[must_use]
    pub fn wrapping(mut self, wrapping: Wrapping) -> Self {
        self.wrapping = wrapping;
        self
    }

    /// Ancho (por defecto, todo el disponible).
    #[must_use]
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Clase del texto (su color).
    #[must_use]
    pub fn class(mut self, class: TextClass) -> Self {
        self.class = class;
        self
    }

    /// Color de los enlaces (por defecto, el del texto).
    #[must_use]
    pub fn link_color(mut self, color: Color) -> Self {
        self.link_color = Some(color);
        self
    }

    /// Color de fondo de la selección (por defecto, el de los campos de texto sobre la ventana).
    #[must_use]
    pub fn selection(mut self, color: Color) -> Self {
        self.selection = Some(color);
        self
    }

    /// Mensaje al hacer clic en un enlace.
    #[must_use]
    pub fn on_link(mut self, on_link: impl Fn(String) -> Message + 'a) -> Self {
        self.on_link = Some(Box::new(on_link));
        self
    }

    /// Mensaje para copiar lo seleccionado (Ctrl+C o el botón derecho).
    #[must_use]
    pub fn on_copy(mut self, on_copy: impl Fn(String) -> Message + 'a) -> Self {
        self.on_copy = Some(Box::new(on_copy));
        self
    }
}

/// Lo que decide cómo se colocan los párrafos: si cambia, se vuelven a dar forma.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Shape {
    width: f32,
    size: Pixels,
    font: Font,
    line_height: LineHeight,
    wrapping: Wrapping,
    link_color: Option<Color>,
}

/// Un trozo de una línea con el mismo aspecto: texto normal o enlace, seleccionado o no.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Piece {
    /// Rango en bytes en el contenido entero.
    range: Range<usize>,
    /// Índice del enlace en `links`.
    link: Option<usize>,
    selected: bool,
}

/// Una línea del contenido (hasta un salto de línea) con su párrafo ya colocado.
struct Line {
    range: Range<usize>,
    pieces: Vec<Piece>,
    /// Parte seleccionada, relativa al contenido entero, con la que se dio forma al párrafo.
    selected: Option<Range<usize>>,
    paragraph: Para,
    top: f32,
    height: f32,
}

#[derive(Default)]
struct State {
    content: String,
    links: Vec<(Range<usize>, String)>,
    shape: Option<Shape>,
    lines: Vec<Line>,
    /// Donde empezó la selección y donde está ahora (en bytes). Iguales: nada seleccionado.
    anchor: usize,
    head: usize,
    dragging: bool,
    /// Enlace bajo el puntero al pulsar, para abrirlo si se suelta sin arrastrar.
    pressed_link: Option<usize>,
    hovered_link: Option<usize>,
    last_click: Option<Click>,
    modifiers: keyboard::Modifiers,
}

impl State {
    fn selection(&self) -> Option<Range<usize>> {
        ordered(self.anchor, self.head)
    }

    fn selected_text(&self) -> Option<String> {
        self.selection()
            .and_then(|range| self.content.get(range))
            .map(str::to_owned)
    }

    fn select(&mut self, anchor: usize, head: usize) -> bool {
        let changed = (self.anchor, self.head) != (anchor, head);
        self.anchor = anchor;
        self.head = head;
        changed
    }

    /// Da forma a todos los párrafos desde cero (contenido o aspecto nuevos).
    fn rebuild(&mut self, shape: Shape) {
        self.shape = Some(shape);
        let selection = self.selection();
        self.lines = line_ranges(&self.content)
            .into_iter()
            .map(|range| {
                let selected = clip(selection.clone(), &range);
                let pieces = pieces(&range, &self.links, selected.clone());
                let paragraph = shape_line(&self.content, &pieces, shape);
                Line {
                    range,
                    pieces,
                    selected,
                    paragraph,
                    top: 0.0,
                    height: 0.0,
                }
            })
            .collect();
        self.place();
    }

    /// Vuelve a dar forma solo a las líneas cuya parte seleccionada cambió.
    fn reselect(&mut self) {
        let Some(shape) = self.shape else {
            return;
        };
        let selection = self.selection();
        for line in &mut self.lines {
            let selected = clip(selection.clone(), &line.range);
            if selected != line.selected {
                line.pieces = pieces(&line.range, &self.links, selected.clone());
                line.paragraph = shape_line(&self.content, &line.pieces, shape);
                line.selected = selected;
            }
        }
    }

    fn resize(&mut self, shape: Shape) {
        self.shape = Some(shape);
        for line in &mut self.lines {
            line.paragraph.resize(bounds(shape.width));
        }
        self.place();
    }

    fn place(&mut self) {
        let mut top = 0.0;
        for line in &mut self.lines {
            line.top = top;
            line.height = line.paragraph.min_bounds().height;
            top += line.height;
        }
    }

    fn size(&self) -> Size {
        let width = self
            .lines
            .iter()
            .map(|line| line.paragraph.min_bounds().width)
            .fold(0.0, f32::max);
        let height = self.lines.last().map_or(0.0, |line| line.top + line.height);
        Size::new(width, height)
    }

    /// Posición en bytes más cercana a un punto (relativo al *widget*); fuera, el principio o el
    /// final según el lado.
    fn offset_at(&self, point: Point) -> usize {
        if point.y < 0.0 {
            return 0;
        }
        let Some(line) = self
            .lines
            .iter()
            .find(|line| point.y < line.top + line.height)
        else {
            return self.content.len();
        };
        let local = Point::new(
            point.x.max(0.0),
            (point.y - line.top).clamp(0.0, (line.height - 0.5).max(0.0)),
        );
        let len = line.range.len();
        let index = match line.paragraph.hit_test(local) {
            Some(hit) => hit.cursor().min(len),
            None if point.x <= 0.0 => 0,
            None => len,
        };
        floor_boundary(&self.content, line.range.start + index)
    }

    /// Si un punto (relativo al *widget*) cae sobre el texto de alguna línea y no sobre el espacio
    /// libre a su derecha o entre líneas: solo ahí empieza una selección.
    fn over_text(&self, point: Point) -> bool {
        // Medio píxel de margen, para que el borde de una letra cuente como letra.
        const SLACK: f32 = 0.5;
        let Some(line) = self
            .lines
            .iter()
            .find(|line| point.y >= line.top && point.y < line.top + line.height)
        else {
            return false;
        };
        let local = Point::new(point.x, point.y - line.top);
        (0..line.pieces.len())
            .flat_map(|span| line.paragraph.span_bounds(span))
            .any(|rect| rect.expand(SLACK).contains(local))
    }

    /// Enlace bajo un punto (relativo al *widget*).
    fn link_at(&self, point: Point) -> Option<usize> {
        let line = self
            .lines
            .iter()
            .find(|line| (line.top..line.top + line.height).contains(&point.y))?;
        let span = line
            .paragraph
            .hit_span(Point::new(point.x, point.y - line.top))?;
        line.pieces.get(span)?.link
    }
}

fn bounds(width: f32) -> Size {
    Size::new(width, f32::INFINITY)
}

fn shape_line(content: &str, pieces: &[Piece], shape: Shape) -> Para {
    let mut spans: Vec<Span<'_, (), Font>> = pieces
        .iter()
        .map(|piece| {
            let span = Span::new(&content[piece.range.clone()]);
            match (piece.link, shape.link_color) {
                (Some(_), Some(color)) => span.color(color),
                _ => span,
            }
        })
        .collect();
    if spans.is_empty() {
        // Una línea vacía ocupa su alto como cualquier otra.
        spans.push(Span::new(" "));
    }
    Para::with_spans(Text {
        content: spans.as_slice(),
        bounds: bounds(shape.width),
        size: shape.size,
        line_height: shape.line_height,
        font: shape.font,
        align_x: Alignment::Default,
        align_y: alignment::Vertical::Top,
        shaping: Shaping::Advanced,
        wrapping: shape.wrapping,
    })
}

/// Rangos de las líneas del contenido, sin los saltos (`\n`, y el `\r` de `\r\n`).
fn line_ranges(content: &str) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, _) in content.match_indices('\n') {
        let end = if content[..index].ends_with('\r') {
            index - 1
        } else {
            index
        };
        lines.push(start..end);
        start = index + 1;
    }
    lines.push(start..content.len());
    lines
}

/// De menor a mayor; `None` si no hay nada seleccionado.
fn ordered(a: usize, b: usize) -> Option<Range<usize>> {
    (a != b).then(|| a.min(b)..a.max(b))
}

/// La parte de `selection` dentro de `line`, si la hay.
fn clip(selection: Option<Range<usize>>, line: &Range<usize>) -> Option<Range<usize>> {
    let selection = selection?;
    let start = selection.start.max(line.start);
    let end = selection.end.min(line.end);
    (start < end).then_some(start..end)
}

/// Parte una línea en trozos por los bordes de los enlaces y de la selección.
fn pieces(
    line: &Range<usize>,
    links: &[(Range<usize>, String)],
    selected: Option<Range<usize>>,
) -> Vec<Piece> {
    let mut cuts = vec![line.start, line.end];
    for (range, _) in links {
        cuts.extend([range.start, range.end]);
    }
    if let Some(selected) = &selected {
        cuts.extend([selected.start, selected.end]);
    }
    cuts.retain(|cut| line.contains(cut) || *cut == line.end);
    cuts.sort_unstable();
    cuts.dedup();
    cuts.windows(2)
        .map(|pair| {
            let range = pair[0]..pair[1];
            let link = links
                .iter()
                .position(|(link, _)| link.start <= range.start && range.end <= link.end);
            let selected = selected
                .as_ref()
                .is_some_and(|selected| selected.start <= range.start && range.end <= selected.end);
            Piece {
                range,
                link,
                selected,
            }
        })
        .collect()
}

/// La palabra (letras, cifras y `_`) alrededor de una posición; si no hay, el carácter de ahí.
fn word_at(content: &str, offset: usize) -> Range<usize> {
    let offset = floor_boundary(content, offset);
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let start = content[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map_or(offset, |(index, _)| index);
    let end = content[offset..]
        .char_indices()
        .find(|(_, c)| !is_word(*c))
        .map_or(content.len(), |(index, _)| offset + index);
    if start < end {
        start..end
    } else {
        // Entre dos signos o espacios: el carácter siguiente, si hay.
        let next = content[offset..]
            .chars()
            .next()
            .map_or(offset, |c| offset + c.len_utf8());
        offset..next
    }
}

/// La línea (sin el salto) que contiene una posición.
fn line_at(content: &str, offset: usize) -> Range<usize> {
    line_ranges(content)
        .into_iter()
        .find(|line| offset <= line.end)
        .unwrap_or(0..content.len())
}

fn floor_boundary(content: &str, mut offset: usize) -> usize {
    offset = offset.min(content.len());
    while !content.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

impl<Message> Widget<Message, Theme, Renderer> for Selectable<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
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
        let state = tree.state.downcast_mut::<State>();
        layout::sized(limits, self.width, Length::Shrink, |limits| {
            let shape = Shape {
                width: limits.max().width,
                size: self.size.unwrap_or_else(|| renderer.default_size()),
                font: self.font.unwrap_or_else(|| renderer.default_font()),
                line_height: self.line_height,
                wrapping: self.wrapping,
                link_color: self.link_color,
            };
            if state.content != self.content || state.links != self.links {
                // Otro mensaje en este sitio: la selección era del anterior.
                self.content.clone_into(&mut state.content);
                state.links.clone_from(&self.links);
                state.anchor = 0;
                state.head = 0;
                state.dragging = false;
                state.pressed_link = None;
                state.hovered_link = None;
                state.rebuild(shape);
            } else {
                match state.shape {
                    Some(old) if old == shape => {}
                    Some(old)
                        if Shape {
                            width: shape.width,
                            ..old
                        } == shape =>
                    {
                        state.resize(shape)
                    }
                    _ => state.rebuild(shape),
                }
            }
            state.size()
        })
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        defaults: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if !bounds.intersects(viewport) {
            return;
        }
        let state = tree.state.downcast_ref::<State>();
        let color = theme
            .style(&self.class)
            .color
            .unwrap_or(defaults.text_color);
        let selection = self
            .selection
            .unwrap_or_else(|| theme.palette.accent_bg.alpha(0.35));
        let link_color = self.link_color.unwrap_or(color);
        let size = state.shape.map_or(14.0, |shape| shape.size.0);
        let line_height = self.line_height.to_absolute(Pixels(size)).0;
        // Como el subrayado de `rich_text`: un poco por encima de la línea base.
        let underline = size + (line_height - size) / 2.0 - size * 0.08;
        for line in &state.lines {
            let origin = Vector::new(bounds.x, bounds.y + line.top);
            for (index, piece) in line.pieces.iter().enumerate() {
                if !piece.selected && piece.link.is_none() {
                    continue;
                }
                for region in line.paragraph.span_bounds(index) {
                    if piece.selected {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: region + origin,
                                ..renderer::Quad::default()
                            },
                            selection,
                        );
                    }
                    if piece.link.is_some() {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: Rectangle::new(
                                    region.position() + origin + Vector::new(0.0, underline),
                                    Size::new(region.width, 1.0),
                                ),
                                ..renderer::Quad::default()
                            },
                            link_color,
                        );
                    }
                }
            }
            renderer.fill_paragraph(&line.paragraph, Point::ORIGIN + origin, color, *viewport);
        }
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let bounds = layout.bounds();
        let relative = |point: Point| Point::ORIGIN + (point - bounds.position());

        match event {
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let hovered = cursor
                    .position_over(bounds)
                    .and_then(|point| state.link_at(relative(point)));
                if hovered != state.hovered_link {
                    state.hovered_link = hovered;
                    shell.request_redraw();
                }
                if state.dragging
                    && let Some(point) = cursor.position()
                {
                    let head = state.offset_at(relative(point));
                    if state.select(state.anchor, head) {
                        state.reselect();
                        shell.request_redraw();
                    }
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                // La selección solo empieza sobre el texto (o ampliando una con mayúsculas): un clic
                // en el hueco a la derecha de una línea corta es como un clic en otro sitio.
                let Some(point) = cursor.position_over(bounds).filter(|point| {
                    state.over_text(relative(*point))
                        || (state.modifiers.shift() && state.selection().is_some())
                }) else {
                    // Clic en otro sitio: se quita la selección (y no se captura el clic).
                    if state.select(0, 0) {
                        state.reselect();
                        shell.request_redraw();
                    }
                    return;
                };
                let offset = state.offset_at(relative(point));
                let click = Click::new(point, mouse::Button::Left, state.last_click);
                state.last_click = Some(click);
                state.pressed_link = None;
                let (anchor, head) = match click.kind() {
                    mouse::click::Kind::Single if state.modifiers.shift() => {
                        state.dragging = true;
                        (state.anchor, offset)
                    }
                    mouse::click::Kind::Single => {
                        state.dragging = true;
                        state.pressed_link = state.link_at(relative(point));
                        (offset, offset)
                    }
                    mouse::click::Kind::Double => {
                        state.dragging = false;
                        let word = word_at(&state.content, offset);
                        (word.start, word.end)
                    }
                    mouse::click::Kind::Triple => {
                        state.dragging = false;
                        let line = line_at(&state.content, offset);
                        (line.start, line.end)
                    }
                };
                if state.select(anchor, head) {
                    state.reselect();
                }
                shell.request_redraw();
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if !state.dragging {
                    return;
                }
                state.dragging = false;
                let pressed = state.pressed_link.take();
                if state.selection().is_none()
                    && let Some(link) = pressed
                    && pressed == state.hovered_link
                    && let (Some(on_link), Some((_, url))) = (&self.on_link, state.links.get(link))
                {
                    shell.publish(on_link(url.clone()));
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) => {
                // Con algo seleccionado, el derecho copia eso y no el mensaje entero del padre.
                if cursor.is_over(bounds)
                    && let (Some(on_copy), Some(text)) = (&self.on_copy, state.selected_text())
                {
                    shell.publish(on_copy(text));
                    shell.capture_event();
                }
            }
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                state.modifiers = *modifiers;
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                ..
            }) => {
                let copy = modifiers.command()
                    && key
                        .to_latin(*physical_key)
                        .is_some_and(|c| c.eq_ignore_ascii_case(&'c'));
                if copy && let (Some(on_copy), Some(text)) = (&self.on_copy, state.selected_text())
                {
                    shell.publish(on_copy(text));
                    shell.capture_event();
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State>();
        if state.dragging {
            mouse::Interaction::Text
        } else if state.hovered_link.is_some() && self.on_link.is_some() {
            mouse::Interaction::Pointer
        } else if cursor
            .position_over(layout.bounds())
            .is_some_and(|point| state.over_text(Point::ORIGIN + (point - layout.position())))
        {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::None
        }
    }
}

impl<'a, Message: 'a> From<Selectable<'a, Message>> for Element<'a, Message, Theme, Renderer> {
    fn from(widget: Selectable<'a, Message>) -> Self {
        Element::new(widget)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_split_at_newlines_without_the_breaks() {
        assert_eq!(line_ranges("hola"), vec![0..4]);
        assert_eq!(line_ranges("a\nbc\r\n\nd"), vec![0..1, 2..4, 6..6, 7..8]);
        assert_eq!(line_ranges("a\n"), vec![0..1, 2..2]);
        assert_eq!(line_ranges(""), vec![0..0]);
    }

    #[test]
    fn selection_is_ordered_and_empty_when_collapsed() {
        assert_eq!(ordered(5, 2), Some(2..5));
        assert_eq!(ordered(2, 5), Some(2..5));
        assert_eq!(ordered(3, 3), None);
    }

    #[test]
    fn selection_is_clipped_to_each_line() {
        assert_eq!(clip(Some(2..9), &(0..4)), Some(2..4));
        assert_eq!(clip(Some(2..9), &(5..12)), Some(5..9));
        assert_eq!(clip(Some(2..4), &(5..12)), None);
        assert_eq!(clip(None, &(0..4)), None);
    }

    #[test]
    fn pieces_split_at_links_and_selection() {
        // "ver https://example.com ya": enlace en 4..23, selección 2..10.
        let links = vec![(4..23, "https://example.com".to_owned())];
        let got = pieces(&(0..26), &links, Some(2..10));
        let want = [
            (0..2, None, false),
            (2..4, None, true),
            (4..10, Some(0), true),
            (10..23, Some(0), false),
            (23..26, None, false),
        ];
        assert_eq!(got.len(), want.len());
        for (piece, (range, link, selected)) in got.iter().zip(want) {
            assert_eq!(piece.range, range);
            assert_eq!(piece.link, link);
            assert_eq!(piece.selected, selected);
        }
    }

    #[test]
    fn pieces_of_a_line_ignore_links_of_other_lines() {
        let links = vec![(10..15, "https://example.com".to_owned())];
        let got = pieces(&(0..5), &links, None);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].range, 0..5);
        assert_eq!(got[0].link, None);
        assert!(pieces(&(6..6), &links, None).is_empty());
    }

    #[test]
    fn double_click_selects_the_word() {
        let text = "¿jugamos ahora_mismo?";
        let jugamos = text.find('j').unwrap();
        assert_eq!(&text[word_at(text, jugamos + 3)], "jugamos");
        assert_eq!(&text[word_at(text, jugamos)], "jugamos");
        assert_eq!(&text[word_at(text, text.find('m').unwrap())], "jugamos");
        assert_eq!(&text[word_at(text, text.find('h').unwrap())], "ahora_mismo");
        // Sobre un signo o un espacio, ese carácter.
        assert_eq!(&text[word_at(text, 0)], "¿");
        assert_eq!(&"a  b"[word_at("a  b", 2)], " ");
        // Justo al final de una palabra, la palabra.
        assert_eq!(&text[word_at(text, text.find(' ').unwrap())], "jugamos");
        // Al final, nada que seleccionar.
        assert_eq!(word_at(text, text.len()), text.len()..text.len());
        // A mitad de un carácter de varios bytes, el de su principio.
        assert_eq!(&text[word_at(text, 1)], "¿");
    }

    #[test]
    fn triple_click_selects_the_line() {
        let text = "uno\ndos tres\ncuatro";
        assert_eq!(&text[line_at(text, 5)], "dos tres");
        assert_eq!(&text[line_at(text, 3)], "uno");
        assert_eq!(&text[line_at(text, text.len())], "cuatro");
    }
}
