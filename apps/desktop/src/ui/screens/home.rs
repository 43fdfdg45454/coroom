//! Inicio: lo primero del riel.
//!
//! Arriba, dos bloques desiguales: «Compartir mi pantalla», el principal, con su botón y la fila
//! de butacas de **tu sala** (la tuya encendida y las libres numeradas con la sala cerrada; quien
//! está de verdad, con su lámpara, si la sala está abierta en este equipo) y su estado en
//! palabras; y «Unirse con código». Debajo, la **bandeja**, con lo que llega de alguien y vence
//! (las invitaciones a una sala, como entradas con su código y su cuenta atrás de
//! [`INVITATION_TTL`]; las solicitudes de amistad, como filas), y **En línea ahora**, los amigos
//! conectados en una fila de butacas. Los avisos no son de la bandeja: el que impide usar el
//! servidor va arriba del todo y los de la versión nueva y el relay, al final. Cada entrada de la
//! bandeja cae desde arriba al llegar y se pliega con su altura al vencer o descartarla: lo que
//! hay debajo se desliza en lugar de saltar. La lista completa de amigos está en su página
//! ([`super::friends`]); aquí queda la lógica de invitar y de **pedir entrar** en la sala de un
//! amigo ([`Msg::AskToJoin`]): el botón queda en «Pedido…» hasta que invita (se entra solo), dice
//! que no o pasa [`JOIN_REQUEST_TTL`].

use std::collections::HashMap;
use std::time::{Duration, Instant};

use coroom_transport::proto::api::{Friend, Friends};
use coroom_transport::proto::signal::{Activity, OwnRoom, ROOM_CODE_LEN, is_valid_room_code};
use coroom_ui::Element;
use coroom_ui::animate::{Collapse, Shift};
use coroom_ui::font::SEMIBOLD;
use coroom_ui::icon::{Icon, icon_with};
use coroom_ui::motion::{EXIT_EASING, Token};
use coroom_ui::style::{ButtonClass, ContainerClass, InputClass, Surface, SvgClass, TextClass};
use coroom_ui::theme::Tone;
use coroom_ui::tokens::{seat, size, space, text as type_size};
use coroom_ui::widget::{self as ui, CodeSize, Kind};
use iced_core::text::{LineHeight, Wrapping};
use iced_core::{Alignment, Length, Padding};
use iced_widget::{Column, Row, Space, column, container, row, text, text_input};

use crate::ui::app::{
    App, Effect, Event, Invitation, JOIN_REQUEST_TTL, Load, Message, Page, Screen,
};
use crate::ui::avatars;
use crate::ui::screens::friends::{self, Msg as FriendsMsg};
use crate::ui::screens::guest;
use crate::ui::stats;

/// Lo que dura una invitación en Inicio: pasado ese tiempo desaparece sola.
pub const INVITATION_TTL: Duration = Duration::from_secs(60);

/// Invitaciones que se ven a la vez (las más nuevas).
const INVITATIONS_SHOWN: usize = 3;

/// Amigos conectados que se ven en «En línea ahora» (el resto, en Amigos).
const ONLINE_SHOWN: usize = 12;

/// Butacas de personas que caben en la fila de «tu sala» (si hay más, se dice cuántas faltan).
const ROOM_SEATS: usize = 5;

/// Lado de las butacas de la fila de «tu sala»: las de una fila de sala, con el nombre debajo.
const ROOM_SEAT: f32 = seat::ROW;

/// El código de ejemplo del campo (uno cualquiera con forma de código de sala), de tres en tres
/// como se lee en el resto de la aplicación: al escribir, el espacio se ignora
/// ([`clean_code`]).
const CODE_EXAMPLE: &str = "R4W 8TX";

/// Ancho como mucho del campo del código (en una ventana estrecha la entrada ocupa todo el ancho
/// y el campo no tiene por qué).
const CODE_INPUT_MAX: f32 = 280.0;

/// Desde qué ancho del contenido los dos bloques de arriba van lado a lado: con menos, la
/// entrada del código no cabría con holgura en sus 2/5.
const HERO_ROW_FROM: f32 = 840.0;

/// Margen interior de los dos bloques de arriba: a los lados y, algo menos, arriba y abajo (la
/// primera vista de Inicio cabe sin desplazar en la ventana de arranque).
const BLOCK_PADDING: Padding = Padding {
    top: space::M,
    right: space::M + space::XXS,
    bottom: space::M,
    left: space::M + space::XXS,
};

/// Alto del título y la explicación de la entrada del código, cada uno en una línea.
const JOIN_HEADER: f32 = 24.0 + 2.0 + 18.0;

/// Alto del campo del código.
const CODE_INPUT_HEIGHT: f32 = 44.0;

/// Aire encima de cada sección de la página (más que debajo de su título).
const SECTION_GAP: f32 = space::XL;

/// Tamaño de la letra del campo del código.
const CODE_INPUT_TEXT: f32 = 22.0;

/// Una invitación que se va de la bandeja (venció o se descartó) y se pliega con su altura.
#[derive(Debug, Clone)]
pub struct Leaving {
    /// La invitación.
    pub invitation: Invitation,
    /// Cuándo llegó (para la cuenta atrás congelada).
    pub received: Instant,
    /// Cuándo empezó a irse.
    pub since: Instant,
}

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
    /// Invitaciones que se van de la bandeja, plegándose.
    pub leaving: Vec<Leaving>,
    /// Cuándo apareció cada solicitud de amistad recibida (por solicitud: cae desde arriba).
    pub requests_seen: HashMap<u64, Instant>,
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
    /// Ir a Amigos con el campo de «Agregar amigo» desplegado.
    AddFriend,
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
pub(crate) fn can_ask(app: &App) -> bool {
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
            if server_down(app) || app.host.is_open() || app.guest.is_some() {
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
            if server_down(app) || app.host.is_open() || app.guest.is_some() {
                return Vec::new();
            }
            app.invitations.retain(|invitation| invitation.code != code);
            prune_times(app);
            guest::start(app, code)
        }
        Msg::Ignore(user) => {
            let now = app.now;
            let (gone, kept): (Vec<Invitation>, Vec<Invitation>) =
                std::mem::take(&mut app.invitations)
                    .into_iter()
                    .partition(|invitation| invitation.from_user == user);
            app.invitations = kept;
            for invitation in gone {
                start_leaving(app, invitation, now);
            }
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
        Msg::AddFriend => {
            let mut effects = app.update(Message::Navigate(Page::Friends));
            if !app.friends_page.adding {
                effects.extend(app.update(Message::Friends(FriendsMsg::ToggleAdd)));
            }
            effects
        }
    }
}

