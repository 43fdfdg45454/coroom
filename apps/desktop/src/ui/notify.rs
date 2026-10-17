//! Qué aviso suena con cada evento ([`on_event`]) y cuándo se juntan ([`sound`]).
//!
//! | Evento | Aviso | Quién lo oye |
//! |---|---|---|
//! | Un invitado entra (`GuestJoined`) / la lista de la sala crece | Entra alguien | todos menos quien entra |
//! | Un invitado sale sin que se le expulse / la lista mengua / el anfitrión cierra | Sale alguien | todos menos quien sale |
//! | Mensaje de otro con el chat cerrado o sin foco (`chat::arrived`) | Mensaje del chat | todos menos quien escribe |
//! | «Pedir entrar» con la sala abierta en este equipo | Alguien pide entrar | solo el anfitrión |
//! | Una invitación, o la oferta de quedarse con la sala | Te invitan | quien la recibe |
//! | La invitación que responde a «Pedir entrar» | Te dejan entrar | quien lo pidió |
//! | El anfitrión confirma que la sala es tuya (`MoveToRoom` con tu código) | Ahora eres el anfitrión | el anfitrión nuevo |
//! | Una solicitud de amistad nueva | Solicitud de amistad | quien la recibe |
//!
//! Lo que hace uno mismo no suena: entrar (la primera lista de la sala), expulsar, pasar la sala o
//! cerrarla (los que salen por ello), sus mensajes. Tampoco el cambio de sala al pasarla: en la
//! vieja no suena quién se va mientras uno mismo se va, y en la nueva no suena la llegada de quien
//! venía de la vieja ([`State::handed_over`]). Varios avisos seguidos se juntan: uno no suena si en el último
//! [`COALESCE`] ya sonó otro de su prioridad o más ([`SoundEvent::priority`]); así cinco que
//! entran a la vez suenan una vez, y una petición de entrar no la tapa una llegada.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use coroom_session::remote::SessionEvent;

use super::app::{App, Effect, EndKind, Event};
use super::screens::{home, host};
use super::settings::SoundEvent;

/// Ventana en la que se juntan los avisos seguidos.
pub const COALESCE: Duration = Duration::from_secs(1);

/// Estado de los avisos.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// Los avisos que sonaron hace menos de [`COALESCE`].
    recent: Vec<(Instant, SoundEvent)>,
    /// Invitado: quién había en la sala en la última lista (id de invitado → usuario, sin uno
    /// mismo); `None` hasta la primera, que es la de entrar uno mismo y no suena.
    members: Option<BTreeMap<u32, u64>>,
    /// Anfitrión: los invitados de la sala abierta (id de invitado → usuario).
    guests: BTreeMap<u32, u64>,
    /// Quién viene de la sala de antes al pasarla a otro anfitrión (por usuario; tanto el que la
    /// pasa como el que se queda con ella y los demás): su primera entrada en la sala nueva no
    /// suena, es la misma gente cambiando de sala. Se vacía al acabar la sala nueva. Sin usuario
    /// (`0`, modo desarrollo) no se distingue a nadie y suena como cualquier entrada.
    handed_over: BTreeSet<u64>,
    /// Quién te pidió amistad en la última lista de amigos (por usuario); `None` hasta la
    /// primera: lo que ya estaba al entrar no suena.
    friend_requests: Option<BTreeSet<u64>>,
}

/// El efecto de sonar `event`, si su sonido está puesto y no se junta con otro reciente.
pub fn sound(app: &mut App, event: SoundEvent) -> Option<Effect> {
    let settings = &app.settings.notifications;
    if !settings.sound(event).is_on() || settings.volume == 0 {
        return None;
    }
    let now = app.now;
    let recent = &mut app.notify.recent;
    recent.retain(|(at, _)| now.saturating_duration_since(*at) < COALESCE);
    if recent
        .iter()
        .any(|(_, other)| other.priority() >= event.priority())
    {
        return None;
    }
    recent.push((now, event));
    Some(Effect::Sound(event))
}

