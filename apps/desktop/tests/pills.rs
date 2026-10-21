//! Lo de dentro de las píldoras (insignias de estado, contadores, etiquetas) va centrado: medido
//! con píxeles de verdad, el margen de la tinta (el punto, las letras, las cifras) hasta el borde
//! de la píldora es el mismo a la izquierda que a la derecha y, con texto sin trazos que suban o
//! bajen de las mayúsculas, arriba que abajo, a escala 1 y 2, con un píxel de tolerancia. También
//! cuando la píldora reserva sitio para una cifra más ancha que la que muestra («● 2 ms» en una
//! píldora de «● 888 ms», o la del servidor tras haber mostrado una medida más ancha): lo que
//! sobra se reparte a los dos lados y no queda al final. Y la del servidor mide lo que muestra,
//! crece con una medida más ancha y no vuelve a estrecharse.
//!
//! Con `COROOM_SHOTS_DIR` deja cada píldora en PNG (`pildora-…`). Sin adaptador gráfico (ni
//! `lavapipe`) se salta, como el de las capturas.

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;
use std::rc::Rc;

use coroom_desktop::ui::app::{App, Message, ServerStatus};
use coroom_desktop::ui::screens::chat;
use coroom_desktop::ui::shots::{self, ShotRenderer};
use coroom_ui::font::Fonts;
use coroom_ui::theme::{Accent, Mode, Tone};
use coroom_ui::widget as ui;
use coroom_ui::{Element, Theme};
use iced_core::widget::Id;
use iced_core::widget::operation::Operation;
use iced_core::{Length, Rectangle, Size};
use iced_runtime::user_interface::{Cache, UserInterface};
use iced_widget::{Space, center, container};

/// Lienzo lógico en el que se dibuja cada píldora, centrada.
const CANVAS: (u32, u32) = (360, 48);

/// Diferencia (en el canal que más cambia) desde la que un píxel ya no es el fondo del lienzo.
const OFF_BACKGROUND: u8 = 3;

/// Diferencia con el relleno de la píldora desde la que un píxel es tinta (letra, cifra, punto).
const INK: u8 = 64;

type Rgba = [u8; 4];

fn diff(a: Rgba, b: Rgba) -> u8 {
    (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0)
}

/// Rectángulo en píxeles físicos, con los bordes incluidos.
#[derive(Debug, Clone, Copy)]
struct Bounds {
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
}

impl Bounds {
    fn grow(bounds: Option<Bounds>, x: u32, y: u32) -> Option<Bounds> {
        Some(match bounds {
            None => Bounds {
                left: x,
                top: y,
                right: x,
                bottom: y,
            },
            Some(b) => Bounds {
                left: b.left.min(x),
                top: b.top.min(y),
                right: b.right.max(x),
                bottom: b.bottom.max(y),
            },
        })
    }
}

/// Márgenes de la tinta dentro de la píldora (izquierda, derecha, arriba, abajo), en píxeles
/// físicos, y los rectángulos de la píldora y de la tinta.
#[derive(Debug)]
struct Margins {
    left: i64,
    right: i64,
    top: i64,
    bottom: i64,
    pill: Bounds,
    ink: Bounds,
}

/// Mide la única píldora del lienzo: la píldora es lo que no es el fondo; su relleno, el color
/// más frecuente dentro; la tinta, lo que se aparta del relleno, buscado solo en el interior (la
/// píldora sin `edge` píxeles de su contorno: el suavizado de las esquinas y el aro, si lo hay).
fn measure(width: u32, height: u32, rgba: &[u8], edge: u32) -> Margins {
    let at = |x: u32, y: u32| -> Rgba {
        let i = ((y * width + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    };
    let background = at(0, 0);
    let inside = |x: u32, y: u32| diff(at(x, y), background) > OFF_BACKGROUND;
    let mut pill = None;
    for y in 0..height {
        for x in 0..width {
            if inside(x, y) {
                pill = Bounds::grow(pill, x, y);
            }
        }
    }
    let pill = pill.expect("hay una píldora en el lienzo");
    // El interior: los píxeles con todo su entorno de `edge` dentro de la píldora.
    let interior = |x: u32, y: u32| {
        let (x0, y0) = (x.saturating_sub(edge), y.saturating_sub(edge));
        let (x1, y1) = ((x + edge).min(width - 1), (y + edge).min(height - 1));
        x >= edge
            && y >= edge
            && x + edge < width
            && y + edge < height
            && (y0..=y1).all(|yy| (x0..=x1).all(|xx| inside(xx, yy)))
    };
    let mut counts = std::collections::HashMap::<Rgba, usize>::new();
    let mut cells = Vec::new();
    for y in pill.top..=pill.bottom {
        for x in pill.left..=pill.right {
            if interior(x, y) {
                *counts.entry(at(x, y)).or_default() += 1;
                cells.push((x, y));
            }
        }
    }
    let fill = counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(color, _)| color)
        .expect("la píldora tiene interior");
    let mut ink = None;
    for (x, y) in cells {
        if diff(at(x, y), fill) > INK {
            ink = Bounds::grow(ink, x, y);
        }
    }
    let ink = ink.expect("la píldora lleva algo dentro");
    Margins {
        left: i64::from(ink.left) - i64::from(pill.left),
        right: i64::from(pill.right) - i64::from(ink.right),
        top: i64::from(ink.top) - i64::from(pill.top),
        bottom: i64::from(pill.bottom) - i64::from(ink.bottom),
        pill,
        ink,
    }
}

fn write_png(name: &str, width: u32, height: u32, rgba: &[u8]) {
    let Some(dir) = std::env::var_os("COROOM_SHOTS_DIR").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).expect("directorio de capturas");
    let file = File::create(dir.join(format!("pildora-{name}.png"))).expect("fichero");
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("cabecera PNG");
    writer.write_image_data(rgba).expect("datos PNG");
}

