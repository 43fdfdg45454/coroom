//! Pantalla 1: inicio.
//!
//! Una sola pantalla bajo la cabecera: dos tarjetas grandes («Compartir mi pantalla» y «Unirse
//! con código»), las invitaciones que llegan (con su cuenta atrás de [`INVITATION_TTL`]) y la
//! lista de amigos, que se actualiza sola con la presencia y la actividad de la señalización
//! (también con la sala abierta o desde dentro de una), con las solicitudes pendientes y «Agregar
//! amigo». A un amigo con sala abierta se le puede **pedir entrar** ([`Msg::AskToJoin`]): el botón
//! queda en «Pedido…» hasta que invita (se entra solo), dice que no o pasa [`JOIN_REQUEST_TTL`].

use std::collections::HashMap;
use std::time::{Duration, Instant};

use coroom_transport::proto::api::{Friend, Friends};
use coroom_transport::proto::signal::{Activity, ROOM_CODE_LEN, is_valid_room_code};
use coroom_ui::Element;
use coroom_ui::font::MONO;
use coroom_ui::icon::{Icon, icon_with};
use coroom_ui::style::{ContainerClass, InputClass, SvgClass, TextClass};
use coroom_ui::theme::Tone;
use coroom_ui::tokens::{size, space, text as type_size};
use coroom_ui::widget::{self as ui, Kind};
use iced_core::text::{LineHeight, Wrapping};
use iced_core::{Alignment, Length, Padding};
use iced_widget::{Column, column, container, row, text, text_input};

use crate::ui::app::{
    App, Effect, Event, Invitation, JOIN_REQUEST_TTL, Load, Message, Page, Screen,
};
use crate::ui::screens::friends::{self, Msg as FriendsMsg};
use crate::ui::screens::guest;

/// Lo que dura una invitación en Inicio: pasado ese tiempo desaparece sola.
pub const INVITATION_TTL: Duration = Duration::from_secs(60);

/// Invitaciones que se ven a la vez (las más nuevas).
const INVITATIONS_SHOWN: usize = 3;

/// Estado.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// Código que se escribe.
    pub code: String,
    /// Error del código.
    pub code_error: Option<String>,
    /// Cuándo llegó la invitación de cada amigo (por usuario: una por amigo).
    pub invited_at: HashMap<u64, Instant>,
    /// Cuándo llegó la lista de amigos o el último cambio de presencia.
    pub friends_updated: Option<Instant>,
    /// A qué amigos se les pidió entrar en su sala y cuándo (sin respuesta todavía).
    pub asked_at: HashMap<u64, Instant>,
}

/// Mensajes.
#[derive(Debug, Clone)]
pub enum Msg {
    /// Código.
    Code(String),
    /// Entrar con el código.
    Join,
    /// Entrar con una invitación (por su código).
    Accept(String),
    /// Descartar la invitación de un amigo.
    Ignore(u64),
    /// Ir a compartir (o a la sala abierta).
    Share,
    /// Invitar a un amigo: con la sala abierta, la invitación; sin sala, a Compartir.
    Invite(u64),
    /// Pedir entrar en la sala abierta de un amigo.
    AskToJoin(u64),
}

/// Normaliza lo que se escribe en el código: mayúsculas, sin espacios ni guiones, como mucho seis.
pub fn clean_code(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .take(ROOM_CODE_LEN)
        .collect()
}

/// Si el servidor no responde (la cabecera lo dice en rojo): no se puede abrir ni entrar.
pub fn server_down(app: &App) -> bool {
    app.server_status.online == Some(false)
}

/// Si se puede pedir entrar en la sala de alguien: sin sala propia (abierta o abriéndose) y sin
/// estar en otra.
fn can_ask(app: &App) -> bool {
    !app.host.is_open() && app.guest.is_none()
}

/// Nombre visible de un amigo (o «tu amigo» si ya no está en la lista).
fn friend_name(app: &App, user: u64) -> String {
    app.friends
        .loaded()
        .and_then(|friends| friends.friends.iter().find(|f| f.id == user))
        .map(|friend| friend.display_name.clone())
        .unwrap_or_else(|| t!("home-your-friend").to_owned())
}

/// Si se le había pedido entrar a `user`, lo olvida y dice que sí (la respuesta llegó).
pub fn take_asked(app: &mut App, user: u64) -> bool {
    app.home.asked_at.remove(&user).is_some()
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
            if server_down(app) {
                return Vec::new();
            }
            let code = app.home.code.clone();
            if !is_valid_room_code(&code) {
                app.home.code_error = Some(if code.len() < ROOM_CODE_LEN {
                    t!("home-code-length", length = ROOM_CODE_LEN)
                } else {
                    t!("home-code-invalid").to_owned()
                });
                return Vec::new();
            }
            app.home.code.clear();
            guest::start(app, code)
        }
        Msg::Accept(code) => {
            if server_down(app) {
                return Vec::new();
            }
            app.invitations.retain(|invitation| invitation.code != code);
            prune_times(app);
            guest::start(app, code)
        }
        Msg::Ignore(user) => {
            app.invitations
                .retain(|invitation| invitation.from_user != user);
            prune_times(app);
            Vec::new()
        }
        // Por `Navigate`, que pide ya lo que se puede compartir. Con la sala abierta, «Ver mi sala»
        // va aunque el servidor no responda.
        Msg::Share => {
            if server_down(app) && !app.host.is_open() {
                return Vec::new();
            }
            app.update(Message::Navigate(Page::Host))
        }
        Msg::Invite(user) => {
            if !app.host.is_open() {
                // Sin sala no hay a qué invitar: a Compartir, que la abre.
                return update(app, Msg::Share);
            }
            if room_code(app).is_none() {
                return Vec::new();
            }
            let name = friend_name(app, user);
            app.toast(Tone::Info, t!("home-invitation-sent", name = name));
            vec![Effect::Invite(user)]
        }
        Msg::AskToJoin(user) => {
            if server_down(app) || !can_ask(app) || app.home.asked_at.contains_key(&user) {
                return Vec::new();
            }
            let hosting = app.friends.loaded().is_some_and(|friends| {
                friends
                    .friends
                    .iter()
                    .any(|f| f.id == user && f.online && f.activity == Activity::Hosting)
            });
            if !hosting {
                return Vec::new();
            }
            let name = friend_name(app, user);
            app.home.asked_at.insert(user, app.now);
            app.toast(Tone::Info, t!("home-ask-sent", name = name));
            vec![Effect::AskToJoin(user)]
        }
    }
}

