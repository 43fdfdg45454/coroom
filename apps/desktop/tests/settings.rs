//! Ajustes, dibujado con `iced_wgpu` sobre las escenas de las capturas:
//!
//! - **Secciones**: las nueve entradas de la navegación están en su orden, de arriba abajo; un
//!   clic en cada una abre su sección, y cada sección dibuja sus filas (con una fila propia de
//!   lo que le toca según la especificación).
//! - **Filas uniformes**: ninguna fila cambia de rectángulo (ni de alto ni de sitio) al cambiar un
//!   valor, al empezar o parar algo (la prueba del micrófono, cambiar un atajo), al aparecer un
//!   error en su sitio o al llegar una lista de dispositivos nueva.
//!
//! Sin adaptador gráfico (ni `lavapipe`) se salta, como el de las capturas.

use std::time::Duration;

use coroom_desktop::ui::app::{App, AudioDevice, Event, Message};
use coroom_desktop::ui::screens::host;
use coroom_desktop::ui::screens::settings::{
    Capture, Choice, Msg, NAV_ID, ROW_ID, SettingsPage, ToneOption,
};
use coroom_desktop::ui::settings::{
    CodecChoice, MotionChoice, QualityPreset, SoundEvent, ThemeChoice,
};
use coroom_desktop::ui::shots::{SIZE, ShotRenderer, scenes};
use coroom_ui::Renderer;
use iced_core::widget::Id;
use iced_core::widget::operation::Operation;
use iced_core::{Event as UiEvent, Point, Rectangle, Size, mouse};
use iced_runtime::user_interface::{Cache, UserInterface};

fn shots() -> Option<ShotRenderer> {
    match ShotRenderer::new() {
        Ok(shots) => Some(shots),
        Err(err) => {
            eprintln!("sin prueba de Ajustes: {err}");
            None
        }
    }
}

/// La escena `name` (en claro).
fn scene(name: &str) -> App {
    let wanted = format!("{name}-claro");
    scenes()
        .into_iter()
        .find(|scene| scene.name == wanted)
        .unwrap_or_else(|| panic!("no hay escena {wanted}"))
        .app
}

/// Los rectángulos de los contenedores cuyo identificador empieza por `prefix`, en orden.
struct Bounds {
    prefix: &'static str,
    found: Vec<(String, Rectangle)>,
}

impl Operation for Bounds {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        // `Id` no expone su texto: se saca de `Debug` (`Id(Custom("ajuste:Salida"))`).
        let Some(id) = id else { return };
        let debug = format!("{id:?}");
        let (Some(start), Some(end)) = (debug.find('"'), debug.rfind('"')) else {
            return;
        };
        let name = &debug[start + 1..end];
        if let Some(name) = name.strip_prefix(self.prefix) {
            self.found.push((name.to_owned(), bounds));
        }
    }
}

/// Maqueta `app` en la ventana de las capturas y devuelve las piezas con `prefix`.
fn measure(renderer: &mut Renderer, app: &App, prefix: &'static str) -> Vec<(String, Rectangle)> {
    let mut interface = UserInterface::build(
        app.view(),
        Size::new(SIZE.0 as f32, SIZE.1 as f32),
        Cache::new(),
        renderer,
    );
    let mut bounds = Bounds {
        prefix,
        found: Vec::new(),
    };
    interface.operate(renderer, &mut bounds);
    bounds.found
}

fn click(at: Point) -> [(UiEvent, Point); 3] {
    [
        (
            UiEvent::Mouse(mouse::Event::CursorMoved { position: at }),
            at,
        ),
        (
            UiEvent::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            at,
        ),
        (
            UiEvent::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            at,
        ),
    ]
}

/// Una fila que cada sección tiene que tener (lo que la especificación pone en ella).
fn expected_row(page: SettingsPage) -> &'static str {
    match page {
        SettingsPage::Network => "Uso del relay este mes",
        SettingsPage::Audio => "Volumen del juego",
        SettingsPage::Video => "Calidad por defecto",
        SettingsPage::Room => "Los invitados entran callados",
        SettingsPage::Notifications => "Sonidos de la interfaz",
        SettingsPage::Gamepads => "Mando que envías",
        SettingsPage::Shortcuts => "Pulsar para hablar",
        SettingsPage::Appearance => "Animaciones",
        SettingsPage::About => "Buscar versiones nuevas",
    }
}

