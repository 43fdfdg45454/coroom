//! Entrada de extremo a extremo en proceso: servidor real (`coroom-server`, puertos
//! efímeros), anfitrión con fuente sintética y un [`RecordingSink`] en lugar del sumidero de la
//! plataforma, e invitados sin ventana que reproducen guiones de entrada, todo en `127.0.0.1`.
//!
//! - (a) sin permiso, nada llega al sumidero y todo se cuenta como descartado;
//! - (b) con teclado y ratón, **10 000 `MouseMove` a 1000 Hz** llegan todos y en orden (la puerta de
//!   la entrada) y las teclas llegan en orden;
//! - (c) ranura de mando 2: botones y ejes llegan con `slot = 2` y otro invitado no puede usarla;
//! - (d) pausa: el sumidero recibe `ReleaseAll` y nada más hasta reanudar;
//! - (e) expulsión: `ReleaseAll` y la ranura queda libre;
//! - (f) latencia de entrada (`recibido − enviado` con el reloj de la sesión) p95 < 5 ms.
//!
//! Los tests van de uno en uno (un cerrojo común) para que no se roben CPU.

mod common;

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use common::TestServer;
use coroom_desktop::remote::play_script;
use coroom_session::capture::synthetic::SyntheticCapturer;
use coroom_session::codec::{DecoderSet, EncoderSet};
use coroom_session::input::script::{InputScript, Step};
use coroom_session::input::{GamepadAxis, GamepadButton, InputEvent, Recorded, RecordingSink};
use coroom_session::latency::{InputLatency, LatencyLedger};
use coroom_session::loopback::{LoopbackConfig, NullSink};
use coroom_session::remote::{GuestStatus, RemoteSession};
use coroom_session::{GuestId, Permissions, Session, SessionError, SessionState};

const LIMIT: Duration = Duration::from_secs(30);

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

/// Anfitrión con vídeo pequeño (la entrada es lo que se mide) y el sumidero de prueba.
fn host(server: &TestServer, sink: &RecordingSink) -> RemoteSession {
    let account = server.config();
    let mut session = RemoteSession::host_with_sink(
        account.identity,
        account.config,
        Box::new(SyntheticCapturer::new(320, 180, 30)),
        EncoderSet::openh264(),
        LoopbackConfig {
            width: 320,
            height: 180,
            fps: 30,
            bitrate_kbps: 500,
            frames: None,
            yuv444: false,
            hdr: false,
        },
        Arc::new(LatencyLedger::new()),
        Box::new(sink.clone()),
    );
    session.start().expect("arranque del anfitrión");
    session
}