/// Código de la sala abierta, si el servidor ya la creó.
fn room_code(app: &App) -> Option<&str> {
    app.host.room.as_ref().and_then(|room| room.code.as_deref())
}

/// Anota lo que Inicio necesita de los eventos (lo llama [`App`] antes de tratarlos).
pub fn on_event(app: &mut App, event: &Event) {
    match event {
        Event::Invited(invitation) => {
            app.home.invited_at.insert(invitation.from_user, app.now);
        }
        Event::Friends(Ok(_)) | Event::Presence { .. } | Event::Activity { .. } => {
            app.home.friends_updated = Some(app.now);
            // Si al que se le pidió entrar cierra la sala o se desconecta, ya no hay a qué esperar.
            match event {
                Event::Presence {
                    user,
                    online: false,
                } => {
                    app.home.asked_at.remove(user);
                }
                Event::Activity { user, activity } if *activity != Activity::Hosting => {
                    app.home.asked_at.remove(user);
                }
                _ => {}
            }
        }
        Event::JoinDeclined { from_user, .. } => {
            app.home.asked_at.remove(from_user);
        }
        // La sala de espera solo manda peticiones de entrar: un error suyo las da por perdidas.
        Event::JoinRequestFailed { .. } => app.home.asked_at.clear(),
        Event::FriendChanged(result) => friends::on_changed(app, result),
        _ => {}
    }
}

/// Cuándo llegó una invitación (si no consta, ahora: la cuenta empieza al verla).
fn received_at(app: &App, invitation: &Invitation) -> Instant {
    app.home
        .invited_at
        .get(&invitation.from_user)
        .copied()
        .unwrap_or(app.now)
}

/// Olvida las horas de invitaciones que ya no están.
fn prune_times(app: &mut App) {
    let invitations = &app.invitations;
    app.home
        .invited_at
        .retain(|user, _| invitations.iter().any(|known| known.from_user == *user));
}

/// Pasa el tiempo: las invitaciones caducadas desaparecen y las peticiones de entrar sin
/// respuesta se dan por perdidas (con un aviso).
pub fn tick(app: &mut App) {
    let now = app.now;
    let times = &mut app.home.invited_at;
    for invitation in &app.invitations {
        times.entry(invitation.from_user).or_insert(now);
    }
    app.invitations.retain(|invitation| {
        times
            .get(&invitation.from_user)
            .is_none_or(|at| now.saturating_duration_since(*at) < INVITATION_TTL)
    });
    prune_times(app);
    let expired: Vec<u64> = app
        .home
        .asked_at
        .iter()
        .filter(|(_, at)| now.saturating_duration_since(**at) >= JOIN_REQUEST_TTL)
        .map(|(user, _)| *user)
        .collect();
    for user in expired {
        app.home.asked_at.remove(&user);
        let name = friend_name(app, user);
        app.toast(Tone::Neutral, t!("home-ask-no-answer", name = name));
    }
}

/// El siguiente múltiplo de `step` desde `since` que está después de `now`.
fn next_step(since: Instant, now: Instant, step: Duration) -> Instant {
    let elapsed = now.saturating_duration_since(since).as_secs();
    let step_secs = step.as_secs().max(1);
    since + Duration::from_secs((elapsed / step_secs + 1) * step_secs)
}

/// Cada cuánto cambia el texto «actualizado hace…».
fn updated_step(elapsed: Duration) -> Duration {
    if elapsed < Duration::from_secs(60) {
        Duration::from_secs(1)
    } else if elapsed < Duration::from_secs(3600) {
        Duration::from_secs(60)
    } else {
        Duration::from_secs(3600)
    }
}

/// Cuándo hace falta el próximo `Tick`: al caducar cada invitación y, con Inicio a la vista,
/// cuando cambian la cuenta atrás y el «actualizado hace…» (siempre después de `now`).
pub fn next_tick(app: &App) -> Option<Instant> {
    let now = app.now;
    let visible = app.screen == Screen::Main && app.page == Page::Home;
    let mut next: Option<Instant> = None;
    let mut consider = |at: Instant| {
        next = Some(next.map_or(at, |next| next.min(at)));
    };
    for invitation in &app.invitations {
        let at = received_at(app, invitation);
        consider((at + INVITATION_TTL).max(now + Duration::from_millis(1)));
        if visible {
            consider(next_step(at, now, Duration::from_secs(1)));
        }
    }
    if visible && let Some(at) = app.home.friends_updated {
        let step = updated_step(now.saturating_duration_since(at));
        consider(next_step(at, now, step));
    }
    for at in app.home.asked_at.values() {
        consider((*at + JOIN_REQUEST_TTL).max(now + Duration::from_millis(1)));
    }
    next
}