/// Código de la sala abierta, si el servidor ya la creó.
pub(crate) fn room_code(app: &App) -> Option<&str> {
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
            if let Event::Friends(Ok(list)) = event {
                // Las solicitudes nuevas caen en la bandeja; las que ya no están se olvidan. Las
                // de la primera lista ya estaban: se ven quietas.
                let now = if app.friends.loaded().is_some() {
                    app.now
                } else {
                    app.now
                        .checked_sub(Duration::from_secs(60))
                        .unwrap_or(app.now)
                };
                let seen = &mut app.home.requests_seen;
                seen.retain(|id, _| list.incoming.iter().any(|request| request.id == *id));
                for request in &list.incoming {
                    seen.entry(request.id).or_insert(now);
                }
            }
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

/// Una invitación empieza a irse de la bandeja (se pliega con su altura).
fn start_leaving(app: &mut App, invitation: Invitation, now: Instant) {
    let received = received_at(app, &invitation);
    if app.motion().exit_duration(Token::Panel).is_zero() {
        return;
    }
    app.home.leaving.push(Leaving {
        invitation,
        received,
        since: now,
    });
}

/// Lo que tarda en plegarse una entrada que se va.
fn exit_duration(app: &App) -> Duration {
    app.motion().exit_duration(Token::Panel)
}

/// Pasa el tiempo: las invitaciones caducadas se pliegan y desaparecen, y las peticiones de
/// entrar sin respuesta se dan por perdidas (con un aviso).
pub fn tick(app: &mut App) {
    let now = app.now;
    let times = &mut app.home.invited_at;
    for invitation in &app.invitations {
        times.entry(invitation.from_user).or_insert(now);
    }
    let (expired, kept): (Vec<Invitation>, Vec<Invitation>) = std::mem::take(&mut app.invitations)
        .into_iter()
        .partition(|invitation| {
            times
                .get(&invitation.from_user)
                .is_some_and(|at| now.saturating_duration_since(*at) >= INVITATION_TTL)
        });
    app.invitations = kept;
    for invitation in expired {
        start_leaving(app, invitation, now);
    }
    let exit = exit_duration(app);
    app.home
        .leaving
        .retain(|leaving| now.saturating_duration_since(leaving.since) < exit);
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

/// Cuándo hace falta el próximo `Tick`: al caducar cada invitación, al acabar de plegarse las
/// que se van y, con Inicio a la vista, cuando cambia la cuenta atrás (siempre después de `now`).
/// El «actualizado hace…» de los amigos va al pasar el ratón: no pide ninguno.
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
    let exit = exit_duration(app);
    for leaving in &app.home.leaving {
        consider((leaving.since + exit).max(now + Duration::from_millis(1)));
    }
    for at in app.home.asked_at.values() {
        consider((*at + JOIN_REQUEST_TTL).max(now + Duration::from_millis(1)));
    }
    next
}

/// Lo que lleva de su entrada una entrada de la bandeja que llegó en `since` (0 a 1, con su curva).
fn entered(app: &App, since: Instant) -> f32 {
    let duration = app.motion().duration(Token::Panel);
    if duration.is_zero() {
        return 1.0;
    }
    let t = (app.now.saturating_duration_since(since).as_secs_f32() / duration.as_secs_f32())
        .clamp(0.0, 1.0);
    if t >= 1.0 {
        1.0
    } else {
        Token::Panel.easing().value(t)
    }
}

/// Lo que lleva de su salida una entrada que empezó a irse en `since` (0 a 1).
fn left(app: &App, since: Instant) -> f32 {
    let duration = exit_duration(app);
    if duration.is_zero() {
        return 1.0;
    }
    let t = (app.now.saturating_duration_since(since).as_secs_f32() / duration.as_secs_f32())
        .clamp(0.0, 1.0);
    if t >= 1.0 { 1.0 } else { EXIT_EASING.value(t) }
}

/// Si algo de la bandeja se mueve (una entrada que cae o una que se pliega), con Inicio delante.
pub fn animating(app: &App) -> bool {
    if app.screen != Screen::Main || app.page != Page::Home {
        return false;
    }
    let arriving = app
        .invitations
        .iter()
        .map(|invitation| received_at(app, invitation))
        .chain(app.home.requests_seen.values().copied())
        .any(|since| entered(app, since) < 1.0);
    arriving
        || app
            .home
            .leaving
            .iter()
            .any(|leaving| left(app, leaving.since) < 1.0)
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

/// Dibuja la página: arriba, compartir (el bloque grande, con la fila de butacas de tu sala) y
/// unirse con un código (una entrada); debajo, la bandeja y quién está en línea. Lo que impide
/// usar el servidor se dice arriba del todo; los avisos que no corren prisa (la versión nueva, el
/// relay), al final.
pub fn view<'a>(app: &'a App) -> Element<'a, Message> {
    let (blocking, notices) = notices(app);
    let below = |content: Element<'a, Message>, top: f32| -> Element<'a, Message> {
        container(content)
            .padding(Padding {
                top,
                ..Padding::ZERO
            })
            .into()
    };
    let blocking = blocking.map(|notice| {
        container(notice)
            .padding(Padding {
                bottom: space::M,
                ..Padding::ZERO
            })
            .into()
    });
    let notices = (!notices.is_empty()).then(|| {
        below(
            Column::with_children(notices).spacing(space::XS).into(),
            SECTION_GAP,
        )
    });
    // Los dos bloques, lado a lado (3/5 y 2/5, igual de altos) mientras la entrada del código
    // quepa con holgura; en una ventana estrecha, uno encima del otro.
    let hero: Element<'a, Message> = if content_width(app) >= HERO_ROW_FROM {
        let portion = |block: Element<'a, Message>, portion: u16, real: bool| {
            container(block)
                .width(Length::FillPortion(portion))
                .height(block_height(real))
                .into()
        };
        ui::equal_height_row(
            |real| {
                vec![
                    portion(share_block(app, real, block_height(real)), 3, real),
                    portion(join_ticket(app, real, block_height(real), false), 2, real),
                ]
            },
            space::M,
        )
    } else {
        column![
            share_block(app, true, Length::Shrink),
            join_ticket(app, true, Length::Shrink, true)
        ]
        .spacing(space::M)
        .into()
    };
    let page = column![
        ui::slot(blocking),
        hero,
        below(tray(app), SECTION_GAP),
        below(online_now(app), SECTION_GAP),
        ui::slot(notices),
    ];
    ui::page("home", page)
}

