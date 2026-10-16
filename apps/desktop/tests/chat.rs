//! Chat de la sala de extremo a extremo en proceso: servidor real (`coroom-server`, puertos
//! efímeros, cuentas: el anfitrión es «Jugador 1» y los invitados, por orden de alta, «Jugador 2»,
//! «Jugador 3»…), anfitrión con vídeo sintético pequeño e invitados sin ventana, todo en
//! `127.0.0.1`.
//!
//! - (a) los mensajes del anfitrión y de los invitados llegan a todos, en el mismo orden, con el
//!   autor y la hora que pone el anfitrión (el autor recibe el suyo con `own`), también en la sala
//!   de espera (antes de que llegue el vídeo); el texto se valida antes de salir;
//! - (b) quien entra tarde recibe el historial, y con más de 50 mensajes solo los 50 últimos; el
//!   límite de ritmo (10 cada 10 s) se rechaza en local con «Vas muy rápido» y lo que deja salir
//!   llega entero; vaciar el chat llega a todos y deja el historial vacío;
//! - (c) un invitado expulsado no recibe ni puede descifrar lo que se escribe después (época MLS
//!   nueva) y lo que escribió se queda en el historial.

mod common;

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::{Account, TestServer};
use coroom_session::capture::synthetic::SyntheticCapturer;
use coroom_session::codec::{DecoderSet, EncoderSet};
use coroom_session::latency::LatencyLedger;
use coroom_session::loopback::{LoopbackConfig, NullSink};
use coroom_session::remote::{
    CHAT_RATE_MESSAGES, CHAT_RATE_WINDOW, ChatLine, RemoteSession, SessionEvent,
};
use coroom_session::{GuestId, Session, SessionError};
use coroom_transport::crypto::ChatKind;
use crossbeam_channel::Receiver;

/// Espera máxima de cada paso.
const LIMIT: Duration = Duration::from_secs(20);

/// Mensajes del historial que recibe quien entra (`proto::control::CHAT_HISTORY_MAX`).
const HISTORY: usize = 50;

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn host(account: Account) -> RemoteSession {
    let mut session = RemoteSession::host(
        account.identity,
        account.config,
        Box::new(SyntheticCapturer::new(320, 180, 15)),
        EncoderSet::openh264(),
        LoopbackConfig {
            width: 320,
            height: 180,
            fps: 15,
            bitrate_kbps: 500,
            frames: None,
            yuv444: false,
            hdr: false,
        },
        Arc::new(LatencyLedger::new()),
    );
    session.start().expect("arranque del anfitrión");
    session
}

fn guest(account: Account, code: &str) -> RemoteSession {
    let mut session = RemoteSession::guest(
        account.identity,
        account.config,
        code,
        DecoderSet::openh264(),
        Box::new(NullSink),
        None,
        Arc::new(LatencyLedger::new()),
    );
    session.start().expect("arranque del invitado");
    session
}

fn room_code(host: &RemoteSession) -> String {
    let deadline = Instant::now() + LIMIT;
    loop {
        assert!(
            !host.is_finished(),
            "el anfitrión terminó: {:?}",
            host.end_reason()
        );
        if let Some(code) = host.room_code() {
            return code;
        }
        assert!(Instant::now() < deadline, "la sala no se creó");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// El primer evento para el que `pick` devuelve algo (los demás se descartan).
fn wait_event<T>(
    events: &Receiver<SessionEvent>,
    what: &str,
    mut pick: impl FnMut(SessionEvent) -> Option<T>,
) -> T {
    let deadline = Instant::now() + LIMIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "tiempo agotado esperando: {what}");
        if let Ok(event) = events.recv_timeout(left.min(Duration::from_millis(100)))
            && let Some(found) = pick(event)
        {
            return found;
        }
    }
}

/// Los `count` mensajes del chat siguientes.
fn chat_messages(events: &Receiver<SessionEvent>, count: usize) -> Vec<ChatLine> {
    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        let what = format!("{} de {count} mensajes del chat", out.len() + 1);
        out.push(wait_event(events, &what, |event| match event {
            SessionEvent::ChatMessage(line) => Some(line),
            _ => None,
        }));
    }
    out
}

/// El historial que manda el anfitrión al conectar (también vacío): el chat ya funciona.
fn chat_history(events: &Receiver<SessionEvent>) -> Vec<ChatLine> {
    wait_event(events, "el historial del chat", |event| match event {
        SessionEvent::ChatHistory { messages } => Some(messages),
        _ => None,
    })
}

