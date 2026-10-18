//! El riel no empuja nada: con la sala abierta, en la ventana ancha (iconos y nombres) y en las
//! normales y estrechas (solo iconos), que llegue una petición de entrada o que cambie el micrófono no mueve
//! la página, el riel, su pie ni sus destinos; y cambiar de destino tampoco mueve el riel.
//!
//! Y los iconos y los nombres de los destinos quedan en el mismo sitio en todos los estados: sin
//! sala, con la sala abierta compartiendo o no, y como invitado viendo el vídeo o navegando por las
//! otras páginas sin salir de la sala.
//!
//! Sin adaptador gráfico (ni `lavapipe`) se salta, como el de las capturas.

use coroom_desktop::ui::app::{
    App, Event, Message, PAGE_ID, Page, RAIL_ACCOUNT_ID, RAIL_ID, RAIL_MIC_ID, Screen,
};
use coroom_desktop::ui::screens::host;
use coroom_desktop::ui::shots::{self, NARROW, SIZE, ShotRenderer, WIDE};
use coroom_ui::Renderer;
use coroom_ui::widget as ui;
use iced_core::widget::Id;
use iced_core::widget::operation::Operation;
use iced_core::{Rectangle, Size};
use iced_runtime::user_interface::{Cache, UserInterface};

/// Los rectángulos de los contenedores con `id` y de los textos.
#[derive(Default)]
struct Bounds(Vec<(Id, Rectangle)>, Vec<(String, Rectangle)>);

impl Operation for Bounds {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if let Some(id) = id {
            self.0.push((id.clone(), bounds));
        }
    }

    fn text(&mut self, _id: Option<&Id>, bounds: Rectangle, text: &str) {
        self.1.push((text.to_owned(), bounds));
    }
}

/// Maqueta `app` en su ventana y devuelve el rectángulo de cada pieza que se vigila.
fn measure(renderer: &mut Renderer, app: &App) -> Vec<(&'static str, Rectangle)> {
    let mut interface = UserInterface::build(app.view(), app.window, Cache::new(), renderer);
    let mut bounds = Bounds::default();
    interface.operate(renderer, &mut bounds);
    [PAGE_ID, RAIL_ID, RAIL_MIC_ID, RAIL_ACCOUNT_ID]
        .into_iter()
        .map(|name| {
            let id = Id::new(name);
            let rect = bounds
                .0
                .iter()
                .find(|(found, _)| *found == id)
                .map(|(_, rect)| *rect)
                .unwrap_or_else(|| panic!("falta «{name}» en la interfaz"));
            (name, rect)
        })
        .collect()
}

fn scene(name: &str) -> App {
    shots::scene(name)
        .unwrap_or_else(|| panic!("escena «{name}»"))
        .app
}

fn room_scene() -> App {
    scene("09-sala-abierta-claro")
}

/// Destinos del riel: Inicio, Amigos y Sala; Ajustes y Cuenta.
const DESTINATIONS: usize = 5;

/// Nombres de los destinos y de la marca, tal como se leen en el riel desplegado.
const LABELS: [&str; 6] = ["coroom", "Inicio", "Amigos", "Sala", "Ajustes", "Cuenta"];

/// El riel y, dentro, la caja del icono de cada destino y el nombre de cada uno (desplegado).
fn measure_items(renderer: &mut Renderer, app: &App) -> Vec<(String, Rectangle)> {
    let mut interface = UserInterface::build(app.view(), app.window, Cache::new(), renderer);
    let mut bounds = Bounds::default();
    interface.operate(renderer, &mut bounds);
    let find = |id: Id, name: &str| {
        bounds
            .0
            .iter()
            .find(|(found, _)| *found == id)
            .map(|(_, rect)| *rect)
            .unwrap_or_else(|| panic!("falta «{name}» en la interfaz"))
    };
    let rail = find(Id::new(RAIL_ID), RAIL_ID);
    let mut items = vec![(RAIL_ID.to_owned(), rail)];
    for index in 0..DESTINATIONS {
        let name = format!("icono {index}");
        items.push((name.clone(), find(ui::rail_glyph_id(index), &name)));
    }
    if ui::rail_is_wide(app.window.width) {
        for label in LABELS {
            let rect = bounds
                .1
                .iter()
                .find(|(text, rect)| text == label && rail.contains(rect.center()))
                .map(|(_, rect)| *rect)
                .unwrap_or_else(|| panic!("falta el nombre «{label}» en el riel"));
            items.push((label.to_owned(), rect));
        }
    }
    items
}