/// Ancho (lógico) del contenido de la página con la ventana de ahora.
fn content_width(app: &App) -> f32 {
    let rail = if ui::rail_is_wide(app.window.width) {
        ui::RAIL_WIDE
    } else {
        ui::RAIL_NARROW
    };
    (app.window.width - rail - 1.0 - 2.0 * space::XL).min(size::CONTENT)
}

/// Los avisos de Inicio, fuera de la bandeja (no son de nadie ni vencen): el que explica por qué
/// no se puede compartir ni entrar (versiones distintas con el servidor o, si no, el servidor sin
/// conexión, con el diagnóstico a un clic), que va arriba del todo; y los demás (la versión nueva,
/// el uso del relay), que van al final de la página. Estar dentro de la sala de otro lo dice el
/// propio bloque de compartir.
fn notices(app: &App) -> (Option<Element<'_, Message>>, Vec<Element<'_, Message>>) {
    let mut out = Vec::new();
    // Versiones distintas con el servidor; después, la versión nueva ([`super::updates`]).
    let mut client_older = false;
    let mut blocking = None;
    if let Some((host, gap)) = &app.update.protocol {
        blocking = Some(super::updates::protocol_banner(app, host, *gap));
        client_older = matches!(gap, crate::update::ProtocolGap::ClientOlder { .. });
    } else if server_down(app) {
        blocking = Some(ui::banner(
            Tone::Error,
            t!("home-offline-reason"),
            Some((
                t!("app-abrir-el-diagnostico"),
                Message::Diagnostics(crate::ui::screens::diagnostics::Msg::Open),
            )),
        ));
    }
    // Si la app es la vieja, su aviso ya lleva la acción de la versión nueva (un fallo sí se
    // explica aparte).
    let redundant = client_older
        && matches!(
            app.update.updater.home_notice(),
            crate::update::HomeNotice::Available | crate::update::HomeNotice::Ready { .. }
        );
    if !redundant {
        out.extend(super::updates::home_banner(app));
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
    (blocking, out)
}

/// Título y explicación de uno de los dos bloques de arriba.
fn block_header<'a>(
    title: impl iced_core::text::IntoFragment<'a>,
    explanation: &'a str,
) -> Column<'a, Message, coroom_ui::Theme, coroom_ui::Renderer> {
    column![
        ui::heading(title).wrapping(Wrapping::WordOrGlyph),
        ui::secondary(explanation).wrapping(Wrapping::WordOrGlyph)
    ]
    .spacing(2)
    .width(Length::Fill)
}

/// El motivo de «Entrar» desactivado con la sala propia abierta: entrar en otra la dejaría
/// abierta sin nadie que la vea.
fn hosting_reason<'a>() -> Element<'a, Message> {
    row![
        icon_with(Icon::Info, 14.0, SvgClass::Tone(Tone::Info)),
        ui::caption(t!("home-hosting-reason")).wrapping(Wrapping::WordOrGlyph)
    ]
    .spacing(6)
    .align_y(Alignment::Start)
    .into()
}

/// Altura de un bloque de [`ui::equal_height_row`]: el de verdad llena la fila; la copia de
/// debajo, que solo mide, se encoge.
fn block_height(real: bool) -> Length {
    if real { Length::Fill } else { Length::Shrink }
}

/// Lo que separa lo de arriba de un bloque de lo de su pie: si el bloque llena un alto dado, un
/// hueco que crece (el pie queda abajo); si mide lo que su contenido, nada.
fn block_filler<'a>(height: Length) -> Element<'a, Message> {
    ui::slot((height == Length::Fill).then(|| Space::new().height(Length::Fill).into()))
}

/// `content` con `top` de aire encima.
fn spaced<'a>(content: impl Into<Element<'a, Message>>, top: f32) -> Element<'a, Message> {
    container(content)
        .padding(Padding {
            top,
            ..Padding::ZERO
        })
        .width(Length::Fill)
        .into()
}

/// El bloque principal. Sin sala ajena: compartir la pantalla, con la fila de butacas de la sala
/// propia en el centro (quién está sentado, con su nombre) y, al pie, su estado en palabras y el
/// botón principal. Dentro de la sala de otro no hay sala propia que enseñar: dice dónde estás y
/// ofrece volver. `real`: ver [`ui::equal_height_row`]; `height`, el alto del bloque.
fn share_block(app: &App, real: bool, height: Length) -> Element<'_, Message> {
    let open = app.host.is_open();
    let content: Element<'_, Message> = match &app.guest {
        Some(state) if !open => {
            let title = match &state.host_name {
                Some(name) => t!("home-visiting", name = name),
                None => t!("home-visiting-unknown").to_owned(),
            };
            column![
                block_header(title, t!("home-visiting-text")),
                block_filler(height),
                spaced(
                    row![
                        ui::fill(),
                        ui::button_with(
                            Kind::Primary,
                            t!("home-back-to-room"),
                            Some(Icon::ArrowLeft),
                            real.then_some(Message::Navigate(Page::Host))
                        )
                    ],
                    space::M
                ),
            ]
            .into()
        }
        _ => {
            let blocked = server_down(app) && !open;
            let mut header =
                Row::new()
                    .spacing(space::S)
                    .align_y(Alignment::Start)
                    .push(block_header(
                        t!("home-share-title"),
                        if open {
                            t!("home-share-open-text")
                        } else {
                            t!("home-share-text")
                        },
                    ));
            if app.sharing_screen() {
                header = header.push(ui::status_badge(Tone::Live, t!("home-room-live")));
            }
            let (tone, text) = room_state(app);
            let foot = row![
                row![
                    ui::dot(tone),
                    ui::secondary(text).wrapping(Wrapping::WordOrGlyph)
                ]
                .spacing(space::XS)
                .align_y(Alignment::Center)
                .width(Length::Fill),
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
            ]
            .spacing(space::M)
            .align_y(Alignment::Center);
            column![
                header,
                spaced(room_seats(app), space::S),
                block_filler(height),
                spaced(foot, space::S),
            ]
            .into()
        }
    };
    container(content)
        .padding(BLOCK_PADDING)
        .width(Length::Fill)
        .height(height)
        .class(ContainerClass::Card)
        .into()
}

