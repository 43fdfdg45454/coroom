//! Todos los componentes se construyen con cualquier tema (las capturas de `apps/desktop`,
//! `tests/shots.rs`, los dibujan de verdad).

use coroom_ui::avatar::{self, AvatarImage, SeatNames, SeatRow, Seated};
use coroom_ui::font::Fonts;
use coroom_ui::icon::Icon;
use coroom_ui::style::{ContainerClass, Surface, image_radius, seat_radius};
use coroom_ui::theme::{Accent, Mode, Tone};
use coroom_ui::tokens::{seat, sign};
use coroom_ui::widget::{self as ui, CodeSize, Kind, NavItem};
use coroom_ui::{Element, Theme};
use iced_widget::container::Catalog as _;

#[derive(Debug, Clone, PartialEq)]
enum Msg {
    Press,
    Text(String),
    Toggle(bool),
    Pick(u32),
}

fn gallery(theme: &Theme) -> Vec<Element<'static, Msg>> {
    let mut all: Vec<Element<'static, Msg>> = vec![
        ui::title("Título").into(),
        ui::heading("Encabezado").into(),
        ui::body("Cuerpo").into(),
        ui::strong("Fuerte").into(),
        ui::secondary("Secundario").into(),
        ui::caption("Leyenda").into(),
        ui::mono("K7Q2MZ", 13.0).into(),
        ui::primary("Entrar", Some(Msg::Press)).into(),
        ui::normal("Cancelar", None).into(),
        ui::flat("Volver", Some(Msg::Press)).into(),
        ui::destructive("Expulsar", Some(Msg::Press)).into(),
        ui::button_with(Kind::Primary, "Compartir", Some(Icon::ShareScreen), None).into(),
        ui::icon_button(Icon::Copy, "Copiar", Some(Msg::Press)),
        ui::with_tooltip(ui::body("?"), "Ayuda"),
        ui::field("Usuario", "ana")
            .placeholder("tu usuario")
            .on_input(Msg::Text)
            .on_submit(Some(Msg::Press))
            .error(Some("Escribe tu nombre de usuario"))
            .hint(Some("Como en el servidor"))
            .into(),
        ui::field("Contraseña", String::from("change-me"))
            .secure(true)
            .monospace()
            .into(),
        ui::switch(true, Some(Msg::Toggle)),
        ui::switch(false, None::<fn(bool) -> Msg>),
        ui::select(vec![30u32, 60, 120], Some(60), Msg::Pick),
        ui::segmented(vec![
            ("Claro".to_owned(), Msg::Press, true),
            ("Oscuro".to_owned(), Msg::Press, false),
        ]),
        ui::avatar(theme, "Bea", 32.0),
        ui::list_row(
            Some(ui::avatar(theme, "Ana", 32.0)),
            "Ana",
            Some("@ana".to_owned()),
            vec![ui::status_badge(Tone::Success, "Conectado")],
        ),
        ui::boxed_list(vec![ui::body("fila").into()]),
        ui::dot(Tone::Warning),
        ui::dot(Tone::Live),
        ui::status_badge(Tone::Live, "en vivo"),
        ui::empty_seat(2, seat::ROW),
        ui::empty_seat_on(3, seat::ROW_SMALL, Surface::Overlay),
        ui::vacant_seat(seat::ROW, Surface::Card),
        ui::vacant_seat(seat::ROW_SMALL, Surface::Overlay),
        ui::code_cells("K7Q2MZ", CodeSize::Large),
        ui::code_cells("k7q 2mz", CodeSize::Small),
        ui::ticket(
            ui::list_row(
                Some(ui::avatar(theme, "Bea", 32.0)),
                "Bea te invita",
                None,
                vec![ui::code_cells("K7Q2MZ", CodeSize::Small)],
            ),
            ui::primary("Entrar", Some(Msg::Press)),
        ),
        ui::ticket_accent(
            ui::strong("Dani pide entrar"),
            ui::primary("Dejar entrar", Some(Msg::Press)),
        ),
        ui::banner(Tone::Info, "Aviso", Some(("Ver", Msg::Press))),
        ui::toast(Tone::Error, "No se pudo"),
        ui::progress("Comprobando…", None, 0.3),
        ui::progress("Subiendo…", Some(0.5), 0.0),
        ui::empty_state(
            Icon::Users,
            "Sin amigos",
            "Añade a alguien por su nombre.",
            None,
        ),
        ui::error_state(
            "No hay red",
            "el servidor no responde",
            Some(("Reintentar", Msg::Press)),
        ),
        ui::skeleton("Cargando…", 3),
        ui::shortcut(&["Ctrl", "Alt", "G"]),
        ui::page_header(
            "Ajustes",
            Some("Se guardan al cambiarlos".to_owned()),
            vec![],
        ),
        ui::card(ui::body("tarjeta")),
        ui::card_button(ui::body("tarjeta pulsable"), Some(Msg::Press)),
        ui::section(
            "Vídeo",
            None,
            vec![ui::pref_row(
                "HDR",
                None,
                ui::switch(false, Some(Msg::Toggle)),
            )],
        ),
        ui::copy_field("a1b2c3", Msg::Press),
        ui::gap(8.0),
        ui::fill(),
    ];
    let sidebar = ui::sidebar(
        ui::heading("coroom").into(),
        vec![NavItem {
            icon: Icon::Home,
            label: "Inicio".to_owned(),
            selected: true,
            on_press: Msg::Press,
            badge: Some("1".to_owned()),
        }],
        ui::body("Ana").into(),
    );
    all.push(sidebar);
    let dialog = ui::dialog(
        "¿Salir?",
        ui::body("Se cierra la sala."),
        vec![ui::normal("Cancelar", Some(Msg::Press)).into()],
    );
    all.push(ui::modal(
        ui::page("galería", ui::body("página")),
        Some(dialog),
        Msg::Press,
        1.0,
        0.0,
    ));
    all.push(ui::with_toasts(
        ui::body("base"),
        vec![ui::toast(Tone::Success, "Hecho")],
    ));
    let picture = AvatarImage::from_rgba_levels(vec![(4, vec![128; 64])]).expect("foto");
    all.push(avatar::avatar_with(theme, "Ana", Some(&picture), 36.0));
    all.push(avatar::ringed(theme, "Ana", None, 36.0, true));
    all.push(avatar::seat_lamp(
        theme,
        "Bea",
        Some(&picture),
        36.0,
        Some(Tone::Success),
        Surface::Card,
    ));
    all.push(avatar::seat_lamp(
        theme,
        "Carla",
        None,
        36.0,
        None,
        Surface::Sidebar,
    ));
    all.push(avatar::ringed_lamp(
        theme,
        "Dani",
        None,
        44.0,
        true,
        Some(Tone::Warning),
        Surface::Window,
    ));
    let people = [
        Seated::new("Ana").picture(Some(&picture)).talking(true),
        Seated::new("Bea").lamp(Some(Tone::Success)),
        Seated::new("Carla").lamp(Some(Tone::Warning)),
    ];
    all.push(avatar::seat_row(theme, &people, SeatRow::default()));
    all.push(avatar::seat_row(
        theme,
        &people,
        SeatRow::default().names(SeatNames::Below).max(2),
    ));
    all.push(avatar::seat_row(
        theme,
        &[],
        SeatRow::overlay().names(SeatNames::Hidden),
    ));
    all
}