#[test]
fn every_section_renders_in_its_order() {
    let Some(mut shots) = shots() else {
        return;
    };
    let mut app = scene("16-ajustes-servidor");
    let nav = measure(shots.renderer(), &app, NAV_ID);
    let order: Vec<String> = (0..SettingsPage::ALL.len())
        .map(|i| i.to_string())
        .collect();
    assert_eq!(
        nav.iter().map(|(name, _)| name.clone()).collect::<Vec<_>>(),
        order,
        "una entrada por sección"
    );
    assert!(
        nav.windows(2).all(|pair| pair[0].1.y < pair[1].1.y),
        "de arriba abajo: {nav:?}"
    );
    for (index, page) in SettingsPage::ALL.into_iter().enumerate() {
        // Un clic en su entrada la elige.
        let entry = nav[index].1;
        let theme = app.theme;
        let (messages, _) = shots.interact(app.view(), SIZE, &theme, &click(entry.center()));
        let chosen = messages
            .into_iter()
            .find(|message| matches!(message, Message::Settings(Msg::Page(_))));
        let Some(Message::Settings(Msg::Page(chosen))) = chosen else {
            panic!("{page:?}: el clic en su entrada no la elige");
        };
        assert_eq!(chosen, page);
        app.update(Message::Settings(Msg::Page(chosen)));
        let rows = measure(shots.renderer(), &app, ROW_ID);
        assert!(
            rows.iter().any(|(name, _)| name == expected_row(page)),
            "{page:?} sin «{}»: {:?}",
            expected_row(page),
            rows.iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            rows.iter()
                .all(|(_, bounds)| bounds.width > 0.0 && bounds.height > 0.0),
            "{page:?}: filas sin tamaño"
        );
        // La navegación no se mueve al cambiar de sección.
        assert_eq!(measure(shots.renderer(), &app, NAV_ID), nav, "{page:?}");
    }
}

fn pick<T>(value: T, label: &str) -> Choice<T> {
    Choice {
        value,
        label: label.to_owned(),
    }
}

/// Un cambio que se hace a la aplicación (un mensaje o un evento).
type Change = Box<dyn Fn(&mut App)>;

/// `changes` sobre la escena `name`, uno tras otro: tras cada uno, las filas tienen los mismos
/// rectángulos que antes.
fn steady(shots: &mut ShotRenderer, name: &str, changes: Vec<(&str, Change)>) {
    let mut app = scene(name);
    let before = measure(shots.renderer(), &app, ROW_ID);
    assert!(!before.is_empty(), "{name}: sin filas");
    for (what, change) in changes {
        change(&mut app);
        let after = measure(shots.renderer(), &app, ROW_ID);
        assert_eq!(
            after.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            before.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            "{name}, {what}: las mismas filas"
        );
        for ((row, was), (_, now)) in before.iter().zip(&after) {
            assert_eq!(
                was, now,
                "{name}, {what}: «{row}» se mueve o cambia de alto"
            );
        }
    }
}

fn settings(msg: Msg) -> Change {
    Box::new(move |app| {
        app.update(Message::Settings(msg.clone()));
    })
}

fn video(msg: host::Msg) -> Change {
    Box::new(move |app| {
        app.update(Message::Host(msg.clone()));
    })
}