/// El estado de la sala propia: la lámpara y la frase que lo dice.
pub(crate) fn room_state(app: &App) -> (Tone, String) {
    match &app.host.room {
        Some(room) if room.code.is_some() => (
            Tone::Success,
            match room.guests.len() {
                0 => t!("home-room-open-empty").to_owned(),
                1 => t!("home-room-open-one").to_owned(),
                count => t!("home-room-open-many", count = count),
            },
        ),
        _ if app.host.is_open() => (Tone::Warning, t!("home-room-opening").to_owned()),
        _ => (Tone::Neutral, t!("home-room-closed").to_owned()),
    }
}

/// La fila de butacas de «tu sala», con el nombre de cada uno debajo. La tuya va primero, con
/// «Tú». Cerrada, las demás van libres y numeradas (aquí una butaca libre es alguien a quien
/// puedes invitar). Abierta en este equipo: quien está de verdad, con la lámpara de su enlace
/// (verde, directo; ámbar, por el relay o conectando) y el anillo de quien habla; detrás, libres
/// hasta completar la fila.
fn room_seats(app: &App) -> Element<'_, Message> {
    use coroom_transport::Path;
    use coroom_ui::avatar::{RING_PADDING, ringed_lamp};
    let room = app.host.room.as_ref();
    let talking = |guest: u32| room.is_some_and(|room| room.voice.talkers.contains(&guest));
    let slot = seat::LABEL.max(ROOM_SEAT + 2.0 * RING_PADDING);
    let place = |seat: Element<'static, Message>, label: Option<String>| {
        let mut column = Column::new()
            .spacing(space::XXS)
            .width(Length::Fixed(slot))
            .align_x(Alignment::Center)
            .push(seat);
        if let Some(label) = label {
            column = column.push(ui::one_line(label, |content| {
                text(content)
                    .size(type_size::CAPTION)
                    .font(coroom_ui::font::UI)
                    .class(TextClass::Secondary)
            }));
        }
        Element::from(column)
    };
    let mut seats: Vec<Element<'_, Message>> = Vec::new();
    if let Some(account) = &app.account {
        seats.push(place(
            ringed_lamp(
                &app.theme,
                &account.display_name,
                avatars::of(app, account.avatar),
                ROOM_SEAT,
                talking(0),
                None,
                Surface::Card,
            ),
            Some(t!("home-you").to_owned()),
        ));
    }
    let guests = room.map(|room| room.guests.as_slice()).unwrap_or_default();
    for guest in guests.iter().take(ROOM_SEATS - 1) {
        seats.push(place(
            ringed_lamp(
                &app.theme,
                &guest.name,
                avatars::of(app, guest.avatar),
                ROOM_SEAT,
                talking(guest.guest.0),
                Some(match guest.path {
                    Some(Path::Direct) => Tone::Success,
                    Some(Path::Relayed) | None => Tone::Warning,
                }),
                Surface::Card,
            ),
            Some(guest.name.clone()),
        ));
    }
    let taken = seats.len();
    for number in taken + 1..=seat::ROW_MIN {
        seats.push(place(
            container(ui::empty_seat(number, ROOM_SEAT))
                .padding(RING_PADDING)
                .into(),
            None,
        ));
    }
    if guests.len() > ROOM_SEATS - 1 {
        seats.push(
            container(ui::secondary(t!(
                "home-online-more",
                count = guests.len() - (ROOM_SEATS - 1)
            )))
            .center_y(Length::Fixed(ROOM_SEAT + 2.0 * RING_PADDING))
            .into(),
        );
    }
    Row::with_children(seats)
        .spacing(space::XS)
        .align_y(Alignment::Start)
        .wrap()
        .vertical_spacing(space::XS)
        .into()
}

/// Campo del código: el campo de texto de siempre, con la letra grande y en seminegrita, como el
/// código en la sala, para que se lea letra a letra.
fn code_input(app: &App, real: bool, enabled: bool) -> Element<'_, Message> {
    let mut input = text_input(CODE_EXAMPLE, &app.home.code)
        .padding(Padding::from([(CODE_INPUT_HEIGHT - 28.0) / 2.0, 14.0]))
        .size(CODE_INPUT_TEXT)
        .line_height(LineHeight::Absolute(ui::px(28.0)))
        .font(SEMIBOLD)
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
    container(input).max_width(CODE_INPUT_MAX).into()
}

