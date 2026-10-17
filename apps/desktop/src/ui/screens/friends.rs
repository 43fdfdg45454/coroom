//! Amigos: la página del riel con la lista completa **por estado** (en sala, jugando,
//! conectados y, plegados, los desconectados), las solicitudes pendientes y «Agregar amigo».
//!
//! Un amigo que cambia de estado cambia de grupo. Cada fila lleva su insignia y su botón en
//! ranuras del ancho de la variante más ancha: el nombre no se reacomoda al cambiar el estado.
//! Invitar y pedir entrar en la sala de un amigo son de Inicio ([`super::home::Msg`]), que
//! también muestra las solicitudes recibidas en su bandeja.

use std::time::Instant;

use coroom_transport::proto::api::{Friend, Friends};
use coroom_transport::proto::signal::Activity;
use coroom_ui::Element;
use coroom_ui::animate::Collapse;
use coroom_ui::icon::{Icon, icon_with};
use coroom_ui::motion::{EXIT_EASING, Token};
use coroom_ui::style::SvgClass;
use coroom_ui::theme::Tone;
use coroom_ui::tokens::{size, space};
use coroom_ui::widget::{self as ui, Kind};
use iced_core::text::Wrapping;
use iced_core::{Alignment, Length, Padding};
use iced_widget::{Column, column, container, row};

use crate::ui::app::{App, Dialog, Effect, Load, Message};
use crate::ui::avatars;
use crate::ui::screens::home::{self, Msg as HomeMsg};

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
    /// Los desconectados están desplegados.
    pub show_offline: bool,
    /// Cuándo se desplegaron o plegaron (para la animación).
    pub offline_at: Option<Instant>,
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
    /// Despliega o pliega los desconectados.
    ToggleOffline,
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
        Msg::ToggleOffline => {
            state.show_offline = !state.show_offline;
            state.offline_at = Some(app.now);
            Vec::new()
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

/// Lo desplegado de los desconectados (0 plegados, 1 abiertos), con su curva.
fn offline_shown(app: &App) -> f32 {
    let state = &app.friends_page;
    let target = if state.show_offline { 1.0 } else { 0.0 };
    let Some(at) = state.offline_at else {
        return target;
    };
    let motion = app.motion();
    let (duration, easing) = if state.show_offline {
        (motion.duration(Token::Panel), Token::Panel.easing())
    } else {
        (motion.exit_duration(Token::Panel), EXIT_EASING)
    };
    if duration.is_zero() {
        return target;
    }
    let t = (app.now.saturating_duration_since(at).as_secs_f32() / duration.as_secs_f32())
        .clamp(0.0, 1.0);
    let eased = if t >= 1.0 { 1.0 } else { easing.value(t) };
    if state.show_offline {
        eased
    } else {
        1.0 - eased
    }
}

/// Si los desconectados se están desplegando o plegando.
pub fn animating(app: &App) -> bool {
    let state = &app.friends_page;
    let Some(at) = state.offline_at else {
        return false;
    };
    let motion = app.motion();
    let duration = if state.show_offline {
        motion.duration(Token::Panel)
    } else {
        motion.exit_duration(Token::Panel)
    };
    app.now.saturating_duration_since(at) < duration
}

/// Los grupos de la lista, en su orden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Con su sala abierta.
    Hosting,
    /// En la sala de otro.
    Playing,
    /// Conectados, sin sala.
    Online,
    /// Desconectados (plegados).
    Offline,
}

impl Group {
    /// Los cuatro, en el orden de la página.
    pub const ALL: [Group; 4] = [
        Group::Hosting,
        Group::Playing,
        Group::Online,
        Group::Offline,
    ];

    /// El grupo de un amigo.
    pub fn of(friend: &Friend) -> Group {
        match (friend.online, friend.activity) {
            (false, _) => Group::Offline,
            (true, Activity::Hosting) => Group::Hosting,
            (true, Activity::Playing) => Group::Playing,
            (true, Activity::Idle) => Group::Online,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Group::Hosting => t!("friends-group-hosting"),
            Group::Playing => t!("friends-group-playing"),
            Group::Online => t!("friends-group-online"),
            Group::Offline => t!("friends-group-offline"),
        }
    }
}

/// Los amigos de un grupo, por nombre.
pub fn group(friends: &Friends, group: Group) -> Vec<&Friend> {
    home::sorted_friends(friends)
        .into_iter()
        .filter(|friend| Group::of(friend) == group)
        .collect()
}