#[test]
fn every_component_builds_in_both_modes_and_any_accent() {
    for mode in [Mode::Light, Mode::Dark] {
        for accent in [Accent::Blue, Accent::Orange, Accent::Slate] {
            let theme = Theme::new(mode, accent, Fonts::default());
            assert!(gallery(&theme).len() > 55);
        }
    }
    assert_eq!(ui::initials("Ana Bea"), "AB");
    assert_eq!(ui::initials("bea"), "B");
}

/// La butaca es la silueta de las personas: arriba, un respaldo redondeado (42 % del lado);
/// abajo, una base casi recta (18 %). Ocupada lleva el color de la persona y nada de sombra;
/// libre, solo el contorno.
#[test]
fn the_seat_has_a_rounded_back_and_a_nearly_straight_base() {
    for mode in [Mode::Light, Mode::Dark] {
        let theme = Theme::new(mode, Accent::Blue, Fonts::default());
        for side in [22.0_f32, 36.0, 44.0, 72.0] {
            let (background, text) = theme.palette.avatar("Ana");
            let taken = theme.style(&ContainerClass::Seat {
                background,
                text,
                side,
            });
            let radius = taken.border.radius;
            assert_eq!(radius, seat_radius(side));
            assert!((radius.top_left - 0.42 * side).abs() < 1e-3, "{side}");
            assert!((radius.top_right - 0.42 * side).abs() < 1e-3, "{side}");
            assert!((radius.bottom_left - 0.18 * side).abs() < 1e-3, "{side}");
            assert!((radius.bottom_right - 0.18 * side).abs() < 1e-3, "{side}");
            assert!(radius.top_left < side / 2.0, "no es un círculo");
            assert_eq!(
                taken.background,
                Some(iced_core::Background::Color(background.color()))
            );
            assert_eq!(taken.text_color, Some(text.color()));
            assert_eq!(taken.border.width, 0.0);
            assert_eq!(taken.shadow, iced_core::Shadow::default());

            for on in [Surface::Card, Surface::Overlay] {
                let free = theme.style(&ContainerClass::SeatEmpty { side, on });
                assert_eq!(free.border.radius, radius, "la libre, el mismo contorno");
                assert_eq!(free.background, None, "la libre va sin relleno");
                assert_eq!(free.border.width, seat::OUTLINE);
                assert!(free.border.color.a > 0.0);
            }

            // El anillo de quien habla sigue la silueta a su distancia.
            let ring = theme.style(&ContainerClass::SeatRing {
                tone: Tone::Success,
                side,
                gap: avatar::RING_PADDING,
            });
            assert_eq!(ring.background, None);
            assert_eq!(
                ring.border.radius.top_left,
                radius.top_left + avatar::RING_PADDING
            );
            assert_eq!(
                ring.border.radius.bottom_left,
                radius.bottom_left + avatar::RING_PADDING
            );
            assert_eq!(ring.border.color, theme.palette.success.solid.color());
        }
        // La lámpara: el color del tono con un filete del plano de detrás.
        for (on, plane) in [
            (Surface::Card, theme.palette.card),
            (Surface::Sidebar, theme.palette.sidebar),
            (Surface::Window, theme.palette.window),
            (Surface::Dialog, theme.palette.dialog),
            (Surface::Overlay, theme.palette.overlay),
        ] {
            let lamp = theme.style(&ContainerClass::SeatLamp {
                tone: Tone::Warning,
                on,
            });
            assert_eq!(
                lamp.background,
                Some(iced_core::Background::Color(
                    theme.palette.warning.solid.color()
                ))
            );
            assert_eq!(lamp.border.color, plane.color(), "{on:?}");
            assert_eq!(lamp.border.width, seat::LAMP_GAP);
        }
    }
}