#[test]
fn no_row_changes_size_when_a_value_changes() {
    let Some(mut shots) = shots() else {
        return;
    };
    steady(
        &mut shots,
        "16-ajustes-servidor",
        vec![
            ("relay forzado", settings(Msg::ForceRelay(true))),
            ("UPnP", settings(Msg::PortMapping(false))),
            ("un puerto que no vale", settings(Msg::Port("80".into()))),
            ("un puerto que vale", settings(Msg::Port("47000".into()))),
        ],
    );
    steady(
        &mut shots,
        "16-ajustes-sin-servidor",
        vec![(
            "el servidor vuelve",
            Box::new(|app: &mut App| {
                app.update(Message::Event(Event::ServerStatus {
                    rtt_ms: Some(30.0),
                    online: true,
                }));
            }),
        )],
    );
    steady(
        &mut shots,
        "17-ajustes-audio",
        vec![
            (
                "salida",
                settings(Msg::Speaker(pick(
                    Some("builtin-speakers".into()),
                    "Altavoces integrados",
                ))),
            ),
            (
                "micrófono",
                settings(Msg::Microphone(pick(None, "Sistema"))),
            ),
            ("volumen de la voz", settings(Msg::MicVolume(150))),
            ("supresión de ruido", settings(Msg::NoiseSuppression(false))),
            ("pulsar para hablar", settings(Msg::PushToTalk(false))),
            ("compartir el micrófono", settings(Msg::HostVoice(false))),
            ("volumen del juego", settings(Msg::GameVolume(0))),
            ("empieza la prueba", settings(Msg::MicTest(true))),
            (
                "nivel de la prueba",
                Box::new(|app: &mut App| {
                    app.update(Message::Event(Event::MicLevel(Ok(0.95))));
                }),
            ),
            ("para la prueba", settings(Msg::MicTest(false))),
            (
                "llega una lista nueva",
                Box::new(|app: &mut App| {
                    app.now += Duration::from_secs(70);
                    app.update(Message::Event(Event::AudioDevices(Ok(vec![
                        AudioDevice {
                            name: "usb-headset".into(),
                            description: "Auriculares USB".into(),
                            input: false,
                            default: true,
                        },
                        AudioDevice {
                            name: "hdmi".into(),
                            description: "Salida HDMI de la tarjeta gráfica de ejemplo".into(),
                            input: false,
                            default: false,
                        },
                    ]))));
                }),
            ),
            (
                "la lista no se puede actualizar",
                Box::new(|app: &mut App| {
                    app.now += Duration::from_secs(3);
                    app.update(Message::Event(Event::AudioDevices(Err(
                        "sin PipeWire".into()
                    ))));
                }),
            ),
        ],
    );
    steady(
        &mut shots,
        "17-ajustes-video",
        vec![
            (
                "perfil",
                video(host::Msg::Preset(QualityPreset::MinLatency)),
            ),
            (
                "códec",
                video(host::Msg::Codec(host::Pick {
                    value: CodecChoice::Av1,
                    label: "AV1".into(),
                })),
            ),
            ("4:4:4", video(host::Msg::Chroma444(true))),
            ("HDR", video(host::Msg::Hdr(true))),
            ("fps", video(host::Msg::Fps(120))),
            ("VSync", settings(Msg::Vsync(false))),
            ("decodificador", settings(Msg::SoftwareDecoder(true))),
        ],
    );
    steady(
        &mut shots,
        "18-ajustes-sala",
        vec![
            ("callados", settings(Msg::GuestsMuted(true))),
            ("quién habla", settings(Msg::TalkingOverlay(false))),
        ],
    );
    steady(
        &mut shots,
        "18-ajustes-notificaciones",
        vec![
            (
                "un tono",
                settings(Msg::Tone(
                    SoundEvent::MemberJoined,
                    pick(ToneOption::Off, ""),
                )),
            ),
            ("volumen", settings(Msg::NotificationVolume(0))),
            (
                "sonidos de la interfaz",
                settings(Msg::InterfaceSounds(true)),
            ),
            ("burbuja", settings(Msg::ChatBubbles(false))),
            ("notificación", settings(Msg::ChatNotifications(false))),
        ],
    );
    steady(
        &mut shots,
        "19-ajustes-mandos",
        vec![
            ("el último que conectes", settings(Msg::PadAuto)),
            ("ranura", settings(Msg::PreferredSlot(pick(2, "Ranura 2")))),
        ],
    );
    steady(
        &mut shots,
        "19-ajustes-atajos",
        vec![
            ("cancelar", settings(Msg::CancelCapture)),
            (
                "cambiar pulsar para hablar",
                settings(Msg::Capture(Capture::PushToTalk)),
            ),
            ("tecla nueva", settings(Msg::KeyCaptured("CapsLock".into()))),
            ("cambiar uno", settings(Msg::Capture(Capture::Shortcut(4)))),
            ("restablecer", settings(Msg::ResetShortcuts)),
        ],
    );
    steady(
        &mut shots,
        "20-ajustes-apariencia",
        vec![
            ("oscuro", settings(Msg::Theme(ThemeChoice::Dark))),
            ("acento", settings(Msg::Accent("purple".into()))),
            ("animaciones", settings(Msg::Motion(MotionChoice::Reduced))),
            (
                "siempre completas",
                settings(Msg::Motion(MotionChoice::Full)),
            ),
        ],
    );
    steady(
        &mut shots,
        "21-ajustes-acerca",
        vec![("sin buscar versiones", settings(Msg::UpdateChecks(false)))],
    );
}