fn guest(server: &TestServer, code: &str) -> RemoteSession {
    let account = server.config();
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

fn wait_until(what: &str, limit: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "tiempo agotado esperando: {what}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn room_code(host: &RemoteSession) -> String {
    let mut code = None;
    wait_until("la sala", Duration::from_secs(10), || {
        assert!(!host.is_finished(), "{:?}", host.end_reason());
        code = host.room_code();
        code.is_some()
    });
    code.expect("código")
}

/// Espera a que el invitado `n` (en orden de llegada) esté conectado, con clave de entrada y con el
/// canal de entrada abierto; devuelve su id en el anfitrión.
fn connected(host: &RemoteSession, guest: &RemoteSession, n: usize) -> GuestId {
    let mut id = None;
    wait_until("el invitado conectado con clave", LIMIT, || {
        assert!(!guest.is_finished(), "{:?}", guest.end_reason());
        let guests = host.guests();
        id = guests
            .get(n)
            .filter(|status| status.path.is_some() && status.input_key)
            .map(|status| status.guest);
        id.is_some() && guest.input().ready() && guest.state() == SessionState::Active
    });
    id.expect("id")
}

fn status(host: &RemoteSession, id: GuestId) -> GuestStatus {
    host.guests()
        .into_iter()
        .find(|status| status.guest == id)
        .expect("el invitado sigue en la sala")
}

fn allow(host: &mut RemoteSession, guest: &RemoteSession, id: GuestId, permissions: Permissions) {
    host.set_permissions(id, permissions).expect("permisos");
    wait_until("los permisos en el invitado", LIMIT, || {
        let allowed = guest.input().allowed();
        allowed.keyboard_mouse == permissions.keyboard_mouse
            && allowed.gamepad_slot == permissions.gamepad_slot
    });
}

fn script(steps: Vec<Step>) -> InputScript {
    InputScript { steps }
}

fn key(code: u16, pressed: bool) -> InputEvent {
    InputEvent::Key { code, pressed }
}

fn kbm() -> Permissions {
    Permissions {
        keyboard_mouse: true,
        ..Permissions::VIEW_ONLY
    }
}

fn print_latency(what: &str, latency: &InputLatency) {
    println!(
        "{what}: latencia de entrada p50 {:.3} ms, p95 {:.3} ms, p99 {:.3} ms, máx {:.3} ms ({} \
         muestras); {} eventos aplicados, {} descartados",
        latency.p50_us as f64 / 1000.0,
        latency.p95_us as f64 / 1000.0,
        latency.p99_us as f64 / 1000.0,
        latency.max_us as f64 / 1000.0,
        latency.samples,
        latency.applied,
        latency.dropped
    );
}

#[test]
fn a_without_permission_nothing_arrives_and_it_is_counted() {
    let _serial = serial();
    let server = TestServer::start();
    let sink = RecordingSink::new();
    let mut host = host(&server, &sink);
    let code = room_code(&host);
    let mut guest = guest(&server, &code);
    let id = connected(&host, &guest, 0);
    assert_eq!(guest.input().allowed().label(), "solo mirar");

    let report = play_script(
        &guest.input(),
        &script(vec![
            Step::MouseMoves {
                count: 200,
                hz: 1000.0,
                dx: 1,
                dy: 1,
                numbered: false,
            },
            Step::Event(key(30, true)),
            Step::Event(key(30, false)),
            Step::Event(InputEvent::GamepadButton {
                slot: 1,
                button: GamepadButton::A,
                pressed: true,
            }),
        ]),
    )
    .expect("guion");
    assert_eq!(report.events, 203);
    wait_until("los descartes contados", LIMIT, || {
        status(&host, id).input.dropped >= 203
    });
    let input = status(&host, id).input;
    print_latency("(a)", &input);
    assert_eq!(input.applied, 0, "{input:?}");
    assert_eq!(input.dropped, 203, "{input:?}");
    assert!(
        sink.events_of(id).is_empty(),
        "nada llega al sumidero: {:?}",
        sink.log()
    );
    // El invitado se entera de sus descartes por el InputReport.
    wait_until("el informe de entrada en el invitado", LIMIT, || {
        guest.input().report().is_some_and(|r| r.dropped == 203)
    });
    guest.close();
    host.close();
    drop(server);
}

#[test]
fn b_a_1000_hz_mouse_loses_nothing_and_keys_keep_their_order() {
    const MOVES: u32 = 10_000;
    let _serial = serial();
    let server = TestServer::start();
    let sink = RecordingSink::new();
    let mut host = host(&server, &sink);
    let code = room_code(&host);
    let mut guest = guest(&server, &code);
    let id = connected(&host, &guest, 0);
    allow(&mut host, &guest, id, kbm());

    // Teclas antes, durante (otro hilo) y después de los 10 000 movimientos.
    let keys: Vec<InputEvent> = (0..40u16)
        .flat_map(|i| [key(16 + i % 20, true), key(16 + i % 20, false)])
        .collect();
    let typing = {
        let input = guest.input();
        let keys = keys[20..60].to_vec();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(2000));
            for event in keys {
                input.send(event);
                std::thread::sleep(Duration::from_millis(50));
            }
        })
    };
    let started = Instant::now();
    let report = play_script(
        &guest.input(),
        &script(
            keys[..20]
                .iter()
                .map(|event| Step::Event(*event))
                .chain([Step::MouseMoves {
                    count: MOVES,
                    hz: 1000.0,
                    dx: 0,
                    dy: -1,
                    numbered: true,
                }])
                .collect(),
        ),
    )
    .expect("guion");
    typing.join().expect("hilo de teclas");
    for event in &keys[60..] {
        guest.input().send(*event);
    }
    println!(
        "(b) guion: {} eventos en {:.2} s (a 1000 Hz serían {:.2} s)",
        report.events,
        report.elapsed.as_secs_f64(),
        f64::from(MOVES) / 1000.0
    );
    assert!(started.elapsed() >= Duration::from_secs(9), "a 1000 Hz");

    let total = u64::from(MOVES) + keys.len() as u64;
    wait_until("todos los eventos en el sumidero", LIMIT, || {
        sink.events_of(id).len() as u64 >= total
    });
    let events = sink.events_of(id);
    let moves: Vec<i32> = events
        .iter()
        .filter_map(|event| match event {
            InputEvent::MouseMove { dx, dy } => {
                assert_eq!(*dy, -1);
                Some(*dx)
            }
            _ => None,
        })
        .collect();
    let expected: Vec<i32> = (1..=MOVES as i32).collect();
    assert_eq!(moves.len(), expected.len(), "ningún movimiento perdido");
    assert!(moves == expected, "los movimientos llegan en orden");
    let typed: Vec<InputEvent> = events
        .iter()
        .copied()
        .filter(|event| matches!(event, InputEvent::Key { .. }))
        .collect();
    assert_eq!(typed, keys, "las teclas llegan en orden");

    let sent = guest.input().sender_stats().expect("emisor");
    let input = status(&host, id).input;
    print_latency("(b)", &input);
    println!(
        "(b) emisor: {} eventos, {} datagramas ({:.0}/s), {} mensajes, {} bytes, {} sumados",
        sent.events,
        sent.datagrams,
        sent.datagrams as f64 / report.elapsed.as_secs_f64(),
        sent.messages,
        sent.bytes,
        sent.merged
    );
    assert_eq!(sent.merged, 0);
    assert!(
        sent.datagrams as f64 / report.elapsed.as_secs_f64() <= 520.0,
        "como mucho un datagrama cada 2 ms"
    );
    assert_eq!(input.dropped, 0, "{input:?}");
    assert_eq!(input.applied, total);
    guest.close();
    host.close();
    drop(server);
}

