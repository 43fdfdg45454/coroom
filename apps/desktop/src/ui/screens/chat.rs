//! Chat de la sala (anfitrión e invitado): panel, burbujas y avisos.
//!
//! Esqueleto: el estado y los mensajes que el resto de la aplicación ya enlaza (atajo
//! `Ctrl+Alt+C`, `Esc`, eventos de la sesión, `Tick`). La pantalla se completa sobre la API del
//! chat de `coroom_session`.

use std::time::Instant;

use coroom_ui::Element;
use coroom_ui::widget as ui;

use crate::ui::app::{App, Effect, Event, Message};

/// Estado del chat.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    /// Panel del chat abierto.
    pub open: bool,
    /// Mensajes sin leer con el panel cerrado.
    pub unread: usize,
    /// Texto que se está escribiendo.
    pub draft: String,
}

/// Mensajes del chat.
#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    /// Abrir o cerrar el panel (`Ctrl+Alt+C`).
    Toggle,
    /// `Esc` con el chat enfocado: devuelve el teclado al juego.
    Escape,
    /// Cambió el texto del borrador.
    Draft(String),
    /// Enviar el borrador.
    Send,
}

impl State {
    /// Si `Esc` lo tiene que recibir el chat (panel abierto con el cuadro de texto).
    pub fn takes_escape(&self) -> bool {
        self.open
    }

    /// Cuándo hace falta el próximo `Tick` (burbujas que caducan).
    pub fn next_tick(&self, _now: Instant) -> Option<Instant> {
        None
    }

    /// Pasa el tiempo.
    pub fn tick(&mut self, _now: Instant) {}
}

/// Reduce un mensaje del chat.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Toggle => {
            app.chat.open = !app.chat.open;
            if app.chat.open {
                app.chat.unread = 0;
            }
            Vec::new()
        }
        Msg::Escape => {
            app.chat.open = false;
            Vec::new()
        }
        Msg::Draft(text) => {
            app.chat.draft = text;
            Vec::new()
        }
        Msg::Send => {
            let text = std::mem::take(&mut app.chat.draft);
            if text.trim().is_empty() {
                return Vec::new();
            }
            vec![Effect::SendChat(text)]
        }
    }
}

/// Mira los eventos de la sesión (mensajes, historial, vaciado) antes que la pantalla.
pub fn on_event(_app: &mut App, _event: &Event) -> Vec<Effect> {
    Vec::new()
}

/// Panel del chat.
pub fn view(app: &App) -> Element<'_, Message> {
    ui::empty_state(
        coroom_ui::icon::Icon::Chat,
        t!("chat-titulo"),
        if app.chat.open {
            t!("chat-aun-sin-mensajes")
        } else {
            ""
        },
        None,
    )
}
