//! Entrada de extremo a extremo en proceso: servidor real (`coroom-server`, puertos
//! efímeros), anfitrión con fuente sintética y un [`RecordingSink`] en lugar del sumidero de la
//! plataforma, e invitados sin ventana que reproducen guiones de entrada, todo en `127.0.0.1`.
//!
//! - (a) sin permiso, nada llega al sumidero y todo se cuenta como descartado;
//! - (b) con teclado y ratón, **3 000 `MouseMove` a 1000 Hz** llegan todos y en orden (la puerta de
//!   la entrada) y las teclas llegan en orden;
//! - (c) ranura de mando 2: botones y ejes llegan con `slot = 2` y otro invitado no puede usarla;
//! - (d) pausa: el sumidero recibe `ReleaseAll` y nada más hasta reanudar;
//! - (e) expulsión: `ReleaseAll` y la ranura queda libre;
//! - (f) latencia de entrada (`recibido − enviado` con el reloj de la sesión) p95 < 5 ms.
//!
//! El emisor suma los `MouseMove` relativos cuando el paquete pendiente se llena (64 eventos: un
//! parón de más de 64 ms del hilo a 1000 Hz) y funde siempre los `GamepadAxis` del mismo eje: el
//! desplazamiento total se conserva, pero el número exacto de eventos depende de la carga. Lo
//! funcional es la suma de los desplazamientos y el orden de las teclas y botones; la cuenta exacta
//! y la latencia van con `timing_assert!` (avisan sin hacer fallar la prueba).
//!
//! Para que no se roben CPU, con `cargo test` van de uno en uno (un cerrojo común); con nextest,
//! cada test es un proceso aparte y su grupo de `.config/nextest.toml` deja dos a la vez.

mod common;

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use common::TestServer;
use coroom_desktop::remote::play_script;
use coroom_session::capture::synthetic::SyntheticCapturer;
use coroom_session::codec::{DecoderSet, EncoderSet};
use coroom_session::input::gamepad::PadLink;
use coroom_session::input::script::{InputScript, Step};
use coroom_session::input::{GamepadAxis, GamepadButton, InputEvent, Recorded, RecordingSink};
use coroom_session::latency::{InputLatency, LatencyLedger};
use coroom_session::loopback::{LoopbackConfig, NullSink};
use coroom_session::remote::{GuestStatus, PadTransport, RemoteSession};
use coroom_session::transport::proto::input::{GamepadDevice, GamepadTransport, PadData};
use coroom_session::{GuestId, Permissions, Session, SessionError, SessionState};
use coroom_testkit::timing_assert;

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

/// Los movimientos numerados (`dx` = 1, 2, 3…) llegan en orden aunque el emisor haya sumado
/// algunos (solo funde uno con el último pendiente): tras cada uno, Σ dx = n(n+1)/2 con `n`
/// creciente. Devuelve los `n`.
fn assert_numbered_in_order(dxs: impl IntoIterator<Item = i32>) -> Vec<i64> {
    let mut sum = 0i64;
    let mut ns = Vec::new();
    for (index, dx) in dxs.into_iter().enumerate() {
        sum += i64::from(dx);
        // n(n+1)/2 = sum → n = (√(8·sum+1) − 1)/2.
        let n = ((((8 * sum + 1) as f64).sqrt() - 1.0) / 2.0).round() as i64;
        assert!(
            n * (n + 1) / 2 == sum && ns.last().is_none_or(|&last| n > last),
            "movimiento {index}: Σ dx = {sum} no sigue a los anteriores ({:?})",
            ns.last()
        );
        ns.push(n);
    }
    ns
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
    // Cada evento que sale se descarta; los que el emisor sumó a otro (parón) no salen solos.
    let merged = || guest.input().sender_stats().map_or(0, |sent| sent.merged);
    wait_until("los descartes contados", LIMIT, || {
        status(&host, id).input.dropped + merged() >= 203
    });
    let input = status(&host, id).input;
    let merged = merged();
    print_latency("(a)", &input);
    assert_eq!(input.applied, 0, "{input:?}");
    assert_eq!(
        input.dropped + merged,
        203,
        "{} descartados y {merged} sumados en el emisor: {input:?}",
        input.dropped
    );
    timing_assert!(
        input.dropped == 203,
        "{} descartados de 203 ({merged} sumados en el emisor)",
        input.dropped
    );
    assert!(
        sink.events_of(id).is_empty(),
        "nada llega al sumidero: {:?}",
        sink.log()
    );
    // El invitado se entera de sus descartes por el InputReport.
    wait_until("el informe de entrada en el invitado", LIMIT, || {
        guest
            .input()
            .report()
            .is_some_and(|r| r.dropped >= input.dropped)
    });
    guest.close();
    host.close();
    drop(server);
}