#[test]
fn c_gamepad_slot_2_is_only_for_its_guest() {
    let _serial = serial();
    let server = TestServer::start();
    let sink = RecordingSink::new();
    let mut host = host(&server, &sink);
    let code = room_code(&host);
    let mut first = guest(&server, &code);
    let first_id = connected(&host, &first, 0);
    let mut second = guest(&server, &code);
    let second_id = connected(&host, &second, 1);

    let pad = Permissions {
        gamepad_slot: Some(2),
        ..Permissions::VIEW_ONLY
    };
    allow(&mut host, &first, first_id, pad);
    wait_until("el mando de la ranura 2", LIMIT, || {
        sink.assigned_slots() == vec![2]
    });
    // La ranura 2 ya es del primero: no se le puede dar al segundo.
    assert!(matches!(
        host.set_permissions(second_id, pad),
        Err(SessionError::InvalidPermissions(_))
    ));
    assert!(matches!(
        host.set_permissions(
            second_id,
            Permissions {
                gamepad_slot: Some(9),
                ..Permissions::VIEW_ONLY
            }
        ),
        Err(SessionError::InvalidPermissions(_))
    ));

    let pad_events = |pressed: bool, value: i16| {
        vec![
            InputEvent::GamepadButton {
                slot: 2,
                button: GamepadButton::A,
                pressed,
            },
            InputEvent::GamepadAxis {
                slot: 2,
                axis: GamepadAxis::LeftX,
                value,
            },
            InputEvent::GamepadAxis {
                slot: 2,
                axis: GamepadAxis::RightTrigger,
                value: value.abs(),
            },
        ]
    };
    let mut expected = Vec::new();
    for i in 0..20i16 {
        let events = pad_events(i % 2 == 0, i * 1000 - 9000);
        expected.extend(events.iter().copied());
        play_script(
            &first.input(),
            &script(vec![
                Step::Events {
                    list: events,
                    hz: 250.0,
                },
                Step::WaitMs(4),
            ]),
        )
        .expect("guion del primero");
    }
    // El segundo intenta usar la ranura 2 sin permiso.
    play_script(
        &second.input(),
        &script(vec![Step::Events {
            list: pad_events(true, 32_000),
            hz: 250.0,
        }]),
    )
    .expect("guion del segundo");

    wait_until("los eventos del mando", LIMIT, || {
        sink.events_of(first_id).len() >= expected.len()
    });
    wait_until("los descartes del segundo", LIMIT, || {
        status(&host, second_id).input.dropped >= 3
    });
    let got = sink.events_of(first_id);
    assert!(
        got.iter().all(|event| matches!(
            event,
            InputEvent::GamepadButton { slot: 2, .. } | InputEvent::GamepadAxis { slot: 2, .. }
        )),
        "{got:?}"
    );
    let buttons = |events: &[InputEvent]| -> Vec<InputEvent> {
        events
            .iter()
            .copied()
            .filter(|e| matches!(e, InputEvent::GamepadButton { .. }))
            .collect()
    };
    assert_eq!(buttons(&got), buttons(&expected), "botones en orden");
    let last_axis = got.iter().rev().find(|e| {
        matches!(
            e,
            InputEvent::GamepadAxis {
                axis: GamepadAxis::LeftX,
                ..
            }
        )
    });
    assert_eq!(
        last_axis,
        Some(&InputEvent::GamepadAxis {
            slot: 2,
            axis: GamepadAxis::LeftX,
            value: 19 * 1000 - 9000
        }),
        "el último valor del eje"
    );
    assert!(sink.events_of(second_id).is_empty(), "{:?}", sink.log());
    assert_eq!(status(&host, second_id).input.applied, 0);
    print_latency("(c) primero", &status(&host, first_id).input);
    second.close();
    first.close();
    host.close();
    drop(server);
}

