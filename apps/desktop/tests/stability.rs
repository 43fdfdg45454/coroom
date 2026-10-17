//! Nada empuja lo de alrededor (`coroom_ui::slot`, las capas del invitado):
//!
//! - **Ranuras**: los rectángulos de una fila son los mismos con y sin cada insignia, icono o
//!   botón que puede aparecer en ella, y con cada valor de una cifra de ancho fijo; una etiqueta
//!   larga se acorta en una línea en vez de cortar línea.
//! - **Capas**: en el vídeo del invitado, el desplazamiento del panel y el foco del campo del chat
//!   sobreviven a que aparezca o desaparezca cada capa (quién habla, el aviso de reconexión, la
//!   indicación del teclado y el ratón, los botones que dependen de un permiso, lo que se ve sin
//!   nada compartido); en la sala de espera, el de los pasos, a que lleguen las burbujas del chat.
//!
//! Sin adaptador gráfico (ni `lavapipe`) se salta, como el de las capturas.

use std::time::Duration;

use coroom_desktop::ui::app::{App, Event, Message};
use coroom_desktop::ui::screens::{chat, guest};
use coroom_desktop::ui::shots::{ShotRenderer, scenes};
use coroom_session::latency::AudioSummary;
use coroom_session::remote::{ChatAuthor, ChatLine, RoomMember, SessionEvent};
use coroom_ui::theme::Tone;
use coroom_ui::widget as ui;
use coroom_ui::{Element, Renderer};
use iced_core::widget::Id;
use iced_core::widget::operation::{self, Operation};
use iced_core::{Length, Rectangle, Size, Vector};
use iced_runtime::user_interface::{Cache, UserInterface};
use iced_widget::{container, row};

/// Un viewport bajo: el panel y los pasos tienen que poder desplazarse.
const VIEWPORT: Size = Size::new(1100.0, 380.0);

/// Desplazamiento que se pide antes de cambiar las capas (o lo que dé de sí la lista).
const OFFSET: f32 = 60.0;

fn shots() -> Option<ShotRenderer> {
    match ShotRenderer::new() {
        Ok(shots) => Some(shots),
        Err(err) => {
            eprintln!("sin prueba de estabilidad: {err}");
            None
        }
    }
}

// --- Ranuras --------------------------------------------------------------------------------------

/// Los rectángulos de los contenedores con `id` (las piezas de la fila), en orden.
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

fn piece<'a>(id: &'static str, content: impl Into<Element<'a, ()>>) -> Element<'a, ()> {
    container(content).id(Id::new(id)).into()
}

/// Maqueta `element` en un viewport de `width` y devuelve los rectángulos de sus piezas.
fn layout(renderer: &mut Renderer, element: Element<'_, ()>, width: f32) -> Vec<(Id, Rectangle)> {
    let mut interface =
        UserInterface::build(element, Size::new(width, 200.0), Cache::new(), renderer);
    let mut bounds = Bounds::default();
    interface.operate(renderer, &mut bounds);
    bounds.0
}

/// Una fila de integrante: nombre, `middle` (lo que puede aparecer o cambiar), «Más».
fn member_row<'a>(middle: Element<'a, ()>) -> Element<'a, ()> {
    row![
        piece("nombre", ui::strong("Ana")),
        piece("ranura", middle),
        piece("mas", ui::normal("Más", Some(()))),
        ui::fill(),
        piece("final", ui::caption("fin")),
    ]
    .spacing(8)
    .into()
}

fn same_rows(renderer: &mut Renderer, what: &str, variants: Vec<Element<'_, ()>>) {
    let mut first: Option<Vec<(Id, Rectangle)>> = None;
    for (index, variant) in variants.into_iter().enumerate() {
        let bounds = layout(renderer, member_row(variant), 640.0);
        assert_eq!(bounds.len(), 4, "{what}: las cuatro piezas");
        match &first {
            None => {
                let slot = bounds[1].1;
                assert!(
                    slot.width > 0.0 && slot.height > 0.0,
                    "{what}: la ranura ocupa su sitio ({slot:?})"
                );
                first = Some(bounds);
            }
            Some(first) => assert_eq!(&bounds, first, "{what}: la variante {index} mueve la fila"),
        }
    }
}