fn on(msg: Msg) -> Message {
    Message::Friends(msg)
}

fn on_home(msg: HomeMsg) -> Message {
    Message::Home(msg)
}

/// «3 conectados».
pub(crate) fn summary(friends: &Friends) -> String {
    match friends.friends.iter().filter(|f| f.online).count() {
        0 => t!("home-online-none").to_owned(),
        1 => t!("home-online-one").to_owned(),
        count => t!("home-online-many", count = count),
    }
}

/// «actualizado hace 1 min», al pasar el ratón por el recuento (no cambia mientras se mira).
pub(crate) fn updated(app: &App) -> Option<String> {
    let at = app.home.friends_updated?;
    let elapsed = app.now.saturating_duration_since(at).as_secs();
    Some(t!("home-updated", ago = super::settings::ago_secs(elapsed)))
}

/// Dibuja la página.
pub fn view<'a>(app: &'a App) -> Element<'a, Message> {
    let state = &app.friends_page;
    let count: Element<'a, Message> =
        ui::caption(app.friends.loaded().map(summary).unwrap_or_default()).into();
    let count = match updated(app) {
        Some(updated) => ui::with_tooltip(count, updated),
        None => count,
    };
    let header = row![
        container(count).width(Length::Fill),
        ui::button_with(
            if state.adding {
                Kind::Normal
            } else {
                Kind::Primary
            },
            if state.adding {
                t!("home-add-friend-close")
            } else {
                t!("home-add-friend")
            },
            (!state.adding).then_some(Icon::UserPlus),
            Some(on(Msg::ToggleAdd))
        )
    ]
    .spacing(space::S)
    .align_y(Alignment::Center);
    // El campo de agregar y las solicitudes tienen su sitio aunque falten (con el espacio de
    // encima dentro): una solicitud que llega no cambia de posición la lista ni sus menús.
    let below = |content: Element<'a, Message>, top: f32| -> Element<'a, Message> {
        container(content)
            .padding(Padding {
                top,
                ..Padding::ZERO
            })
            .into()
    };
    let add = state.adding.then(|| below(add_friend(app), space::M));
    let mut pending = None;
    let body = match &app.friends {
        Load::Idle | Load::Loading => ui::skeleton(t!("home-loading"), 3),
        Load::Failed(err) => ui::card(ui::error_state(
            t!("home-load-failed"),
            err.as_str(),
            Some((t!("home-retry"), on(Msg::Reload))),
        )),
        Load::Loaded(friends) => {
            pending = requests(app, friends).map(|requests| below(requests, space::L));
            if friends.friends.is_empty() {
                ui::card(ui::empty_state(
                    Icon::Users,
                    t!("home-empty-title"),
                    t!("home-empty-text"),
                    (!state.adding).then(|| {
                        ui::button_with(
                            Kind::Normal,
                            t!("home-add-friend"),
                            Some(Icon::UserPlus),
                            Some(on(Msg::ToggleAdd)),
                        )
                        .into()
                    }),
                ))
            } else {
                groups(app, friends)
            }
        }
    };
    let page = column![
        header,
        ui::slot(add),
        ui::slot(pending),
        below(body, space::L)
    ];
    ui::page("friends", page)
}

/// Los grupos por estado; cada uno está siempre entre los hijos (vacío, un hueco), así que un
/// amigo que cambia de grupo no mueve el estado de los demás.
fn groups<'a>(app: &'a App, friends: &'a Friends) -> Element<'a, Message> {
    let mut out = Column::new();
    let mut first = true;
    for kind in Group::ALL {
        let members = group(friends, kind);
        if kind == Group::Offline {
            out = out.push(ui::slot((!members.is_empty()).then(|| {
                container(offline_group(app, members))
                    .padding(Padding {
                        top: if first { 0.0 } else { space::L },
                        ..Padding::ZERO
                    })
                    .into()
            })));
            continue;
        }
        let shown = (!members.is_empty()).then(|| {
            let top = if first { 0.0 } else { space::L };
            first = false;
            container(
                column![
                    group_title(kind, members.len()),
                    ui::boxed_list(
                        members
                            .into_iter()
                            .map(|friend| friend_row(app, friend))
                            .collect()
                    )
                ]
                .spacing(space::XS),
            )
            .padding(Padding {
                top,
                ..Padding::ZERO
            })
            .into()
        });
        out = out.push(ui::slot(shown));
    }
    out.into()
}