/// Los avisos que suenan con `event`. Se llama antes de que la pantalla lo procese (así ve aún,
/// por ejemplo, si se había pedido entrar a quien invita).
pub fn on_event(app: &mut App, event: &Event) -> Vec<Effect> {
    let mut sounds = Vec::new();
    match event {
        Event::HostStarted(_) => app.notify.guests.clear(),
        // La sala propia para quedarse con la de otro: quien está en aquella vendrá a esta.
        Event::Host(SessionEvent::RoomCreated { .. })
            if app.host.takeover == host::Takeover::Opening =>
        {
            let users = users(app.notify.members.iter().flat_map(BTreeMap::values));
            app.notify.handed_over.extend(users);
        }
        Event::Host(SessionEvent::GuestJoined { guest, user, .. }) => {
            app.notify.guests.insert(guest.0, *user);
            if !app.notify.handed_over.remove(user) {
                sounds.push(SoundEvent::MemberJoined);
            }
        }
        Event::Host(SessionEvent::GuestLeft { guest, kicked }) => {
            let user = app.notify.guests.remove(&guest.0);
            let handing_over = app.host.handing_over || app.host.moving_to.is_some();
            if handing_over {
                // Se va a la sala nueva, adonde va también uno mismo.
                app.notify.handed_over.extend(users(user.as_ref()));
            }
            // Expulsarlo, pasar la sala o cerrarla lo hizo uno mismo.
            if !kicked && !handing_over && !app.host.stopping {
                sounds.push(SoundEvent::MemberLeft);
            }
        }
        Event::HandedOver(Ok(_)) if app.host.room.is_some() => {
            let users = users(app.notify.guests.values());
            app.notify.handed_over.extend(users);
        }
        Event::HandedOver(Err(_)) => app.notify.handed_over.clear(),
        Event::HostEnded(_) => {
            app.notify.guests.clear();
            // Al pasar la sala, la nueva aún no ha empezado.
            if app.host.moving_to.is_none() {
                app.notify.handed_over.clear();
            }
        }
        Event::GuestStarted(_) => app.notify.members = None,
        Event::Guest(SessionEvent::Members(members)) => {
            let now: BTreeMap<u32, u64> = members
                .iter()
                .filter(|member| !member.you)
                .map(|member| (member.guest, member.user))
                .collect();
            let taking_over = matches!(
                app.host.takeover,
                host::Takeover::Opening | host::Takeover::Replied(_)
            );
            if taking_over {
                app.notify.handed_over.extend(users(now.values()));
            }
            // Saliendo (también hacia la sala nueva) o con la respuesta de quedarse con la sala
            // ya dada: los demás también se van, y no suena.
            let leaving = matches!(app.host.takeover, host::Takeover::Replied(_))
                || app
                    .guest
                    .as_ref()
                    .is_some_and(|state| state.leaving || state.moving_to.is_some());
            if let Some(before) = app.notify.members.replace(now.clone())
                && !leaving
            {
                let mut joined = false;
                for (guest, user) in &now {
                    // Todos los que vienen de la sala de antes salen de la lista, aunque suene otro.
                    if !before.contains_key(guest) && !app.notify.handed_over.remove(user) {
                        joined = true;
                    }
                }
                if joined {
                    sounds.push(SoundEvent::MemberJoined);
                }
                if before.keys().any(|guest| !now.contains_key(guest)) {
                    sounds.push(SoundEvent::MemberLeft);
                }
            }
        }
        Event::Guest(SessionEvent::HandoffOffered) => sounds.push(SoundEvent::Invitation),
        Event::Guest(SessionEvent::MoveToRoom { code, .. }) => {
            if app.host.takeover == host::Takeover::Replied(code.clone()) {
                sounds.push(SoundEvent::BecameHost);
            } else if app
                .guest
                .as_ref()
                .is_some_and(|state| state.moving_to.is_none() && !state.leaving)
            {
                // Se pasa a la sala nueva con los demás de esta (el anfitrión que la pasa incluido).
                let users = users(app.notify.members.iter().flat_map(BTreeMap::values));
                app.notify.handed_over.extend(users);
            }
        }
        Event::GuestEnded(reason) => {
            // Si se va a la sala nueva (o es la propia, que ya está abierta), lo de la sala de
            // antes sigue haciendo falta allí.
            let moving = app
                .guest
                .as_ref()
                .is_some_and(|state| state.moving_to.is_some());
            if !moving && app.host.room.is_none() {
                app.notify.handed_over.clear();
            }
            app.notify.members = None;
            if host_closed(app, reason.as_ref().map(|end| end.kind)) {
                sounds.push(SoundEvent::MemberLeft);
            }
        }
        Event::Invited(invitation) => {
            // La respuesta a «Pedir entrar» (`App::event` entra ya; ver `home::take_asked`).
            let asked = app.home.asked_at.contains_key(&invitation.from_user);
            sounds.push(if asked && !home::server_down(app) {
                SoundEvent::Admitted
            } else {
                SoundEvent::Invitation
            });
        }
        // Solo suena donde está la sala: otro dispositivo de la cuenta solo enseña el aviso.
        Event::JoinRequested { .. } if app.host.room.is_some() => {
            sounds.push(SoundEvent::JoinRequest);
        }
        Event::Friends(Ok(friends)) => {
            let now: BTreeSet<u64> = friends
                .incoming
                .iter()
                .map(|request| request.user.id)
                .collect();
            if let Some(before) = app.notify.friend_requests.replace(now.clone())
                && now.difference(&before).next().is_some()
            {
                sounds.push(SoundEvent::FriendRequest);
            }
        }
        Event::FriendRequested { from_user, .. } => match app.notify.friend_requests.as_mut() {
            // Llega antes que la lista: así, al recargarla, no vuelve a sonar.
            Some(known) => {
                if known.insert(*from_user) {
                    sounds.push(SoundEvent::FriendRequest);
                }
            }
            // Aún sin la primera lista: suena, y la primera queda como la que ya se conoce (sin
            // sonar por las solicitudes de antes, ni otra vez por esta).
            None => sounds.push(SoundEvent::FriendRequest),
        },
        Event::SignedIn(_) | Event::SignedOut(_) => {
            app.notify.friend_requests = None;
            app.notify.handed_over.clear();
        }
        _ => {}
    }
    sounds
        .into_iter()
        .filter_map(|event| sound(app, event))
        .collect()
}