/// Unirse con un código, como una entrada: en el cuerpo, el título y el campo del código; tras
/// el punteado, el talón con «Entrar», apagado hasta que el código está entero y es de una sala,
/// y principal desde entonces. `real`: ver [`ui::equal_height_row`] (la copia no lleva el `id`
/// del campo ni mensajes); `height`, el alto de la entrada; `wide`, si ocupa todo el ancho de la
/// página (lo que haya que decir del campo va entonces a su lado y no debajo).
fn join_ticket(app: &App, real: bool, height: Length, wide: bool) -> Element<'_, Message> {
    let hosting = app.host.is_open();
    let visiting = app.guest.is_some();
    let blocked = server_down(app) || hosting || visiting;
    let ready = is_valid_room_code(&app.home.code) && !blocked;
    // El título y la explicación, cada uno en una línea de alto fijo: el campo queda siempre a la
    // misma altura y el talón puede ir a la suya.
    let header = column![
        ui::one_line(t!("home-join-title"), |content| {
            ui::heading(content).line_height(LineHeight::Absolute(ui::px(24.0)))
        }),
        ui::one_line(t!("home-join-text"), |content| {
            ui::secondary(content).line_height(LineHeight::Absolute(ui::px(18.0)))
        }),
    ]
    .spacing(2);
    // Lo que haya que decir del campo: el error del código y por qué no se puede entrar.
    let mut notes = Column::new().spacing(space::XXS);
    if let Some(error) = &app.home.code_error {
        notes = notes.push(
            row![
                icon_with(Icon::CircleAlert, 14.0, SvgClass::Tone(Tone::Error)),
                text(error.as_str())
                    .size(type_size::SECONDARY)
                    .class(TextClass::Tone(Tone::Error))
                    .wrapping(Wrapping::WordOrGlyph)
            ]
            .spacing(6)
            .align_y(Alignment::Start),
        );
    }
    if hosting {
        notes = notes.push(hosting_reason());
    }
    let field = code_input(app, real, !blocked);
    let body: Element<'_, Message> = if wide {
        column![
            header,
            row![field, notes.width(Length::Fill)]
                .spacing(space::S)
                .align_y(Alignment::Center)
                .height(Length::Fixed(CODE_INPUT_HEIGHT))
        ]
        .spacing(space::S)
        .into()
    } else {
        column![header, field, notes].spacing(space::S).into()
    };
    // El margen de la entrada más lo que falta hasta el del bloque de al lado: los dos títulos
    // quedan a la misma altura.
    let inset = BLOCK_PADDING.top - space::S;
    let body = container(body)
        .padding(Padding {
            top: inset,
            right: 0.0,
            bottom: inset,
            left: BLOCK_PADDING.left - space::M,
        })
        .height(height);
    let enter = ui::button_with(
        if ready { Kind::Primary } else { Kind::Normal },
        t!("home-enter"),
        Some(Icon::LogIn),
        (real && ready).then(|| on(Msg::Join)),
    );
    // Dentro de la sala de otro el bloque de al lado ya lo dice; aquí, al pasar el ratón.
    let enter: Element<'_, Message> = if visiting && !hosting {
        ui::with_tooltip(enter, t!("room-ya-estas-en-una-sala"))
    } else {
        enter.into()
    };
    // El talón, a la altura del campo en todos los estados: desde arriba, lo que hay hasta el
    // campo y lo que le falta al botón para centrarse en él. Si la entrada llena un alto dado,
    // el talón lo llena también y queda arriba; si mide lo que su contenido, lleva debajo lo
    // mismo que el campo y mide lo que el cuerpo.
    let centring = (CODE_INPUT_HEIGHT - size::CONTROL) / 2.0;
    let fills = height == Length::Fill;
    let stub = container(enter).padding(Padding {
        top: inset + JOIN_HEADER + space::S + centring,
        bottom: if fills { 0.0 } else { inset + centring },
        ..Padding::ZERO
    });
    let stub = if fills {
        stub.height(Length::Fill)
    } else {
        stub
    };
    ui::ticket_with_height(body, stub, height)
}

// --- Bandeja --------------------------------------------------------------------------------------

/// La bandeja: lo que llega de alguien y vence (las invitaciones a una sala, como entradas; las
/// solicitudes de amistad, como filas). Siempre está, con su título; cada entrada cae desde
/// arriba al llegar y se pliega con su altura al irse, así que lo de debajo se desliza en vez de
/// saltar.
fn tray<'a>(app: &'a App) -> Element<'a, Message> {
    let mut invitations: Vec<&Invitation> = app.invitations.iter().collect();
    invitations.sort_by_key(|invitation| std::cmp::Reverse(received_at(app, invitation)));
    let invitations: Vec<&Invitation> = invitations.into_iter().take(INVITATIONS_SHOWN).collect();
    let requests = app
        .friends
        .loaded()
        .map(|friends| friends.incoming.as_slice())
        .unwrap_or_default();
    let own = own_rooms(app);
    let count = own.len() + invitations.len() + requests.len();
    let summary = match count {
        0 => t!("home-tray-empty").to_owned(),
        _ => t!("home-tray-detail").to_owned(),
    };
    let header = row![
        ui::heading(t!("home-tray")),
        ui::slot((count > 0).then(|| ui::rail_count(count.to_string()))),
        ui::secondary(summary).wrapping(Wrapping::None),
    ]
    .spacing(space::XS)
    .align_y(Alignment::Center);
    let mut entries: Vec<Element<'a, Message>> = Vec::new();
    for room in own {
        entries.push(tray_entry(app, own_room_ticket(app, room), 1.0, 0.0));
    }
    for invitation in invitations {
        let since = received_at(app, invitation);
        entries.push(tray_entry(
            app,
            invitation_ticket(app, invitation, since, false),
            entered(app, since),
            0.0,
        ));
    }
    for leaving in &app.home.leaving {
        entries.push(tray_entry(
            app,
            invitation_ticket(app, &leaving.invitation, leaving.received, true),
            1.0,
            left(app, leaving.since),
        ));
    }
    for request in requests {
        let since = app
            .home
            .requests_seen
            .get(&request.id)
            .copied()
            .unwrap_or(app.now);
        entries.push(tray_entry(
            app,
            request_card(app, request),
            entered(app, since),
            0.0,
        ));
    }
    column![
        header,
        container(Column::with_children(entries)).padding(Padding {
            top: space::S,
            ..Padding::ZERO
        })
    ]
    .into()
}

/// Una entrada de la bandeja con su movimiento: al llegar (`entered` de 0 a 1) crece y cae desde
/// arriba; al irse (`left` de 0 a 1) se pliega. El espacio de debajo va dentro: plegada no ocupa
/// nada.
fn tray_entry<'a>(
    app: &App,
    content: Element<'a, Message>,
    entered: f32,
    left: f32,
) -> Element<'a, Message> {
    let fall = app.motion().slide(space::M) * (1.0 - entered);
    Collapse::new(
        Shift::new(
            container(content).padding(Padding {
                bottom: space::XS,
                ..Padding::ZERO
            }),
            iced_core::Vector::new(0.0, -fall),
        ),
        entered * (1.0 - left),
    )
    .into()
}

/// «Entrar» de una entrada: el talón. Con la sala propia abierta (o dentro de otra), desactivado
/// y con el motivo al pasar el ratón.
fn enter_stub<'a>(code: &str, blocked: bool, hosting: bool) -> Element<'a, Message> {
    let button = ui::button_with(
        Kind::Primary,
        t!("home-enter"),
        Some(Icon::LogIn),
        (!blocked).then(|| on(Msg::Accept(code.to_owned()))),
    );
    if hosting {
        ui::with_tooltip(button, t!("home-hosting-reason"))
    } else {
        button.into()
    }
}