#[test]
fn rows_keep_their_rectangles_with_and_without_each_slot() {
    let Some(mut shots) = shots() else {
        return;
    };
    let renderer = shots.renderer();

    // «Hablando»: reservada con la insignia de muestra, se vea o no.
    let speaking = || ui::status_badge(Tone::Success, "Hablando");
    same_rows(
        renderer,
        "Hablando",
        vec![
            ui::reserve([speaking()], Some(speaking())).into(),
            ui::reserve([speaking()], None).into(),
        ],
    );

    // La calidad: del ancho de la más ancha de sus variantes, con cualquier nivel y cifra.
    let quality = |tone, label: &str| ui::status_badge(tone, label.to_owned());
    let samples = || {
        [
            quality(Tone::Success, "Excelente · 888 ms"),
            quality(Tone::Success, "Buena · 888 ms"),
            quality(Tone::Warning, "Aceptable · 888 ms"),
            quality(Tone::Error, "Mala · 888 ms"),
        ]
    };
    same_rows(
        renderer,
        "calidad",
        vec![
            ui::reserve(samples(), None).into(),
            ui::reserve(samples(), Some(quality(Tone::Success, "Excelente · 4 ms"))).into(),
            ui::reserve(samples(), Some(quality(Tone::Warning, "Aceptable · 98 ms"))).into(),
            ui::reserve(samples(), Some(quality(Tone::Error, "Mala · 140 ms"))).into(),
        ],
    );

    // Un botón que depende de un permiso: de tamaño fijo, esté o no.
    let button = || ui::icon_button(coroom_ui::icon::Icon::Mic, "Silenciar", Some(()));
    same_rows(
        renderer,
        "botón con permiso",
        vec![
            ui::reserved(36.0, 36.0, Some(button())),
            ui::reserved(36.0, 36.0, None),
        ],
    );

    // Cifras de ancho fijo: latencias, bitrates y cuentas atrás.
    for (what, widest, values) in [
        (
            "latencia",
            &["888 ms"][..],
            &["4 ms", "48 ms", "131 ms", "—"][..],
        ),
        (
            "bitrate",
            &["888 kbit/s", "88,8 Mbit/s", "888 Mbit/s"][..],
            &["850 kbit/s", "8,4 Mbit/s", "120 Mbit/s"][..],
        ),
        ("cuenta atrás", &["8:88"][..], &["1:00", "0:42", "0:07"][..]),
    ] {
        same_rows(
            renderer,
            what,
            values
                .iter()
                .map(|value| ui::figure(ui::secondary, *value, widest))
                .collect(),
        );
    }

    // Píldoras y etiquetas en una línea: una larga en poco sitio mide lo mismo de alto que una
    // corta (se acorta con «…», no corta línea).
    let narrow = |label: &str| -> Element<'_, ()> {
        container(row![
            piece(
                "insignia",
                ui::status_badge(Tone::Neutral, label.to_owned())
            ),
            piece("etiqueta", coroom_ui::tag::later(label.to_owned())),
        ])
        .width(Length::Fixed(120.0))
        .into()
    };
    let short = layout(renderer, narrow("Corto"), 640.0);
    let long = layout(
        renderer,
        narrow("Un texto bastante más largo de lo que cabe en la píldora"),
        640.0,
    );
    assert_eq!(short.len(), 2);
    for ((_, short), (_, long)) in short.iter().zip(&long) {
        assert_eq!(short.height, long.height, "en una línea");
        assert!(long.x + long.width <= 120.5, "no se sale: {long:?}");
    }
}

// --- Capas ----------------------------------------------------------------------------------------

/// Lee (y opcionalmente mueve) el primer `scrollable` y cuenta lo que tiene el foco.
#[derive(Default)]
struct Probe {
    to: Option<f32>,
    offset: Option<f32>,
    focused: usize,
}

impl Operation for Probe {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
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

    fn focusable(
        &mut self,
        _id: Option<&Id>,
        _bounds: Rectangle,
        state: &mut dyn operation::Focusable,
    ) {
        if state.is_focused() {
            self.focused += 1;
        }
    }
}

/// Un cuadro de `app` sobre la caché: aplica `operation` y devuelve la caché.
fn frame(renderer: &mut Renderer, app: &App, cache: Cache, operation: &mut dyn Operation) -> Cache {
    let mut interface = UserInterface::build(app.view(), VIEWPORT, cache, renderer);
    interface.operate(renderer, operation);
    interface.into_cache()
}

fn probe(renderer: &mut Renderer, app: &App, cache: Cache) -> (Probe, Cache) {
    let mut probe = Probe::default();
    let cache = frame(renderer, app, cache, &mut probe);
    (probe, cache)
}

fn state(app: &mut App) -> &mut guest::State {
    app.guest.as_mut().expect("en una sala")
}

fn stats(app: &mut App) -> &mut guest::Stats {
    state(app).stats.as_mut().expect("con estadísticas")
}

/// Quién habla (los `talker_label`); vacío, nadie.
fn speaking(app: &mut App, who: &[&str]) {
    let audio = stats(app)
        .summary
        .audio
        .get_or_insert_with(|| AudioSummary {
            tracks: Vec::new(),
            av: None,
            speaking: Vec::new(),
            status: String::new(),
        });
    audio.speaking = who.iter().map(|name| (*name).to_owned()).collect();
}