#[test]
fn d_pause_releases_everything_and_drops_until_resumed() {
    let _serial = serial();
    let server = TestServer::start();
    let sink = RecordingSink::new();
    let mut host = host(&server, &sink);
    let code = room_code(&host);
    let mut guest = guest(&server, &code);
    let id = connected(&host, &guest, 0);
    allow(&mut host, &guest, id, kbm());

    guest.input().send(key(17, true));
    wait_until("la W pulsada", LIMIT, || {
        sink.events_of(id) == vec![key(17, true)]
    });
    host.set_paused(true).expect("pausa");
    assert_eq!(host.state(), SessionState::Paused);
    wait_until("ReleaseAll", LIMIT, || {
        sink.log().contains(&Recorded::ReleaseAll { guest: id })
    });
    let at_pause = sink.len();
    play_script(
        &guest.input(),
        &script(vec![
            Step::Event(key(30, true)),
            Step::MouseMoves {
                count: 50,
                hz: 1000.0,
                dx: 2,
                dy: 0,
                numbered: false,
            },
            Step::Event(key(30, false)),
        ]),
    )
    .expect("guion en pausa");
    wait_until("los descartes de la pausa", LIMIT, || {
        status(&host, id).input.dropped >= 52
    });
    assert_eq!(
        sink.len(),
        at_pause,
        "nada más tras ReleaseAll: {:?}",
        sink.log()
    );

    host.set_paused(false).expect("reanudar");
    guest.input().send(key(31, true));
    wait_until("la S tras reanudar", LIMIT, || {
        sink.events_of(id).last() == Some(&key(31, true))
    });
    assert_eq!(sink.len(), at_pause + 1);
    guest.close();
    host.close();
    drop(server);
}

#[test]
fn e_kicking_releases_everything_and_frees_the_slot() {
    let _serial = serial();
    let server = TestServer::start();
    let sink = RecordingSink::new();
    let mut host = host(&server, &sink);
    let code = room_code(&host);
    let mut guest = guest(&server, &code);
    let id = connected(&host, &guest, 0);
    allow(
        &mut host,
        &guest,
        id,
        Permissions {
            keyboard_mouse: true,
            gamepad_slot: Some(2),
            voice: false,
        },
    );
    guest.input().send(InputEvent::GamepadButton {
        slot: 2,
        button: GamepadButton::Start,
        pressed: true,
    });
    guest.input().send(key(57, true));
    wait_until("los dos pulsados", LIMIT, || sink.events_of(id).len() == 2);
    assert_eq!(sink.assigned_slots(), vec![2]);

    host.kick(id).expect("expulsión");
    wait_until("ReleaseAll y la ranura libre", LIMIT, || {
        let log = sink.log();
        log.contains(&Recorded::ReleaseAll { guest: id })
            && log.contains(&Recorded::GamepadReleased { slot: 2 })
    });
    assert!(sink.assigned_slots().is_empty());
    assert!(guest.wait(LIMIT), "el invitado termina");
    assert_eq!(guest.end_reason(), Some(SessionError::Removed));
    wait_until("la salida", LIMIT, || host.guests().is_empty());

    // La ranura 2 vuelve a estar libre para otro.
    let mut other = self::guest(&server, &code);
    let other_id = connected(&host, &other, 0);
    allow(
        &mut host,
        &other,
        other_id,
        Permissions {
            gamepad_slot: Some(2),
            ..Permissions::VIEW_ONLY
        },
    );
    wait_until("la ranura 2 del nuevo", LIMIT, || {
        sink.assigned_slots() == vec![2]
    });
    other.close();
    guest.close();
    host.close();
    drop(server);
}