/// Las tarjetas son tonales: relleno y filete, sin sombra en reposo. La sombra queda para lo que
/// flota, y siempre con desplazamiento y desenfoque.
#[test]
fn only_what_floats_casts_a_shadow() {
    for mode in [Mode::Light, Mode::Dark] {
        let theme = Theme::new(mode, Accent::Blue, Fonts::default());
        for class in [
            ContainerClass::Card,
            ContainerClass::CardAccent,
            ContainerClass::Ticket,
            ContainerClass::CodeCell {
                radius: sign::LARGE.radius,
            },
            ContainerClass::Sidebar,
            ContainerClass::Window,
            ContainerClass::CardLive {
                live: 1.0,
                glow: 0.0,
            },
        ] {
            let style = theme.style(&class);
            assert_eq!(style.shadow.color.a, 0.0, "{mode:?} {class:?}: sin sombra");
            assert!(style.background.is_some(), "{mode:?} {class:?}");
        }
        for class in [ContainerClass::Card, ContainerClass::Ticket] {
            let style = theme.style(&class);
            assert_eq!(style.border.width, 1.0, "{class:?}: filete fino");
            assert_eq!(style.border.color, theme.palette.border.color());
        }
        for class in [ContainerClass::Dialog, ContainerClass::Toast] {
            let shadow = theme.style(&class).shadow;
            assert!(shadow.color.a > 0.0, "{mode:?} {class:?}: flota");
            assert!(shadow.offset.y > 0.0, "{mode:?} {class:?}: desplazada");
            assert!(shadow.blur_radius > 0.0, "{mode:?} {class:?}: difuminada");
        }
    }
}

