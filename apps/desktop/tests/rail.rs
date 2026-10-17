//! El riel no empuja nada: con la sala abierta, en la ventana ancha (iconos y nombres) y en las
//! normales y estrechas (solo iconos), que llegue una petición de entrada o que cambie el micrófono no mueve
//! la página, el riel, su pie ni sus destinos; y cambiar de destino tampoco mueve el riel.
//!
//! Sin adaptador gráfico (ni `lavapipe`) se salta, como el de las capturas.

use coroom_desktop::ui::app::{
    App, Event, Message, PAGE_ID, Page, RAIL_ACCOUNT_ID, RAIL_ID, RAIL_MIC_ID,
};
use coroom_desktop::ui::screens::host;
use coroom_desktop::ui::shots::{NARROW, SIZE, ShotRenderer, WIDE, scenes};
use coroom_ui::Renderer;
use coroom_ui::widget as ui;
use iced_core::widget::Id;
use iced_core::widget::operation::Operation;
use iced_core::{Rectangle, Size};
use iced_runtime::user_interface::{Cache, UserInterface};

/// Los rectángulos de los contenedores con `id`.
#[derive(Default)]
struct Bounds(Vec<(Id, Rectangle)>);

impl Operation for Bounds {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if let Some(id) = id {
            self.0.push((id.clone(), bounds));
        }
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

fn room_scene() -> App {
    scenes()
        .into_iter()
        .find(|scene| scene.name == "09-sala-abierta-claro")
        .expect("escena de la sala abierta")
        .app
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