/// El cuerpo de una entrada: la butaca de quien invita, el texto (título y lo de debajo) y el
/// código de la sala en señalética pequeña.
fn ticket_body<'a>(
    seat: Element<'a, Message>,
    title: String,
    detail: Element<'a, Message>,
    code: &str,
) -> Row<'a, Message, coroom_ui::Theme, coroom_ui::Renderer> {
    row![
        seat,
        column![ui::strong(title).wrapping(Wrapping::WordOrGlyph), detail]
            .spacing(2)
            .width(Length::Fill),
        ui::code_cells(code, CodeSize::Small),
    ]
    .spacing(space::S)
    .align_y(Alignment::Center)
}

/// Una invitación que llega, como una entrada: quien invita, el código de su sala, la cuenta
/// atrás y, en el talón, «Entrar». `leaving`: se va (sin botones que hagan nada).
fn invitation_ticket<'a>(
    app: &'a App,
    invitation: &'a Invitation,
    received: Instant,
    leaving: bool,
) -> Element<'a, Message> {
    let ago = app
        .now
        .saturating_duration_since(received)
        .min(INVITATION_TTL);
    let remaining = INVITATION_TTL.saturating_sub(ago);
    let left = remaining.as_secs().max(u64::from(!leaving));
    let hosting = app.host.is_open() || app.guest.is_some();
    let blocked = server_down(app) || hosting || leaving;
    // La cuenta atrás en m:ss en una ranura de ancho fijo: cambia cada segundo sin mover el texto
    // ni repartir las líneas.
    let countdown = row![
        ui::secondary(t!("home-invite-expires")).wrapping(Wrapping::None),
        ui::figure(ui::secondary, stats::clock(left), &[stats::CLOCK_WIDEST]),
    ]
    .spacing(space::XXS);
    let body = ticket_body(
        avatars::view(
            app,
            &invitation.from_name,
            avatars::of_user(app, invitation.from_user),
            size::AVATAR,
        ),
        t!("home-invite-title", name = invitation.from_name),
        countdown.into(),
        &invitation.code,
    )
    .push(ui::flat(
        t!("home-ignore"),
        (!leaving).then(|| on(Msg::Ignore(invitation.from_user))),
    ));
    ui::ticket(body, enter_stub(&invitation.code, blocked, hosting))
}

/// Las salas de la cuenta abiertas en sus otros dispositivos (sin la que comparte este).
pub(crate) fn own_rooms(app: &App) -> Vec<&OwnRoom> {
    let hosted = room_code(app);
    app.own_rooms
        .iter()
        .filter(|room| Some(room.code.as_str()) != hosted)
        .collect()
}

/// La sala que la cuenta tiene abierta en otro dispositivo, como una entrada con la butaca
/// propia: se entra con un toque, sin escribir el código ni esperar invitación. Se queda mientras
/// siga abierta.
fn own_room_ticket<'a>(app: &'a App, room: &'a OwnRoom) -> Element<'a, Message> {
    let hosting = app.host.is_open() || app.guest.is_some();
    let blocked = server_down(app) || hosting;
    let seat: Element<'a, Message> = match &app.account {
        Some(account) => avatars::view(app, &account.display_name, account.avatar, size::AVATAR),
        None => icon_with(Icon::Monitor, size::AVATAR, SvgClass::Accent),
    };
    let body = ticket_body(
        seat,
        t!("home-own-room-title", device = room.device.as_str()),
        ui::secondary(t!("home-own-room-detail"))
            .wrapping(Wrapping::WordOrGlyph)
            .into(),
        &room.code,
    );
    ui::ticket(body, enter_stub(&room.code, blocked, hosting))
}

/// Una solicitud de amistad recibida, como una fila: aceptar o rechazar sin ir a Amigos.
fn request_card<'a>(
    app: &'a App,
    request: &'a coroom_transport::proto::api::FriendRequest,
) -> Element<'a, Message> {
    let busy = app.friends_page.busy;
    container(ui::list_row(
        Some(avatars::view(
            app,
            &request.user.display_name,
            request.user.avatar,
            size::AVATAR,
        )),
        t!(
            "app-name-quiere-ser-tu-amigo",
            name = request.user.display_name
        ),
        Some(format!("@{}", request.user.username)),
        vec![
            ui::flat(
                t!("home-decline"),
                (!busy).then(|| Message::Friends(FriendsMsg::Decline(request.id))),
            )
            .into(),
            // Tonal: en Inicio el relleno de acento es de «Compartir» y de las entradas.
            ui::normal(
                t!("home-accept"),
                (!busy).then(|| Message::Friends(FriendsMsg::Accept(request.id))),
            )
            .into(),
        ],
    ))
    .width(Length::Fill)
    .class(ContainerClass::Card)
    .into()
}

// --- En línea ahora -------------------------------------------------------------------------------

/// Un amigo conectado en «En línea ahora»: su butaca con la lámpara verde, su nombre debajo y,
/// en palabras, qué hace (o que se espera su respuesta, si se le pidió entrar; o que ya está en
/// tu sala). Pulsarlo lleva a Amigos. Con tu sala abierta, debajo va «Invitar», a un clic. Sin
/// conexión con el servidor la lámpara se apaga: lo que se ve es lo último que se supo.
fn online_seat<'a>(app: &App, friend: &Friend) -> Element<'a, Message> {
    let in_room = app
        .host
        .room
        .as_ref()
        .is_some_and(|room| room.guests.iter().any(|guest| guest.user == friend.id));
    let status = if in_room {
        t!("home-in-your-room")
    } else if app.home.asked_at.contains_key(&friend.id) {
        t!("home-asked-status")
    } else {
        activity_text(friend)
    };
    let seat = column![
        coroom_ui::avatar::seat_lamp(
            &app.theme,
            &friend.display_name,
            avatars::of(app, friend.avatar),
            seat::ROW,
            (!server_down(app)).then_some(Tone::Success),
            Surface::Window,
        ),
        column![
            ui::one_line(friend.display_name.clone(), |content| {
                text(content)
                    .size(type_size::CAPTION)
                    .font(coroom_ui::font::UI)
            }),
            // En dos líneas si hace falta: «Esperando respuesta» no cabe en una.
            ui::caption(status)
                .align_x(iced_core::alignment::Horizontal::Center)
                .wrapping(Wrapping::Word),
        ]
        .align_x(Alignment::Center),
    ]
    .spacing(space::XXS)
    .width(Length::Fixed(seat::LABEL))
    .align_x(Alignment::Center);
    let seat = ui::with_tooltip(
        coroom_ui::animate::button(seat)
            .padding(space::XXS)
            .class(ButtonClass::Flat)
            .press_scale(true)
            .on_press(Message::Navigate(Page::Friends)),
        t!(
            "home-online-face",
            name = friend.display_name,
            status = status
        ),
    );
    // Con la sala abierta, invitar está a un clic (mientras se abre, todavía sin código, apagado).
    let invite = (app.host.is_open() && !in_room).then(|| {
        coroom_ui::animate::button(
            text(t!("home-invite"))
                .size(type_size::CAPTION)
                .font(SEMIBOLD),
        )
        .padding(Padding::from([space::XXS, space::S]))
        .class(ButtonClass::Normal)
        .on_press_maybe(room_code(app).map(|_| on(Msg::Invite(friend.id))))
        .into()
    });
    column![seat, ui::slot(invite)]
        .spacing(2)
        .align_x(Alignment::Center)
        .into()
}