/// Una píldora que se mide.
struct Case {
    name: &'static str,
    /// Si el texto no tiene trazos por encima de las mayúsculas ni por debajo de la línea base
    /// (cifras, mayúsculas, minúsculas cortas): entonces también se mide el centrado en alto.
    vertical: bool,
    /// Píxeles lógicos del contorno que no cuentan como interior (el aro del contador).
    edge: f32,
    build: Box<dyn Fn() -> Element<'static, Message>>,
}

fn case(
    name: &'static str,
    vertical: bool,
    edge: f32,
    build: impl Fn() -> Element<'static, Message> + 'static,
) -> Case {
    Case {
        name,
        vertical,
        edge,
        build: Box::new(build),
    }
}

/// La aplicación de las capturas con el servidor a `rtt` ms.
fn app_with_rtt(rtt: f64) -> App {
    let mut app = shots::scene("05-inicio-claro")
        .expect("escena de inicio")
        .app;
    app.server_status = ServerStatus {
        rtt_ms: Some(rtt),
        online: Some(true),
    };
    app
}

/// La píldora del servidor de cada aplicación de [`servers`].
type Servers = Rc<Vec<App>>;

/// Aplicaciones con el servidor a 2, 48 y 150 ms. Se montan antes de elegir la fuente de cada
/// pasada: montar una escena vuelve a elegir Inter.
fn servers() -> Servers {
    Rc::new([2.0, 48.0, 150.0].map(app_with_rtt).into())
}

fn cases(servers: &Servers) -> Vec<Case> {
    let server = |index: usize| {
        let servers = Rc::clone(servers);
        move || servers[index].server_badge()
    };
    let line = |value: &str| ui::badge_text(value.to_owned());
    vec![
        case("estado", true, 2.0, || {
            ui::status_badge(Tone::Success, "EN LINEA")
        }),
        case("estado-texto", false, 2.0, || {
            ui::status_badge(Tone::Neutral, "Esperando invitados")
        }),
        // Una cifra estrecha en la píldora de la más ancha (la latencia de una fila de la sala).
        case("latencia-1-cifra", true, 2.0, move || {
            ui::status_badge_steady(Tone::Success, line("2 ms"), [line("888 ms").into()], true)
        }),
        case("latencia-2-cifras", true, 2.0, move || {
            ui::status_badge_steady(Tone::Warning, line("14 ms"), [line("888 ms").into()], true)
        }),
        // El contador del riel y de la bandeja, con una cifra y con dos.
        case("contador-1", true, 1.0, || ui::rail_count("2")),
        case("contador-2", true, 1.0, || ui::rail_count("12")),
        // Las etiquetas de «cuándo aplica».
        case("etiqueta-verde", true, 1.0, || {
            coroom_ui::tag::live("AL MOMENTO")
        }),
        case("etiqueta-gris", true, 1.0, || {
            coroom_ui::tag::later("AL ENTRAR")
        }),
        // El contador de no leídos del chat (con su aro), redondo y con dos cifras. Con un 8, de
        // tinta simétrica: el texto se centra por su avance y un «3» o un «1» tienen la tinta
        // cargada a un lado aunque estén centrados.
        case("no-leidos-1", true, 3.0, || {
            chat::with_unread(Space::new().width(32).height(32), 8, 32.0)
        }),
        case("no-leidos-2", true, 3.0, || {
            chat::with_unread(Space::new().width(32).height(32), 42, 32.0)
        }),
        // La píldora del servidor, arriba a la derecha: el punto y «host · N ms» centrados.
        case("servidor-2-ms", false, 2.0, server(0)),
        case("servidor-48-ms", false, 2.0, server(1)),
        case("servidor-150-ms", false, 2.0, server(2)),
        // La del servidor tras una medida más ancha: [`ShotRenderer::draw`] maqueta dos veces con
        // la misma interfaz; la primera a 150 ms y la segunda a 2 ms, que se dibuja en la píldora
        // que quedó del ancho de 150 ms (`keep_widest`), centrada.
        case("servidor-2-ms-tras-150", false, 2.0, {
            let servers = Rc::clone(servers);
            let calls = std::cell::Cell::new(0_u32);
            move || {
                calls.set(calls.get() + 1);
                let index = if calls.get() % 2 == 1 { 2 } else { 0 };
                servers[index].server_badge()
            }
        }),
    ]
}

