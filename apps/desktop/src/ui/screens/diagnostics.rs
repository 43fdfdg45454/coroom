//! Diagnóstico: por qué la sala va por el relay, los candidatos probados, la prueba del enlace,
//! la latencia por etapas y «Copiar informe». Se abre encima de cualquier pantalla desde la
//! insignia del servidor, la píldora de latencia o el panel de conexión.
//!
//! Esqueleto: estado abierto/cerrado y los enganches (`on_event` recibe los eventos de las
//! sesiones y de `coroom doctor`).

use coroom_ui::Element;
use coroom_ui::widget as ui;

use crate::ui::app::{App, Effect, Event, Message};

/// Estado del diagnóstico.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    /// Abierto encima de la pantalla.
    pub open: bool,
}

/// Mensajes del diagnóstico.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Msg {
    /// Abrir.
    Open,
    /// Cerrar.
    Close,
    /// Reintentar la conexión directa ya.
    Retry,
    /// Copiar el informe (sin direcciones ni nombres).
    CopyReport,
}

/// Reduce un mensaje.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Open => {
            app.diagnostics.open = true;
            Vec::new()
        }
        Msg::Close => {
            app.diagnostics.open = false;
            Vec::new()
        }
        Msg::Retry => vec![Effect::Reconnect],
        Msg::CopyReport => Vec::new(),
    }
}

/// Mira los eventos de las sesiones y de `coroom doctor` (se acumulan aunque esté cerrado).
pub fn on_event(_app: &mut App, _event: &Event) {}

/// El diagnóstico (contenido del panel que la aplicación pone encima de la pantalla).
pub fn view(_app: &App) -> Element<'_, Message> {
    ui::dialog(
        t!("diagnostics-titulo"),
        ui::body(t!("diagnostics-en-construccion")),
        vec![
            ui::fill(),
            ui::normal(t!("app-cerrar"), Some(Message::Diagnostics(Msg::Close))).into(),
        ],
    )
}

/// Escenas del diagnóstico para las capturas.
pub(crate) mod shots {
    use super::Msg;
    use crate::ui::app::{App, Message};
    use crate::ui::shots::Builder;
    /// El diagnóstico abierto encima de Inicio.
    pub(crate) fn over_home(app: &mut App) {
        crate::ui::screens::home::shots::home(app);
        app.update(Message::Diagnostics(Msg::Open));
    }

    /// Escenas de esta pantalla.
    pub(crate) const BUILDERS: &[(&str, Builder)] = &[("24-diagnostico-panel", over_home)];
}
