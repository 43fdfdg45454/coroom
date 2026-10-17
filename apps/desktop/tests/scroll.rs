//! El scroll de las páginas (`coroom_ui::scroll`): lo que crece arriba no empuja lo que se está
//! mirando, y cada página vuelve a la posición en que se dejó.

use coroom_desktop::ui::shots::ShotRenderer;
use coroom_ui::widget as ui;
use coroom_ui::{Element, Renderer};
use iced_core::widget::Id;
use iced_core::widget::operation::{self, Operation};
use iced_core::{Length, Rectangle, Size, Vector};
use iced_runtime::user_interface::{self, UserInterface};
use iced_widget::{Column, Space, container};

const VIEWPORT: Size = Size::new(600.0, 400.0);

/// Lee o mueve el primer `scrollable` que encuentra.
struct Scroll {
    to: Option<f32>,
    offset: Option<f32>,
}

impl Operation for Scroll {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        if self.offset.is_none() {
            operate(self);
        }
    }

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        _bounds: Rectangle,
        _content_bounds: Rectangle,
        translation: Vector,
        state: &mut dyn operation::Scrollable,
    ) {
        if self.offset.is_some() {
            return;
        }
        self.offset = Some(translation.y);
        if let Some(y) = self.to {
            state.scroll_to(operation::scrollable::AbsoluteOffset {
                x: None,
                y: Some(y),
            });
        }
    }
}

/// Una página `key` con una franja arriba de `banner` px y diez bloques de 120 px.
fn page(key: &'static str, banner: f32) -> Element<'static, ()> {
    let mut column = Column::new()
        .spacing(8)
        .push(Space::new().height(Length::Fixed(banner)));
    for _ in 0..10 {
        column = column.push(container(Space::new()).height(Length::Fixed(120.0)));
    }
    ui::page(key, column)
}

/// Construye un cuadro de `element`, opcionalmente mueve el scroll a `to`, y devuelve la
/// posición que tenía.
fn frame(
    renderer: &mut Renderer,
    cache: user_interface::Cache,
    element: Element<'static, ()>,
    to: Option<f32>,
) -> (f32, user_interface::Cache) {
    let mut interface = UserInterface::build(element, VIEWPORT, cache, renderer);
    let mut scroll = Scroll { to, offset: None };
    interface.operate(renderer, &mut scroll);
    (
        scroll.offset.expect("hay un scrollable"),
        interface.into_cache(),
    )
}

#[test]
fn what_you_look_at_stays_put_and_each_page_keeps_its_place() {
    let mut shots = match ShotRenderer::new() {
        Ok(shots) => shots,
        Err(err) => {
            eprintln!("sin prueba de scroll: {err}");
            return;
        }
    };
    let renderer = shots.renderer();
    let cache = user_interface::Cache::new();
    let (offset, cache) = frame(renderer, cache, page("inicio", 40.0), Some(400.0));
    assert_eq!(offset, 0.0, "abre arriba");
    let (offset, cache) = frame(renderer, cache, page("inicio", 40.0), None);
    assert_eq!(offset, 400.0);

    // La franja de arriba crece 100 px: lo que se miraba sigue en su sitio.
    let (_, cache) = frame(renderer, cache, page("inicio", 140.0), None);
    let (offset, cache) = frame(renderer, cache, page("inicio", 140.0), None);
    assert_eq!(offset, 500.0, "el scroll sigue a lo que se miraba");
    // Y al encogerse, vuelve.
    let (_, cache) = frame(renderer, cache, page("inicio", 40.0), None);
    let (offset, cache) = frame(renderer, cache, page("inicio", 40.0), None);
    assert_eq!(offset, 400.0);

    // Otra página se abre arriba, no a la altura de la anterior.
    let (_, cache) = frame(renderer, cache, page("compartir", 40.0), None);
    let (offset, cache) = frame(renderer, cache, page("compartir", 40.0), Some(120.0));
    assert_eq!(offset, 0.0, "cada página con su scroll");
    let (offset, cache) = frame(renderer, cache, page("compartir", 40.0), None);
    assert_eq!(offset, 120.0);

    // Al volver, cada una donde se dejó.
    let (_, cache) = frame(renderer, cache, page("inicio", 40.0), None);
    let (offset, cache) = frame(renderer, cache, page("inicio", 40.0), None);
    assert_eq!(offset, 400.0, "Inicio donde se dejó");
    let (_, cache) = frame(renderer, cache, page("compartir", 40.0), None);
    let (offset, _) = frame(renderer, cache, page("compartir", 40.0), None);
    assert_eq!(offset, 120.0, "Compartir donde se dejó");
}