fn texts(lines: &[ChatLine]) -> Vec<&str> {
    lines.iter().map(|line| line.text.as_str()).collect()
}

fn guest_id(host: &RemoteSession, name: &str) -> GuestId {
    host.guests()
        .into_iter()
        .find(|status| status.name == name)
        .map(|status| status.guest)
        .unwrap_or_else(|| panic!("{name} no está en la sala"))
}

/// Manda `text` en cuanto el límite de ritmo lo deja (lo que se rechaza no cuenta).
fn send_when_allowed(session: &RemoteSession, text: &str) {
    let deadline = Instant::now() + CHAT_RATE_WINDOW * 3;
    loop {
        match session.send_chat(text) {
            Ok(()) => return,
            Err(SessionError::Chat(reason)) if reason.starts_with("Vas muy rápido") => {
                assert!(
                    Instant::now() < deadline,
                    "el límite no se libera: {reason}"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(err) => panic!("no se pudo enviar «{text}»: {err}"),
        }
    }
}

/// (a) Mensajes en los dos sentidos: todos los reciben en el mismo orden, con el autor correcto.
#[test]
fn a_messages_reach_everyone_with_the_right_author() {
    let _serial = serial();
    let server = TestServer::start();
    let mut host = host(server.config());
    let host_events = host.events();
    let code = room_code(&host);

    // Sala de espera: el primer invitado mide la calidad 30 s antes de pedir el vídeo (no lo
    // necesita en todo el test, y así ningún runner lento lo ve llegar antes que el mensaje), de
    // modo que el anfitrión aún no codifica nada; el chat ya funciona.
    let mut first_account = server.config();
    first_account.quality_probe = Duration::from_secs(30);
    let mut first = guest(first_account, &code);
    let first_events = first.events();
    assert!(chat_history(&first_events).is_empty());
    let before = now_ms();
    host.send_chat("hola, ¿se me lee? 👋").expect("enviar");
    let waiting = chat_messages(&first_events, 1);
    assert_eq!(
        first.summary().frames_presented,
        0,
        "llegó antes que el vídeo"
    );
    assert_eq!(waiting[0].text, "hola, ¿se me lee? 👋");
    assert_eq!(
        (waiting[0].author.guest, waiting[0].author.name.as_str()),
        (0, "Jugador 1")
    );
    assert!(!waiting[0].own);
    let mine = chat_messages(&host_events, 1);
    assert!(mine[0].own && mine[0].author.guest == 0);
    assert_eq!(mine[0].at_unix_ms, waiting[0].at_unix_ms);
    assert!((before..=now_ms()).contains(&waiting[0].at_unix_ms));

    let mut second = guest(server.config(), &code);
    let second_events = second.events();
    let history = chat_history(&second_events);
    assert_eq!(texts(&history), ["hola, ¿se me lee? 👋"]);
    assert_eq!(history[0].author.name, "Jugador 1");
    let first_id = guest_id(&host, "Jugador 2");
    let second_id = guest_id(&host, "Jugador 3");

    // Los dos sentidos, intercalados.
    first.send_chat("  hola desde el dos  ").expect("enviar");
    second
        .send_chat("y yo desde el tres\nen dos líneas")
        .expect("enviar");
    host.send_chat("bienvenidos").expect("enviar");
    let sent = [
        "hola desde el dos",
        "y yo desde el tres\nen dos líneas",
        "bienvenidos",
    ];
    let at_host = chat_messages(&host_events, 3);
    let at_first = chat_messages(&first_events, 3);
    let at_second = chat_messages(&second_events, 3);
    // El anfitrión fija el orden: es el mismo en todos (el suyo puede adelantarse a los de los
    // invitados que aún vienen de camino, pero ya no cambia al repartirse).
    let order = texts(&at_host);
    assert_eq!(texts(&at_first), order);
    assert_eq!(texts(&at_second), order);
    let mut sorted = order.clone();
    sorted.sort_unstable();
    let mut expected = sent.to_vec();
    expected.sort_unstable();
    assert_eq!(sorted, expected);
    for (index, line) in at_host.iter().enumerate() {
        let (guest, name) = match line.text.as_str() {
            "hola desde el dos" => (first_id.0, "Jugador 2"),
            "y yo desde el tres\nen dos líneas" => (second_id.0, "Jugador 3"),
            _ => (0, ""),
        };
        assert_eq!(
            (line.author.guest, line.author.name.as_str()),
            (guest, name)
        );
        assert_eq!(line.own, guest == 0, "{line:?}");
        // Lo mismo en cada invitado, con su `own` y con el nombre del anfitrión.
        for (copy, me) in [
            (&at_first[index], first_id.0),
            (&at_second[index], second_id.0),
        ] {
            assert_eq!(copy.author.guest, guest);
            assert_eq!(copy.at_unix_ms, line.at_unix_ms);
            assert_eq!(copy.own, guest == me, "{copy:?}");
            let shown = if guest == 0 { "Jugador 1" } else { name };
            assert_eq!(copy.author.name, shown);
        }
    }
    assert!(
        at_host
            .windows(2)
            .all(|pair| pair[0].at_unix_ms <= pair[1].at_unix_ms)
    );

    // El texto se valida antes de salir.
    for (text, reason) in [
        ("   ", "vacío"),
        (&"a".repeat(2001)[..], "como mucho 2000 caracteres"),
    ] {
        match first.send_chat(text) {
            Err(SessionError::Chat(why)) => assert!(why.contains(reason), "{why}"),
            other => panic!("se esperaba un error: {other:?}"),
        }
    }
    assert_eq!(host.clear_chat(), Ok(()));
    assert_eq!(first.clear_chat(), Err(SessionError::HostOnly));

    second.close();
    first.close();
    host.close();
    drop(server);
}

/// (b) Historial para quien entra tarde (como mucho 50), límite de ritmo y vaciar el chat.
#[test]
fn b_late_guests_get_the_last_fifty_and_the_rate_limit_holds() {
    let _serial = serial();
    let server = TestServer::start();
    let mut host = host(server.config());
    let host_events = host.events();
    let code = room_code(&host);
    let mut first = guest(server.config(), &code);
    let first_events = first.events();
    let mut second = guest(server.config(), &code);
    let second_events = second.events();
    assert!(chat_history(&first_events).is_empty());
    assert!(chat_history(&second_events).is_empty());

    // Diez cada uno; el undécimo se rechaza en local, sin salir.
    let senders = [("anfitrión", &host), ("dos", &first), ("tres", &second)];
    for (who, session) in senders {
        for i in 0..CHAT_RATE_MESSAGES {
            session
                .send_chat(&format!("{who} {i}"))
                .unwrap_or_else(|err| panic!("{who} {i}: {err}"));
        }
        match session.send_chat(&format!("{who} de más")) {
            Err(SessionError::Chat(reason)) => {
                assert!(reason.starts_with("Vas muy rápido"), "{reason}");
                assert!(reason.contains("como mucho 10 cada 10 s"), "{reason}");
            }
            other => panic!("{who}: se esperaba el límite de ritmo: {other:?}"),
        }
    }
    let first_round = chat_messages(&host_events, 3 * CHAT_RATE_MESSAGES);
    assert!(
        texts(&first_round)
            .iter()
            .all(|text| !text.contains("de más"))
    );

    // Pasada la ventana, otra ronda: 52 mensajes en total.
    let mut second_round = Vec::new();
    for (who, session) in senders {
        let count = if who == "tres" { 2 } else { CHAT_RATE_MESSAGES };
        for i in 0..count {
            let text = format!("{who} otra vez {i}");
            send_when_allowed(session, &text);
            second_round.push(text);
        }
    }
    let rest = chat_messages(&host_events, second_round.len());
    let mut all: Vec<ChatLine> = first_round;
    all.extend(rest);
    assert_eq!(all.len(), 52);
    assert!(texts(&all).iter().all(|text| !text.contains("de más")));
    // Los invitados reciben los 52 en el orden del anfitrión.
    for events in [&first_events, &second_events] {
        assert_eq!(texts(&chat_messages(events, all.len())), texts(&all));
    }

    // El tercero entra tarde: los 50 últimos, en orden, con sus autores.
    let mut late = guest(server.config(), &code);
    let late_events = late.events();
    let history = chat_history(&late_events);
    assert_eq!(history.len(), HISTORY);
    assert_eq!(texts(&history), texts(&all[all.len() - HISTORY..]));
    for (got, sent) in history.iter().zip(&all[all.len() - HISTORY..]) {
        assert_eq!(got.author.guest, sent.author.guest);
        assert_eq!(got.at_unix_ms, sent.at_unix_ms);
        assert!(!got.own);
    }
    assert!(history.iter().any(|line| line.author.name == "Jugador 1"));
    assert!(history.iter().any(|line| line.author.name == "Jugador 2"));

    // Vaciar: llega a todos y quien entra después no recibe nada.
    host.clear_chat().expect("vaciar");
    for events in [&host_events, &first_events, &second_events, &late_events] {
        wait_event(events, "el chat vaciado", |event| {
            matches!(event, SessionEvent::ChatCleared).then_some(())
        });
    }
    let mut after = guest(server.config(), &code);
    assert!(chat_history(&after.events()).is_empty());

    after.close();
    late.close();
    second.close();
    first.close();
    host.close();
    drop(server);
}

/// (c) Expulsión: el expulsado no recibe ni descifra lo que se escribe después; lo que escribió
/// se queda.
#[test]
fn c_a_kicked_guest_cannot_read_what_comes_after() {
    let _serial = serial();
    let server = TestServer::start();
    let mut host = host(server.config());
    let host_events = host.events();
    let code = room_code(&host);
    let mut stays = guest(server.config(), &code);
    let stays_events = stays.events();
    let mut kicked = guest(server.config(), &code);
    let kicked_events = kicked.events();
    assert!(chat_history(&stays_events).is_empty());
    assert!(chat_history(&kicked_events).is_empty());

    kicked.send_chat("me van a echar").expect("enviar");
    assert_eq!(texts(&chat_messages(&host_events, 1)), ["me van a echar"]);
    host.send_chat("antes de la expulsión").expect("enviar");
    assert_eq!(
        texts(&chat_messages(&host_events, 1)),
        ["antes de la expulsión"]
    );
    for events in [&stays_events, &kicked_events] {
        assert_eq!(
            texts(&chat_messages(events, 2)),
            ["me van a echar", "antes de la expulsión"]
        );
    }
    let before = host.chat_epoch().expect("época");
    assert_eq!(kicked.chat_epoch(), Some(before));

    host.kick(guest_id(&host, "Jugador 3")).expect("expulsión");
    assert!(kicked.wait(LIMIT), "el expulsado sigue en marcha");
    assert_eq!(kicked.end_reason(), Some(SessionError::Removed));
    let deadline = Instant::now() + LIMIT;
    while host.chat_epoch() == Some(before) || !host.guests().iter().all(|g| g.name != "Jugador 3")
    {
        assert!(Instant::now() < deadline, "la época del chat no cambió");
        std::thread::sleep(Duration::from_millis(20));
    }
    host.send_chat("después de la expulsión").expect("enviar");
    assert_eq!(
        texts(&chat_messages(&stays_events, 1)),
        ["después de la expulsión"]
    );
    let late: Vec<SessionEvent> = kicked_events.try_iter().collect();
    assert!(
        !late
            .iter()
            .any(|event| matches!(event, SessionEvent::ChatMessage(_))),
        "el expulsado no recibe nada: {late:?}"
    );
    assert!(kicked.send_chat("¿sigo aquí?").is_err());

    // Época nueva en el anfitrión que el expulsado no tiene: no puede abrir lo de ahora.
    let new = host.chat_key().expect("clave");
    assert!(new.epoch() > before);
    assert_eq!(stays.chat_epoch(), Some(new.epoch()));
    let old = kicked.chat_key().expect("su última clave");
    assert_eq!(old.epoch(), before);
    let sealed = new
        .seal(ChatKind::Message, host.own_leaf(), u64::MAX, b"secreto")
        .expect("sellar");
    assert!(
        old.open(ChatKind::Message, host.own_leaf(), u64::MAX, &sealed)
            .is_err()
    );
    let stays_key = stays.chat_key().expect("clave");
    assert_eq!(
        stays_key
            .open(ChatKind::Message, host.own_leaf(), u64::MAX, &sealed)
            .expect("el que sigue sí")
            .as_slice(),
        b"secreto"
    );

    // Lo que escribió el expulsado sigue en el historial de quien entra después.
    let mut newcomer = guest(server.config(), &code);
    let history = chat_history(&newcomer.events());
    assert_eq!(
        texts(&history),
        [
            "me van a echar",
            "antes de la expulsión",
            "después de la expulsión"
        ]
    );
    assert_eq!(history[0].author.name, "Jugador 3");

    newcomer.close();
    kicked.close();
    stays.close();
    host.close();
    drop(server);
}