#[test]
fn join_requests_and_the_mic_do_not_move_the_rail_or_the_page() {
    let mut shots = match ShotRenderer::new() {
        Ok(shots) => shots,
        Err(err) => {
            eprintln!("sin prueba del riel: {err}");
            return;
        }
    };
    for (width, height) in [WIDE, SIZE, NARROW] {
        let mut app = room_scene();
        app.window = Size::new(width as f32, height as f32);
        assert_eq!(
            ui::rail_is_wide(app.window.width),
            width == WIDE.0,
            "{width}: ancho del riel"
        );
        for page in [Page::Host, Page::Home, Page::Friends] {
            app.update(Message::Navigate(page));
            app.page_change = None;
            let before = measure(shots.renderer(), &app);
            // Llega una petición de entrada: el número sale en su ranura.
            app.update(Message::Event(Event::JoinRequested {
                from_user: 4,
                from_name: "Dani".to_owned(),
            }));
            assert_eq!(app.host.join_requests.len(), 1, "la petición llegó");
            let requested = measure(shots.renderer(), &app);
            assert_eq!(before, requested, "{width} {page:?}: con la petición");
            // Cambia el micrófono: el icono, en su caja.
            let muted = app.host.room.as_ref().expect("sala").voice.mic_muted;
            app.update(Message::Host(host::Msg::SelfMute(!muted)));
            assert_ne!(
                app.host.room.as_ref().expect("sala").voice.mic_muted,
                muted,
                "el micrófono cambió"
            );
            let toggled = measure(shots.renderer(), &app);
            assert_eq!(before, toggled, "{width} {page:?}: con el micrófono");
            // Se va la petición y se deshace el micrófono para el destino siguiente.
            app.host.join_requests.clear();
            app.update(Message::Host(host::Msg::SelfMute(muted)));
            app.toasts.clear();
        }
    }
}

#[test]
fn changing_destination_keeps_the_rail_and_the_room() {
    let mut shots = match ShotRenderer::new() {
        Ok(shots) => shots,
        Err(err) => {
            eprintln!("sin prueba del riel: {err}");
            return;
        }
    };
    let mut app = room_scene();
    let mut first: Option<Vec<(&'static str, Rectangle)>> = None;
    for page in [
        Page::Home,
        Page::Friends,
        Page::Settings,
        Page::Account,
        Page::Host,
    ] {
        app.update(Message::Navigate(page));
        app.page_change = None;
        assert!(app.host.is_open(), "{page:?}: la sala sigue abierta");
        let now = measure(shots.renderer(), &app);
        match &first {
            None => first = Some(now),
            Some(first) => assert_eq!(first, &now, "{page:?}: el riel no se mueve"),
        }
    }
}

/// Cómo queda la aplicación en un estado que se compara.
type Step = fn(&mut App);

#[test]
fn icons_and_labels_stay_put_while_sharing_and_navigating() {
    let mut shots = match ShotRenderer::new() {
        Ok(shots) => shots,
        Err(err) => {
            eprintln!("sin prueba del riel: {err}");
            return;
        }
    };
    // Cada estado: una escena y lo que se hace después (navegar, como en uso real).
    let states: &[(&str, &str, Step)] = &[
        ("sin sala", "05-inicio-claro", |_| {}),
        ("compartiendo", "09-sala-abierta-claro", |_| {}),
        ("compartiendo en Inicio", "09-sala-abierta-claro", |app| {
            app.update(Message::Navigate(Page::Home));
        }),
        ("sala sin compartir", "09-sala-sin-compartir-claro", |_| {}),
        ("invitado conectando", "11-conectando-claro", |_| {}),
        ("invitado con vídeo", "13-video-claro", |_| {}),
        ("invitado en Inicio", "13-video-claro", |app| {
            app.update(Message::Navigate(Page::Home));
        }),
        ("invitado en Amigos", "13-video-claro", |app| {
            app.update(Message::Navigate(Page::Friends));
        }),
        ("invitado en Ajustes", "13-video-claro", |app| {
            app.update(Message::Navigate(Page::Settings));
        }),
        ("invitado de vuelta", "13-video-claro", |app| {
            app.update(Message::Navigate(Page::Home));
            app.update(Message::Navigate(Page::Host));
        }),
    ];
    for (width, height) in [WIDE, SIZE, NARROW] {
        let mut first: Option<(&str, Vec<(String, Rectangle)>)> = None;
        for (state, name, step) in states {
            let mut app = scene(name);
            app.window = Size::new(width as f32, height as f32);
            step(&mut app);
            app.page_change = None;
            app.toasts.clear();
            if state.starts_with("invitado") {
                // Que de verdad se vea el vídeo o se esté fuera de él.
                let away = state.ends_with("Inicio")
                    || state.ends_with("Amigos")
                    || state.ends_with("Ajustes");
                let expected = if away { Screen::Main } else { Screen::Guest };
                assert_eq!(app.screen, expected, "{width} {state}: pantalla");
            }
            let now = measure_items(shots.renderer(), &app);
            match &first {
                None => first = Some((state, now)),
                Some((reference, first)) => {
                    for ((what, was), (_, is)) in first.iter().zip(&now) {
                        assert_eq!(
                            was, is,
                            "{width}: «{what}» se mueve entre «{reference}» y «{state}»"
                        );
                    }
                }
            }
        }
    }
}
