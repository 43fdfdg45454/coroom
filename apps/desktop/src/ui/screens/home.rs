//! Pantalla 2: inicio.
//!
//! Dos tarjetas grandes, "Compartir mi pantalla" y "Unirme a una sala" (con el código o las
//! invitaciones recibidas), los amigos conectados (con "Invitar" mientras se aloja una sala) y los
//! avisos de cuota del relay.

use coroom_transport::proto::signal::{ROOM_CODE_LEN, is_valid_room_code};
use coroom_ui::Element;
use coroom_ui::icon::{Icon, icon_with};
use coroom_ui::style::SvgClass;
use coroom_ui::theme::Tone;
use coroom_ui::tokens::{size, space};
use coroom_ui::widget::{self as ui, Kind};
use iced_core::text::Wrapping;
use iced_core::{Alignment, Length};
use iced_widget::{Column, column, row};

use crate::ui::app::{App, Effect, Load, Message, Page};
use crate::ui::screens::guest;

/// Estado.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// Código que se escribe.
    pub code: String,
    /// Error del código.
    pub code_error: Option<String>,
}

/// Mensajes.
#[derive(Debug, Clone)]
pub enum Msg {
    /// Código.
    Code(String),
    /// Entrar con el código.
    Join,
    /// Entrar con una invitación.
    Accept(String),
    /// Ir a compartir.
    Share,
    /// Invitar a un amigo conectado a la sala abierta.
    Invite(u64),
    /// Ir a "Amigos" para añadir.
    AddFriends,
}

/// Normaliza lo que se escribe en el código: mayúsculas, sin espacios ni guiones, como mucho seis.
pub fn clean_code(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .take(ROOM_CODE_LEN)
        .collect()
}

/// Reduce un mensaje.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Code(text) => {
            app.home.code = clean_code(&text);
            app.home.code_error = None;
            Vec::new()
        }
        Msg::Join => {
            let code = app.home.code.clone();
            if !is_valid_room_code(&code) {
                app.home.code_error = Some(if code.len() < ROOM_CODE_LEN {
                    t!(
                        "pantalla-home-el-codigo-tiene-room-code-len",
                        ROOM_CODE_LEN = ROOM_CODE_LEN
                    )
                } else {
                    t!("pantalla-home-ese-codigo-no-es-de-una").to_owned()
                });
                return Vec::new();
            }
            app.home.code.clear();
            guest::start(app, code)
        }
        Msg::Accept(code) => {
            app.invitations.retain(|invitation| invitation.code != code);
            guest::start(app, code)
        }
        Msg::Share => {
            app.page = Page::Host;
            Vec::new()
        }
        Msg::Invite(user) => {
            let name = app
                .friends
                .loaded()
                .and_then(|friends| friends.friends.iter().find(|f| f.id == user))
                .map(|friend| friend.display_name.clone())
                .unwrap_or_else(|| t!("pantalla-home-tu-amigo").to_owned());
            app.toast(
                Tone::Info,
                t!("pantalla-home-invitacion-enviada-a-name", name = name),
            );
            vec![Effect::Invite(user)]
        }
        Msg::AddFriends => {
            app.page = Page::Friends;
            Vec::new()
        }
    }
}

fn on(msg: Msg) -> Message {
    Message::Home(msg)
}

/// Dibuja la página.
pub fn view(app: &App) -> Element<'_, Message> {
    let name = app
        .account
        .as_ref()
        .map_or("", |account| account.display_name.as_str());
    let host = app.account.as_ref().map(|account| account.host());
    let mut page = Column::new().spacing(space::L).push(ui::page_header(
        t!("pantalla-home-hola-name", name = name),
        host.map(|host| t!("pantalla-home-conectado-a-host", host = host)),
        vec![],
    ));
    for notice in notices(app) {
        page = page.push(notice);
    }
    page = page.push(row![share_card(app), join_card(app)].spacing(space::M));
    page = page.push(online_friends(app));
    ui::page(page)
}

