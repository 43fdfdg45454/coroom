//! El árbol de widgets de `iced` sobrevive a los cambios de estado y de tamaño como en la ventana:
//! cada escena de [`scenes`] se dibuja varias veces conservando la caché entre fotogramas (lo que
//! hace el bucle de la ventana), cambiando el tamaño entre medias, y el chat de la sala se llena,
//! se vacía y se vuelve a escribir. Un árbol que no casa con su widget entra en pánico al medirlo
//! («Downcast on stateless state»), como pasaba al vaciar el chat.

use coroom_desktop::ui::app::{App, Event, Message};
use coroom_desktop::ui::screens::chat;
use coroom_desktop::ui::shots::{SIZE, scenes};
use coroom_render::wgpu::{GpuContext, create_instance};
use coroom_session::remote::{ChatAuthor, ChatLine, SessionEvent};
use iced_core::{Event as UiEvent, Font, Pixels, Point, Size, mouse, renderer, window};
use iced_runtime::user_interface::{Cache, UserInterface};
use iced_wgpu::graphics::Shell;
use iced_wgpu::{Engine, Renderer, wgpu};

/// Tamaños por los que pasa la ventana: el de arranque, pequeño, estrecho y 4K.
const SIZES: [(f32, f32); 4] = [
    (SIZE.0 as f32, SIZE.1 as f32),
    (640.0, 480.0),
    (380.0, 900.0),
    (3840.0, 2160.0),
];

fn renderer() -> Option<Renderer> {
    let gpu = match GpuContext::new(&create_instance(None), None) {
        Ok(gpu) => gpu,
        Err(err) => {
            eprintln!("sin adaptador gráfico, se omite: {err}");
            return None;
        }
    };
    let engine = Engine::new(
        &gpu.adapter,
        gpu.device.clone(),
        gpu.queue.clone(),
        wgpu::TextureFormat::Rgba8Unorm,
        None,
        Shell::headless(),
    );
    Some(Renderer::new(engine, Font::DEFAULT, Pixels(14.0)))
}

/// Un fotograma de la ventana: construye la interfaz sobre la caché, le pasa el ratón y el
/// redibujado, la dibuja y aplica los mensajes que produjo.
fn frame(app: &mut App, renderer: &mut Renderer, size: (f32, f32), cache: Cache) -> Cache {
    let theme = app.theme;
    let mut interface =
        UserInterface::build(app.view(), Size::new(size.0, size.1), cache, renderer);
    let position = Point::new(size.0 * 0.8, size.1 * 0.5);
    let cursor = mouse::Cursor::Available(position);
    let mut messages = Vec::new();
    let mut clipboard = iced_core::clipboard::Null;
    let _ = interface.update(
        &[
            UiEvent::Mouse(mouse::Event::CursorMoved { position }),
            UiEvent::Window(window::Event::RedrawRequested(app.now)),
        ],
        cursor,
        renderer,
        &mut clipboard,
        &mut messages,
    );
    interface.draw(
        renderer,
        &theme,
        &renderer::Style {
            text_color: theme.text_color(),
        },
        cursor,
    );
    let cache = interface.into_cache();
    for message in messages {
        let _ = app.update(message);
    }
    cache
}

fn say(app: &mut App, seq: u64, text: &str) {
    let _ = app.update(Message::Event(Event::Host(SessionEvent::ChatMessage(
        ChatLine {
            author: ChatAuthor {
                guest: 0,
                leaf: 0,
                name: String::new(),
            },
            text: text.to_owned(),
            at_unix_ms: 1_000 + seq,
            own: true,
        },
    ))));
}

#[test]
fn every_scene_survives_resizing_with_the_cache() {
    let Some(mut renderer) = renderer() else {
        return;
    };
    for scene in scenes() {
        let mut app = scene.app;
        let mut cache = Cache::new();
        for size in SIZES.iter().chain(SIZES.iter().rev()) {
            cache = frame(&mut app, &mut renderer, *size, cache);
        }
    }
}

#[test]
fn clearing_the_room_chat_keeps_the_tree() {
    let Some(mut renderer) = renderer() else {
        return;
    };
    let mut rooms = 0;
    for scene in scenes() {
        let mut app = scene.app;
        if !app.host.is_open() {
            continue;
        }
        rooms += 1;
        let size = SIZES[0];
        for (seq, text) in ["hola", "¿se oye?", "https://coroom.example.com"]
            .into_iter()
            .enumerate()
        {
            say(&mut app, seq as u64, text);
        }
        let mut cache = frame(&mut app, &mut renderer, size, Cache::new());
        cache = frame(&mut app, &mut renderer, size, cache);
        let _ = app.update(Message::Chat(chat::Msg::AskClear));
        assert!(app.chat.confirm_clear, "{}: pide confirmación", scene.name);
        cache = frame(&mut app, &mut renderer, size, cache);
        let _ = app.update(Message::Chat(chat::Msg::ConfirmClear));
        cache = frame(&mut app, &mut renderer, size, cache);
        let _ = app.update(Message::Event(Event::Host(SessionEvent::ChatCleared)));
        cache = frame(&mut app, &mut renderer, size, cache);
        say(&mut app, 10, "otra vez");
        cache = frame(&mut app, &mut renderer, size, cache);
        let _ = frame(&mut app, &mut renderer, SIZES[1], cache);
    }
    assert!(rooms > 0, "ninguna escena con la sala abierta");
}