/// Los usuarios de verdad de `users` (sin el `0` del modo desarrollo, que es el de todos).
fn users<'a>(users: impl IntoIterator<Item = &'a u64>) -> impl Iterator<Item = u64> {
    users.into_iter().copied().filter(|user| *user != 0)
}

/// Si la sesión del invitado terminó porque el anfitrión cerró la sala o le expulsó (no al salir
/// uno mismo, ni al pasar a la sala de otro anfitrión, ni al quedarse con la sala).
fn host_closed(app: &App, kind: Option<EndKind>) -> bool {
    let Some(state) = app.guest.as_ref() else {
        return false;
    };
    if state.leaving
        || state.probe
        || state.moving_to.is_some()
        || matches!(app.host.takeover, host::Takeover::Replied(_))
    {
        return false;
    }
    matches!(kind, None | Some(EndKind::RoomClosed | EndKind::Removed))
}

#[cfg(test)]
mod tests {
    use coroom_session::remote::RoomMember;
    use coroom_session::{GuestId, Permissions};
    use coroom_transport::crypto::SafetyNumber;
    use coroom_transport::proto::api::{Friend, FriendRequest, Friends};

    use super::*;
    use crate::ui::app::tests::app;
    use crate::ui::app::{Invitation, Message};
    use crate::ui::screens::guest;
    use crate::ui::settings::SoundChoice;
    use crate::ui::video::VideoFeed;

