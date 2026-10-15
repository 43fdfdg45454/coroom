//! Amigos: la lógica de pedir, aceptar, rechazar y quitar amistades.
//!
//! Ya no es una página: la lista, las solicitudes y «Agregar amigo» se dibujan en Inicio
//! ([`super::home`]); aquí queda su estado (el campo de usuario desplegable) y sus mensajes.

use coroom_ui::theme::Tone;

use crate::ui::app::{App, Dialog, Effect, Load};

/// Estado.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// El campo para pedir amistad está desplegado.
    pub adding: bool,
    /// Nombre que se escribe para pedir amistad.
    pub add_name: String,
    /// Error al pedirla.
    pub add_error: Option<String>,
    /// Esperando al servidor.
    pub busy: bool,
}

/// Mensajes.
#[derive(Debug, Clone)]
pub enum Msg {
    /// Despliega o recoge el campo de «Agregar amigo».
    ToggleAdd,
    /// Nombre.
    AddName(String),
    /// Pedir amistad.
    Add,
    /// Aceptar una solicitud.
    Accept(u64),
    /// Rechazar o retirar una solicitud.
    Decline(u64),
    /// Quitar a un amigo (pide confirmación).
    Remove {
        /// Usuario.
        user: u64,
        /// Nombre.
        name: String,
    },
    /// Volver a cargar.
    Reload,
}

/// Reduce un mensaje.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    let state = &mut app.friends_page;
    match msg {
        Msg::ToggleAdd => {
            state.adding = !state.adding;
            // Un error de otra operación (aceptar, rechazar) no se queda bajo el campo.
            state.add_error = None;
            if !state.adding {
                state.add_name.clear();
            }
            Vec::new()
        }
        Msg::AddName(name) => {
            state.add_name = name;
            state.add_error = None;
            Vec::new()
        }
        Msg::Add => {
            let name = state.add_name.trim().to_owned();
            if name.is_empty() {
                state.add_error = Some(t!("home-write-username").to_owned());
                return Vec::new();
            }
            if app
                .account
                .as_ref()
                .is_some_and(|account| account.username.eq_ignore_ascii_case(&name))
            {
                state.add_error = Some(t!("home-thats-you").to_owned());
                return Vec::new();
            }
            state.busy = true;
            vec![Effect::AddFriend(name)]
        }
        Msg::Accept(request) => {
            state.busy = true;
            vec![Effect::AcceptFriend(request)]
        }
        Msg::Decline(request) => {
            state.busy = true;
            vec![Effect::DeclineFriend(request)]
        }
        Msg::Remove { user, name } => {
            app.dialog = Some(Dialog::RemoveFriend { user, name });
            Vec::new()
        }
        Msg::Reload => {
            app.friends = Load::Loading;
            vec![Effect::LoadFriends]
        }
    }
}

/// Un cambio de amistades terminó (lo mira antes que [`App`], que limpia el campo): si fue bien,
/// el campo se recoge; si falló con el campo recogido (aceptar o rechazar), el motivo sale en un
/// aviso, porque bajo el campo no se vería.
pub fn on_changed(app: &mut App, result: &Result<String, String>) {
    match result {
        Ok(_) => app.friends_page.adding = false,
        Err(err) if !app.friends_page.adding => app.toast(Tone::Error, err.clone()),
        Err(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::{Event, Message, tests::app};

    #[test]
    fn adding_friends_validates_and_reloads() {
        let mut app = app();
        app.update(Message::Friends(Msg::ToggleAdd));
        assert!(app.friends_page.adding);
        assert!(app.update(Message::Friends(Msg::Add)).is_empty());
        assert!(app.friends_page.add_error.is_some());
        app.update(Message::Friends(Msg::AddName(" bea ".into())));
        assert_eq!(
            app.update(Message::Friends(Msg::Add)),
            [Effect::AddFriend("bea".into())]
        );
        assert!(app.friends_page.busy);
        let effects = app.update(Message::Event(Event::FriendChanged(Ok(
            "Solicitud enviada a bea".into(),
        ))));
        assert_eq!(effects, [Effect::LoadFriends]);
        assert!(!app.friends_page.busy && app.friends_page.add_name.is_empty());
        assert!(!app.friends_page.adding, "el campo se recoge");
        assert_eq!(app.toasts.len(), 1);
        // Un error al pedir amistad sale bajo el campo desplegado.
        app.update(Message::Friends(Msg::ToggleAdd));
        app.update(Message::Event(Event::FriendChanged(Err(
            "no existe ningún usuario «carla»".into(),
        ))));
        assert!(app.friends_page.add_error.is_some());
        assert_eq!(app.toasts.len(), 1);
    }

    #[test]
    fn errors_with_the_field_folded_go_to_a_toast() {
        let mut app = app();
        app.update(Message::Friends(Msg::Accept(4)));
        app.update(Message::Event(Event::FriendChanged(Err(
            "la solicitud ya no existe".into(),
        ))));
        assert_eq!(app.toasts.len(), 1);
        assert_eq!(app.toasts[0].tone, Tone::Error);
        // Al desplegar el campo, ese error no aparece debajo.
        app.update(Message::Friends(Msg::ToggleAdd));
        assert_eq!(app.friends_page.add_error, None);
        app.update(Message::Friends(Msg::AddName("bea".into())));
        app.update(Message::Friends(Msg::ToggleAdd));
        assert!(!app.friends_page.adding && app.friends_page.add_name.is_empty());
    }

    #[test]
    fn removing_asks_first() {
        let mut app = app();
        let effects = app.update(Message::Friends(Msg::Remove {
            user: 9,
            name: "Bea".into(),
        }));
        assert!(effects.is_empty());
        assert!(matches!(
            app.dialog,
            Some(Dialog::RemoveFriend { user: 9, .. })
        ));
        assert_eq!(
            app.update(Message::ConfirmDialog),
            [Effect::RemoveFriend(9)]
        );
        assert_eq!(app.dialog, None);
    }
}