/// El elegido del riel: relleno suave de acento, sin franja en el borde; los demás destinos, sin
/// fondo hasta que pasa el ratón.
#[test]
fn the_chosen_destination_is_a_soft_accent_fill() {
    use coroom_ui::style::ButtonClass;
    use iced_widget::button::{Catalog, Status};
    let style_of = |theme: &Theme, class: ButtonClass, status: Status| {
        <Theme as Catalog>::style(theme, &class, status)
    };
    for mode in [Mode::Light, Mode::Dark] {
        let theme = Theme::new(mode, Accent::Blue, Fonts::default());
        let chosen = style_of(&theme, ButtonClass::Nav { selected: true }, Status::Active);
        assert_eq!(
            chosen.background,
            Some(iced_core::Background::Color(theme.palette.selected.color()))
        );
        assert_eq!(chosen.border.width, 0.0, "sin franja ni borde");
        assert_eq!(chosen.text_color, theme.palette.text.color());
        let other = style_of(&theme, ButtonClass::Nav { selected: false }, Status::Active);
        assert_eq!(other.background, None);
        let hovered = style_of(
            &theme,
            ButtonClass::Nav { selected: false },
            Status::Hovered,
        );
        assert_eq!(
            hovered.background,
            Some(iced_core::Background::Color(
                theme.palette.state.alpha(theme.palette.hover_alpha)
            ))
        );
        // Los planos que no flotan no proyectan sombra ni pulsados ni elegidos.
        for class in [
            ButtonClass::Card,
            ButtonClass::Choice { selected: true },
            ButtonClass::ChoiceLit { lit: 1.0 },
            ButtonClass::SegmentThumb,
            ButtonClass::Nav { selected: true },
        ] {
            for status in [Status::Active, Status::Hovered, Status::Pressed] {
                let style = style_of(&theme, class, status);
                assert_eq!(style.shadow.color.a, 0.0, "{mode:?} {class:?} {status:?}");
            }
        }
    }
}

// --- Píxeles --------------------------------------------------------------------------------------

/// Espera a `future` sin ejecutor: abrir el adaptador gráfico no se queda pendiente de nada.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        std::thread::yield_now();
    }
}

/// Un renderizador sin ventana (en contenedores, `lavapipe`); `None` si no lo hay y la prueba de
/// píxeles se salta, como las capturas de la aplicación. Dos motivos: que no haya adaptador
/// gráfico, o que esta compilación no lleve ningún backend de `wgpu` (los activa `coroom-render`:
/// con `cargo test -p coroom-ui` a solas no están y `wgpu` aborta al crear la instancia; con
/// `--workspace`, como en la CI, o añadiendo `-p coroom-render`, sí).
fn headless() -> Option<coroom_ui::Renderer> {
    use iced_core::renderer::Headless;
    let opened = std::panic::catch_unwind(|| {
        block_on(<coroom_ui::Renderer as Headless>::new(
            iced_core::Font::DEFAULT,
            iced_core::Pixels(14.0),
            None,
        ))
    });
    match opened {
        Ok(Some(renderer)) => Some(renderer),
        Ok(None) => {
            eprintln!("sin prueba de píxeles: no hay adaptador gráfico");
            None
        }
        Err(_) => {
            eprintln!(
                "sin prueba de píxeles: esta compilación no lleva backend de wgpu \
                 (ejecútala con --workspace o con -p coroom-render)"
            );
            None
        }
    }
}

/// Dibuja `element` en un lienzo de `side` × `side` con el fondo de la ventana: los píxeles RGBA.
fn paint(
    renderer: &mut coroom_ui::Renderer,
    theme: &Theme,
    mut element: Element<'_, Msg>,
    side: u32,
) -> Vec<u8> {
    use iced_core::renderer::Headless;
    use iced_core::{Renderer as _, Size, layout, mouse, renderer, widget};

    let size = Size::new(side as f32, side as f32);
    let bounds = iced_core::Rectangle::with_size(size);
    let mut tree = widget::Tree::new(&element);
    let node =
        element
            .as_widget_mut()
            .layout(&mut tree, renderer, &layout::Limits::new(Size::ZERO, size));
    renderer.reset(bounds);
    element.as_widget().draw(
        &tree,
        renderer,
        theme,
        &renderer::Style {
            text_color: theme.text_color(),
        },
        iced_core::Layout::new(&node),
        mouse::Cursor::Unavailable,
        &bounds,
    );
    let pixels = Headless::screenshot(renderer, Size::new(side, side), 1.0, theme.background());
    assert_eq!(pixels.len(), (side * side * 4) as usize);
    pixels
}

/// Lo que se aparta un píxel del fondo del lienzo (el de la esquina de arriba a la izquierda,
/// que queda fuera de cualquier butaca), en el canal que más cambia.
fn off_background(pixels: &[u8]) -> Vec<u8> {
    let background = [pixels[0], pixels[1], pixels[2]];
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| {
            (0..3)
                .map(|channel| pixel[channel].abs_diff(background[channel]))
                .max()
                .unwrap_or(0)
        })
        .collect()
}

