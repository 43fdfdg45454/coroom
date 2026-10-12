//! Todos los componentes se construyen con cualquier tema (las capturas de `apps/desktop`,
//! `tests/shots.rs`, los dibujan de verdad).

use coroom_ui::font::Fonts;
use coroom_ui::icon::Icon;
use coroom_ui::theme::{Accent, Mode, Tone};
use coroom_ui::widget::{self as ui, Kind, NavItem};
use coroom_ui::{Element, Theme};

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
        ui::page(ui::body("página")),
        Some(dialog),
        Msg::Press,
    ));
    all.push(ui::with_toasts(
        ui::body("base"),
        vec![ui::toast(Tone::Success, "Hecho")],
        0.0,
    ));
    all
}

#[test]
fn every_component_builds_in_both_modes_and_any_accent() {
    for mode in [Mode::Light, Mode::Dark] {
        for accent in [Accent::Blue, Accent::Orange, Accent::Slate] {
            let theme = Theme::new(mode, accent, Fonts::default());
            assert!(gallery(&theme).len() > 40);
        }
    }
    assert_eq!(ui::initials("Ana Bea"), "AB");
    assert_eq!(ui::initials("bea"), "B");
}