    /// Los avisos que pidió `effects`, en orden.
    fn sounds(effects: &[Effect]) -> Vec<SoundEvent> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Sound(event) => Some(*event),
                _ => None,
            })
            .collect()
    }

    fn event(app: &mut App, event: Event) -> Vec<SoundEvent> {
        sounds(&app.update(Message::Event(event)))
    }

    /// Pasa el tiempo justo para que no se junte con lo anterior.
    fn later(app: &mut App) {
        app.now += COALESCE;
    }

    fn hosting() -> App {
        let mut app = app();
        app.update(Message::Host(host::Msg::Start));
        app.update(Message::Event(Event::HostStarted(
            Ok(VideoFeed::detached()),
        )));
        app.update(Message::Event(Event::Host(SessionEvent::RoomCreated {
            code: "K7Q2MZ".into(),
            fingerprint: "cd".repeat(32),
        })));
        assert!(app.host.room.is_some());
        app
    }

    fn guest_joined(guest: u32, name: &str) -> Event {
        Event::Host(SessionEvent::GuestJoined {
            guest: GuestId(guest),
            user: u64::from(guest) + 10,
            name: name.into(),
            fingerprint: "ab".repeat(32),
            safety_number: SafetyNumber::between(&[1; 32], &[2; 32]),
            permissions: Permissions::VIEW_ONLY,
        })
    }

    fn watching() -> App {
        let mut app = app();
        guest::start(&mut app, "K7Q2MZ".into());
        app.update(Message::Event(Event::GuestStarted(Ok(
            VideoFeed::detached(),
        ))));
        app
    }

    fn members(names: &[(u32, &str)]) -> Event {
        let names: Vec<_> = names
            .iter()
            .map(|(guest, name)| (*guest, u64::from(*guest) + 10, *name))
            .collect();
        members_of(&names)
    }

    /// Como [`members`], con el usuario de cada uno (para seguir a alguien de una sala a otra).
    fn members_of(names: &[(u32, u64, &str)]) -> Event {
        Event::Guest(SessionEvent::Members(
            names
                .iter()
                .map(|(guest, user, name)| RoomMember {
                    guest: *guest,
                    leaf: *guest,
                    name: (*name).into(),
                    user: *user,
                    avatar: None,
                    you: *name == "Ana",
                })
                .collect(),
        ))
    }

    fn guest_left(guest: u32) -> Event {
        Event::Host(SessionEvent::GuestLeft {
            guest: GuestId(guest),
            kicked: false,
        })
    }

    fn friends(incoming: &[(u64, &str)]) -> Event {
        let person = |id: u64, name: &str| Friend {
            id,
            username: name.to_lowercase(),
            display_name: name.into(),
            online: false,
            activity: Default::default(),
            avatar: None,
        };
        Event::Friends(Ok(Friends {
            friends: Vec::new(),
            incoming: incoming
                .iter()
                .map(|(id, name)| FriendRequest {
                    id: id + 100,
                    user: person(*id, name),
                    created_at: 0,
                })
                .collect(),
            outgoing: Vec::new(),
        }))
    }

    /// El anfitrión oye a quien entra y a quien sale, pero no a quien expulsa él.
    #[test]
    fn the_host_hears_who_joins_and_leaves_but_not_its_own_kicks() {
        let mut app = hosting();
        assert_eq!(
            event(&mut app, guest_joined(1, "Bea")),
            [SoundEvent::MemberJoined]
        );
        later(&mut app);
        assert_eq!(
            event(
                &mut app,
                Event::Host(SessionEvent::GuestLeft {
                    guest: GuestId(1),
                    kicked: false,
                })
            ),
            [SoundEvent::MemberLeft]
        );
        later(&mut app);
        event(&mut app, guest_joined(2, "Carla"));
        later(&mut app);
        assert!(
            event(
                &mut app,
                Event::Host(SessionEvent::GuestLeft {
                    guest: GuestId(2),
                    kicked: true,
                })
            )
            .is_empty(),
            "expulsar lo hizo uno mismo"
        );
    }

    /// El invitado no oye su propia entrada (la primera lista), sí a los que llegan y se van
    /// después, y que el anfitrión cierre la sala; salir uno mismo no suena.
    #[test]
    fn guests_hear_the_others_but_not_themselves() {
        let mut app = watching();
        assert!(event(&mut app, members(&[(0, "Bea"), (1, "Ana")])).is_empty());
        later(&mut app);
        assert_eq!(
            event(&mut app, members(&[(0, "Bea"), (1, "Ana"), (2, "Carla")])),
            [SoundEvent::MemberJoined]
        );
        later(&mut app);
        assert!(
            event(&mut app, members(&[(0, "Bea"), (1, "Ana"), (2, "Carla")])).is_empty(),
            "la misma lista otra vez no suena"
        );
        assert_eq!(
            event(&mut app, members(&[(0, "Bea"), (1, "Ana")])),
            [SoundEvent::MemberLeft]
        );
        later(&mut app);
        assert_eq!(
            event(&mut app, Event::GuestEnded(None)),
            [SoundEvent::MemberLeft],
            "el anfitrión cerró la sala"
        );

        let mut app = watching();
        event(&mut app, members(&[(0, "Bea"), (1, "Ana")]));
        guest::leave(&mut app);
        later(&mut app);
        assert!(
            event(&mut app, Event::GuestEnded(None)).is_empty(),
            "salir uno mismo"
        );
        // Una sala nueva empieza de cero: su primera lista tampoco suena.
        let mut app = watching();
        event(&mut app, members(&[(0, "Bea"), (1, "Ana")]));
        later(&mut app);
        app.update(Message::Event(Event::GuestStarted(Ok(
            VideoFeed::detached(),
        ))));
        assert!(event(&mut app, members(&[(0, "Dani"), (1, "Ana"), (2, "Eva")])).is_empty());
    }

    /// Cinco que entran a la vez suenan una vez; lo que pide una respuesta no lo tapa una
    /// llegada, pero una llegada sí queda tapada por ello.
    #[test]
    fn bursts_are_coalesced_by_priority() {
        let mut app = hosting();
        let mut heard = Vec::new();
        for guest in 1..=5 {
            heard.extend(event(&mut app, guest_joined(guest, "Bea")));
            app.now += COALESCE / 10;
        }
        assert_eq!(heard, [SoundEvent::MemberJoined], "una sola vez");
        assert_eq!(
            event(
                &mut app,
                Event::JoinRequested {
                    from_user: 30,
                    from_name: "Dani".into(),
                }
            ),
            [SoundEvent::JoinRequest],
            "pedir entrar se oye aunque acabe de entrar alguien"
        );
        assert!(event(&mut app, guest_joined(6, "Eva")).is_empty());
        later(&mut app);
        assert_eq!(
            event(&mut app, guest_joined(7, "Fran")),
            [SoundEvent::MemberJoined]
        );
    }

    /// «Pedir entrar» solo suena donde está la sala abierta.
    #[test]
    fn join_requests_sound_only_for_the_host() {
        let request = || Event::JoinRequested {
            from_user: 30,
            from_name: "Dani".into(),
        };
        let mut app = app();
        assert!(
            event(&mut app, request()).is_empty(),
            "sin sala aquí, solo el aviso"
        );
        let mut app = watching();
        assert!(
            event(&mut app, request()).is_empty(),
            "como invitado, nunca"
        );
        let mut app = hosting();
        assert_eq!(event(&mut app, request()), [SoundEvent::JoinRequest]);
    }

    /// Una invitación suena como invitación; si responde a «Pedir entrar», como «te dejan
    /// entrar». Que el anfitrión te ofrezca su sala también es una invitación, y cuando la sala ya
    /// es tuya suena «eres el anfitrión».
    #[test]
    fn invitations_admissions_and_handovers() {
        let invited = |from_user: u64| {
            Event::Invited(Invitation {
                from_user,
                from_name: "Bea".into(),
                code: "K7Q2MZ".into(),
            })
        };
        let mut app = app();
        assert_eq!(event(&mut app, invited(2)), [SoundEvent::Invitation]);
        later(&mut app);
        app.home.asked_at.insert(3, app.now);
        assert_eq!(event(&mut app, invited(3)), [SoundEvent::Admitted]);

        let mut app = watching();
        assert_eq!(
            event(&mut app, Event::Guest(SessionEvent::HandoffOffered)),
            [SoundEvent::Invitation]
        );
        later(&mut app);
        assert!(
            event(
                &mut app,
                Event::Guest(SessionEvent::MoveToRoom {
                    code: "NEWRM2".into(),
                    host_name: "Carla".into(),
                })
            )
            .is_empty(),
            "pasar a la sala de otro anfitrión no suena"
        );
        let mut app = watching();
        app.host.takeover = host::Takeover::Replied("NEWRM2".into());
        assert_eq!(
            event(
                &mut app,
                Event::Guest(SessionEvent::MoveToRoom {
                    code: "NEWRM2".into(),
                    host_name: "Ana".into(),
                })
            ),
            [SoundEvent::BecameHost]
        );
    }

    /// Lo que ya estaba en la lista al entrar no suena; una solicitud nueva, sí (en vivo o al
    /// recargar la lista), y una sola vez.
    #[test]
    fn new_friend_requests_sound_once() {
        let mut app = app();
        assert!(event(&mut app, friends(&[(2, "Bea")])).is_empty());
        later(&mut app);
        assert_eq!(
            event(&mut app, friends(&[(2, "Bea"), (3, "Carla")])),
            [SoundEvent::FriendRequest]
        );
        later(&mut app);
        let effects = app.update(Message::Event(Event::FriendRequested {
            from_user: 4,
            from_name: "Dani".into(),
        }));
        assert_eq!(sounds(&effects), [SoundEvent::FriendRequest]);
        assert!(effects.contains(&Effect::LoadFriends), "{effects:?}");
        assert_eq!(app.toasts.len(), 1, "{:?}", app.toasts);
        later(&mut app);
        assert!(
            event(&mut app, friends(&[(2, "Bea"), (3, "Carla"), (4, "Dani")])).is_empty(),
            "la lista que llega después ya la conoce"
        );
        assert!(
            event(
                &mut app,
                Event::FriendRequested {
                    from_user: 4,
                    from_name: "Dani".into(),
                }
            )
            .is_empty()
        );
        assert_eq!(app.toasts.len(), 1, "ni otro aviso");
    }

    /// Quien pasa la sala no oye salir a los que se van con ella (ni al cerrarla él); en la sala
    /// nueva, ya de invitado, no suena la llegada de los que venían de la suya, pero sí la de
    /// alguien nuevo, y la de quien vuelve después.
    #[test]
    fn handing_the_room_over_is_silent_for_the_old_host() {
        let mut app = hosting();
        event(&mut app, guest_joined(1, "Bea"));
        event(&mut app, guest_joined(2, "Carla"));
        later(&mut app);
        app.host.handoff = Some((GuestId(1), "Bea".into()));
        app.update(Message::Event(Event::Host(SessionEvent::HandoffReply {
            guest: GuestId(1),
            code: Some("NEWRM2".into()),
        })));
        assert!(app.host.handing_over);
        assert!(
            event(&mut app, guest_left(1)).is_empty(),
            "Bea se va a la suya"
        );
        later(&mut app);
        app.update(Message::Event(Event::HandedOver(Ok("NEWRM2".into()))));
        assert!(app.host.moving_to.is_some() && app.host.stopping);
        assert!(event(&mut app, guest_left(2)).is_empty(), "Carla también");
        later(&mut app);
        assert!(event(&mut app, Event::HostEnded(None)).is_empty());
        assert!(app.guest.is_some(), "entra de invitado en la de Bea");
        app.update(Message::Event(Event::GuestStarted(Ok(
            VideoFeed::detached(),
        ))));
        // Ana (la que la pasó) es el usuario 30; Bea, 11; Carla, 12.
        assert!(event(&mut app, members_of(&[(0, 11, "Bea"), (1, 30, "Ana")])).is_empty());
        later(&mut app);
        assert!(
            event(
                &mut app,
                members_of(&[(0, 11, "Bea"), (1, 30, "Ana"), (2, 12, "Carla")])
            )
            .is_empty(),
            "Carla viene de la sala de antes"
        );
        later(&mut app);
        assert_eq!(
            event(
                &mut app,
                members_of(&[
                    (0, 11, "Bea"),
                    (1, 30, "Ana"),
                    (2, 12, "Carla"),
                    (3, 13, "Dani")
                ])
            ),
            [SoundEvent::MemberJoined],
            "Dani no"
        );
        later(&mut app);
        event(
            &mut app,
            members_of(&[(0, 11, "Bea"), (1, 30, "Ana"), (3, 13, "Dani")]),
        );
        later(&mut app);
        assert_eq!(
            event(
                &mut app,
                members_of(&[
                    (0, 11, "Bea"),
                    (1, 30, "Ana"),
                    (3, 13, "Dani"),
                    (4, 12, "Carla")
                ])
            ),
            [SoundEvent::MemberJoined],
            "Carla vuelve: ya cuenta como una llegada"
        );

        // Cerrar la sala uno mismo tampoco suena por los que salen al cerrarse.
        let mut app = hosting();
        event(&mut app, guest_joined(1, "Bea"));
        later(&mut app);
        app.update(Message::Host(host::Msg::Stop));
        app.update(Message::ConfirmDialog);
        assert!(app.host.stopping);
        assert!(event(&mut app, guest_left(1)).is_empty());
    }

    /// Los demás invitados: en la sala vieja no suena quién se va mientras uno mismo se va, y en
    /// la nueva no suena la llegada de los que venían de ella (el anfitrión que la pasó incluido).
    #[test]
    fn moving_with_the_room_is_silent_for_the_other_guests() {
        let mut app = watching();
        // Bea (10) es la anfitriona; Ana (20), este equipo; Carla (12) se queda con la sala.
        let old_room = [
            (0, 10, "Bea"),
            (1, 20, "Ana"),
            (2, 12, "Carla"),
            (3, 13, "Dani"),
        ];
        event(&mut app, members_of(&old_room));
        later(&mut app);
        assert!(
            event(
                &mut app,
                Event::Guest(SessionEvent::MoveToRoom {
                    code: "NEWRM2".into(),
                    host_name: "Carla".into(),
                })
            )
            .is_empty()
        );
        later(&mut app);
        assert!(
            event(&mut app, members_of(&[(0, 10, "Bea"), (1, 20, "Ana")])).is_empty(),
            "los demás también se van"
        );
        later(&mut app);
        assert!(event(&mut app, Event::GuestEnded(None)).is_empty());
        app.update(Message::Event(Event::GuestStarted(Ok(
            VideoFeed::detached(),
        ))));
        assert!(event(&mut app, members_of(&[(0, 12, "Carla"), (1, 20, "Ana")])).is_empty());
        later(&mut app);
        assert!(
            event(
                &mut app,
                members_of(&[
                    (0, 12, "Carla"),
                    (1, 20, "Ana"),
                    (2, 10, "Bea"),
                    (3, 13, "Dani")
                ])
            )
            .is_empty(),
            "Bea y Dani vienen de la sala de antes"
        );
        later(&mut app);
        assert_eq!(
            event(
                &mut app,
                members_of(&[
                    (0, 12, "Carla"),
                    (1, 20, "Ana"),
                    (2, 10, "Bea"),
                    (3, 13, "Dani"),
                    (4, 40, "Eva")
                ])
            ),
            [SoundEvent::MemberJoined]
        );

        // Al acabar la sala nueva se olvida de dónde venía cada uno.
        later(&mut app);
        event(&mut app, members_of(&[(0, 12, "Carla"), (1, 20, "Ana")]));
        guest::leave(&mut app);
        event(&mut app, Event::GuestEnded(None));
        assert!(app.notify.handed_over.is_empty());
    }

    /// Quien se queda con la sala: no suena la llegada a la suya de los que venían de la otra
    /// (aunque lleguen antes de que el anfitrión confirme), ni quién se va de la otra; sí suena
    /// «eres el anfitrión» y alguien que entra de nuevas.
    #[test]
    fn taking_the_room_over_is_silent_for_the_new_host() {
        let mut app = watching();
        // Bea (10) es la anfitriona; Ana (20), este equipo; Carla (12) y Dani (13).
        event(
            &mut app,
            members_of(&[
                (0, 10, "Bea"),
                (1, 20, "Ana"),
                (2, 12, "Carla"),
                (3, 13, "Dani"),
            ]),
        );
        later(&mut app);
        app.update(Message::Event(Event::Guest(SessionEvent::HandoffOffered)));
        app.update(Message::ConfirmDialog);
        app.update(Message::Event(Event::HostStarted(
            Ok(VideoFeed::detached()),
        )));
        app.update(Message::Event(Event::Host(SessionEvent::RoomCreated {
            code: "NEWRM2".into(),
            fingerprint: "ab".repeat(32),
        })));
        assert_eq!(app.host.takeover, host::Takeover::Replied("NEWRM2".into()));
        later(&mut app);
        let joined = |guest: u32, user: u64, name: &str| {
            Event::Host(SessionEvent::GuestJoined {
                guest: GuestId(guest),
                user,
                name: name.into(),
                fingerprint: "ab".repeat(32),
                safety_number: SafetyNumber::between(&[1; 32], &[2; 32]),
                permissions: Permissions::VIEW_ONLY,
            })
        };
        assert!(
            event(&mut app, joined(1, 12, "Carla")).is_empty(),
            "Carla llega antes de la confirmación"
        );
        assert!(
            event(
                &mut app,
                members_of(&[(0, 10, "Bea"), (1, 20, "Ana"), (3, 13, "Dani")])
            )
            .is_empty(),
            "en la otra, que se vaya Carla no suena"
        );
        assert_eq!(
            event(
                &mut app,
                Event::Guest(SessionEvent::MoveToRoom {
                    code: "NEWRM2".into(),
                    host_name: "Ana".into(),
                })
            ),
            [SoundEvent::BecameHost]
        );
        later(&mut app);
        assert!(event(&mut app, Event::GuestEnded(None)).is_empty());
        assert!(event(&mut app, joined(2, 13, "Dani")).is_empty());
        assert!(
            event(&mut app, joined(3, 10, "Bea")).is_empty(),
            "la anfitriona de antes"
        );
        later(&mut app);
        assert_eq!(
            event(&mut app, joined(4, 40, "Eva")),
            [SoundEvent::MemberJoined]
        );
        later(&mut app);
        assert_eq!(
            event(&mut app, joined(5, 13, "Dani")),
            [SoundEvent::MemberJoined],
            "Dani vuelve: ya es una llegada"
        );
    }

    /// Una solicitud que llega antes de la primera lista de amigos suena una vez: la lista, que
    /// la trae junto con las que ya estaban, no suena.
    #[test]
    fn a_friend_request_before_the_first_list_sounds_once() {
        let mut app = app();
        assert_eq!(
            event(
                &mut app,
                Event::FriendRequested {
                    from_user: 4,
                    from_name: "Dani".into(),
                }
            ),
            [SoundEvent::FriendRequest]
        );
        later(&mut app);
        assert!(
            event(&mut app, friends(&[(2, "Bea"), (4, "Dani")])).is_empty(),
            "Bea ya estaba y Dani ya sonó"
        );
        later(&mut app);
        assert_eq!(
            event(&mut app, friends(&[(2, "Bea"), (4, "Dani"), (5, "Eva")])),
            [SoundEvent::FriendRequest]
        );
    }

    /// Un aviso en «Ninguno» o con el volumen a cero no pide sonar, ni ocupa el hueco de otro.
    #[test]
    fn silenced_notifications_never_ask_to_sound() {
        let mut app = hosting();
        app.settings.notifications.member_joined.tone = SoundChoice::Off;
        assert!(event(&mut app, guest_joined(1, "Bea")).is_empty());
        assert_eq!(
            event(
                &mut app,
                Event::Host(SessionEvent::GuestLeft {
                    guest: GuestId(1),
                    kicked: false,
                })
            ),
            [SoundEvent::MemberLeft]
        );
        later(&mut app);
        app.settings.notifications.volume = 0;
        assert!(
            event(
                &mut app,
                Event::JoinRequested {
                    from_user: 30,
                    from_name: "Dani".into(),
                }
            )
            .is_empty()
        );
    }
}