fn notices(app: &App) -> Vec<Element<'_, Message>> {
    let mut out = Vec::new();
    if let Some(update) = &app.update.available {
        out.push(ui::banner(
            Tone::Info,
            t!(
                "pantalla-home-hay-una-version-nueva-de-coroom",
                version = update.version,
                version2 = app.version
            ),
            Some((
                t!("pantalla-home-ver-la-version"),
                Message::OpenUrl(update.url.clone()),
            )),
        ));
    }
    if let Some(code) = app.host.room.as_ref().and_then(|room| room.code.as_deref()) {
        out.push(ui::banner(
            Tone::Info,
            t!(
                "pantalla-home-estas-compartiendo-tu-pantalla-en-la",
                code = code
            ),
            Some((t!("pantalla-home-ver-la-sala"), on(Msg::Share))),
        ));
    }
    if let Some(usage) = &app.usage {
        if !usage.relay_enabled {
            out.push(ui::banner(
                Tone::Warning,
                t!("pantalla-home-este-servidor-no-tiene-relay-solo"),
                None,
            ));
        } else if let Some(quota) = usage.quota_bytes.filter(|quota| *quota > 0) {
            let used = usage.bytes as f64 / quota as f64;
            if used >= 0.8 {
                out.push(ui::banner(
                    if used >= 1.0 {
                        Tone::Error
                    } else {
                        Tone::Warning
                    },
                    t!(
                        "pantalla-home-has-usado-el-0-del-relay",
                        valor = format!("{:.0}", (used * 100.0).min(100.0)),
                        valor2 = crate::ui::stats::bytes(usage.bytes),
                        valor3 = crate::ui::stats::bytes(quota)
                    ),
                    None,
                ));
            }
        }
    }
    out
}

fn big_card_header<'a>(icon: Icon, title: &'a str, text: &'a str) -> Element<'a, Message> {
    column![
        icon_with(icon, size::ICON_LARGE, SvgClass::Accent),
        ui::heading(title),
        ui::secondary(text).wrapping(Wrapping::WordOrGlyph)
    ]
    .spacing(space::XS)
    .into()
}

fn share_card(app: &App) -> Element<'_, Message> {
    let open = app.host.is_open();
    ui::card(
        column![
            big_card_header(
                Icon::ShareScreen,
                t!("pantalla-home-compartir-mi-pantalla"),
                t!("pantalla-home-crea-una-sala-elige-que-compartir")
            ),
            ui::gap(space::XS),
            ui::button_with(
                Kind::Primary,
                if open {
                    t!("pantalla-home-ver-mi-sala")
                } else {
                    t!("pantalla-home-compartir")
                },
                Some(Icon::ShareScreen),
                Some(on(Msg::Share))
            )
        ]
        .spacing(space::XS),
    )
}

fn join_card(app: &App) -> Element<'_, Message> {
    let state = &app.home;
    let code_field = Element::from(
        ui::field(t!("pantalla-home-codigo-de-la-sala"), &state.code)
            .placeholder("K7Q2MZ")
            .monospace()
            .on_input(|value| on(Msg::Code(value)))
            .on_submit(Some(on(Msg::Join)))
            .error(state.code_error.as_deref())
            .id("room-code"),
    );
    let mut content = column![
        big_card_header(
            Icon::LogIn,
            t!("pantalla-home-unirme-a-una-sala"),
            t!("pantalla-home-con-el-codigo-que-te-dio")
        ),
        row![
            iced_widget::container(code_field).width(Length::Fill),
            ui::primary(t!("pantalla-home-entrar"), Some(on(Msg::Join)))
        ]
        .spacing(space::XS)
        .align_y(Alignment::End)
    ]
    .spacing(space::XS);
    if !app.invitations.is_empty() {
        let rows = app
            .invitations
            .iter()
            .take(3)
            .map(|invitation| {
                ui::list_row(
                    Some(ui::avatar(&app.theme, &invitation.from_name, 28.0)),
                    t!("pantalla-home-te-invita", from_name = invitation.from_name),
                    Some(t!("pantalla-home-sala", code = invitation.code)),
                    vec![
                        ui::button_with(
                            Kind::Primary,
                            t!("pantalla-home-entrar"),
                            None,
                            Some(on(Msg::Accept(invitation.code.clone()))),
                        )
                        .into(),
                    ],
                )
            })
            .collect();
        content = content
            .push(ui::gap(space::XXS))
            .push(ui::caption(t!("pantalla-home-invitaciones")))
            .push(ui::boxed_list(rows));
    }
    ui::card(content)
}