#[test]
fn b_a_1000_hz_mouse_loses_nothing_and_keys_keep_their_order() {
    const MOVES: u32 = 3_000;
    let _serial = serial();
    let server = TestServer::start();
    let sink = RecordingSink::new();
    let mut host = host(&server, &sink);
    let code = room_code(&host);
    let mut guest = guest(&server, &code);
    let id = connected(&host, &guest, 0);
    allow(&mut host, &guest, id, kbm());

    // Teclas antes, durante (otro hilo) y después de los 3 000 movimientos.
    let keys: Vec<InputEvent> = (0..40u16)
        .flat_map(|i| [key(16 + i % 20, true), key(16 + i % 20, false)])
        .collect();
    let typing = {
        let input = guest.input();
        let keys = keys[20..60].to_vec();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
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
    // El guion va contra reloj: no puede acabar antes de lo que dan 3 000 eventos a 1000 Hz.
    assert!(
        started.elapsed() >= Duration::from_millis(2_900),
        "a 1000 Hz"
    );

    let total = u64::from(MOVES) + keys.len() as u64;
    let moves_of = |events: &[InputEvent]| -> Vec<(i32, i32)> {
        events
            .iter()
            .filter_map(|event| match event {
                InputEvent::MouseMove { dx, dy } => Some((*dx, *dy)),
                _ => None,
            })
            .collect()
    };
    // Sumados o no, los desplazamientos llegan enteros: Σ dx = 1 + 2 + … + MOVES y Σ dy = −MOVES.
    let sum_dx = i64::from(MOVES) * (i64::from(MOVES) + 1) / 2;
    let keys_of = |events: &[InputEvent]| -> Vec<InputEvent> {
        events
            .iter()
            .copied()
            .filter(|event| matches!(event, InputEvent::Key { .. }))
            .collect()
    };
    wait_until("todos los eventos en el sumidero", LIMIT, || {
        let events = sink.events_of(id);
        let moves = moves_of(&events);
        moves.iter().map(|&(dx, _)| i64::from(dx)).sum::<i64>() == sum_dx
            && moves.iter().map(|&(_, dy)| i64::from(dy)).sum::<i64>() == -i64::from(MOVES)
            && keys_of(&events).len() == keys.len()
    });
    let events = sink.events_of(id);
    let moves = moves_of(&events);
    let expected: Vec<(i32, i32)> = (1..=MOVES as i32).map(|dx| (dx, -1)).collect();
    // En orden aunque se sumen: Σ dx es triangular tras cada uno y −Σ dy cuenta los sumados.
    let ns = assert_numbered_in_order(moves.iter().map(|&(dx, _)| dx));
    let mut sum_dy = 0i64;
    for (index, (&(_, dy), n)) in moves.iter().zip(&ns).enumerate() {
        sum_dy += i64::from(dy);
        assert_eq!(
            -sum_dy, *n,
            "movimiento {index}: Σ dy no cuenta los movimientos"
        );
    }
    timing_assert!(
        moves.len() == expected.len(),
        "{} movimientos de {MOVES}: el emisor sumó los de algún parón",
        moves.len()
    );
    timing_assert!(
        moves == expected,
        "los movimientos llegan uno a uno y en orden"
    );
    let typed = keys_of(&events);
    assert_eq!(typed, keys, "las teclas llegan en orden");

    let sent = guest.input().sender_stats().expect("emisor");
    // Los contadores se publican cada 100 ms: esperar a que conste todo lo que llegó al sumidero.
    let arrived = events.len() as u64;
    wait_until("los contadores publicados", LIMIT, || {
        let input = status(&host, id).input;
        input.applied + input.dropped >= arrived
    });
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
    timing_assert!(
        sent.merged == 0,
        "el emisor sumó {} movimientos",
        sent.merged
    );
    let rate = sent.datagrams as f64 / report.elapsed.as_secs_f64();
    timing_assert!(
        rate <= 1_040.0,
        "como mucho un datagrama cada ms: {rate:.0} por segundo"
    );
    assert_eq!(sent.messages, 0, "nada va por el stream");
    assert_eq!(input.dropped, 0, "{input:?}");
    timing_assert!(
        input.applied == total,
        "{} aplicados de {total}",
        input.applied
    );
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

    let buttons = |events: &[InputEvent]| -> Vec<InputEvent> {
        events
            .iter()
            .copied()
            .filter(|e| matches!(e, InputEvent::GamepadButton { .. }))
            .collect()
    };
    let last_axis = |events: &[InputEvent]| -> Option<InputEvent> {
        events.iter().rev().copied().find(|e| {
            matches!(
                e,
                InputEvent::GamepadAxis {
                    axis: GamepadAxis::LeftX,
                    ..
                }
            )
        })
    };
    let final_axis = InputEvent::GamepadAxis {
        slot: 2,
        axis: GamepadAxis::LeftX,
        value: 19 * 1000 - 9000,
    };
    // Los ejes del mismo eje se funden en el emisor si coinciden en un paquete: se espera a los 20
    // botones y al último valor del eje, no a un número de eventos.
    wait_until("los eventos del mando", LIMIT, || {
        let got = sink.events_of(first_id);
        buttons(&got).len() >= 20 && last_axis(&got) == Some(final_axis)
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
    assert_eq!(buttons(&got), buttons(&expected), "botones en orden");
    assert_eq!(last_axis(&got), Some(final_axis), "el último valor del eje");
    timing_assert!(
        got.len() == expected.len(),
        "{} eventos del mando de {}: el emisor fundió ejes",
        got.len(),
        expected.len()
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
            gamepad_raw: true,
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
    const MOVES: u64 = 1_000;
    let keys: Vec<InputEvent> = (0..25)
        .flat_map(|i| [key(30 + i % 10, true), key(30 + i % 10, false)])
        .collect();
    let total = MOVES + keys.len() as u64;
    play_script(
        &guest.input(),
        &script(vec![
            Step::MouseMoves {
                count: MOVES as u32,
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
    // Lo que el emisor sumó a otro movimiento (en un parón) no se aplica por separado.
    wait_until("todo aplicado", LIMIT, || {
        let merged = guest.input().sender_stats().map_or(0, |sent| sent.merged);
        status(&host, id).input.applied + merged >= total
    });
    let input = status(&host, id).input;
    print_latency("(f) anfitrión", &input);
    let summary = host.summary();
    let in_summary = summary.input.expect("latencia de entrada en el resumen");
    print_latency("(f) resumen del anfitrión", &in_summary);
    // Una muestra por paquete, y el invitado puede juntar varios eventos por paquete: con 1 050
    // eventos salen unas 950 en Linux y menos en Windows, con temporizadores más gruesos.
    // Trescientas bastan para el p95.
    assert!(input.samples > 0, "{input:?}");
    timing_assert!(
        input.samples >= 300,
        "solo {} muestras de latencia",
        input.samples
    );
    timing_assert!(
        input.p95_us < 5_000,
        "p95 de entrada {:.3} ms",
        input.p95_us as f64 / 1000.0
    );
    // El invitado recibe la misma medida para su overlay.
    wait_until("el informe en el invitado", LIMIT, || {
        guest
            .summary()
            .input
            .is_some_and(|report| report.applied >= input.applied)
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
    let keys_of = |events: &[InputEvent]| -> Vec<InputEvent> {
        events
            .iter()
            .copied()
            .filter(|event| matches!(event, InputEvent::Key { .. }))
            .collect()
    };
    let moves_of = |events: &[InputEvent]| -> Vec<i32> {
        events
            .iter()
            .filter_map(|event| match event {
                InputEvent::MouseMove { dx, .. } => Some(*dx),
                _ => None,
            })
            .collect()
    };
    // Sumados o no en un parón del emisor, Σ dx = 1 + 2 + … + 1000.
    wait_until("todo por el relay", LIMIT, || {
        let events = sink.events_of(id);
        keys_of(&events).len() >= 2
            && moves_of(&events)
                .iter()
                .map(|&dx| i64::from(dx))
                .sum::<i64>()
                == 500_500
    });
    let events = sink.events_of(id);
    // Todo va por datagramas, en orden.
    assert_eq!(keys_of(&events), vec![key(44, true), key(44, false)]);
    assert_numbered_in_order(moves_of(&events));
    timing_assert!(
        moves_of(&events) == (1..=1000).collect::<Vec<_>>(),
        "los 1000 movimientos no llegaron uno a uno ({}): el emisor sumó los de algún parón",
        moves_of(&events).len()
    );
    print_latency("(g) relay", &status(&host, id).input);
    guest.close();
    host.close();
    drop(server);
}

/// Un mando HID sencillo: 8 botones y dos ejes, informe 1.
const PAD_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x85, 0x01, 0x05, 0x09, 0x19, 0x01, 0x29, 0x08, 0x15, 0x00,
    0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00,
    0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x02, 0x81, 0x02, 0xc0,
];

/// El mismo mando con un teclado de arranque en el mismo descriptor.
fn pad_with_keyboard() -> Vec<u8> {
    let mut descriptor = PAD_DESCRIPTOR.to_vec();
    descriptor.extend_from_slice(&[
        0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x85, 0x02, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15,
        0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0xc0,
    ]);
    descriptor
}

fn hid_pad(slot: u8, descriptor: Vec<u8>) -> PadData {
    PadData::Device {
        slot,
        device: GamepadDevice {
            name: "Mando de prueba".to_owned(),
            vendor: 0x1234,
            product: 0x5678,
            version: 0x0100,
            bus: 3,
            transport: GamepadTransport::Hid {
                descriptor,
                features: Vec::new(),
            },
        },
    }
}

#[test]
fn h_a_raw_gamepad_follows_the_host_choice() {
    let _serial = serial();
    let server = TestServer::start();
    let sink = RecordingSink::new();
    let mut host = host(&server, &sink);
    let code = room_code(&host);
    let mut guest = guest(&server, &code);
    let id = connected(&host, &guest, 0);
    let pad = Permissions {
        gamepad_slot: Some(2),
        ..Permissions::VIEW_ONLY
    };
    allow(&mut host, &guest, id, pad);
    wait_until("la ranura 2", LIMIT, || sink.assigned_slots() == vec![2]);

    // Tal cual: el anfitrión recrea el mando y se lo dice al invitado.
    let input = guest.input();
    input.send_pad(hid_pad(2, PAD_DESCRIPTOR.to_vec()));
    wait_until("el mando tal cual", LIMIT, || {
        sink.raw_slots() == vec![2] && input.raw(2)
    });
    let shown = status(&host, id).gamepad.expect("el mando del invitado");
    assert_eq!((shown.vendor, shown.product), (0x1234, 0x5678));
    assert_eq!(shown.transport, PadTransport::Hid);
    assert!(shown.raw && shown.note.is_none(), "{shown:?}");
    input.send_pad(PadData::Report {
        slot: 2,
        data: vec![1, 0x05, 0x80, 0x7f],
    });
    wait_until("el informe", LIMIT, || {
        sink.log()
            .contains(&Recorded::RawReport { slot: 2, len: 4 })
    });

    // Convertido: se quita el mando tal cual y vuelve el de Xbox 360.
    allow(
        &mut host,
        &guest,
        id,
        Permissions {
            gamepad_raw: false,
            ..pad
        },
    );
    wait_until("el convertido", LIMIT, || {
        sink.raw_slots().is_empty() && !input.raw(2)
    });
    assert!(sink.log().contains(&Recorded::RawDisconnected { slot: 2 }));

    // Un descriptor que también declara teclado no va tal cual sin permiso de teclado.
    input.send_pad(hid_pad(2, pad_with_keyboard()));
    allow(&mut host, &guest, id, pad);
    wait_until("el motivo", LIMIT, || {
        status(&host, id)
            .gamepad
            .and_then(|shown| shown.note)
            .is_some_and(|note| note.contains("teclado"))
    });
    assert!(sink.raw_slots().is_empty());
    assert!(!input.raw(2));
    guest.close();
    host.close();
    drop(server);
}