/// Amigos en el orden de la lista: conectados primero, entre ellos los que tienen una sala
/// abierta, y dentro de cada grupo por nombre (el identificador desempata, para que el orden no
/// salte).
pub fn sorted_friends(friends: &Friends) -> Vec<&Friend> {
    let mut sorted: Vec<_> = friends.friends.iter().collect();
    sorted.sort_by_cached_key(|friend| {
        (
            !friend.online,
            !(friend.online && friend.activity == Activity::Hosting),
            friend.display_name.to_lowercase(),
            friend.id,
        )
    });
    sorted
}

/// Insignia del estado de un amigo: desconectado, disponible, en su sala o jugando en una.
pub fn activity_badge<'a, M: 'a>(friend: &Friend) -> Element<'a, M> {
    match (friend.online, friend.activity) {
        (false, _) => ui::status_badge(Tone::Neutral, t!("home-offline")),
        (true, Activity::Idle) => ui::status_badge(Tone::Success, t!("home-available")),
        (true, Activity::Hosting) => ui::status_badge(Tone::Info, t!("home-hosting")),
        (true, Activity::Playing) => ui::status_badge(Tone::Info, t!("home-playing")),
    }
}

fn on(msg: Msg) -> Message {
    Message::Home(msg)
}

fn on_friends(msg: FriendsMsg) -> Message {
    Message::Friends(msg)
}

/// Dibuja la página.
pub fn view(app: &App) -> Element<'_, Message> {
    let mut page = Column::new().spacing(space::M);
    for notice in notices(app) {
        page = page.push(notice);
    }
    page = page.push(ui::equal_height_row(
        |real| vec![share_card(app, real), join_card(app, real)],
        space::M,
    ));
    let mut invitations: Vec<&Invitation> = app.invitations.iter().collect();
    invitations.sort_by_key(|invitation| std::cmp::Reverse(received_at(app, invitation)));
    for invitation in invitations.into_iter().take(INVITATIONS_SHOWN) {
        page = page.push(invitation_card(app, invitation));
    }
    page = page.push(ui::gap(space::XXS));
    page = page.push(friends_section(app));
    ui::page(page)
}

fn notices(app: &App) -> Vec<Element<'_, Message>> {
    let mut out = Vec::new();
    if let Some(update) = &app.update.available {
        out.push(ui::banner(
            Tone::Info,
            t!(
                "home-update-available",
                version = update.version,
                current = app.version
            ),
            Some((t!("home-update-see"), Message::OpenUrl(update.url.clone()))),
        ));
    }
    if let Some(usage) = &app.usage {
        if !usage.relay_enabled {
            out.push(ui::banner(Tone::Warning, t!("home-relay-disabled"), None));
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
                        "home-relay-usage",
                        percent = format!("{:.0}", (used * 100.0).min(100.0)),
                        used = crate::ui::stats::bytes(usage.bytes),
                        quota = crate::ui::stats::bytes(quota)
                    ),
                    None,
                ));
            }
        }
    }
    out
}

/// Cabecera de una tarjeta grande: icono del acento, título y explicación.
fn big_card_header<'a>(icon: Icon, title: &'a str, explanation: &'a str) -> Element<'a, Message> {
    row![
        icon_with(icon, size::ICON_LARGE, SvgClass::Accent),
        column![
            ui::heading(title),
            ui::secondary(explanation).wrapping(Wrapping::WordOrGlyph)
        ]
        .spacing(2)
        .width(Length::Fill)
    ]
    .spacing(space::S)
    .align_y(Alignment::Center)
    .into()
}

/// El motivo de un botón desactivado por el servidor caído.
fn offline_reason<'a>() -> Element<'a, Message> {
    row![
        icon_with(Icon::CloudOff, 14.0, SvgClass::Tone(Tone::Error)),
        ui::caption(t!("home-offline-reason")).wrapping(Wrapping::WordOrGlyph)
    ]
    .spacing(6)
    .align_y(Alignment::Center)
    .into()
}

/// Altura de una tarjeta de [`ui::equal_height_row`]: la de verdad llena la fila; la copia de
/// debajo, que solo mide, se encoge.
fn card_height(real: bool) -> Length {
    if real { Length::Fill } else { Length::Shrink }
}

fn big_card<'a>(
    content: Column<'a, Message, coroom_ui::Theme, coroom_ui::Renderer>,
    real: bool,
) -> Element<'a, Message> {
    container(content.spacing(space::S))
        .padding(space::M)
        .width(Length::Fill)
        .height(card_height(real))
        .class(ContainerClass::Card)
        .into()
}

/// Tarjeta de compartir (`real`: ver [`ui::equal_height_row`]).
fn share_card(app: &App, real: bool) -> Element<'_, Message> {
    let open = app.host.is_open();
    let blocked = server_down(app) && !open;
    let mut content = column![
        big_card_header(
            Icon::ShareScreen,
            t!("home-share-title"),
            t!("home-share-text")
        ),
        ui::button_with(
            Kind::Primary,
            if open {
                t!("home-see-room")
            } else {
                t!("home-share")
            },
            Some(Icon::ShareScreen),
            (real && !blocked).then(|| on(Msg::Share))
        )
    ];
    if blocked {
        content = content.push(offline_reason());
    }
    big_card(content, real)
}

/// Campo del código: monoespaciado y grande, para que se lea letra a letra.
fn code_input(app: &App, real: bool, enabled: bool) -> Element<'_, Message> {
    let mut input = text_input("K7Q2MZ", &app.home.code)
        .padding(Padding::from([6.0, 14.0]))
        .size(20)
        .line_height(LineHeight::Absolute(ui::px(24.0)))
        .font(MONO)
        .class(if app.home.code_error.is_some() {
            InputClass::Invalid
        } else {
            InputClass::Normal
        });
    if real {
        input = input.id("room-code");
        if enabled {
            input = input
                .on_input(|value| on(Msg::Code(value)))
                .on_submit(on(Msg::Join));
        }
    }
    input.into()
}

