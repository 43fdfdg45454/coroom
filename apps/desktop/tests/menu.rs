//! Los desplegables de la interfaz (`coroom_ui::widget::menu`, el «Más» de cada integrante de la
//! sala, y los selectores) eligen al **soltar** el botón sobre una opción, no al pulsarlo:
//!
//! - pulsar sobre una opción no hace nada todavía; soltar sobre ella la elige;
//! - pulsar sobre una opción y soltar fuera del menú no elige nada.
//!
//! Con eventos de ratón reales sobre el *widget* dibujado con `iced_wgpu`; sin adaptador gráfico
//! (ni `lavapipe`) se salta, como el de las capturas.

use coroom_desktop::ui::shots::ShotRenderer;
use coroom_ui::Theme;
use coroom_ui::font::Fonts;
use coroom_ui::theme::{Accent, Mode};
use iced_core::{Event, Point, mouse};

const SIZE: (u32, u32) = (320, 200);
const ACTIONS: [&str; 3] = ["Silenciar", "Expulsar", "Pasar el anfitrión"];

fn moved(at: Point) -> (Event, Point) {
    (Event::Mouse(mouse::Event::CursorMoved { position: at }), at)
}

fn press(at: Point) -> (Event, Point) {
    (
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        at,
    )
}

fn release(at: Point) -> (Event, Point) {
    (
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        at,
    )
}

/// Abre el menú con un clic en su botón y pasa `events`; devuelve las acciones elegidas.
fn run(renderer: &mut ShotRenderer, events: &[(Event, Point)]) -> Vec<String> {
    let theme = Theme::new(Mode::Dark, Accent::Blue, Fonts::default());
    let menu = coroom_ui::widget::menu(
        ACTIONS.iter().map(|action| action.to_string()).collect(),
        "Más",
        |action| action,
    );
    let header = Point::new(15.0, 12.0);
    let mut all = vec![moved(header), press(header), release(header)];
    all.extend_from_slice(events);
    renderer.interact(menu, SIZE, &theme, &all).0
}

#[test]
fn menus_pick_on_release_not_on_press() {
    let mut renderer = match ShotRenderer::new() {
        Ok(renderer) => renderer,
        Err(err) => {
            eprintln!("sin prueba de menús: {err}");
            return;
        }
    };
    // La primera opción, justo debajo del botón.
    let option = Point::new(30.0, 44.0);
    let away = Point::new(300.0, 190.0);

    // Pulsar sobre la opción todavía no elige nada.
    let picked = run(&mut renderer, &[moved(option), press(option)]);
    assert_eq!(picked, Vec::<String>::new(), "eligió al pulsar");

    // Soltar sobre ella, sí.
    let picked = run(
        &mut renderer,
        &[moved(option), press(option), release(option)],
    );
    assert_eq!(
        picked.len(),
        1,
        "soltar sobre una opción la elige: {picked:?}"
    );
    assert!(ACTIONS.contains(&picked[0].as_str()), "{picked:?}");

    // Pulsar sobre la opción y soltar fuera del menú no elige nada.
    let picked = run(
        &mut renderer,
        &[moved(option), press(option), moved(away), release(away)],
    );
    assert_eq!(picked, Vec::<String>::new(), "eligió aunque se soltó fuera");
}

/// Opciones de sobra para que el menú, de 200 px de ancho, no quepa en la ventana y lleve barra
/// de desplazamiento.
fn long_options() -> Vec<String> {
    (1..=30).map(|n| format!("Opción {n}")).collect()
}

/// Abre un selector largo con un clic en su botón y pasa `events`; devuelve lo elegido.
fn run_long(renderer: &mut ShotRenderer, events: &[(Event, Point)]) -> Vec<String> {
    let theme = Theme::new(Mode::Dark, Accent::Blue, Fonts::default());
    let select = coroom_ui::widget::select_fixed(long_options(), None, |option| option, 200.0);
    let header = Point::new(15.0, 12.0);
    let mut all = vec![moved(header), press(header), release(header)];
    all.extend_from_slice(events);
    renderer.interact(select, SIZE, &theme, &all).0
}

#[test]
fn the_scrollbar_of_a_long_menu_scrolls_and_picks_nothing() {
    let mut renderer = match ShotRenderer::new() {
        Ok(renderer) => renderer,
        Err(err) => {
            eprintln!("sin prueba de menús: {err}");
            return;
        }
    };
    // La barra va pegada al borde derecho del menú (200 px de ancho, `Scrollbar::default()` de
    // `iced`: 10 px); abajo del todo, para llevar la lista al final.
    let rail = Point::new(197.0, 195.0);
    let rail_up = Point::new(197.0, 192.0);
    // La primera opción visible, justo debajo del botón.
    let top = Point::new(30.0, 50.0);
    let options = long_options();

    // Pulsar en la barra, arrastrar un poco y soltar no elige nada.
    let picked = run_long(
        &mut renderer,
        &[moved(rail), press(rail), moved(rail_up), release(rail_up)],
    );
    assert_eq!(picked, Vec::<String>::new(), "la barra eligió una opción");

    // Sin tocar la barra, la primera visible es la primera.
    let picked = run_long(&mut renderer, &[moved(top), press(top), release(top)]);
    assert_eq!(picked, vec![options[0].clone()], "{picked:?}");

    // Tras la barra la lista bajó, el arrastre terminó con el soltar (moverse ya no desplaza) y
    // un clic en la primera visible elige una de más abajo.
    let picked = run_long(
        &mut renderer,
        &[
            moved(rail),
            press(rail),
            release(rail),
            moved(top),
            press(top),
            release(top),
        ],
    );
    assert_eq!(picked.len(), 1, "{picked:?}");
    assert_ne!(picked[0], options[0], "la barra no desplazó la lista");
}

#[test]
fn a_menu_closed_with_the_button_held_does_not_pick_on_the_next_open() {
    let mut renderer = match ShotRenderer::new() {
        Ok(renderer) => renderer,
        Err(err) => {
            eprintln!("sin prueba de menús: {err}");
            return;
        }
    };
    let header = Point::new(15.0, 12.0);
    let option = Point::new(30.0, 44.0);
    let away = Point::new(300.0, 190.0);
    let finger = (
        Event::Touch(iced_core::touch::Event::FingerPressed {
            id: iced_core::touch::Finger(0),
            position: away,
        }),
        away,
    );
    let picked = run(
        &mut renderer,
        &[
            // Pulsado sobre una opción, el menú se cierra (un toque fuera) antes de soltar.
            moved(option),
            press(option),
            finger,
            release(away),
            // Se vuelve a abrir y se suelta sobre una opción sin haberla pulsado.
            moved(header),
            press(header),
            moved(option),
            release(option),
        ],
    );
    assert_eq!(
        picked,
        Vec::<String>::new(),
        "la pulsación del menú anterior eligió en el nuevo"
    );
}