/// Qué píxeles no son el fondo: cualquier trazo cuenta (para contornos finos).
fn mask(pixels: &[u8]) -> Vec<bool> {
    off_background(pixels)
        .into_iter()
        .map(|distance| distance > 24)
        .collect()
}

/// Qué píxeles son de una butaca llena: los cubiertos al menos a medias por su relleno (el del
/// píxel `inside`, dentro de la butaca y fuera de las iniciales). Así el suavizado del borde,
/// distinto en un rectángulo y en una imagen, no cuenta.
fn filled_mask(pixels: &[u8], side: usize, inside: (usize, usize)) -> Vec<bool> {
    let distances = off_background(pixels);
    let full = distances[inside.1 * side + inside.0];
    assert!(full > 48, "el relleno se distingue del fondo ({full})");
    distances
        .into_iter()
        .map(|distance| distance > full / 2)
        .collect()
}

/// Píxeles de fondo en cada esquina (cuadros de `corner` de lado): arriba a la izquierda, arriba
/// a la derecha, abajo a la derecha y abajo a la izquierda. Cuanto más redonda la esquina, más.
fn cut_corners(mask: &[bool], side: usize, corner: usize) -> [usize; 4] {
    let outside = |xs: std::ops::Range<usize>, ys: std::ops::Range<usize>| {
        ys.flat_map(|y| xs.clone().map(move |x| (x, y)))
            .filter(|(x, y)| !mask[y * side + x])
            .count()
    };
    [
        outside(0..corner, 0..corner),
        outside(side - corner..side, 0..corner),
        outside(side - corner..side, side - corner..side),
        outside(0..corner, side - corner..side),
    ]
}

/// La butaca con foto tiene la misma silueta que la de iniciales: respaldo redondeado arriba y
/// base casi recta. (El sombreador de imágenes de `iced_wgpu` lee los radios girados media vuelta
/// y la foto salía boca abajo: `image_radius` lo compensa.)
#[test]
fn a_seat_with_a_photo_has_the_same_outline_as_one_with_initials() {
    // Lo que se le pide a la imagen son los radios de la butaca girados media vuelta.
    let wanted = seat_radius(64.0);
    let given = image_radius(wanted);
    assert_eq!(
        (given.top_left, given.top_right),
        (wanted.bottom_right, wanted.bottom_left)
    );
    assert_eq!(
        (given.bottom_right, given.bottom_left),
        (wanted.top_left, wanted.top_right)
    );
    assert_eq!(image_radius(given), wanted);

    let Some(mut renderer) = headless() else {
        return;
    };
    const SIDE: u32 = 64;
    let side = SIDE as usize;
    let corner = (seat::BACK * SIDE as f32).ceil() as usize;
    // Una foto lisa y opaca, de un color que no es el del fondo en ningún modo.
    let level = |side: u32| {
        let pixels = [200u8, 60, 60, 255].repeat((side * side) as usize);
        (side, pixels)
    };
    let picture = AvatarImage::from_rgba_levels(vec![level(128), level(64)]).expect("foto");
    // Un píxel del relleno: a media altura, cerca del borde izquierdo (lejos de las iniciales).
    let fill_at = (side / 10, side / 2);
    for mode in [Mode::Light, Mode::Dark] {
        let theme = Theme::new(mode, Accent::Blue, Fonts::default());
        let initials = filled_mask(
            &paint(
                &mut renderer,
                &theme,
                ui::avatar(&theme, "Ana", SIDE as f32),
                SIDE,
            ),
            side,
            fill_at,
        );
        let photo = filled_mask(
            &paint(
                &mut renderer,
                &theme,
                avatar::avatar_with(&theme, "Ana", Some(&picture), SIDE as f32),
                SIDE,
            ),
            side,
            fill_at,
        );
        for (what, seat_mask) in [("iniciales", &initials), ("foto", &photo)] {
            let [top_left, top_right, bottom_right, bottom_left] =
                cut_corners(seat_mask, side, corner);
            assert!(
                seat_mask[side * side / 2 + side / 2],
                "{mode:?} {what}: la butaca se dibuja"
            );
            // Arriba, redondeada; abajo, casi recta.
            assert!(
                top_left > 3 * bottom_left && top_right > 3 * bottom_right,
                "{mode:?} {what}: arriba {top_left} y {top_right}, abajo {bottom_left} y \
                 {bottom_right} píxeles recortados"
            );
            assert!(bottom_left > 0 && bottom_right > 0, "{mode:?} {what}");
            assert!(
                top_left.abs_diff(top_right) <= 4 && bottom_left.abs_diff(bottom_right) <= 4,
                "{mode:?} {what}: simétrica"
            );
        }
        // Y las dos coinciden, esquina a esquina y píxel a píxel (salvo el suavizado del borde).
        let (a, b) = (
            cut_corners(&initials, side, corner),
            cut_corners(&photo, side, corner),
        );
        // El margen cubre lo que difieren el sombreador de rectángulos y el de imágenes al
        // suavizar el borde (medio píxel a lo largo de la curva); una butaca boca abajo se
        // aparta más de cien píxeles por esquina.
        for (index, (a, b)) in a.iter().zip(b).enumerate() {
            assert!(
                a.abs_diff(b) <= 24 + a / 8,
                "{mode:?}: esquina {index}: iniciales {a}, foto {b}"
            );
        }
        // Píxel a píxel, fuera del centro (donde van las iniciales, que no son silueta).
        let middle = side / 4..side - side / 4;
        let different = (0..side * side)
            .filter(|index| !(middle.contains(&(index % side)) && middle.contains(&(index / side))))
            .filter(|index| initials[*index] != photo[*index])
            .count();
        assert!(
            different <= side * side / 24,
            "{mode:?}: {different} píxeles de silueta distintos"
        );
    }
}

