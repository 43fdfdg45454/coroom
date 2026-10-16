//! Texto seleccionable de los mensajes del chat (`coroom_ui::selectable`), con eventos de ratón y
//! teclado reales sobre el *widget* dibujado con `iced_wgpu`:
//!
//! - arrastrar selecciona y `Ctrl+C` copia lo seleccionado, también a través de varias líneas;
//! - triple clic selecciona la línea; un clic en otro sitio quita la selección;
//! - un clic sin arrastrar sobre un enlace lo abre; empezar a arrastrar sobre él no;
//! - el botón derecho copia la selección y, sin selección, no publica nada (sigue al padre).
//!
//! Sin adaptador gráfico (ni `lavapipe`) se salta, como el de las capturas. Con
//! `COROOM_SHOTS_DIR` guarda la captura de una selección.

use coroom_desktop::ui::shots::ShotRenderer;
use coroom_ui::Theme;
use coroom_ui::font::Fonts;
use coroom_ui::selectable::selectable;
use coroom_ui::theme::{Accent, Mode};
use iced_core::keyboard::{self, Key, Location, Modifiers, key};
use iced_core::{Event, Point, mouse};

const SIZE: (u32, u32) = (480, 120);

#[derive(Debug, Clone, PartialEq)]
enum Out {
    Copy(String),
    Link(String),
}

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

fn right(at: Point) -> (Event, Point) {
    (
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)),
        at,
    )
}

fn ctrl_c(at: Point) -> (Event, Point) {
    (
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: Key::Character("c".into()),
            modified_key: Key::Character("c".into()),
            physical_key: key::Physical::Code(key::Code::KeyC),
            location: Location::Standard,
            modifiers: Modifiers::CTRL,
            text: None,
            repeat: false,
        }),
        at,
    )
}

fn drag(from: Point, to: Point) -> Vec<(Event, Point)> {
    let middle = Point::new((from.x + to.x) / 2.0, (from.y + to.y) / 2.0);
    vec![
        moved(from),
        press(from),
        moved(middle),
        moved(to),
        release(to),
    ]
}

/// Pasa `events` al texto `content` (con un enlace en `link`) y devuelve lo que publicó.
fn run(
    renderer: &mut ShotRenderer,
    content: &str,
    link: Option<std::ops::Range<usize>>,
    events: &[(Event, Point)],
) -> (Vec<Out>, (u32, u32, Vec<u8>)) {
    let theme = Theme::new(Mode::Dark, Accent::Blue, Fonts::default());
    let links = link.map(|range| (range, "https://coroom.example.com".to_owned()));
    let widget = selectable(content)
        .links(links)
        .size(16)
        .on_copy(Out::Copy)
        .on_link(Out::Link);
    renderer.interact(widget.into(), SIZE, &theme, events)
}

#[test]
fn chat_text_selects_copies_and_opens_links() {
    let mut renderer = match ShotRenderer::new() {
        Ok(renderer) => renderer,
        Err(err) => {
            eprintln!("sin prueba de selección: {err}");
            return;
        }
    };
    let text = "hola mundo, nos vemos en la sala";
    let start = Point::new(1.0, 8.0);

    // Arrastrar un trozo y copiar: un principio del texto, ni vacío ni entero.
    let mut events = drag(start, Point::new(70.0, 8.0));
    events.push(ctrl_c(Point::new(70.0, 8.0)));
    let (out, shot) = run(&mut renderer, text, None, &events);
    let [Out::Copy(copied)] = out.as_slice() else {
        panic!("un solo copiado, no {out:?}");
    };
    assert!(
        !copied.is_empty() && copied.len() < text.len() && text.starts_with(copied.as_str()),
        "copió {copied:?}"
    );
    if let Some(dir) = std::env::var_os("COROOM_SHOTS_DIR") {
        let (width, height, rgba) = shot;
        let file = std::fs::File::create(std::path::Path::new(&dir).join("seleccion-chat.png"))
            .expect("fichero de la captura");
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .expect("cabecera PNG")
            .write_image_data(&rgba)
            .expect("datos PNG");
    }

    // Arrastrar hasta pasado el final, por debajo: todo.
    let mut events = drag(start, Point::new(470.0, 110.0));
    events.push(ctrl_c(start));
    let (out, _) = run(&mut renderer, text, None, &events);
    assert_eq!(out, [Out::Copy(text.to_owned())]);

    // A través de dos líneas: lleva el salto.
    let two = "primera línea\nsegunda línea";
    let mut events = drag(start, Point::new(470.0, 30.0));
    events.push(ctrl_c(start));
    let (out, _) = run(&mut renderer, two, None, &events);
    let [Out::Copy(copied)] = out.as_slice() else {
        panic!("un solo copiado, no {out:?}");
    };
    assert!(copied.starts_with("primera línea\nseg"), "copió {copied:?}");

    // Triple clic: la línea entera, sin el salto.
    let at = Point::new(20.0, 8.0);
    let events = [
        moved(at),
        press(at),
        release(at),
        press(at),
        release(at),
        press(at),
        release(at),
        ctrl_c(at),
    ];
    let (out, _) = run(&mut renderer, two, None, &events);
    assert_eq!(out, [Out::Copy("primera línea".to_owned())]);

    // Un clic en otro sitio quita la selección: Ctrl+C ya no copia nada.
    let mut events = drag(start, Point::new(70.0, 8.0));
    let away = Point::new(200.0, 100.0);
    events.extend([moved(away), press(away), release(away), ctrl_c(away)]);
    let (out, _) = run(&mut renderer, text, None, &events);
    assert_eq!(out, []);

    // Sin selección, ni Ctrl+C ni el botón derecho publican (el derecho sigue al padre).
    let (out, _) = run(
        &mut renderer,
        text,
        None,
        &[moved(at), ctrl_c(at), right(at)],
    );
    assert_eq!(out, []);

    // Con selección, el botón derecho la copia.
    let mut events = drag(start, Point::new(70.0, 8.0));
    events.push(right(Point::new(30.0, 8.0)));
    let (out, _) = run(&mut renderer, text, None, &events);
    assert!(matches!(out.as_slice(), [Out::Copy(copied)] if text.starts_with(copied.as_str())));

    // Un clic sobre el enlace lo abre; empezar a arrastrar sobre él, no (selecciona).
    let linked = "coroom.example.com y algo más";
    let on_link = Point::new(10.0, 8.0);
    let events = [moved(on_link), press(on_link), release(on_link)];
    let (out, _) = run(&mut renderer, linked, Some(0..18), &events);
    assert_eq!(out, [Out::Link("https://coroom.example.com".to_owned())]);
    let mut events = drag(on_link, Point::new(120.0, 8.0));
    events.push(ctrl_c(on_link));
    let (out, _) = run(&mut renderer, linked, Some(0..18), &events);
    assert!(
        matches!(out.as_slice(), [Out::Copy(copied)] if !copied.is_empty() && linked.contains(copied.as_str())),
        "arrastrar sobre el enlace selecciona: {out:?}"
    );
}
