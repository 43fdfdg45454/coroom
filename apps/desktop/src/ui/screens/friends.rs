//! Pantalla 3: amigos.
//!
//! Lista con presencia, solicitudes recibidas (aceptar o rechazar) y enviadas (retirar), añadir
//! por nombre de usuario y quitar con confirmación.

use coroom_ui::Element;
use coroom_ui::icon::Icon;
use coroom_ui::theme::Tone;
use coroom_ui::tokens::{size, space};
use coroom_ui::widget::{self as ui, Kind};
use iced_core::{Alignment, Length};
use iced_widget::{Column, column, container, row};

use crate::ui::app::{App, Dialog, Effect, Load, Message};

/// Estado.
#[derive(Debug, Clone, Default)]
pub struct State {
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
        Msg::AddName(name) => {
            state.add_name = name;
            state.add_error = None;
            Vec::new()
        }
        Msg::Add => {
            let name = state.add_name.trim().to_owned();
            if name.is_empty() {
                state.add_error =
                    Some(t!("pantalla-friends-escribe-su-nombre-de-usuario").to_owned());
                return Vec::new();
            }
            if app
                .account
                .as_ref()
                .is_some_and(|account| account.username.eq_ignore_ascii_case(&name))
            {
                state.add_error = Some(t!("pantalla-friends-ese-eres-tu").to_owned());
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

fn on(msg: Msg) -> Message {
    Message::Friends(msg)
}

/// Dibuja la página.
pub fn view(app: &App) -> Element<'_, Message> {
    let state = &app.friends_page;
    let subtitle = app.friends.loaded().map(|friends| {
        let online = friends.friends.iter().filter(|f| f.online).count();
        match friends.friends.len() {
            0 => t!("pantalla-friends-todavia-sin-amigos-en-este-servidor").to_owned(),
            1 => t!("pantalla-friends-1-amigo-online-conectado", online = online),
            total => t!(
                "pantalla-friends-total-amigos-online-conectados",
                total = total,
                online = online
            ),
        }
    });
    let header = ui::page_header(
        t!("pantalla-friends-amigos"),
        subtitle,
        vec![ui::icon_button(
            Icon::Refresh,
            t!("pantalla-friends-volver-a-cargar"),
            Some(on(Msg::Reload)),
        )],
    );
    let add = ui::card(
        column![
            ui::strong(t!("pantalla-friends-anadir-un-amigo")),
            row![
                container(Element::from(
                    ui::field(t!("pantalla-friends-nombre-de-usuario"), &state.add_name)
                        .placeholder("bea")
                        .on_input(|value| on(Msg::AddName(value)))
                        .on_submit((!state.busy).then(|| on(Msg::Add)))
                        .error(state.add_error.as_deref())
                        .id("friend-name")
                ))
                .width(Length::Fill),
                ui::button_with(
                    Kind::Primary,
                    t!("pantalla-friends-enviar-solicitud"),
                    Some(Icon::UserPlus),
                    (!state.busy).then(|| on(Msg::Add))
                )
            ]
            .spacing(space::XS)
            .align_y(Alignment::End),
            ui::caption(t!(
                "pantalla-friends-cuando-acepte-vereis-quien-esta-conectado"
            ))
        ]
        .spacing(space::XS),
    );
    let mut page = Column::new().spacing(space::L).push(header).push(add);
    match &app.friends {
        Load::Idle | Load::Loading => {
            page = page.push(ui::skeleton(t!("pantalla-friends-cargando-tus-amigos"), 3))
        }
        Load::Failed(err) => {
            page = page.push(ui::error_state(
                t!("pantalla-friends-no-se-pudo-cargar-la-lista"),
                err.as_str(),
                Some((t!("pantalla-friends-reintentar"), on(Msg::Reload))),
            ));
        }
        Load::Loaded(friends) => {
            if !friends.incoming.is_empty() {
                let rows = friends
                    .incoming
                    .iter()
                    .map(|request| {
                        ui::list_row(
                            Some(ui::avatar(
                                &app.theme,
                                &request.user.display_name,
                                size::AVATAR,
                            )),
                            request.user.display_name.as_str(),
                            Some(t!(
                                "pantalla-friends-quiere-ser-tu-amigo",
                                username = request.user.username
                            )),
                            vec![
                                ui::flat(
                                    t!("pantalla-friends-rechazar"),
                                    Some(on(Msg::Decline(request.id))),
                                )
                                .into(),
                                ui::primary(
                                    t!("pantalla-friends-aceptar"),
                                    Some(on(Msg::Accept(request.id))),
                                )
                                .into(),
                            ],
                        )
                    })
                    .collect();
                page = page.push(ui::section(
                    t!(
                        "pantalla-friends-solicitudes-recibidas",
                        valor = friends.incoming.len()
                    ),
                    None,
                    rows,
                ));
            }
            if friends.friends.is_empty() {
                page = page.push(ui::empty_state(
                    Icon::Users,
                    t!("pantalla-friends-todavia-no-tienes-amigos-en-este"),
                    t!("pantalla-friends-pide-amistad-con-su-nombre-de"),
                    None,
                ));
            } else {
                let mut sorted: Vec<_> = friends.friends.iter().collect();
                sorted.sort_by_key(|friend| (!friend.online, friend.display_name.to_lowercase()));
                let rows = sorted
                    .into_iter()
                    .map(|friend| {
                        ui::list_row(
                            Some(ui::avatar(&app.theme, &friend.display_name, size::AVATAR)),
                            friend.display_name.as_str(),
                            Some(format!("@{}", friend.username)),
                            vec![
                                if friend.online {
                                    ui::status_badge(
                                        Tone::Success,
                                        t!("pantalla-friends-conectado"),
                                    )
                                } else {
                                    ui::status_badge(
                                        Tone::Neutral,
                                        t!("pantalla-friends-desconectado"),
                                    )
                                },
                                ui::icon_button(
                                    Icon::Trash,
                                    t!("pantalla-friends-quitar-de-tus-amigos"),
                                    Some(on(Msg::Remove {
                                        user: friend.id,
                                        name: friend.display_name.clone(),
                                    })),
                                ),
                            ],
                        )
                    })
                    .collect();
                page = page.push(ui::section(t!("pantalla-friends-tus-amigos"), None, rows));
            }
            if !friends.outgoing.is_empty() {
                let rows = friends
                    .outgoing
                    .iter()
                    .map(|request| {
                        ui::list_row(
                            Some(ui::avatar(
                                &app.theme,
                                &request.user.display_name,
                                size::AVATAR,
                            )),
                            request.user.display_name.as_str(),
                            Some(t!(
                                "pantalla-friends-pendiente-de-aceptar",
                                username = request.user.username
                            )),
                            vec![
                                ui::flat(
                                    t!("pantalla-friends-retirar"),
                                    Some(on(Msg::Decline(request.id))),
                                )
                                .into(),
                            ],
                        )
                    })
                    .collect();
                page = page.push(ui::section(
                    t!("pantalla-friends-solicitudes-enviadas"),
                    None,
                    rows,
                ));
            }
        }
    }
    ui::page(page)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::{Event, tests::app};

    #[test]
    fn adding_friends_validates_and_reloads() {
        let mut app = app();
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
        assert_eq!(app.toasts.len(), 1);
        app.update(Message::Event(Event::FriendChanged(Err(
            "no existe ningún usuario «carla»".into(),
        ))));
        assert!(app.friends_page.add_error.is_some());
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