/// Las capas que aparecen y desaparecen: qué cambia (para el mensaje), cómo se pone y cómo se
/// quita.
type Toggle = (&'static str, fn(&mut App), fn(&mut App));

const TOGGLES: [Toggle; 5] = [
    (
        "quién habla",
        |app| {
            // Alguien más que uno mismo en la sala (las escenas del panel no siempre lo tienen).
            if state(app).members.is_empty() {
                state(app).members = ["Bea", "Ana", "Carla"]
                    .into_iter()
                    .zip(0..)
                    .map(|(name, guest)| RoomMember {
                        guest,
                        leaf: guest,
                        name: name.to_owned(),
                        user: 0,
                        avatar: None,
                        you: guest == 1,
                    })
                    .collect();
            }
            speaking(app, &["anfitrión", "invitado 2"]);
            assert!(
                !guest::talking_names(app.guest.as_ref().expect("sala")).is_empty(),
                "la capa de quién habla se ve"
            );
        },
        |app| speaking(app, &[]),
    ),
    (
        "aviso de reconexión",
        |app| {
            let at = app.now + Duration::from_secs(3);
            state(app).reconnecting = Some(guest::Reconnecting {
                attempt: 2,
                of: 5,
                at,
                reason: "la red dejó de responder".to_owned(),
            });
        },
        |app| state(app).reconnecting = None,
    ),
    (
        "teclado y ratón",
        |app| stats(app).allowed.keyboard_mouse = true,
        |app| stats(app).allowed.keyboard_mouse = false,
    ),
    (
        // El botón del micrófono de la barra (con el micrófono silenciado, la fila de la voz
        // propia sigue en el panel: lo que cambia es solo la barra).
        "botón del micrófono",
        |app| stats(app).voice = true,
        |app| {
            let stats = stats(app);
            stats.voice = false;
            stats.mic_muted = true;
        },
    ),
    (
        "nada compartido",
        |app| {
            app.update(Message::Event(Event::Guest(SessionEvent::Sharing(false))));
        },
        |app| {
            app.update(Message::Event(Event::Guest(SessionEvent::Sharing(true))));
        },
    ),
];

#[test]
fn the_guest_panel_keeps_its_scroll_and_focus_as_layers_come_and_go() {
    let Some(mut shots) = shots() else {
        return;
    };
    let renderer = shots.renderer();
    let mut scrolled = 0;
    let mut focused = 0;
    for scene in scenes() {
        let name = scene.name.clone();
        if !["14-sala-oscuro", "14-conexion-oscuro", "14-chat-oscuro"].contains(&name.as_str()) {
            continue;
        }
        let mut app = scene.app;
        let chat_tab = name.starts_with("14-chat");
        let (_, cache) = probe(renderer, &app, Cache::new());
        let mut cache = if chat_tab {
            // El foco en el campo del chat, como al escribir.
            let mut focus = operation::focusable::focus(Id::new(chat::INPUT_ID));
            frame(renderer, &app, cache, &mut focus)
        } else {
            let mut scroll = Probe {
                to: Some(OFFSET),
                ..Probe::default()
            };
            frame(renderer, &app, cache, &mut scroll)
        };
        let (before, next) = probe(renderer, &app, cache);
        cache = next;
        if chat_tab {
            assert_eq!(before.focused, 1, "{name}: el campo del chat tiene el foco");
            focused += 1;
        } else {
            // Hasta `OFFSET` o lo que dé de sí el panel.
            assert!(
                before.offset.is_some_and(|offset| offset > 0.0),
                "{name}: el panel se desplaza ({:?})",
                before.offset
            );
            scrolled += 1;
        }
        for (what, on, off) in TOGGLES {
            for (step, change) in [("aparece", on), ("desaparece", off)] {
                change(&mut app);
                let (after, next) = probe(renderer, &app, cache);
                cache = next;
                if chat_tab {
                    assert_eq!(
                        after.focused, 1,
                        "{name}: {what} {step} y el chat pierde el foco"
                    );
                } else {
                    assert_eq!(
                        after.offset, before.offset,
                        "{name}: {what} {step} y el panel vuelve arriba"
                    );
                }
            }
        }
    }
    assert!(scrolled >= 2 && focused >= 1, "faltan escenas del panel");
}

#[test]
fn the_waiting_room_keeps_its_scroll_when_bubbles_arrive() {
    let Some(mut shots) = shots() else {
        return;
    };
    let renderer = shots.renderer();
    let scene = scenes()
        .into_iter()
        .find(|scene| scene.name == "12-estimacion-oscuro")
        .expect("escena de la sala de espera");
    let mut app = scene.app;
    assert!(app.chat.bubbles.is_empty(), "sin burbujas al empezar");
    let (_, cache) = probe(renderer, &app, Cache::new());
    let mut scroll = Probe {
        to: Some(OFFSET),
        ..Probe::default()
    };
    let cache = frame(renderer, &app, cache, &mut scroll);
    let (before, cache) = probe(renderer, &app, cache);
    assert!(
        before.offset.is_some_and(|offset| offset > 0.0),
        "los pasos se desplazan ({:?})",
        before.offset
    );
    app.update(Message::Event(Event::Guest(SessionEvent::ChatMessage(
        ChatLine {
            author: ChatAuthor {
                guest: 0,
                leaf: 0,
                name: "Bea".to_owned(),
            },
            text: "¿me oís?".to_owned(),
            at_unix_ms: 1_000,
            own: false,
        },
    ))));
    assert!(!app.chat.bubbles.is_empty(), "llega una burbuja");
    let (after, _) = probe(renderer, &app, cache);
    assert_eq!(
        after.offset, before.offset,
        "al llegar la burbuja los pasos vuelven arriba"
    );
}