#[test]
fn f_input_latency_on_localhost_is_under_5_ms_at_p95() {
    let _serial = serial();
    let server = TestServer::start();
    let sink = RecordingSink::new();
    let mut host = host(&server, &sink);
    let code = room_code(&host);
    let mut guest = guest(&server, &code);
    let id = connected(&host, &guest, 0);
    allow(&mut host, &guest, id, kbm());
    // El reloj de la sesión necesita algún `Ping`/`Pong` (uno por segundo) para el desfase.
    wait_until("el desfase del reloj", LIMIT, || {
        guest.link().is_some_and(|link| link.rtt_ms > 0.0)
    });
    std::thread::sleep(Duration::from_millis(1500));
    let keys: Vec<InputEvent> = (0..100)
        .flat_map(|i| [key(30 + i % 10, true), key(30 + i % 10, false)])
        .collect();
    play_script(
        &guest.input(),
        &script(vec![
            Step::MouseMoves {
                count: 3000,
                hz: 1000.0,
                dx: 1,
                dy: 1,
                numbered: false,
            },
            Step::Events {
                list: keys,
                hz: 100.0,
            },
        ]),
    )
    .expect("guion");
    wait_until("todo aplicado", LIMIT, || {
        status(&host, id).input.applied >= 3200
    });
    let input = status(&host, id).input;
    print_latency("(f) anfitrión", &input);
    let summary = host.summary();
    let total = summary.input.expect("latencia de entrada en el resumen");
    print_latency("(f) resumen del anfitrión", &total);
    assert!(input.samples >= 1500, "{input:?}");
    assert!(
        input.p95_us < 5_000,
        "p95 de entrada {:.3} ms",
        input.p95_us as f64 / 1000.0
    );
    // El invitado recibe la misma medida para su overlay.
    wait_until("el informe en el invitado", LIMIT, || {
        guest
            .summary()
            .input
            .is_some_and(|report| report.applied >= 3200)
    });
    let shown = guest.summary().input.expect("informe");
    println!(
        "(f) overlay del invitado: entrada: {:.1} ms (p95 {:.1})",
        shown.p50_us as f64 / 1000.0,
        shown.p95_us as f64 / 1000.0
    );
    guest.close();
    host.close();
    drop(server);
}

/// (Extra) Por el relay: varios invitados en una sola conexión del anfitrión; la entrada de cada
/// uno se identifica por su clave pareja a pareja y llega igual de completa.
#[test]
fn g_input_through_the_relay_arrives_complete() {
    let _serial = serial();
    let server = TestServer::start();
    let sink = RecordingSink::new();
    let mut host = host(&server, &sink);
    let code = room_code(&host);
    let mut account = server.config();
    account.force_relay = true;
    let mut guest = RemoteSession::guest(
        account.identity,
        account.config,
        &code,
        DecoderSet::openh264(),
        Box::new(NullSink),
        None,
        Arc::new(LatencyLedger::new()),
    );
    guest.start().expect("arranque del invitado");
    let id = connected(&host, &guest, 0);
    assert_eq!(
        status(&host, id).path,
        Some(coroom_transport::Path::Relayed)
    );
    allow(&mut host, &guest, id, kbm());
    play_script(
        &guest.input(),
        &script(vec![
            Step::Event(key(44, true)),
            Step::MouseMoves {
                count: 1000,
                hz: 1000.0,
                dx: 0,
                dy: 0,
                numbered: true,
            },
            Step::Event(key(44, false)),
        ]),
    )
    .expect("guion");
    wait_until("todo por el relay", LIMIT, || {
        sink.events_of(id).len() >= 1002
    });
    let events = sink.events_of(id);
    // El stream y los datagramas no se ordenan entre sí; dentro de cada canal, sí.
    let keys: Vec<InputEvent> = events
        .iter()
        .copied()
        .filter(|event| matches!(event, InputEvent::Key { .. }))
        .collect();
    assert_eq!(keys, vec![key(44, true), key(44, false)]);
    let moves: Vec<i32> = events
        .iter()
        .filter_map(|event| match event {
            InputEvent::MouseMove { dx, .. } => Some(*dx),
            _ => None,
        })
        .collect();
    assert_eq!(moves, (1..=1000).collect::<Vec<_>>());
    print_latency("(g) relay", &status(&host, id).input);
    guest.close();
    host.close();
    drop(server);
}