/// Quién está conectado ahora: una fila de butacas con su lámpara, el nombre y qué hace cada
/// uno debajo, que llevan a Amigos. Sin amigos todavía, dice cómo agregarlos.
fn online_now(app: &App) -> Element<'_, Message> {
    let online: Vec<&Friend> = app
        .friends
        .loaded()
        .map(|friends| {
            sorted_friends(friends)
                .into_iter()
                .filter(|friend| friend.online)
                .collect()
        })
        .unwrap_or_default();
    let summary = match online.len() {
        0 => t!("home-online-now-none").to_owned(),
        1 => t!("home-online-now-one").to_owned(),
        n => t!("home-online-now-many", count = n),
    };
    // Sin conexión con el servidor, se dice de cuándo es lo que se ve.
    let summary = match friends::last_known(app).filter(|_| server_down(app)) {
        Some(updated) => format!("{summary} · {updated}"),
        None => summary,
    };
    let header = row![
        ui::heading(t!("home-online-now")),
        ui::secondary(summary).wrapping(Wrapping::None),
        ui::fill(),
        ui::button_with(
            Kind::Link,
            t!("home-see-friends"),
            Some(Icon::Users),
            Some(Message::Navigate(Page::Friends))
        ),
    ]
    .spacing(space::XS)
    .align_y(Alignment::Center);
    let body: Element<'_, Message> = match &app.friends {
        Load::Idle | Load::Loading => ui::secondary(t!("home-loading")).into(),
        Load::Failed(err) => row![
            ui::secondary(t!("home-load-failed-detail", err = err)).wrapping(Wrapping::WordOrGlyph),
            ui::button_with(
                Kind::Link,
                t!("home-retry"),
                Some(Icon::Refresh),
                Some(Message::Friends(FriendsMsg::Reload))
            )
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center)
        .into(),
        // Sin amigos todavía: qué hacer, con el botón al lado.
        Load::Loaded(friends) if friends.friends.is_empty() => row![
            column![
                ui::strong(t!("home-empty-title")),
                ui::secondary(t!("home-empty-text")).wrapping(Wrapping::WordOrGlyph)
            ]
            .spacing(2)
            .width(Length::Fill),
            ui::button_with(
                Kind::Normal,
                t!("home-add-friend"),
                Some(Icon::UserPlus),
                Some(on(Msg::AddFriend))
            )
        ]
        .spacing(space::M)
        .align_y(Alignment::Center)
        .into(),
        Load::Loaded(_) if online.is_empty() => ui::secondary(t!("home-online-now-empty"))
            .wrapping(Wrapping::WordOrGlyph)
            .into(),
        Load::Loaded(_) => {
            let mut seats = Row::new().spacing(space::XXS).align_y(Alignment::Start);
            for friend in online.iter().take(ONLINE_SHOWN) {
                seats = seats.push(online_seat(app, friend));
            }
            if online.len() > ONLINE_SHOWN {
                seats = seats.push(
                    container(ui::caption(t!(
                        "home-online-more",
                        count = online.len() - ONLINE_SHOWN
                    )))
                    .center_y(Length::Fixed(seat::ROW + 2.0 * space::XXS)),
                );
            }
            seats.wrap().vertical_spacing(space::XS).into()
        }
    };
    column![header, body].spacing(space::S).into()
}

/// Qué hace un amigo, en palabras (el texto de su insignia).
pub fn activity_text(friend: &Friend) -> &'static str {
    match (friend.online, friend.activity) {
        (false, _) => t!("home-offline"),
        (true, Activity::Idle) => t!("home-available"),
        (true, Activity::Hosting) => t!("home-hosting"),
        (true, Activity::Playing) => t!("home-playing"),
    }
}

/// Escenas de Inicio para las capturas.
pub(crate) mod shots {
    use std::time::Duration;

    use super::Msg;
    use crate::ui::app::{App, Event, Invitation, Message, Page};
    use crate::ui::screens::updates::shots as updates;
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
        // Aviso de versión nueva.
        app.update.updater = crate::update::Updater::new(
            crate::update::Channel::Notice(crate::update::NoticeOnly::Unpackaged),
            true,
        );
        app.update
            .updater
            .found(Some(crate::update::Release::new("0.2.0")));
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

    /// Inicio en una ventana estrecha, con la invitación de Bea y la sala propia abierta en otro
    /// equipo: las dos entradas de la bandeja y el estado de la sala bajo las butacas.
    fn home_narrow(app: &mut App) {
        home(app);
        app.window = iced_core::Size::new(
            crate::ui::shots::NARROW.0 as f32,
            crate::ui::shots::NARROW.1 as f32,
        );
        app.update(Message::Event(Event::OwnRooms(vec![
            coroom_transport::proto::signal::OwnRoom {
                code: "H4W9TC".to_owned(),
                device: "Sobremesa".to_owned(),
            },
        ])));
    }