fn group_title<'a>(kind: Group, count: usize) -> Element<'a, Message> {
    ui::caption(t!("friends-group", group = kind.title(), count = count)).into()
}

/// Los desconectados: un botón que los despliega y, debajo, la lista plegable.
fn offline_group<'a>(app: &'a App, members: Vec<&'a Friend>) -> Element<'a, Message> {
    let open = app.friends_page.show_offline;
    let toggle = coroom_ui::animate::button(
        row![
            icon_with(
                if open {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                },
                16.0,
                SvgClass::Secondary
            ),
            ui::caption(t!(
                "friends-group",
                group = Group::Offline.title(),
                count = members.len()
            )),
        ]
        .spacing(space::XXS)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([2.0, 4.0]))
    .class(coroom_ui::style::ButtonClass::Flat)
    .on_press(on(Msg::ToggleOffline));
    let list = container(ui::boxed_list(
        members
            .into_iter()
            .map(|friend| friend_row(app, friend))
            .collect(),
    ))
    .padding(Padding {
        top: space::XS,
        ..Padding::ZERO
    });
    column![
        ui::with_tooltip(
            toggle,
            if open {
                t!("friends-hide-offline")
            } else {
                t!("friends-show-offline")
            }
        ),
        Collapse::new(list, offline_shown(app))
    ]
    .into()
}

/// El campo desplegado de «Agregar amigo».
fn add_friend(app: &App) -> Element<'_, Message> {
    let state = &app.friends_page;
    ui::card(
        column![
            row![
                container(Element::from(
                    ui::field(t!("home-username"), &state.add_name)
                        .placeholder("bea")
                        .on_input(|value| on(Msg::AddName(value)))
                        .on_submit((!state.busy).then(|| on(Msg::Add)))
                        .error(state.add_error.as_deref())
                        .id("friend-name")
                ))
                .width(Length::Fill),
                ui::button_with(
                    Kind::Primary,
                    t!("home-send-request"),
                    Some(Icon::UserPlus),
                    (!state.busy).then(|| on(Msg::Add))
                )
            ]
            .spacing(space::XS)
            // Con un error debajo del campo, el botón sigue a la altura del campo.
            .align_y(if state.add_error.is_some() {
                Alignment::Center
            } else {
                Alignment::End
            }),
            ui::caption(t!("home-request-hint")).wrapping(Wrapping::WordOrGlyph)
        ]
        .spacing(space::XS),
    )
}

/// Solicitudes pendientes: las recibidas (aceptar o rechazar) y las enviadas (retirar).
fn requests<'a>(app: &'a App, friends: &'a Friends) -> Option<Element<'a, Message>> {
    if friends.incoming.is_empty() && friends.outgoing.is_empty() {
        return None;
    }
    let busy = app.friends_page.busy;
    let incoming = friends.incoming.iter().map(|request| {
        ui::list_row(
            Some(avatars::view(
                app,
                &request.user.display_name,
                request.user.avatar,
                size::AVATAR,
            )),
            request.user.display_name.as_str(),
            Some(t!(
                "home-wants-to-be-friend",
                username = request.user.username
            )),
            vec![
                ui::flat(
                    t!("home-decline"),
                    (!busy).then(|| on(Msg::Decline(request.id))),
                )
                .into(),
                ui::primary(
                    t!("home-accept"),
                    (!busy).then(|| on(Msg::Accept(request.id))),
                )
                .into(),
            ],
        )
    });
    let outgoing = friends.outgoing.iter().map(|request| {
        ui::list_row(
            Some(avatars::view(
                app,
                &request.user.display_name,
                request.user.avatar,
                size::AVATAR,
            )),
            request.user.display_name.as_str(),
            Some(t!("home-pending", username = request.user.username)),
            vec![
                ui::flat(
                    t!("home-withdraw"),
                    (!busy).then(|| on(Msg::Decline(request.id))),
                )
                .into(),
            ],
        )
    });
    Some(
        column![
            ui::caption(t!("home-requests")),
            ui::boxed_list(incoming.chain(outgoing).collect())
        ]
        .spacing(space::XS)
        .into(),
    )
}

