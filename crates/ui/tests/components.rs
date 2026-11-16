//! Todos los componentes se construyen con cualquier tema (las capturas de `apps/desktop`,
//! `tests/shots.rs`, los dibujan de verdad).

use coroom_ui::avatar::{self, AvatarImage, SeatNames, SeatRow, Seated};
use coroom_ui::font::Fonts;
use coroom_ui::icon::Icon;
use coroom_ui::style::{ContainerClass, Surface, seat_radius};
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