/// Tarjeta de unirse (`real`: ver [`ui::equal_height_row`]; la copia no lleva el `id` del campo
/// ni mensajes).
fn join_card(app: &App, real: bool) -> Element<'_, Message> {
    let blocked = server_down(app);
    let mut content = column![
        big_card_header(Icon::LogIn, t!("home-join-title"), t!("home-join-text")),
        row![
            container(code_input(app, real, !blocked)).width(Length::Fill),
            ui::button_with(
                Kind::Primary,
                t!("home-enter"),
                Some(Icon::LogIn),
                (real && !blocked).then(|| on(Msg::Join))
            )
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center)
    ];
    if let Some(error) = &app.home.code_error {
        content = content.push(
            row![
                icon_with(Icon::CircleAlert, 14.0, SvgClass::Tone(Tone::Error)),
                text(error.as_str())
                    .size(type_size::SECONDARY)
                    .class(TextClass::Tone(Tone::Error))
                    .wrapping(Wrapping::WordOrGlyph)
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        );
    }
    if blocked {
        content = content.push(offline_reason());
    }
    big_card(content, real)
}

/// Tarjeta de una invitación que llega, con el borde del acento y su cuenta atrás.
fn invitation_card<'a>(app: &'a App, invitation: &'a Invitation) -> Element<'a, Message> {
    let ago = app
        .now
        .saturating_duration_since(received_at(app, invitation))
        .min(INVITATION_TTL);
    let left = INVITATION_TTL.saturating_sub(ago).as_secs().max(1);
    let blocked = server_down(app);
    container(
        row![
            ui::avatar(&app.theme, &invitation.from_name, size::AVATAR),
            column![
                ui::strong(t!("home-invite-title", name = invitation.from_name))
                    .wrapping(Wrapping::WordOrGlyph),
                ui::secondary(t!(
                    "home-invite-detail",
                    code = invitation.code,
                    ago = ago.as_secs(),
                    left = left
                ))
                .wrapping(Wrapping::WordOrGlyph)
            ]
            .spacing(2)
            .width(Length::Fill),
            ui::flat(
                t!("home-ignore"),
                Some(on(Msg::Ignore(invitation.from_user)))
            ),
            ui::button_with(
                Kind::Primary,
                t!("home-enter"),
                Some(Icon::LogIn),
                (!blocked).then(|| on(Msg::Accept(invitation.code.clone())))
            )
        ]
        .spacing(space::S)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([12.0, 16.0]))
    .width(Length::Fill)
    .class(ContainerClass::CardAccent)
    .into()
}

/// «3 conectados · actualizado hace 2 s».
fn friends_summary(app: &App, friends: &Friends) -> String {
    let online = friends.friends.iter().filter(|f| f.online).count();
    let count = match online {
        0 => t!("home-online-none").to_owned(),
        1 => t!("home-online-one").to_owned(),
        count => t!("home-online-many", count = count),
    };
    let Some(at) = app.home.friends_updated else {
        return count;
    };
    let elapsed = app.now.saturating_duration_since(at).as_secs();
    let updated = match elapsed {
        0 => t!("home-updated-now").to_owned(),
        1..60 => t!("home-updated-seconds", seconds = elapsed),
        60..3600 => t!("home-updated-minutes", minutes = elapsed / 60),
        _ => t!("home-updated-hours", hours = elapsed / 3600),
    };
    format!("{count} · {updated}")
}