fn online_friends(app: &App) -> Element<'_, Message> {
    let hosting = app
        .host
        .room
        .as_ref()
        .is_some_and(|room| room.code.is_some());
    let body: Element<'_, Message> = match &app.friends {
        Load::Idle | Load::Loading => ui::skeleton(t!("pantalla-home-cargando-tus-amigos"), 2),
        Load::Failed(err) => ui::banner(
            Tone::Error,
            t!("pantalla-home-no-se-pudo-cargar-la-lista", err = err),
            Some((
                t!("pantalla-home-reintentar"),
                Message::Navigate(Page::Home),
            )),
        ),
        Load::Loaded(friends) => {
            let online: Vec<_> = friends.friends.iter().filter(|f| f.online).collect();
            if online.is_empty() {
                ui::empty_state(
                    Icon::Users,
                    if friends.friends.is_empty() {
                        t!("pantalla-home-todavia-no-tienes-amigos-aqui")
                    } else {
                        t!("pantalla-home-ninguno-de-tus-amigos-esta-conectado")
                    },
                    if friends.friends.is_empty() {
                        t!("pantalla-home-anadelos-por-su-nombre-de-usuario")
                    } else {
                        t!("pantalla-home-cuando-se-conecten-apareceran-aqui-tambien")
                    },
                    friends.friends.is_empty().then(|| {
                        ui::button_with(
                            Kind::Normal,
                            t!("pantalla-home-anadir-amigos"),
                            Some(Icon::UserPlus),
                            Some(on(Msg::AddFriends)),
                        )
                        .into()
                    }),
                )
            } else {
                let rows = online
                    .into_iter()
                    .map(|friend| {
                        let invite: Element<'_, Message> = if hosting {
                            ui::button_with(
                                Kind::Normal,
                                t!("pantalla-home-invitar"),
                                Some(Icon::Mail),
                                Some(on(Msg::Invite(friend.id))),
                            )
                            .into()
                        } else {
                            ui::with_tooltip(
                                ui::button_with(
                                    Kind::Normal,
                                    t!("pantalla-home-invitar"),
                                    Some(Icon::Mail),
                                    None,
                                ),
                                t!("pantalla-home-abre-una-sala-para-invitar"),
                            )
                        };
                        ui::list_row(
                            Some(ui::avatar(&app.theme, &friend.display_name, size::AVATAR)),
                            friend.display_name.as_str(),
                            Some(format!("@{}", friend.username)),
                            vec![
                                ui::status_badge(Tone::Success, t!("pantalla-home-conectado")),
                                invite,
                            ],
                        )
                    })
                    .collect();
                ui::boxed_list(rows)
            }
        }
    };
    column![ui::heading(t!("pantalla-home-amigos-conectados")), body]
        .spacing(space::S)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::{Screen, tests::app};

    #[test]
    fn room_codes_are_cleaned_and_validated() {
        assert_eq!(clean_code(" k7q-2mz9 "), "K7Q2MZ");
        let mut app = app();
        app.screen = Screen::Main;
        app.update(Message::Home(Msg::Code("k7q".into())));
        assert!(app.update(Message::Home(Msg::Join)).is_empty());
        assert!(
            app.home
                .code_error
                .as_deref()
                .is_some_and(|e| e.contains("6"))
        );
        app.update(Message::Home(Msg::Code("ABC10O".into())));
        assert!(app.update(Message::Home(Msg::Join)).is_empty());
        assert!(
            app.home
                .code_error
                .as_deref()
                .is_some_and(|e| e.contains("«0»"))
        );
        app.update(Message::Home(Msg::Code("k7q2mz".into())));
        let effects = app.update(Message::Home(Msg::Join));
        assert!(
            effects.contains(&Effect::Join("K7Q2MZ".into())),
            "{effects:?}"
        );
        assert_eq!(app.screen, Screen::Guest);
        assert!(app.home.code.is_empty());
    }

    #[test]
    fn invitations_join_and_disappear() {
        let mut app = app();
        app.screen = Screen::Main;
        app.invitations.push(crate::ui::app::Invitation {
            from_user: 3,
            from_name: "Bea".into(),
            code: "K7Q2MZ".into(),
        });
        let effects = app.update(Message::Home(Msg::Accept("K7Q2MZ".into())));
        assert!(effects.contains(&Effect::Join("K7Q2MZ".into())));
        assert!(app.invitations.is_empty());
    }
}
