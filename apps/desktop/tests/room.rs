//! La sala del anfitrión no empuja nada: las filas de «Personas» tienen siempre la misma altura y
//! el mismo sitio, hable quien hable, llegue lo que llegue de la sesión (latencia, mando,
//! permisos) y haya o no peticiones de entrada (van debajo de la lista). Los desplegables solo se
//! abren cuando el usuario lo pide.
//!
//! Sin adaptador gráfico (ni `lavapipe`) se salta, como el de las capturas.

use coroom_desktop::ui::app::{App, Event, HostVoice, Message};
use coroom_desktop::ui::screens::host::{self, Person};
use coroom_desktop::ui::shots::{NARROW, SIZE, ShotRenderer, WIDE, scenes};
use coroom_session::GuestId;
use coroom_session::remote::{GuestPad, GuestStatus, PadTransport};
use coroom_ui::Renderer;
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

/// Todos en la sala: tú y cada invitado.
fn people(app: &App) -> Vec<Person> {
    let room = app.host.room.as_ref().expect("sala abierta");
    std::iter::once(Person::Me)
        .chain(room.guests.iter().map(|status| Person::Guest(status.guest)))
        .collect()
}

/// Maqueta `app` (con lo que entra o sale ya en su sitio) y devuelve el rectángulo de la cabecera
/// de la fila de cada persona.
fn measure(renderer: &mut Renderer, app: &mut App) -> Vec<(Person, Rectangle)> {
    host::settle(app);
    let mut interface = UserInterface::build(app.view(), app.window, Cache::new(), renderer);
    let mut bounds = Bounds::default();
    interface.operate(renderer, &mut bounds);
    people(app)
        .into_iter()
        .map(|person| {
            let id = host::person_id(person);
            let rect = bounds
                .0
                .iter()
                .find(|(found, _)| *found == id)
                .map(|(_, rect)| *rect)
                .unwrap_or_else(|| panic!("falta la fila de {person:?}"));
            (person, rect)
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

fn guests(app: &App) -> Vec<GuestStatus> {
    app.host.room.as_ref().expect("sala").guests.clone()
}

/// Llega la lista de invitados `list` con quién habla (`talkers`; 0 eres tú).
fn send(app: &mut App, list: Vec<GuestStatus>, talkers: Vec<u32>) {
    app.update(Message::Event(Event::HostGuests(
        list,
        HostVoice {
            enabled: true,
            mic_muted: false,
            talkers,
        },
    )));
}

fn open(app: &App) -> Vec<Person> {
    people(app)
        .into_iter()
        .filter(|person| {
            app.host
                .motion
                .people
                .get(person)
                .is_some_and(host::Fold::is_open)
        })
        .collect()
}

/// Comprueba en `app`, tal como está, que lo que llega no mueve ninguna fila ni abre nada.
fn data_does_not_move_the_rows(shots: &mut ShotRenderer, app: &mut App, case: &str) {
    let opened = open(app);
    let before = measure(shots.renderer(), app);
    let list = guests(app);

    // Habla todo el mundo y después nadie: «Hablando» sale en su ranura.
    let ids: Vec<u32> = std::iter::once(0)
        .chain(list.iter().map(|status| status.guest.0))
        .collect();
    send(app, list.clone(), ids);
    assert_eq!(
        before,
        measure(shots.renderer(), app),
        "{case}: todos hablan"
    );
    send(app, list.clone(), Vec::new());
    assert_eq!(
        before,
        measure(shots.renderer(), app),
        "{case}: nadie habla"
    );

    // Otra latencia (y una sin medir), otro mando con notas, otros permisos.
    let mut changed = list.clone();
    for (index, status) in changed.iter_mut().enumerate() {
        status.rtt_ms = if index == 0 { None } else { Some(312.7) };
        status.permissions.keyboard_mouse = !status.permissions.keyboard_mouse;
        status.permissions.voice = !status.permissions.voice;
        status.input_key = false;
        status.gamepad = Some(GuestPad {
            name: "Mando de ejemplo con un nombre larguísimo de fábrica".to_owned(),
            vendor: 0x1234,
            product: 0x5678,
            transport: PadTransport::XInput,
            raw: false,
            note: Some("los gatillos van como botones".to_owned()),
        });
    }
    send(app, changed, vec![1]);
    assert_eq!(
        before,
        measure(shots.renderer(), app),
        "{case}: datos nuevos"
    );
    assert_eq!(
        opened,
        open(app),
        "{case}: lo que llega no abre ni cierra nada"
    );

    // Llega una petición de entrada (y otra): van en la bandeja, debajo de la lista.
    for (user, name) in [(4, "Dani"), (5, "Eli")] {
        app.update(Message::Event(Event::JoinRequested {
            from_user: user,
            from_name: name.to_owned(),
        }));
    }
    assert_eq!(
        app.host.join_requests.len(),
        2,
        "{case}: las peticiones llegaron"
    );
    assert_eq!(
        before,
        measure(shots.renderer(), app),
        "{case}: con peticiones"
    );
    app.update(Message::Host(host::Msg::DeclineJoin(4)));
    app.update(Message::Host(host::Msg::DeclineJoin(5)));
    assert!(app.host.join_requests.is_empty(), "{case}: contestadas");
    assert_eq!(
        before,
        measure(shots.renderer(), app),
        "{case}: sin peticiones"
    );
    assert_eq!(opened, open(app), "{case}: sigue igual de abierto");

    send(app, list, vec![1]);
    app.toasts.clear();
}

#[test]
fn people_rows_stay_put() {
    let mut shots = match ShotRenderer::new() {
        Ok(shots) => shots,
        Err(err) => {
            eprintln!("sin prueba de la sala: {err}");
            return;
        }
    };
    for (width, height) in [WIDE, SIZE, NARROW] {
        let mut app = room_scene();
        app.window = Size::new(width as f32, height as f32);
        assert!(
            open(&app).is_empty(),
            "{width}: todo cerrado al abrir la sala"
        );
        data_does_not_move_the_rows(&mut shots, &mut app, &format!("{width} cerrado"));

        // Abrir la fila de Bea solo baja a las de después; con ella abierta, lo mismo.
        let closed = measure(shots.renderer(), &mut app);
        app.update(Message::Host(host::Msg::Expand(Person::Guest(GuestId(1)))));
        assert_eq!(open(&app), [Person::Guest(GuestId(1))], "{width}: abierta");
        let expanded = measure(shots.renderer(), &mut app);
        assert_eq!(closed[..2], expanded[..2], "{width}: tú y Bea, en su sitio");
        assert!(
            expanded[2].1.y > closed[2].1.y,
            "{width}: Carla baja con el desplegable"
        );
        assert_eq!(
            closed[2].1.height, expanded[2].1.height,
            "{width}: misma altura"
        );
        data_does_not_move_the_rows(&mut shots, &mut app, &format!("{width} abierto"));

        app.update(Message::Host(host::Msg::Expand(Person::Guest(GuestId(1)))));
        assert!(open(&app).is_empty(), "{width}: cerrada por el usuario");
        assert_eq!(
            closed,
            measure(shots.renderer(), &mut app),
            "{width}: como antes"
        );
    }
}