fn friends_section(app: &App) -> Element<'_, Message> {
    let state = &app.friends_page;
    let mut title = row![ui::heading(t!("home-friends"))]
        .spacing(space::XS)
        .align_y(Alignment::Center);
    if let Some(friends) = app.friends.loaded() {
        title = title.push(ui::caption(friends_summary(app, friends)));
    }
    let header = row![
        title.width(Length::Fill),
        ui::button_with(
            Kind::Link,
            if state.adding {
                t!("home-add-friend-close")
            } else {
                t!("home-add-friend")
            },
            (!state.adding).then_some(Icon::UserPlus),
            Some(on_friends(FriendsMsg::ToggleAdd))
        )
    ]
    .spacing(space::S)
    .align_y(Alignment::Center);
    let mut section = column![header].spacing(space::S);
    if state.adding {
        section = section.push(add_friend(app));
    }
    match &app.friends {
        Load::Idle | Load::Loading => {
            section = section.push(ui::skeleton(t!("home-loading"), 3));
        }
        Load::Failed(err) => {
            section = section.push(ui::card(ui::error_state(
                t!("home-load-failed"),
                err.as_str(),
                Some((t!("home-retry"), on_friends(FriendsMsg::Reload))),
            )));
        }
        Load::Loaded(friends) => {
            if let Some(requests) = requests(app, friends) {
                section = section.push(requests);
            }
            section = section.push(if friends.friends.is_empty() {
                ui::card(ui::empty_state(
                    Icon::Users,
                    t!("home-empty-title"),
                    t!("home-empty-text"),
                    (!state.adding).then(|| {
                        ui::button_with(
                            Kind::Normal,
                            t!("home-add-friend"),
                            Some(Icon::UserPlus),
                            Some(on_friends(FriendsMsg::ToggleAdd)),
                        )
                        .into()
                    }),
                ))
            } else {
                ui::boxed_list(
                    sorted_friends(friends)
                        .into_iter()
                        .map(|friend| friend_row(app, friend))
                        .collect(),
                )
            });
        }
    }
    section.into()
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
                        .on_input(|value| on_friends(FriendsMsg::AddName(value)))
                        .on_submit((!state.busy).then(|| on_friends(FriendsMsg::Add)))
                        .error(state.add_error.as_deref())
                        .id("friend-name")
                ))
                .width(Length::Fill),
                ui::button_with(
                    Kind::Primary,
                    t!("home-send-request"),
                    Some(Icon::UserPlus),
                    (!state.busy).then(|| on_friends(FriendsMsg::Add))
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
            Some(ui::avatar(
                &app.theme,
                &request.user.display_name,
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
                    (!busy).then(|| on_friends(FriendsMsg::Decline(request.id))),
                )
                .into(),
                ui::primary(
                    t!("home-accept"),
                    (!busy).then(|| on_friends(FriendsMsg::Accept(request.id))),
                )
                .into(),
            ],
        )
    });
    let outgoing = friends.outgoing.iter().map(|request| {
        ui::list_row(
            Some(ui::avatar(
                &app.theme,
                &request.user.display_name,
                size::AVATAR,
            )),
            request.user.display_name.as_str(),
            Some(t!("home-pending", username = request.user.username)),
            vec![
                ui::flat(
                    t!("home-withdraw"),
                    (!busy).then(|| on_friends(FriendsMsg::Decline(request.id))),
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

/// Fila de un amigo: avatar, nombre, usuario, estado, «Invitar» (o «Pedir entrar» si tiene una
/// sala abierta y aquí no hay ninguna) y quitar.
fn friend_row<'a>(app: &'a App, friend: &'a Friend) -> Element<'a, Message> {
    let badge = activity_badge(friend);
    let invite_button = |on_press: Option<Message>| {
        ui::button_with(Kind::Normal, t!("home-invite"), None, on_press)
    };
    let hosting = friend.online && friend.activity == Activity::Hosting;
    let invite: Element<'_, Message> = if hosting && can_ask(app) {
        if app.home.asked_at.contains_key(&friend.id) {
            ui::with_tooltip(
                ui::button_with(Kind::Normal, t!("home-asked"), Some(Icon::LogIn), None),
                t!("home-asked-tip", name = friend.display_name),
            )
        } else {
            ui::button_with(
                Kind::Primary,
                t!("home-ask-to-join"),
                Some(Icon::LogIn),
                (!server_down(app)).then(|| on(Msg::AskToJoin(friend.id))),
            )
            .into()
        }
    } else if !friend.online {
        ui::with_tooltip(invite_button(None), t!("home-invite-offline-tip"))
    } else if !app.host.is_open() {
        ui::with_tooltip(
            invite_button(Some(on(Msg::Invite(friend.id)))),
            t!("home-invite-no-room-tip"),
        )
    } else if room_code(app).is_none() {
        ui::with_tooltip(invite_button(None), t!("home-invite-opening-tip"))
    } else {
        invite_button(Some(on(Msg::Invite(friend.id)))).into()
    };
    ui::list_row(
        Some(ui::avatar(&app.theme, &friend.display_name, size::AVATAR)),
        friend.display_name.as_str(),
        Some(format!("@{}", friend.username)),
        vec![
            badge,
            invite,
            ui::icon_button(
                Icon::Trash,
                t!("home-remove-friend"),
                Some(on_friends(FriendsMsg::Remove {
                    user: friend.id,
                    name: friend.display_name.clone(),
                })),
            ),
        ],
    )
}

/// Escenas de Inicio para las capturas.
pub(crate) mod shots {
    use std::time::Duration;

    use super::Msg;
    use crate::ui::app::{App, Event, Invitation, Message, Page};
    use crate::ui::shots::{Builder, signed_in};
    use coroom_transport::proto::api::Friends;
    use coroom_transport::proto::signal::Activity;

    /// Hace `secs` segundos (o ahora, si el reloj no llega tan atrás).
    fn ago(app: &App, secs: u64) -> std::time::Instant {
        app.now
            .checked_sub(Duration::from_secs(secs))
            .unwrap_or(app.now)
    }

    /// Inicio con una invitación de Bea de hace 8 s, amigos (Bea con su sala abierta, Carla
    /// jugando en una) y solicitudes pendientes.
    pub(crate) fn home(app: &mut App) {
        signed_in(app, Page::Home);
        app.update(Message::Event(Event::ServerStatus {
            rtt_ms: Some(12.0),
            online: true,
        }));
        app.update(Message::Event(Event::Activity {
            user: 2,
            activity: Activity::Hosting,
        }));
        app.update(Message::Event(Event::Activity {
            user: 3,
            activity: Activity::Playing,
        }));
        app.invitations = vec![Invitation {
            from_user: 2,
            from_name: "Bea".to_owned(),
            code: "K7Q2MZ".to_owned(),
        }];
        app.home.invited_at.insert(2, ago(app, 8));
        app.home.friends_updated = Some(ago(app, 2));
        app.home.code = "K7Q2MZ".to_owned();
        // Aviso de versión nueva.
        app.update.available = Some(crate::update::Update {
            version: "0.2.0".to_owned(),
            url: format!("{}/tag/v0.2.0", crate::update::RELEASES_PAGE),
        });
    }

    /// Inicio tras pedirle entrar a Bea: «Pedido…» en su fila.
    pub(crate) fn home_asked(app: &mut App) {
        home(app);
        app.invitations.clear();
        app.home.invited_at.clear();
        app.update(Message::Home(Msg::AskToJoin(2)));
        app.home.asked_at.insert(2, ago(app, 4));
        app.toasts.clear();
    }

    pub(crate) fn home_empty(app: &mut App) {
        signed_in(app, Page::Home);
        app.update(Message::Event(Event::Friends(Ok(Friends {
            friends: Vec::new(),
            incoming: Vec::new(),
            outgoing: Vec::new(),
        }))));
        app.home.friends_updated = Some(ago(app, 5));
        app.usage = None;
    }

    pub(crate) fn home_offline(app: &mut App) {
        signed_in(app, Page::Home);
        app.update(Message::Event(Event::ServerStatus {
            rtt_ms: None,
            online: false,
        }));
        app.home.friends_updated = Some(ago(app, 75));
    }

    pub(crate) fn add_friend(app: &mut App) {
        signed_in(app, Page::Home);
        app.home.friends_updated = Some(ago(app, 2));
        app.friends_page.adding = true;
        app.friends_page.add_name = "hugo".to_owned();
        app.friends_page.add_error = Some("No existe ningún usuario «hugo».".to_owned());
    }

    /// Escenas de esta pantalla.
    pub(crate) const BUILDERS: &[(&str, Builder)] = &[
        ("05-inicio", home),
        ("05-inicio-menu-cuenta", |app| {
            home(app);
            app.update(Message::ToggleAccountMenu);
        }),
        ("05-inicio-pedido", home_asked),
        ("06-inicio-vacio", home_empty),
        ("06-inicio-sin-servidor", home_offline),
        ("07-agregar-amigo", add_friend),
    ];
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::tests::app;
    use crate::ui::screens::host::SourceChoice;

    fn friend(id: u64, name: &str, online: bool) -> Friend {
        Friend {
            id,
            username: name.to_lowercase(),
            display_name: name.to_owned(),
            online,
            activity: Default::default(),
        }
    }

    fn hosting(id: u64, name: &str) -> Friend {
        Friend {
            activity: Activity::Hosting,
            ..friend(id, name, true)
        }
    }

    /// Inicio con la lista de amigos cargada: Bea con su sala abierta y Carla disponible.
    fn with_friends(app: &mut App) {
        app.update(Message::Event(Event::Friends(Ok(Friends {
            friends: vec![hosting(2, "Bea"), friend(3, "Carla", true)],
            incoming: Vec::new(),
            outgoing: Vec::new(),
        }))));
        app.toasts.clear();
    }

    fn main_app() -> App {
        let mut app = app();
        app.screen = Screen::Main;
        app.page = Page::Home;
        app
    }

    fn invited(app: &mut App, user: u64, code: &str) {
        app.update(Message::Event(Event::Invited(Invitation {
            from_user: user,
            from_name: "Bea".into(),
            code: code.into(),
        })));
    }

    #[test]
    fn room_codes_are_cleaned_and_validated() {
        assert_eq!(clean_code(" k7q-2mz9 "), "K7Q2MZ");
        let mut app = main_app();
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
        // Escribir de nuevo quita el error.
        app.update(Message::Home(Msg::Code("k7q2mz".into())));
        assert_eq!(app.home.code_error, None);
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
        let mut app = main_app();
        invited(&mut app, 3, "K7Q2MZ");
        assert_eq!(app.home.invited_at.get(&3), Some(&app.now));
        let effects = app.update(Message::Home(Msg::Accept("K7Q2MZ".into())));
        assert!(effects.contains(&Effect::Join("K7Q2MZ".into())));
        assert!(app.invitations.is_empty());
        assert!(app.home.invited_at.is_empty());
    }

    #[test]
    fn ignoring_an_invitation_only_drops_it() {
        let mut app = main_app();
        invited(&mut app, 3, "K7Q2MZ");
        invited(&mut app, 4, "BBBBBB");
        assert!(app.update(Message::Home(Msg::Ignore(3))).is_empty());
        assert_eq!(app.invitations.len(), 1);
        assert_eq!(app.invitations[0].from_user, 4);
        assert!(!app.home.invited_at.contains_key(&3));
        assert_eq!(app.screen, Screen::Main);
    }

    #[test]
    fn invitations_count_down_and_expire() {
        let mut app = main_app();
        let start = app.now;
        invited(&mut app, 3, "K7Q2MZ");
        app.toasts.clear();
        // Con Inicio a la vista, un tick por segundo para la cuenta atrás.
        assert_eq!(app.next_tick(), Some(start + Duration::from_secs(1)));
        app.now = start + Duration::from_millis(8_400);
        assert_eq!(app.next_tick(), Some(start + Duration::from_secs(9)));
        app.update(Message::Tick);
        assert_eq!(app.invitations.len(), 1);
        // Fuera de Inicio solo hace falta el de la caducidad.
        app.page = Page::Settings;
        assert_eq!(app.next_tick(), Some(start + INVITATION_TTL));
        app.now = start + INVITATION_TTL;
        app.update(Message::Tick);
        assert!(app.invitations.is_empty(), "caducó");
        assert!(app.home.invited_at.is_empty());
        assert_eq!(app.next_tick(), None);
    }

    #[test]
    fn a_new_invitation_from_the_same_friend_restarts_the_count() {
        let mut app = main_app();
        let start = app.now;
        invited(&mut app, 3, "AAAAAA");
        app.now = start + Duration::from_secs(50);
        invited(&mut app, 3, "BBBBBB");
        app.now = start + Duration::from_secs(70);
        app.update(Message::Tick);
        assert_eq!(app.invitations.len(), 1);
        assert_eq!(app.invitations[0].code, "BBBBBB");
    }

    #[test]
    fn friends_update_time_is_recorded_and_ticks_slow_down() {
        let mut app = main_app();
        let start = app.now;
        app.update(Message::Event(Event::Friends(Ok(Friends {
            friends: vec![friend(2, "Bea", false)],
            incoming: Vec::new(),
            outgoing: Vec::new(),
        }))));
        assert_eq!(app.home.friends_updated, Some(start));
        assert_eq!(app.next_tick(), Some(start + Duration::from_secs(1)));
        app.now = start + Duration::from_secs(90);
        assert_eq!(app.next_tick(), Some(start + Duration::from_secs(120)));
        // La presencia también cuenta como actualización (y llega con la sala abierta).
        app.update(Message::Event(Event::Presence {
            user: 2,
            online: true,
        }));
        assert_eq!(app.home.friends_updated, Some(app.now));
        assert!(app.friends.loaded().is_some_and(|f| f.friends[0].online));
        let summary = friends_summary(&app, app.friends.loaded().unwrap());
        assert!(summary.starts_with("1 conectado"), "{summary}");
    }

    #[test]
    fn friends_sort_online_first_then_by_name() {
        let friends = Friends {
            friends: vec![
                friend(5, "eli", false),
                friend(3, "Carla", true),
                friend(4, "Dani", false),
                friend(2, "bea", true),
                friend(6, "Bea", true),
            ],
            incoming: Vec::new(),
            outgoing: Vec::new(),
        };
        let order: Vec<u64> = sorted_friends(&friends).iter().map(|f| f.id).collect();
        assert_eq!(order, [2, 6, 3, 4, 5]);
    }

    /// Entre los conectados, los que tienen una sala abierta van primero; jugar en una no cuenta
    /// y un desconectado con actividad (no debería pasar) sigue al final.
    #[test]
    fn friends_with_an_open_room_sort_first_among_the_online() {
        let friends = Friends {
            friends: vec![
                friend(2, "Bea", true),
                hosting(5, "Eli"),
                Friend {
                    activity: Activity::Playing,
                    ..friend(1, "Ana", true)
                },
                Friend {
                    activity: Activity::Hosting,
                    ..friend(4, "Dani", false)
                },
                hosting(3, "Carla"),
            ],
            incoming: Vec::new(),
            outgoing: Vec::new(),
        };
        let order: Vec<u64> = sorted_friends(&friends).iter().map(|f| f.id).collect();
        assert_eq!(order, [3, 5, 1, 2, 4]);
    }

    /// La actividad llega por la señalización y cambia la insignia; desconectarse la borra.
    #[test]
    fn activity_updates_the_friend_and_counts_as_an_update() {
        let mut app = main_app();
        with_friends(&mut app);
        app.now += Duration::from_secs(3);
        app.update(Message::Event(Event::Activity {
            user: 3,
            activity: Activity::Playing,
        }));
        assert_eq!(app.home.friends_updated, Some(app.now));
        let carla = |app: &App| app.friends.loaded().unwrap().friends[1].clone();
        assert_eq!(carla(&app).activity, Activity::Playing);
        app.update(Message::Event(Event::Presence {
            user: 3,
            online: false,
        }));
        assert!(!carla(&app).online);
        assert_eq!(carla(&app).activity, Activity::Idle, "desconectado: nada");
    }

    /// Pedir entrar: solo a un amigo con sala abierta, sin sala propia ni estar en otra; una vez
    /// hasta que conteste.
    #[test]
    fn ask_to_join_needs_a_hosting_friend_and_no_room() {
        let mut app = main_app();
        with_friends(&mut app);
        // Carla está disponible, sin sala: nada que pedir.
        assert!(app.update(Message::Home(Msg::AskToJoin(3))).is_empty());
        assert!(app.home.asked_at.is_empty());
        // Con la sala propia abriéndose, tampoco.
        app.host.starting = true;
        assert!(app.update(Message::Home(Msg::AskToJoin(2))).is_empty());
        app.host.starting = false;
        // Bea tiene su sala abierta: se le pide.
        let effects = app.update(Message::Home(Msg::AskToJoin(2)));
        assert_eq!(effects, [Effect::AskToJoin(2)]);
        assert_eq!(app.home.asked_at.get(&2), Some(&app.now));
        assert!(app.toasts.iter().any(|t| t.text.contains("Bea")));
        // Repetir mientras espera no manda nada.
        assert!(app.update(Message::Home(Msg::AskToJoin(2))).is_empty());
    }

    /// Sin respuesta, la petición caduca con un aviso (y el `Tick` se pide para entonces).
    #[test]
    fn a_pending_ask_expires() {
        let mut app = main_app();
        with_friends(&mut app);
        let start = app.now;
        app.update(Message::Home(Msg::AskToJoin(2)));
        app.toasts.clear();
        app.page = Page::Settings;
        assert_eq!(app.next_tick(), Some(start + JOIN_REQUEST_TTL));
        app.now = start + JOIN_REQUEST_TTL - Duration::from_secs(1);
        app.update(Message::Tick);
        assert!(app.home.asked_at.contains_key(&2), "todavía espera");
        app.now = start + JOIN_REQUEST_TTL;
        app.update(Message::Tick);
        assert!(app.home.asked_at.is_empty());
        assert!(
            app.toasts
                .iter()
                .any(|t| t.text.contains("Bea") && t.text.contains("no respondió")),
            "{:?}",
            app.toasts
        );
        assert_eq!(next_tick(&app), None, "ya no hay nada que esperar");
    }

    /// La invitación del anfitrión al que se le pidió entrar es la respuesta: se entra sin tocar
    /// nada; una invitación de otro sigue siendo una tarjeta.
    #[test]
    fn an_invitation_from_the_asked_host_joins_at_once() {
        let mut app = main_app();
        with_friends(&mut app);
        app.update(Message::Home(Msg::AskToJoin(2)));
        app.toasts.clear();
        invited(&mut app, 3, "CCCCCC");
        assert_eq!(app.invitations.len(), 1, "Carla no fue la pedida");
        assert_eq!(app.screen, Screen::Main);
        let effects = app.update(Message::Event(Event::Invited(Invitation {
            from_user: 2,
            from_name: "Bea".into(),
            code: "K7Q2MZ".into(),
        })));
        assert!(
            effects.contains(&Effect::Join("K7Q2MZ".into())),
            "{effects:?}"
        );
        assert_eq!(app.screen, Screen::Guest);
        assert!(app.home.asked_at.is_empty());
        assert_eq!(app.invitations.len(), 1, "la de Carla sigue");
        assert!(
            app.toasts
                .iter()
                .any(|t| t.text.contains("Bea te deja entrar")),
            "{:?}",
            app.toasts
        );
    }

    /// Un «ahora no» quita la espera y avisa; que el anfitrión cierre la sala o se desconecte,
    /// también la quita (sin aviso).
    #[test]
    fn a_decline_clears_the_pending_ask() {
        let mut app = main_app();
        with_friends(&mut app);
        app.update(Message::Home(Msg::AskToJoin(2)));
        app.toasts.clear();
        assert!(
            app.update(Message::Event(Event::JoinDeclined {
                from_user: 2,
                from_name: "Bea".into(),
            }))
            .is_empty()
        );
        assert!(app.home.asked_at.is_empty());
        assert!(
            app.toasts
                .iter()
                .any(|t| t.text.contains("Bea no puede dejarte entrar ahora")),
            "{:?}",
            app.toasts
        );
        app.update(Message::Home(Msg::AskToJoin(2)));
        app.update(Message::Event(Event::Activity {
            user: 2,
            activity: Activity::Idle,
        }));
        assert!(app.home.asked_at.is_empty(), "cerró la sala");
        // Un error de la sala de espera también la da por perdida.
        app.update(Message::Event(Event::Activity {
            user: 2,
            activity: Activity::Hosting,
        }));
        app.update(Message::Home(Msg::AskToJoin(2)));
        app.toasts.clear();
        app.update(Message::Event(Event::JoinRequestFailed {
            message: "tu amigo no tiene ninguna sala abierta".into(),
        }));
        assert!(app.home.asked_at.is_empty());
        assert_eq!(app.toasts.len(), 1);
    }

    /// «Compartir» pasa por `Navigate`: pide ya qué se puede compartir.
    #[test]
    fn share_asks_for_what_can_be_shared() {
        let mut app = main_app();
        assert_eq!(
            app.update(Message::Home(Msg::Share)),
            [Effect::LoadShareOptions(SourceChoice::Screen)]
        );
        assert_eq!(app.page, Page::Host);
        assert!(app.host.refreshing);
    }

    /// Sin sala, «Invitar» lleva a Compartir; con la sala abierta, invita.
    #[test]
    fn invite_needs_an_open_room() {
        let mut app = main_app();
        let effects = app.update(Message::Home(Msg::Invite(2)));
        assert_eq!(effects, [Effect::LoadShareOptions(SourceChoice::Screen)]);
        assert_eq!(app.page, Page::Host);
        // Abriéndose (sin código todavía), nada.
        app.page = Page::Home;
        app.host.starting = true;
        assert!(app.update(Message::Home(Msg::Invite(2))).is_empty());
        assert_eq!(app.page, Page::Home);
    }

    #[test]
    fn server_down_blocks_share_and_join() {
        let mut app = main_app();
        app.update(Message::Event(Event::ServerStatus {
            rtt_ms: None,
            online: false,
        }));
        assert!(app.update(Message::Home(Msg::Share)).is_empty());
        assert_eq!(app.page, Page::Home);
        app.update(Message::Home(Msg::Code("K7Q2MZ".into())));
        assert!(app.update(Message::Home(Msg::Join)).is_empty());
        invited(&mut app, 3, "K7Q2MZ");
        assert!(
            app.update(Message::Home(Msg::Accept("K7Q2MZ".into())))
                .is_empty()
        );
        assert_eq!(app.invitations.len(), 1, "la invitación sigue");
        // Vuelve el servidor: ya se puede.
        app.update(Message::Event(Event::ServerStatus {
            rtt_ms: Some(10.0),
            online: true,
        }));
        assert!(
            app.update(Message::Home(Msg::Join))
                .contains(&Effect::Join("K7Q2MZ".into()))
        );
    }

    #[test]
    fn add_friend_toggles_the_field() {
        let mut app = main_app();
        app.update(Message::Friends(FriendsMsg::ToggleAdd));
        assert!(app.friends_page.adding);
        app.update(Message::Friends(FriendsMsg::AddName("ana".into())));
        app.account = Some(crate::ui::app::AccountInfo {
            server: crate::ui::shots::server_entry(),
            username: "ana".into(),
            display_name: "Ana".into(),
            user_id: 1,
            device: "equipo".into(),
            fingerprint: "a1".repeat(32),
        });
        assert!(app.update(Message::Friends(FriendsMsg::Add)).is_empty());
        assert!(app.friends_page.add_error.is_some(), "no a uno mismo");
        app.update(Message::Friends(FriendsMsg::AddName("bea".into())));
        assert_eq!(
            app.update(Message::Friends(FriendsMsg::Add)),
            [Effect::AddFriend("bea".into())]
        );
    }
}