    /// Inicio con un código entero escrito en la entrada: «Entrar» ya es la acción principal.
    fn home_code(app: &mut App) {
        home_empty(app);
        app.update(Message::Home(Msg::Code("w3rn7p".to_owned())));
    }

    /// Escenas de esta pantalla.
    pub(crate) const BUILDERS: &[(&str, Builder)] = &[
        ("05-inicio", home),
        ("05-inicio-estrecho", home_narrow),
        ("05-inicio-pedido", home_asked),
        ("06-inicio-vacio", home_empty),
        ("06-inicio-codigo", home_code),
        ("06-inicio-sin-servidor", home_offline),
        // La versión nueva ([`crate::ui::screens::updates`]): lista, lista con la sala abierta,
        // no se pudo descargar y carpeta sin permiso; y las versiones distintas con el servidor.
        ("05-inicio-version-lista", |app| {
            home_update(app);
            updates::ready(app, false);
        }),
        ("05-inicio-version-lista-con-sala", |app| {
            crate::ui::screens::host::shots::host_room_join_request(app);
            app.update(Message::Navigate(Page::Home));
            updates::ready(app, true);
        }),
        ("05-inicio-version-fallo", |app| {
            home_update(app);
            updates::failed(app);
        }),
        ("05-inicio-version-carpeta", |app| {
            home_update(app);
            updates::unwritable(app);
        }),
        ("05-inicio-version-no-instalada", |app| {
            home_update(app);
            updates::install_failed(app);
        }),
        ("05-inicio-version-app-vieja", |app| {
            home_update(app);
            updates::ready(app, false);
            updates::protocol(app, true);
        }),
        ("05-inicio-version-app-vieja-descargando", |app| {
            home_update(app);
            updates::downloading(app);
            app.update.updater.update_now();
            updates::protocol(app, true);
        }),
        ("05-inicio-version-servidor-viejo", |app| {
            home_update(app);
            updates::up_to_date(app);
            updates::protocol(app, false);
        }),
    ];

    /// Inicio sin la invitación, para ver el aviso de la versión nueva.
    fn home_update(app: &mut App) {
        home(app);
        app.invitations.clear();
        app.home.invited_at.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::tests::app;
    use crate::ui::screens::host::SourceChoice;

    /// La sala abierta con la misma cuenta en otro dispositivo sale en la bandeja y se entra con
    /// un toque; la que comparte este equipo no sale.
    #[test]
    fn the_own_room_on_another_device_is_one_tap_away() {
        let mut app = app();
        app.update(Message::Event(Event::OwnRooms(vec![
            OwnRoom {
                code: "K7Q2MZ".into(),
                device: "Equipo de ejemplo".into(),
            },
            OwnRoom {
                code: "ABCDEF".into(),
                device: "Portátil".into(),
            },
        ])));
        let codes: Vec<&str> = own_rooms(&app)
            .iter()
            .map(|room| room.code.as_str())
            .collect();
        assert_eq!(codes, ["K7Q2MZ", "ABCDEF"]);
        let effects = app.update(Message::Home(Msg::Accept("K7Q2MZ".into())));
        assert!(
            effects.contains(&Effect::Join("K7Q2MZ".into())),
            "{effects:?}"
        );

        app.update(Message::Event(Event::OwnRooms(Vec::new())));
        assert!(own_rooms(&app).is_empty(), "cerrada en el otro, desaparece");
    }

    fn friend(id: u64, name: &str, online: bool) -> Friend {
        Friend {
            id,
            username: name.to_lowercase(),
            display_name: name.to_owned(),
            online,
            activity: Default::default(),
            avatar: None,
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
        // Se pliega con su altura antes de irse del todo (sin movimiento, se va ya).
        let exit = app.motion().exit_duration(Token::Panel);
        if exit.is_zero() {
            assert!(app.home.leaving.is_empty());
        } else {
            assert_eq!(app.home.leaving.len(), 1, "se está plegando");
            assert_eq!(app.next_tick(), Some(app.now + exit));
            app.now += exit;
            app.update(Message::Tick);
            assert!(app.home.leaving.is_empty());
        }
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
    fn friends_update_time_is_recorded_without_ticks() {
        let mut app = main_app();
        let start = app.now;
        app.update(Message::Event(Event::Friends(Ok(Friends {
            friends: vec![friend(2, "Bea", false)],
            incoming: Vec::new(),
            outgoing: Vec::new(),
        }))));
        assert_eq!(app.home.friends_updated, Some(start));
        // El «actualizado hace…» va al pasar el ratón: no pide ticks.
        assert_eq!(app.next_tick(), None);
        app.now = start + Duration::from_secs(90);
        assert_eq!(app.next_tick(), None);
        assert!(
            friends::updated(&app).is_some_and(|text| text.contains("actualizado")),
            "{:?}",
            friends::updated(&app)
        );
        // La presencia también cuenta como actualización (y llega con la sala abierta).
        app.update(Message::Event(Event::Presence {
            user: 2,
            online: true,
        }));
        assert_eq!(app.home.friends_updated, Some(app.now));
        assert!(app.friends.loaded().is_some_and(|f| f.friends[0].online));
        let summary = friends::summary(app.friends.loaded().unwrap());
        assert_eq!(summary, "1 conectado");
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

    /// «Agregar amigo» desde Inicio (sin amigos todavía) lleva a Amigos con el campo desplegado, y
    /// repetirlo no lo recoge.
    #[test]
    fn add_friend_from_home_opens_the_field_in_friends() {
        let mut app = main_app();
        app.update(Message::Home(Msg::AddFriend));
        assert_eq!(app.page, Page::Friends);
        assert!(app.friends_page.adding);
        app.page = Page::Home;
        app.update(Message::Home(Msg::AddFriend));
        assert!(app.friends_page.adding, "sigue desplegado");
    }

    /// El estado de la sala propia se dice en palabras, con su lámpara: cerrada, abriéndose y
    /// abierta.
    #[test]
    fn the_own_room_state_is_said_in_words() {
        let mut app = main_app();
        let (tone, text) = room_state(&app);
        assert_eq!(tone, Tone::Neutral);
        assert!(text.contains("cerrada"), "{text}");
        app.host.starting = true;
        let (tone, text) = room_state(&app);
        assert_eq!(tone, Tone::Warning);
        assert!(text.contains("Abriendo"), "{text}");
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
            avatar: None,
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