/// Las cuatro insignias de estado, para medir la ranura (todas del mismo ancho en la lista).
fn badge_samples<'a>() -> Vec<Element<'a, Message>> {
    [
        (Tone::Neutral, t!("home-offline")),
        (Tone::Success, t!("home-available")),
        (Tone::Info, t!("home-hosting")),
        (Tone::Info, t!("home-playing")),
    ]
    .into_iter()
    .map(|(tone, label)| ui::status_badge(tone, label))
    .collect()
}

/// Los botones que puede llevar una fila, para medir la ranura.
fn action_samples<'a>() -> Vec<Element<'a, Message>> {
    vec![
        ui::button_with(Kind::Normal, t!("home-invite"), None, None).into(),
        ui::button_with(
            Kind::Primary,
            t!("home-ask-to-join"),
            Some(Icon::LogIn),
            None,
        )
        .into(),
        ui::button_with(Kind::Normal, t!("home-asked"), Some(Icon::LogIn), None).into(),
    ]
}

/// Fila de un amigo: avatar, nombre, usuario, estado, «Invitar» (o «Pedir entrar» si tiene una
/// sala abierta y aquí no hay ninguna; nada si está desconectado) y quitar.
fn friend_row<'a>(app: &'a App, friend: &'a Friend) -> Element<'a, Message> {
    let invite_button = |on_press: Option<Message>| {
        ui::button_with(Kind::Normal, t!("home-invite"), None, on_press)
    };
    let hosting = friend.online && friend.activity == Activity::Hosting;
    let invite: Option<Element<'_, Message>> = if !friend.online {
        None
    } else if hosting && home::can_ask(app) {
        Some(if app.home.asked_at.contains_key(&friend.id) {
            ui::with_tooltip(
                ui::button_with(Kind::Normal, t!("home-asked"), Some(Icon::LogIn), None),
                t!("home-asked-tip", name = friend.display_name),
            )
        } else {
            ui::button_with(
                Kind::Primary,
                t!("home-ask-to-join"),
                Some(Icon::LogIn),
                (!home::server_down(app)).then(|| on_home(HomeMsg::AskToJoin(friend.id))),
            )
            .into()
        })
    } else if !app.host.is_open() {
        Some(ui::with_tooltip(
            invite_button(Some(on_home(HomeMsg::Invite(friend.id)))),
            t!("home-invite-no-room-tip"),
        ))
    } else if home::room_code(app).is_none() {
        Some(ui::with_tooltip(
            invite_button(None),
            t!("home-invite-opening-tip"),
        ))
    } else {
        Some(invite_button(Some(on_home(HomeMsg::Invite(friend.id)))).into())
    };
    ui::list_row(
        Some(avatars::view(
            app,
            &friend.display_name,
            friend.avatar,
            size::AVATAR,
        )),
        friend.display_name.as_str(),
        Some(format!("@{}", friend.username)),
        vec![
            ui::reserve(badge_samples(), Some(home::activity_badge(friend)))
                .align_x(iced_core::alignment::Horizontal::Right)
                .into(),
            ui::reserve(action_samples(), invite)
                .align_x(iced_core::alignment::Horizontal::Right)
                .into(),
            ui::icon_button(
                Icon::Trash,
                t!("home-remove-friend"),
                Some(on(Msg::Remove {
                    user: friend.id,
                    name: friend.display_name.clone(),
                })),
            ),
        ],
    )
}

/// Escenas de Amigos para las capturas.
pub(crate) mod shots {
    use super::Msg;
    use crate::ui::app::{App, Message, Page};
    use crate::ui::shots::Builder;

    /// La página con los grupos (Bea en su sala, Carla jugando) y los desconectados
    /// desplegados.
    fn friends(app: &mut App) {
        crate::ui::screens::home::shots::home(app);
        app.update(Message::Navigate(Page::Friends));
        app.update(Message::Friends(Msg::ToggleOffline));
        app.friends_page.offline_at = None;
        app.page_change = None;
    }

    /// «Agregar amigo» desplegado.
    fn add_friend(app: &mut App) {
        crate::ui::shots::signed_in(app, Page::Friends);
        app.home.friends_updated = Some(app.now);
        app.update(Message::Friends(Msg::ToggleAdd));
        app.friends_page.add_name = "hugo".to_owned();
        app.friends_page.add_error = Some("No existe ningún usuario «hugo».".to_owned());
        app.page_change = None;
    }

    /// Escenas de esta pantalla.
    pub(crate) const BUILDERS: &[(&str, Builder)] =
        &[("07-agregar-amigo", add_friend), ("07-amigos", friends)];
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