/// La butaca vacante es solo el contorno de una butaca: mismo tamaño y silueta que la libre, sin
/// número ni nada dentro.
#[test]
fn a_vacant_seat_is_an_outline_with_nothing_inside() {
    let theme = Theme::new(Mode::Dark, Accent::Blue, Fonts::default());
    let class = ContainerClass::SeatEmpty {
        side: 64.0,
        on: Surface::Card,
    };
    let style = theme.style(&class);
    assert_eq!(style.background, None);
    assert_eq!(style.border.radius, seat_radius(64.0));

    let Some(mut renderer) = headless() else {
        return;
    };
    const SIDE: u32 = 64;
    let side = SIDE as usize;
    for mode in [Mode::Light, Mode::Dark] {
        let theme = Theme::new(mode, Accent::Blue, Fonts::default());
        let vacant = mask(&paint(
            &mut renderer,
            &theme,
            ui::vacant_seat(SIDE as f32, Surface::Window),
            SIDE,
        ));
        // El contorno está (a media altura, en los dos bordes)...
        let row = side / 2;
        assert!(
            (0..3).any(|x| vacant[row * side + x])
                && (0..3).any(|x| vacant[row * side + side - 1 - x]),
            "{mode:?}: se ve el contorno"
        );
        // ...y dentro no hay nada: ni relleno ni número.
        let inset = 4;
        let inside = (inset..side - inset)
            .flat_map(|y| (inset..side - inset).map(move |x| (x, y)))
            .filter(|(x, y)| {
                // Fuera de los cuadros de las esquinas, por donde pasa el contorno curvo.
                let corner = (seat::BACK * SIDE as f32).ceil() as usize;
                let in_corner =
                    (*x < corner || *x >= side - corner) && (*y < corner || *y >= side - corner);
                !in_corner && vacant[y * side + x]
            })
            .count();
        assert_eq!(inside, 0, "{mode:?}: la vacante va vacía");
        // Con la misma silueta que una butaca ocupada: en cada fila, el borde izquierdo cae
        // en el mismo píxel (uno de margen por el suavizado).
        let taken = mask(&paint(
            &mut renderer,
            &theme,
            ui::avatar(&theme, "Ana", SIDE as f32),
            SIDE,
        ));
        let left_edge = |mask: &[bool], y: usize| (0..side).find(|x| mask[y * side + x]);
        for y in 0..side {
            match (left_edge(&vacant, y), left_edge(&taken, y)) {
                (Some(a), Some(b)) => {
                    assert!(a.abs_diff(b) <= 1, "{mode:?}: fila {y}: {a} y {b}");
                }
                (a, b) => assert!(
                    y < 2 || y >= side - 2,
                    "{mode:?}: fila {y}: vacante {a:?}, ocupada {b:?}"
                ),
            }
        }
    }
}