/// Fuentes con las que se mide: Inter (la embebida, con las mayúsculas justo en el centro de la
/// línea) y, si está instalada, FreeSans (con las mayúsculas por encima del centro: lo compensa
/// [`ui::cap_centered`]). La fuente es global en el proceso: por eso todo va en un solo test.
const FONTS: [&str; 2] = ["Inter", "FreeSans"];

#[test]
fn pill_content_is_centered_and_the_server_pill_keeps_its_width() {
    let mut shots = match ShotRenderer::new() {
        Ok(shots) => shots,
        Err(err) => {
            eprintln!("sin prueba de las píldoras: {err}");
            return;
        }
    };
    let servers = servers();
    let mut failures = Vec::new();
    for family in FONTS {
        let fonts = Fonts::detect(Some(&format!("{family} 11")));
        if fonts.family != family {
            eprintln!("sin {family}: se mide solo con las demás");
            continue;
        }
        eprintln!(
            "{family}: mayúsculas {:+.4} em del centro",
            coroom_ui::font::cap_drop()
        );
        for mode in [Mode::Light, Mode::Dark] {
            let theme = Theme::new(mode, Accent::Blue, fonts);
            for case in cases(&servers) {
                for scale in [1.0_f32, 2.0] {
                    let build = || -> Element<'static, Message> {
                        center((case.build)())
                            .width(Length::Fill)
                            .height(Length::Fill)
                            .into()
                    };
                    let (width, height, rgba) = shots.draw(build, CANVAS, &theme, scale);
                    let name = format!("{family}-{}-{mode:?}-x{scale}", case.name).to_lowercase();
                    write_png(&name, width, height, &rgba);
                    let edge = (case.edge * scale).ceil() as u32;
                    let m = measure(width, height, &rgba, edge);
                    eprintln!(
                        "{name}: izq {} der {} arriba {} abajo {} (píldora {:?}, tinta {:?})",
                        m.left, m.right, m.top, m.bottom, m.pill, m.ink
                    );
                    if (m.left - m.right).abs() > 1 {
                        failures.push(format!(
                            "{name}: izquierda {} y derecha {}",
                            m.left, m.right
                        ));
                    }
                    if case.vertical && (m.top - m.bottom).abs() > 1 {
                        failures.push(format!("{name}: arriba {} y abajo {}", m.top, m.bottom));
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "contenido descentrado en sus píldoras:\n{}",
        failures.join("\n")
    );

    // La píldora del servidor, con la misma interfaz viva mientras cambia el RTT: mide lo que
    // muestra, crece cuando una medida es más ancha y no vuelve a estrecharse.
    Fonts::detect(Some("Inter 11"));
    let widths: Vec<f32> = {
        let renderer = shots.renderer();
        let mut cache = Cache::new();
        let mut widths = Vec::new();
        for rtt in [2.0, 48.0, 2.0, 150.0, 9.0] {
            let app = app_with_rtt(rtt);
            let element: Element<'_, Message> =
                container(app.server_badge()).id(Id::new(PILL_ID)).into();
            let mut interface =
                UserInterface::build(element, Size::new(400.0, 60.0), cache, renderer);
            let mut found = Found(None);
            interface.operate(renderer, &mut found);
            widths.push(found.0.expect("la píldora del servidor").width);
            cache = interface.into_cache();
        }
        widths
    };
    eprintln!("anchos de la píldora del servidor (2, 48, 2, 150, 9 ms): {widths:?}");
    assert!(
        widths[1] > widths[0],
        "48 ms ensancha la de 2 ms: {widths:?}"
    );
    assert_eq!(
        widths[2], widths[1],
        "al volver a 2 ms no se estrecha: {widths:?}"
    );
    assert!(
        widths[3] > widths[2],
        "150 ms la ensancha otra vez: {widths:?}"
    );
    assert_eq!(
        widths[4], widths[3],
        "y tampoco se estrecha después: {widths:?}"
    );
}

/// Identificador del contenedor de la píldora que se mide.
const PILL_ID: &str = "pildora-servidor";

/// El rectángulo del contenedor [`PILL_ID`].
struct Found(Option<Rectangle>);

impl Operation for Found {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id == Some(&Id::new(PILL_ID)) {
            self.0 = Some(bounds);
        }
    }
}
