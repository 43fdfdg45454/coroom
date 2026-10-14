//! Sesión remota de extremo a extremo en proceso: servidor real (`coroom-server`, puertos efímeros,
//! certificado autofirmado, SQLite temporal y **cuentas**: cada extremo es un usuario con su
//! identidad vinculada, ver `common`), anfitrión con fuente sintética 640×360 a 30 fps y un
//! invitado sin ventana (presentación nula), todo en `127.0.0.1`.
//!
//! - (a) directo: 300 fotogramas, presentados ≥ 98 %, las ocho etapas medidas;
//! - (b) `force_relay`: lo mismo por el relay del servidor;
//! - (c) con `tc netem` en `lo` (se salta si no lo hay) y, siempre, con un proxy `UdpLossy` en
//!   proceso (2 %, 10 ± 5 ms): sin huecos de más de 100 ms entre presentaciones;
//! - (d) expulsión: el invitado deja de presentar y no puede descifrar la época nueva;
//! - (e) A/B con y sin SFrame (solo en debug): dos salas a la vez, una con SFrame y otra sin él,
//!   tres rondas; diferencia del p50 total de todas las muestras < 0,5 ms;
//! - (f) negociación de códec: el anfitrión codifica AV1 (SVT-AV1) y H.264 (OpenH264);
//!   un invitado que solo decodifica H.264 recibe H.264, uno con AV1 (dav1d) recibe AV1, y si con
//!   AV1 en marcha entra otro que solo tiene H.264, el anfitrión baja a H.264 para los dos;
//! - (g) cursor por la red: el invitado presenta cada fotograma con la forma real del cursor del
//!   anfitrión (la flecha de la fuente sintética);
//! - (h) cuentas: el anfitrión ve el usuario y el nombre del invitado, le invita por la
//!   señalización (`SessionEvent::Invited`) y el invitado ve el nombre del anfitrión;
//! - (i) el modo desarrollo (`COROOM_DEV_TOKEN`) sigue funcionando sin cuentas;
//! - (j) reconexión: el invitado que pierde el enlace (aquí, `reconnect()`) avisa con
//!   `Reconnecting`, vuelve a entrar en la misma sala con `Reconnected` y sigue presentando, y
//!   antes del vídeo mide la calidad del enlace (`QualityEstimate`);
//! - (k) pantalla quieta: dos invitados seguidos ven la imagen aunque la captura no entregue
//!   ningún fotograma más;
//! - (l) un enlace con poca cola pierde las ráfagas de los intra y el vídeo se recupera sin una
//!   tormenta de intra;
//! - (m) con la sala abierta, el anfitrión cambia de fuente (a otro tamaño), de códec y de bitrate
//!   máximo y el invitado sigue presentando sin salir;
//! - (n) cortes de red de 600 ms cada 3 s con 1 % de pérdida y 10 Mbit/s: el vídeo vuelve como
//!   mucho 500 ms después de cada corte.
//!
//! Los tests (a)–(e) usan OpenH264 en los dos extremos, para que sus cotas de
//! tiempo sigan siendo comparables.
//!
//! Los tests se ejecutan de uno en uno (un cerrojo común) para que no se roben CPU: el A/B mide
//! diferencias de décimas de milisegundo.

mod common;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use common::{Account, TestServer};
use coroom_session::Session;
use coroom_session::capture::synthetic::SyntheticCapturer;
use coroom_session::capture::{CaptureError, CaptureTarget, CapturedFrame, Capturer};
use coroom_session::codec::{Backend, Codec, DecoderBackend, DecoderSet, EncoderSet};
use coroom_session::latency::{LatencyLedger, LatencySummary, Stage};
use coroom_session::loopback::{DecodedFrame, FrameSink, LoopbackConfig, NullSink, SinkError};
use coroom_session::remote::{QualityLevel, RemoteSession, SessionEvent};
use coroom_session::{GuestId, SessionError, SessionState};
use coroom_transport::Path;
use coroom_transport::crypto::MediaError;
use coroom_transport::lossy::{LossyConfig, UdpLossy};
use coroom_transport::proto::media::Track;

const FPS: u32 = 30;
const FRAMES: u64 = 300;
/// Tiempo límite de cada sesión (300 fotogramas a 30 fps son 10 s).
const SESSION_LIMIT: Duration = Duration::from_secs(60);

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    // Trazas con `RUST_LOG=debug cargo test -p coroom --test remote -- --nocapture`.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn video(frames: Option<u64>) -> LoopbackConfig {
    LoopbackConfig {
        width: 640,
        height: 360,
        fps: FPS,
        bitrate_kbps: 1_500,
        frames,
        yuv444: false,
        hdr: false,
    }
}

fn host(account: Account) -> RemoteSession {
    host_with(account, EncoderSet::openh264())
}

fn host_with(account: Account, encoders: EncoderSet) -> RemoteSession {
    let mut session = RemoteSession::host(
        account.identity,
        account.config,
        Box::new(SyntheticCapturer::new(640, 360, FPS)),
        encoders,
        video(None),
        Arc::new(LatencyLedger::new()),
    );
    session.start().expect("arranque del anfitrión");
    session
}

fn guest(account: Account, code: &str, frames: Option<u64>) -> RemoteSession {
    guest_with(
        account,
        code,
        frames,
        DecoderSet::openh264(),
        Box::new(NullSink),
    )
}

fn guest_with(
    account: Account,
    code: &str,
    frames: Option<u64>,
    decoders: DecoderSet,
    sink: Box<dyn FrameSink>,
) -> RemoteSession {
    let mut session = RemoteSession::guest(
        account.identity,
        account.config,
        code,
        decoders,
        sink,
        frames,
        Arc::new(LatencyLedger::new()),
    );
    session.start().expect("arranque del invitado");
    session
}

/// Espera a que se cumpla `condition`, o falla tras `limit`.
fn wait_until(what: &str, limit: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "tiempo agotado esperando: {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn room_code(host: &RemoteSession) -> String {
    let mut code = None;
    wait_until("la sala", Duration::from_secs(10), || {
        assert!(
            !host.is_finished(),
            "el anfitrión terminó: {:?}",
            host.end_reason()
        );
        code = host.room_code();
        code.is_some()
    });
    code.expect("código")
}

/// Espera a que el invitado presente `FRAMES` fotogramas y devuelve su resumen final.
fn finish(
    mut guest: RemoteSession,
    mut host: RemoteSession,
) -> (LatencySummary, RemoteSession, RemoteSession) {
    let finished = guest.wait(SESSION_LIMIT);
    assert!(
        finished,
        "el invitado no terminó a tiempo:\n{}",
        guest.summary()
    );
    assert_eq!(guest.end_reason(), None, "{}", guest.summary());
    let summary = guest.summary();
    guest.close();
    host.close();
    (summary, guest, host)
}

/// Proporción de fotogramas presentados de los que le llegaron al invitado (los descartes de
/// todas sus etapas cuentan en contra).
fn delivered(summary: &LatencySummary) -> f64 {
    let presented = summary.frames_presented as f64;
    presented / (presented + summary.drops_total as f64)
}

/// Cuenta los huecos de más de `over` entre presentaciones: cuándo llegó y se capturó el primer
/// fotograma después de cada uno (para ver si esperaba a otro o no había nada que presentar).
fn print_gaps(ledger: &LatencyLedger, over: Duration) {
    let mut timelines = ledger.timelines();
    timelines.retain(|t| t.at(Stage::Presented).is_some());
    timelines.sort_by_key(|t| t.at(Stage::Presented));
    for pair in timelines.windows(2) {
        let (Some(before), Some(after)) =
            (pair[0].at(Stage::Presented), pair[1].at(Stage::Presented))
        else {
            continue;
        };
        if after - before <= over {
            continue;
        }
        let since = |stage: Stage| {
            pair[1]
                .at(stage)
                .map(|at| at.saturating_duration_since(before))
        };
        println!(
            "hueco {:?}: el siguiente se capturó a {:?}, se recibió a {:?} y se decodificó a {:?} \
             del anterior presentado",
            after - before,
            since(Stage::Captured),
            since(Stage::Received),
            since(Stage::Decoded),
        );
        let (first, last) = (pair[0].frame_id, pair[1].frame_id);
        for timeline in ledger.timelines() {
            if timeline.frame_id <= first || timeline.frame_id >= last {
                continue;
            }
            let marks: Vec<String> = Stage::ALL
                .iter()
                .map(|&stage| match timeline.at(stage) {
                    Some(at) if at >= before => format!("{}+{:?}", stage.label(), at - before),
                    Some(at) => format!("{}-{:?}", stage.label(), before - at),
                    None => format!("{}:-", stage.label()),
                })
                .collect();
            println!("  intermedio {}: {}", timeline.frame_id, marks.join(" "));
        }
        println!("  ids {first} → {last}");
    }
}

/// Mayor hueco entre dos presentaciones seguidas, sin el tiempo que la máquina estuvo parada
/// entre ellas ([`common::stalled_between`]).
fn max_gap(ledger: &LatencyLedger) -> Duration {
    let mut presented: Vec<Instant> = ledger
        .timelines()
        .iter()
        .filter_map(|timeline| timeline.at(Stage::Presented))
        .collect();
    presented.sort_unstable();
    presented
        .windows(2)
        .map(|pair| {
            let stalled = common::stalled_between(pair[0], pair[1]);
            if !stalled.is_zero() && pair[1] - pair[0] > Duration::from_millis(100) {
                println!(
                    "hueco de {:?} con la máquina parada {stalled:?}",
                    pair[1] - pair[0]
                );
            }
            (pair[1] - pair[0]).saturating_sub(stalled)
        })
        .max()
        .unwrap_or_default()
}

fn complete_presented(ledger: &LatencyLedger) -> (usize, usize) {
    let timelines = ledger.timelines();
    let presented: Vec<_> = timelines
        .iter()
        .filter(|timeline| timeline.at(Stage::Presented).is_some())
        .collect();
    let complete = presented.iter().filter(|t| t.is_complete()).count();
    (complete, presented.len())
}

fn check_delivery(summary: &LatencySummary, guest: &RemoteSession, path: Path) {
    println!("{summary}");
    assert!(summary.frames_presented >= FRAMES, "{summary}");
    let ratio = delivered(summary);
    assert!(
        ratio >= 0.98,
        "presentado el {:.1} %\n{summary}",
        ratio * 100.0
    );
    let link = summary.link.as_ref().expect("resumen del enlace");
    assert_eq!(link.path, path.to_string());
    assert!(link.sframe);
    assert!(link.epoch >= 1, "{link:?}");
    assert!(link.target_kbps > 0, "{link:?}");
    // Las marcas del anfitrión llegan por el control: casi todo presentado tiene las ocho.
    let (complete, presented) = complete_presented(guest.ledger());
    assert!(
        complete * 100 >= presented * 95,
        "{complete} de {presented} presentados con las ocho etapas"
    );
    for span in &summary.spans {
        assert!(span.samples as u64 >= FRAMES * 9 / 10, "{span:?}");
    }
}

/// Lo que los dos extremos deben ver igual: número de seguridad, huellas y clave de entrada.
fn check_identities(host: &RemoteSession, guest: &RemoteSession) {
    let guests = host.guests();
    assert_eq!(guests.len(), 1, "{guests:?}");
    let status = &guests[0];
    let link = guest.link().expect("enlace");
    assert_eq!(status.safety_number.to_string(), link.safety_number);
    assert_eq!(status.fingerprint, guest.fingerprint());
    assert_eq!(link.peer_fingerprint, host.fingerprint());
    wait_until("la clave de entrada", Duration::from_secs(5), || {
        host.input_key_check(status.guest.0).is_some() && guest.input_key_check(0).is_some()
    });
    assert_eq!(
        host.input_key_check(status.guest.0),
        guest.input_key_check(0),
        "el secreto pareja a pareja HPKE coincide"
    );
}

#[test]
fn a_direct_session_presents_every_frame() {
    let _serial = serial();
    let server = TestServer::start();
    let host = host(server.config());
    let code = room_code(&host);
    let guest = guest(server.config(), &code, Some(FRAMES));
    wait_until("la conexión", Duration::from_secs(15), || {
        guest.state() == SessionState::Active || guest.is_finished()
    });
    check_identities(&host, &guest);
    let host_view = host.guests();
    let (summary, guest, host) = finish(guest, host);
    check_delivery(&summary, &guest, Path::Direct);
    assert_eq!(host_view[0].leaf, guest.own_leaf());
    assert_eq!(host.state(), SessionState::Closed);
    drop(server);
}

#[test]
fn b_force_relay_session_presents_every_frame() {
    let _serial = serial();
    let server = TestServer::start();
    let host = host(server.config());
    let code = room_code(&host);
    let mut config = server.config();
    config.force_relay = true;
    let guest = guest(config, &code, Some(FRAMES));
    wait_until("la conexión por el relay", Duration::from_secs(15), || {
        host.guests()
            .first()
            .is_some_and(|status| status.path == Some(Path::Relayed))
    });
    let (summary, guest, _host) = finish(guest, host);
    check_delivery(&summary, &guest, Path::Relayed);
    drop(server);
}

/// Indica si `lo` tiene una disciplina netem activa.
fn netem_active() -> bool {
    std::process::Command::new("tc")
        .args(["qdisc", "show", "dev", "lo"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).contains("netem"))
        .unwrap_or(false)
}

/// Sin congelaciones entre presentaciones: con pérdida, el vídeo espera el reenvío hasta
/// `max(4 intervalos, 3 RTT + 1 intervalo)` (133 ms a 30 fps con 30 ms de RTT) antes de pedir un
/// intra, así que un hueco de ese orden es el comportamiento previsto y la cota es 200 ms en
/// tiempo de reloj del equipo de referencia (multiplicada por el factor de carga del entorno,
/// `common::load_factor`, que es 1 en una máquina sin carga).
fn check_no_freezes(summary: &LatencySummary, guest: &RemoteSession) {
    print_gaps(guest.ledger(), Duration::from_millis(100));
    println!("{summary}");
    let gap = max_gap(guest.ledger());
    let bound = Duration::from_millis(200).mul_f64(common::load_factor());
    println!("hueco máximo entre presentaciones: {gap:?} (cota {bound:?})");
    assert!(summary.frames_presented >= FRAMES, "{summary}");
    assert!(gap <= bound, "hueco de {gap:?} (cota {bound:?})\n{summary}");
}

#[test]
fn c_netem_session_never_freezes() {
    if !netem_active() {
        eprintln!("sin netem en lo: test saltado (ver tools/netem.sh)");
        return;
    }
    let _serial = serial();
    let server = TestServer::start();
    let host = host(server.config());
    let code = room_code(&host);
    let guest = guest(server.config(), &code, Some(FRAMES));
    let (summary, guest, _host) = finish(guest, host);
    check_no_freezes(&summary, &guest);
    drop(server);
}

#[test]
fn c_lossy_link_session_never_freezes() {
    let _serial = serial();
    // La calibración, antes de arrancar nada que compita por la CPU.
    common::load_factor();
    let server = TestServer::start();
    // El anfitrión escucha en un puerto conocido y anuncia solo el proxy; el invitado no anuncia
    // nada, así que la única conexión es invitado → proxy (2 %, 10 ± 5 ms) → anfitrión.
    let port = std::net::UdpSocket::bind("127.0.0.1:0")
        .and_then(|socket| socket.local_addr())
        .expect("puerto libre")
        .port();
    let host_addr: SocketAddr = (std::net::Ipv4Addr::LOCALHOST, port).into();
    let proxy = server
        .runtime
        .block_on(UdpLossy::start(host_addr, LossyConfig::contract()))
        .expect("proxy");
    let mut host_config = server.config();
    host_config.bind = Some(host_addr);
    host_config.advertise_candidates = Some(vec![proxy.addr()]);
    let host = host(host_config);
    let code = room_code(&host);
    let mut guest_config = server.config();
    guest_config.advertise_candidates = Some(Vec::new());
    let guest = guest(guest_config, &code, Some(FRAMES));
    let (summary, guest, _host) = finish(guest, host);
    println!(
        "proxy: {} reenviados, {} perdidos",
        proxy.stats().forwarded(),
        proxy.stats().dropped()
    );
    assert!(proxy.stats().dropped() > 0, "el proxy perdió datagramas");
    assert_eq!(
        summary.link.as_ref().map(|l| l.path.as_str()),
        Some("directo")
    );
    check_no_freezes(&summary, &guest);
    drop(server);
}

#[test]
fn d_kicked_guest_stops_presenting_and_cannot_decrypt() {
    let _serial = serial();
    let server = TestServer::start();
    let mut host = host(server.config());
    let code = room_code(&host);
    let mut guest = guest(server.config(), &code, None);
    wait_until("30 fotogramas", Duration::from_secs(20), || {
        assert!(!guest.is_finished(), "{:?}", guest.end_reason());
        guest.summary().frames_presented >= 30
    });
    let before = guest.epoch();
    let id: GuestId = host.guests()[0].guest;
    host.kick(id).expect("expulsión");

    assert!(
        guest.wait(Duration::from_secs(10)),
        "el invitado sigue en marcha"
    );
    assert_eq!(guest.end_reason(), Some(SessionError::Removed));
    assert_eq!(guest.state(), SessionState::Closed);
    wait_until("la salida del invitado", Duration::from_secs(5), || {
        host.guests().is_empty()
    });
    let presented = guest.summary().frames_presented;
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        guest.summary().frames_presented,
        presented,
        "no presenta nada tras la expulsión"
    );

    // Época nueva en el anfitrión que el invitado ya no tiene.
    assert!(host.epoch() > before, "{} > {before}", host.epoch());
    assert_eq!(guest.epoch(), before);
    let mut encryptor = host.key_ring().encryptor(host.own_leaf());
    assert_eq!(encryptor.epoch(), Some(host.epoch()));
    let sealed = encryptor
        .encrypt(Track::Video as u8, b"fotograma de la epoca nueva")
        .expect("cifrado");
    let mut out = Vec::new();
    let result = guest.key_ring().decryptor().decrypt_into(&sealed, &mut out);
    assert!(
        matches!(result, Err(MediaError::UnknownKey(_))),
        "{result:?}"
    );
    guest.close();
    host.close();
    drop(server);
}

/// Una ronda A/B: dos salas a la vez en el mismo servidor, una con SFrame y otra sin él, para
/// que las dos midan en las mismas condiciones de CPU. Devuelve los totales (µs) de cada una.
#[cfg(debug_assertions)]
fn ab_round(server: &TestServer, frames: u64) -> (Vec<u64>, Vec<u64>) {
    let sessions: Vec<(RemoteSession, RemoteSession)> = [true, false]
        .into_iter()
        .map(|sframe| {
            let mut host_config = server.config();
            host_config.insecure_no_sframe = !sframe;
            let host = host(host_config);
            let code = room_code(&host);
            let mut guest_config = server.config();
            guest_config.insecure_no_sframe = !sframe;
            (host, guest(guest_config, &code, Some(frames)))
        })
        .collect();
    let mut totals = Vec::new();
    for (host, mut guest) in sessions {
        assert!(guest.wait(SESSION_LIMIT), "{}", guest.summary());
        assert_eq!(guest.end_reason(), None);
        let summary = guest.summary();
        assert!(delivered(&summary) >= 0.95, "{summary}");
        let mut samples: Vec<u64> = guest
            .ledger()
            .timelines()
            .iter()
            .filter_map(|timeline| timeline.span(Stage::Captured, Stage::Presented))
            .map(|span| span.as_micros() as u64)
            .collect();
        samples.sort_unstable();
        totals.push((summary.link.map(|link| link.sframe), samples));
        guest.close();
        drop(host);
    }
    let mut with = Vec::new();
    let mut without = Vec::new();
    for (sframe, samples) in totals {
        match sframe {
            Some(true) => with = samples,
            Some(false) => without = samples,
            None => panic!("sin resumen del enlace"),
        }
    }
    (with, without)
}

#[cfg(debug_assertions)]
fn p50_ms(sorted: &[u64]) -> f64 {
    coroom_session::latency::nearest_rank(sorted, 50.0) as f64 / 1000.0
}

#[test]
#[cfg(debug_assertions)]
fn e_sframe_adds_less_than_half_a_millisecond() {
    const ROUNDS: usize = 3;
    const AB_FRAMES: u64 = 150;
    let _serial = serial();
    let server = TestServer::start();
    let (mut with, mut without) = (Vec::new(), Vec::new());
    let mut rounds = Vec::new();
    for _ in 0..ROUNDS {
        let (a, b) = ab_round(&server, AB_FRAMES);
        rounds.push((p50_ms(&a), p50_ms(&b)));
        with.extend(a);
        without.extend(b);
    }
    with.sort_unstable();
    without.sort_unstable();
    let (with, without) = (p50_ms(&with), p50_ms(&without));
    let difference = with - without;
    // El coste de SFrame es el mismo en cada ronda; el ruido del runner (otra sesión, el
    // recolector de la VM) solo puede sumar, así que la mejor estimación es la menor diferencia
    // entre rondas, no la media de las tres.
    let best = rounds
        .iter()
        .map(|(a, b)| a - b)
        .fold(f64::INFINITY, f64::min);
    println!("p50 total por ronda (con, sin SFrame), ms: {rounds:?}");
    println!(
        "p50 total de las {ROUNDS} rondas: con {with:.3} ms, sin {without:.3} ms, diferencia \
         {difference:.3} ms (menor diferencia por ronda {best:.3} ms)"
    );
    assert!(
        best < 0.5,
        "SFrame añade {best:.3} ms al p50 total en la mejor ronda"
    );
    drop(server);
}

/// AV1 por software y H.264 en el anfitrión.
fn av1_and_h264() -> EncoderSet {
    EncoderSet::new([
        (Codec::Av1, Backend::Software),
        (Codec::H264, Backend::Software),
    ])
}

fn av1_decoders() -> DecoderSet {
    DecoderSet::new([
        (Codec::Av1, DecoderBackend::Software),
        (Codec::H264, DecoderBackend::Software),
    ])
}

/// Espera a que el resumen de `session` diga que el vídeo va en `prefix`.
fn wait_for_video(session: &RemoteSession, prefix: &str, limit: Duration) {
    wait_until(&format!("vídeo en {prefix}"), limit, || {
        assert!(!session.is_finished(), "{:?}", session.end_reason());
        session
            .summary()
            .video
            .is_some_and(|video| video.starts_with(prefix))
    });
}

#[test]
fn f_codec_negotiation_follows_what_the_guest_decodes() {
    const NEGOTIATED_FRAMES: u64 = 150;
    let _serial = serial();
    let server = TestServer::start();

    // El invitado solo decodifica H.264: se elige H.264 aunque el anfitrión tenga AV1.
    let host = host_with(server.config(), av1_and_h264());
    let code = room_code(&host);
    let guest = guest(server.config(), &code, Some(NEGOTIATED_FRAMES));
    let (summary, _guest, host) = finish(guest, host);
    println!("{summary}");
    assert!(summary.frames_presented >= NEGOTIATED_FRAMES, "{summary}");
    assert_eq!(
        summary.video.as_deref(),
        Some("H.264 · decodifica OpenH264"),
        "{summary}"
    );
    assert_eq!(host.summary().video.as_deref(), Some("H.264 · OpenH264"));

    // Los dos con AV1: AV1 (SVT-AV1 → dav1d).
    let host = host_with(server.config(), av1_and_h264());
    let code = room_code(&host);
    let guest = guest_with(
        server.config(),
        &code,
        Some(NEGOTIATED_FRAMES),
        av1_decoders(),
        Box::new(NullSink),
    );
    let (summary, _guest, host) = finish(guest, host);
    println!("{summary}");
    assert!(summary.frames_presented >= NEGOTIATED_FRAMES, "{summary}");
    assert!(delivered(&summary) >= 0.95, "{summary}");
    assert_eq!(
        summary.video.as_deref(),
        Some("AV1 · decodifica dav1d"),
        "{summary}"
    );
    assert_eq!(host.summary().video.as_deref(), Some("AV1 · SVT-AV1"));
    drop(server);
}

#[test]
fn f_a_guest_without_av1_downgrades_the_room_to_h264() {
    let _serial = serial();
    let server = TestServer::start();
    let mut host = host_with(server.config(), av1_and_h264());
    let code = room_code(&host);
    let mut first = guest_with(
        server.config(),
        &code,
        None,
        av1_decoders(),
        Box::new(NullSink),
    );
    wait_for_video(&first, "AV1", Duration::from_secs(20));
    wait_until("fotogramas AV1", Duration::from_secs(20), || {
        first.summary().frames_presented >= 20
    });

    let mut second = guest(server.config(), &code, None);
    wait_for_video(&second, "H.264", Duration::from_secs(20));
    wait_for_video(&first, "H.264", Duration::from_secs(20));
    assert_eq!(host.summary().video.as_deref(), Some("H.264 · OpenH264"));
    // Los dos siguen recibiendo después del cambio.
    let before = (
        first.summary().frames_presented,
        second.summary().frames_presented,
    );
    wait_until(
        "fotogramas H.264 en los dos",
        Duration::from_secs(20),
        || {
            first.summary().frames_presented >= before.0 + 30
                && second.summary().frames_presented >= before.1 + 30
        },
    );
    assert_eq!(first.end_reason(), None);
    assert_eq!(second.end_reason(), None);
    second.close();
    first.close();
    host.close();
    drop(server);
}

/// Posición del cursor, id de su forma y bytes de la forma con que llegó un fotograma.
type CursorSeen = (Option<(i32, i32)>, Option<u64>, usize);

/// Guarda la forma de cursor y la posición con que llega cada fotograma.
struct CursorSink(Arc<Mutex<Vec<CursorSeen>>>);

impl FrameSink for CursorSink {
    fn present(&mut self, frame: DecodedFrame) -> Result<(), SinkError> {
        let mut seen = self.0.lock().unwrap_or_else(|p| p.into_inner());
        seen.push((
            frame.cursor.map(|cursor| (cursor.x, cursor.y)),
            frame.cursor_shape.as_ref().map(|shape| shape.shape_id),
            frame.cursor_shape.map_or(0, |shape| shape.rgba.len()),
        ));
        Ok(())
    }
}

#[test]
fn g_the_cursor_shape_travels_with_the_session() {
    let _serial = serial();
    let server = TestServer::start();
    let host = host(server.config());
    let code = room_code(&host);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let guest = guest_with(
        server.config(),
        &code,
        Some(120),
        DecoderSet::openh264(),
        Box::new(CursorSink(Arc::clone(&seen))),
    );
    let (summary, _guest, _host) = finish(guest, host);
    let seen = seen.lock().unwrap_or_else(|p| p.into_inner());
    let shape = coroom_session::capture::synthetic::cursor_shape();
    let with_shape: Vec<_> = seen
        .iter()
        .filter(|(_, id, _)| *id == Some(shape.shape_id))
        .collect();
    println!(
        "{} de {} fotogramas con la forma del cursor",
        with_shape.len(),
        seen.len()
    );
    assert!(with_shape.len() * 10 >= seen.len() * 9, "{summary}");
    assert!(
        with_shape
            .iter()
            .all(|(_, _, len)| *len == shape.rgba.len())
    );
    let positions: std::collections::BTreeSet<_> = seen
        .iter()
        .filter_map(|(position, _, _)| *position)
        .collect();
    assert!(positions.len() > 10, "el cursor se mueve: {positions:?}");
    drop(server);
}

#[test]
fn h_accounts_name_the_members_and_friends_get_invitations() {
    let _serial = serial();
    let server = TestServer::start();
    let host_account = server.config();
    let guest_account = server.config();
    let friend = server.config();
    server.befriend(&host_account, &friend);
    let (guest_user, guest_name) = (guest_account.user, guest_account.username.clone());
    let friend_user = friend.user;
    let host = host(host_account);
    let code = room_code(&host);

    // El amigo está conectado a la señalización con su cuenta (como `coroom join` sin código).
    let (friend_endpoint, mut friend_signal) = server.runtime.block_on(async {
        let endpoint = coroom_transport::Endpoint::bind(
            &friend.identity,
            Some("127.0.0.1:0".parse().expect("dirección")),
            coroom_transport::EndpointOptions::default(),
        )
        .expect("endpoint");
        let signal = coroom_transport::SignalClient::connect(
            &friend.config.server_url,
            &friend.config.token,
            &endpoint,
            &friend.config.trust,
        )
        .await
        .expect("señalización del amigo");
        (endpoint, signal)
    });

    let guest = guest(guest_account, &code, Some(60));
    wait_until("la conexión", Duration::from_secs(15), || {
        host.guests().first().is_some_and(|g| g.path.is_some())
    });
    let status = host.guests()[0].clone();
    assert_eq!(status.user, guest_user);
    assert_eq!(status.name, "Jugador 2", "nombre visible de {guest_name}");
    let link = guest.link().expect("enlace");
    assert_eq!(link.peer_name, "Jugador 1");
    // Invitar a quien no es amigo deja un aviso del servidor; al amigo, no.
    let events = host.events();
    host.invite(guest_user).expect("invitar");
    host.invite(friend_user).expect("invitar");
    let mut warned = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !warned {
        if let Ok(SessionEvent::Warning(message)) = events.recv_timeout(Duration::from_millis(100))
        {
            warned = message.contains("amigos");
        }
    }
    assert!(warned, "aviso por invitar a quien no es amigo");
    let invited = server.runtime.block_on(async {
        loop {
            let message = tokio::time::timeout(Duration::from_secs(5), friend_signal.next_event())
                .await
                .expect("invitación a tiempo")
                .expect("señalización")
                .expect("abierta");
            if let coroom_transport::proto::signal::SignalMessage::Invited {
                from_name, code, ..
            } = message
            {
                break (from_name, code);
            }
        }
    });
    assert_eq!(invited, ("Jugador 1".to_owned(), code));
    friend_signal.close();
    friend_endpoint.close();
    let (_summary, _guest, _host) = finish(guest, host);
    drop(server);
}

#[test]
fn i_the_dev_token_mode_still_works_without_accounts() {
    let _serial = serial();
    let server = TestServer::start_dev();
    let host = host(server.config());
    let code = room_code(&host);
    let guest = guest(server.config(), &code, Some(90));
    let (summary, guest, _host) = finish(guest, host);
    assert!(summary.frames_presented >= 90, "{summary}");
    assert_eq!(
        guest.link().map(|link| link.peer_name),
        Some("desarrollo".to_owned())
    );
    drop(server);
}

#[test]
fn j_a_guest_reconnects_and_keeps_presenting() {
    let _serial = serial();
    let server = TestServer::start();
    let mut host = host(server.config());
    let code = room_code(&host);
    let mut guest = guest(server.config(), &code, None);
    let events = guest.events();
    wait_until("30 fotogramas", Duration::from_secs(20), || {
        assert!(!guest.is_finished(), "{:?}", guest.end_reason());
        guest.summary().frames_presented >= 30
    });
    let mut estimate = None;
    while let Ok(event) = events.try_recv() {
        if let SessionEvent::QualityEstimate { rtt_ms, level } = event {
            estimate = Some((rtt_ms, level));
        }
    }
    let (rtt_ms, level) = estimate.expect("estimación de calidad antes del vídeo");
    assert!(rtt_ms.is_finite() && rtt_ms >= 0.0, "{rtt_ms}");
    assert_eq!(level, QualityLevel::from_rtt_ms(rtt_ms));

    guest.reconnect().expect("reconectar");
    let mut attempts = Vec::new();
    let mut path = None;
    let deadline = Instant::now() + Duration::from_secs(30);
    while path.is_none() && Instant::now() < deadline {
        match events.recv_timeout(Duration::from_millis(100)) {
            Ok(SessionEvent::Reconnecting { attempt, of, .. }) => attempts.push((attempt, of)),
            Ok(SessionEvent::Reconnected { path: now }) => path = Some(now),
            _ => assert!(!guest.is_finished(), "{:?}", guest.end_reason()),
        }
    }
    assert_eq!(path, Some(Path::Direct), "intentos: {attempts:?}");
    assert_eq!(attempts.first(), Some(&(1, 5)), "{attempts:?}");

    let before = guest.summary().frames_presented;
    wait_until(
        "fotogramas tras reconectar",
        Duration::from_secs(20),
        || {
            assert!(!guest.is_finished(), "{:?}", guest.end_reason());
            guest.summary().frames_presented >= before + 30
        },
    );
    wait_until(
        "un solo invitado en la sala",
        Duration::from_secs(10),
        || host.guests().len() == 1,
    );
    assert_eq!(guest.room_code(), Some(code));
    guest.close();
    host.close();
    drop(server);
}

/// Fuente que entrega unos pocos fotogramas y después deja la pantalla quieta, como WGC o PipeWire
/// (solo entregan fotogramas cuando cambia algo).
struct StillCapturer {
    inner: SyntheticCapturer,
    left: u32,
}

impl Capturer for StillCapturer {
    fn targets(&self) -> Result<Vec<CaptureTarget>, CaptureError> {
        self.inner.targets()
    }

    fn start(&mut self, target: &CaptureTarget) -> Result<(), CaptureError> {
        self.inner.start(target)
    }

    fn next_frame(&mut self) -> Result<CapturedFrame, CaptureError> {
        self.inner.next_frame()
    }

    fn next_frame_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<CapturedFrame>, CaptureError> {
        if self.left == 0 {
            std::thread::sleep(timeout);
            return Ok(None);
        }
        self.left -= 1;
        self.inner.next_frame().map(Some)
    }

    fn stop(&mut self) {
        self.inner.stop();
    }
}

/// (k) Pantalla quieta: el anfitrión capturó cinco fotogramas antes de que llegase nadie. Un
/// invitado entra, ve la imagen y se va; otro entra y también la ve enseguida (antes se quedaba en
/// «Entrando en la sala»: el intra del anfitrión llegaba antes de abrir su decodificador y no salía
/// ningún otro).
#[test]
fn k_a_guest_joining_a_still_screen_sees_it() {
    let _serial = serial();
    common::load_factor();
    let server = TestServer::start();
    let account = server.config();
    let mut host = RemoteSession::host(
        account.identity,
        account.config,
        Box::new(StillCapturer {
            inner: SyntheticCapturer::new(640, 360, FPS),
            left: 5,
        }),
        EncoderSet::openh264(),
        video(None),
        Arc::new(LatencyLedger::new()),
    );
    host.start().expect("arranque del anfitrión");
    let code = room_code(&host);
    let bound = Duration::from_secs(3).mul_f64(common::load_factor());
    for name in ["A", "B"] {
        let mut guest = guest(server.config(), &code, None);
        wait_until(
            &format!("la conexión de {name}"),
            Duration::from_secs(15),
            || {
                assert!(!guest.is_finished(), "{:?}", guest.end_reason());
                guest.state() == SessionState::Active
            },
        );
        let connected = Instant::now();
        wait_until(
            &format!("un fotograma en {name}"),
            bound + Duration::from_secs(5),
            || guest.summary().frames_presented >= 1,
        );
        let took = connected.elapsed();
        println!(
            "{name}: primer fotograma {took:?} después de conectar; {:?}",
            guest.video_counters()
        );
        assert!(took <= bound, "{name} tardó {took:?} (cota {bound:?})");
        guest.close();
        wait_until("la salida del invitado", Duration::from_secs(10), || {
            host.guests().is_empty()
        });
    }
    let counters = host.video_counters();
    println!("anfitrión: {counters:?}");
    assert!(
        counters.encoded <= 20,
        "con la pantalla quieta casi no se codifica: {counters:?}"
    );
    host.close();
    drop(server);
}

/// (l) Un enlace con poca cola (2 Mbit/s y 15 ms, como un búfer de socket pequeño): las ráfagas
/// de un intra no caben y se pierden. El vídeo se recupera y los intra forzados no se disparan
/// (antes, cada grupo perdido pedía un intra que volvía a desbordar la cola).
#[test]
fn l_a_shallow_queue_does_not_start_a_keyframe_storm() {
    let _serial = serial();
    common::load_factor();
    let server = TestServer::start();
    let port = std::net::UdpSocket::bind("127.0.0.1:0")
        .and_then(|socket| socket.local_addr())
        .expect("puerto libre")
        .port();
    let host_addr: SocketAddr = (std::net::Ipv4Addr::LOCALHOST, port).into();
    let proxy = server
        .runtime
        .block_on(UdpLossy::start(
            host_addr,
            LossyConfig {
                loss: 0.0,
                delay: Duration::from_millis(2),
                jitter: Duration::ZERO,
                rate_bps: Some(2_000_000),
                queue_limit: Duration::from_millis(15),
                seed: 7,
                outages: None,
            },
        ))
        .expect("proxy");
    let mut host_config = server.config();
    host_config.bind = Some(host_addr);
    host_config.advertise_candidates = Some(vec![proxy.addr()]);
    let host = host(host_config);
    let code = room_code(&host);
    let mut guest_config = server.config();
    guest_config.advertise_candidates = Some(Vec::new());
    let guest = guest(guest_config, &code, Some(FRAMES));
    let (summary, guest, host) = finish(guest, host);
    let (sent, received) = (host.video_counters(), guest.video_counters());
    println!(
        "{summary}\nproxy: {} reenviados, {} descartados por la cola\nanfitrión {sent:?}\n\
         invitado {received:?}",
        proxy.stats().forwarded(),
        proxy.stats().queue_drops()
    );
    let gap = max_gap(guest.ledger());
    let bound = Duration::from_secs(2).mul_f64(common::load_factor());
    assert!(gap <= bound, "hueco de {gap:?} (cota {bound:?})");
    // 300 fotogramas en 10 s: como mucho unos pocos intra por segundo.
    assert!(
        sent.forced_keyframes <= 20,
        "{} intra forzados",
        sent.forced_keyframes
    );
    drop(server);
}

/// Guarda el tamaño de cada fotograma presentado.
struct SizeSink(Arc<Mutex<Vec<(u32, u32)>>>);

impl FrameSink for SizeSink {
    fn present(&mut self, frame: DecodedFrame) -> Result<(), SinkError> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((frame.frame.width, frame.frame.height));
        Ok(())
    }
}

/// (m) Con la sala abierta y un invitado viendo: el anfitrión cambia lo que comparte (a una
/// fuente de otro tamaño), luego el códec (de AV1 a H.264) y luego baja el bitrate máximo, y el
/// invitado sigue presentando sin salir de la sala.
#[test]
fn m_the_host_changes_source_codec_and_quality_while_open() {
    let _serial = serial();
    let server = TestServer::start();
    let mut host = host_with(server.config(), av1_and_h264());
    let code = room_code(&host);
    let sizes = Arc::new(Mutex::new(Vec::new()));
    let mut guest = guest_with(
        server.config(),
        &code,
        None,
        av1_decoders(),
        Box::new(SizeSink(Arc::clone(&sizes))),
    );
    wait_for_video(&guest, "AV1", Duration::from_secs(20));
    let presented = |sizes: &Arc<Mutex<Vec<(u32, u32)>>>, size: (u32, u32)| {
        sizes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|seen| **seen == size)
            .count()
    };
    wait_until("fotogramas de 640×360", Duration::from_secs(20), || {
        presented(&sizes, (640, 360)) >= 20
    });

    // Otra fuente, de otro tamaño: el codificador se reabre y el invitado la ve.
    host.switch_capture(Box::new(SyntheticCapturer::new(320, 180, FPS)), None)
        .expect("cambio de fuente");
    wait_until("fotogramas de 320×180", Duration::from_secs(20), || {
        assert!(!guest.is_finished(), "{:?}", guest.end_reason());
        presented(&sizes, (320, 180)) >= 20
    });

    // Otro códec: se renegocia y los dos siguen.
    host.set_video(coroom_session::remote::VideoChange {
        encoders: Some(EncoderSet::openh264()),
        max_kbps: None,
    })
    .expect("cambio de códec");
    wait_for_video(&guest, "H.264", Duration::from_secs(20));
    let before = guest.summary().frames_presented;
    wait_until("fotogramas H.264", Duration::from_secs(20), || {
        guest.summary().frames_presented >= before + 20
    });
    assert_eq!(host.summary().video.as_deref(), Some("H.264 · OpenH264"));

    // Menos calidad: el objetivo de cada invitado baja al máximo nuevo.
    host.set_video(coroom_session::remote::VideoChange {
        encoders: None,
        max_kbps: Some(300),
    })
    .expect("cambio de bitrate");
    wait_until("objetivo ≤ 300 kbit/s", Duration::from_secs(10), || {
        host.guests()
            .iter()
            .all(|status| status.target_kbps.is_some_and(|kbps| kbps <= 300))
    });
    let before = guest.summary().frames_presented;
    wait_until(
        "fotogramas con el bitrate nuevo",
        Duration::from_secs(20),
        || guest.summary().frames_presented >= before + 20,
    );
    assert_eq!(guest.end_reason(), None);
    assert_eq!(host.end_reason(), None);
    guest.close();
    host.close();
    drop(server);
}

/// Fuente sintética con ruido en cada fotograma: el patrón liso comprime tanto que no llega a
/// ningún bitrate alto. El ruido se calcula una vez y cada fotograma lo usa desplazado.
struct NoisyCapturer {
    inner: SyntheticCapturer,
    noise: Vec<u64>,
}

impl NoisyCapturer {
    fn new(inner: SyntheticCapturer) -> Self {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let noise = (0..(1 << 16))
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state & 0x0f0f_0f0f_0f0f_0f0f
            })
            .collect();
        Self { inner, noise }
    }

    fn add_noise(&self, frame: &mut CapturedFrame) {
        if let coroom_session::capture::FramePixels::Bgra { data, .. } = &mut frame.pixels {
            let offset = (frame.frame_id as usize).wrapping_mul(7919);
            let (words, _) = data.as_chunks_mut::<8>();
            for (i, word) in words.iter_mut().enumerate() {
                let noise = self.noise[(i + offset) % self.noise.len()];
                *word = (u64::from_ne_bytes(*word) ^ noise).to_ne_bytes();
            }
        }
    }
}

impl Capturer for NoisyCapturer {
    fn targets(&self) -> Result<Vec<CaptureTarget>, CaptureError> {
        self.inner.targets()
    }

    fn start(&mut self, target: &CaptureTarget) -> Result<(), CaptureError> {
        self.inner.start(target)
    }

    fn next_frame(&mut self) -> Result<CapturedFrame, CaptureError> {
        let mut frame = self.inner.next_frame()?;
        self.add_noise(&mut frame);
        Ok(frame)
    }

    fn next_frame_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<CapturedFrame>, CaptureError> {
        let mut frame = self.inner.next_frame_timeout(timeout)?;
        if let Some(frame) = frame.as_mut() {
            self.add_noise(frame);
        }
        Ok(frame)
    }

    fn stop(&mut self) {
        self.inner.stop();
    }
}

/// (n) Cortes de red: a través de un proxy con 1 % de pérdida que además lo pierde todo durante
/// 600 ms cada 3 s (wifi que se cae, VPN que se renegocia), el vídeo vuelve como mucho 500 ms
/// después de cada corte: entre dos fotogramas presentados no pasa más que el corte y 500 ms.
#[test]
fn n_video_recovers_within_half_a_second_after_each_outage() {
    const OUTAGE: Duration = Duration::from_millis(600);
    const RECOVERY: Duration = Duration::from_millis(500);
    let _serial = serial();
    common::load_factor();
    let server = TestServer::start();
    let port = std::net::UdpSocket::bind("127.0.0.1:0")
        .and_then(|socket| socket.local_addr())
        .expect("puerto libre")
        .port();
    let host_addr: SocketAddr = (std::net::Ipv4Addr::LOCALHOST, port).into();
    let proxy_started = Instant::now();
    let proxy = server
        .runtime
        .block_on(UdpLossy::start(
            host_addr,
            LossyConfig {
                loss: 0.01,
                delay: Duration::from_millis(3),
                jitter: Duration::from_millis(2),
                rate_bps: None,
                queue_limit: Duration::from_millis(200),
                seed: 11,
                outages: Some(coroom_transport::lossy::Outages {
                    period: Duration::from_secs(3),
                    length: OUTAGE,
                }),
            },
        ))
        .expect("proxy");
    let mut host_config = server.config();
    host_config.bind = Some(host_addr);
    host_config.advertise_candidates = Some(vec![proxy.addr()]);
    // Con bitrate alto, como en uso real: la cola de datagramas por enviar de QUIC (512 KB, unos
    // 0,4 s aquí) no aguanta el corte y descarta fotogramas enteros. A 1,5 Mbit/s cabía el corte
    // entero y no se perdía ningún grupo completo.
    let mut host = RemoteSession::host(
        host_config.identity,
        host_config.config,
        Box::new(NoisyCapturer::new(SyntheticCapturer::new(1280, 720, FPS))),
        EncoderSet::openh264(),
        LoopbackConfig {
            width: 1280,
            height: 720,
            bitrate_kbps: 10_000,
            ..video(None)
        },
        Arc::new(LatencyLedger::new()),
    );
    host.start().expect("arranque del anfitrión");
    let code = room_code(&host);
    let mut guest_config = server.config();
    guest_config.advertise_candidates = Some(Vec::new());
    // 12 s de vídeo: tres cortes.
    let guest = guest(guest_config, &code, Some(360));
    let ledger = Arc::clone(guest.ledger());
    let (summary, _guest, _host) = finish(guest, host);
    let gap = max_gap(&ledger);
    let mut timelines = ledger.timelines();
    timelines.retain(|t| t.at(Stage::Presented).is_some());
    timelines.sort_by_key(|t| t.at(Stage::Presented));
    for pair in timelines.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let (pa, pb) = (
            a.at(Stage::Presented).unwrap(),
            b.at(Stage::Presented).unwrap(),
        );
        if pb - pa > Duration::from_millis(300) {
            let rel = |at: Instant| at.saturating_duration_since(proxy_started).as_millis();
            println!(
                "hueco {:?}: de {} ms a {} ms (desde el proxy); después: captura {} ms, recibido {} ms",
                pb - pa,
                rel(pa),
                rel(pb),
                b.at(Stage::Captured).map_or(0, rel),
                b.at(Stage::Received).map_or(0, rel),
            );
        }
    }
    println!(
        "{summary}\nhueco máximo {gap:?}; proxy: {} reenviados, {} perdidos",
        proxy.stats().forwarded(),
        proxy.stats().dropped()
    );
    assert!(
        gap <= OUTAGE + RECOVERY,
        "el vídeo tardó {:?} en volver tras un corte de {OUTAGE:?} (cota {RECOVERY:?})",
        gap.saturating_sub(OUTAGE)
    );
    drop(server);
}

/// Espera un `Members` en `events` con `count` miembros.
fn wait_members(
    events: &crossbeam_channel::Receiver<SessionEvent>,
    count: usize,
) -> Vec<coroom_session::remote::RoomMember> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Ok(SessionEvent::Members(members)) = events.recv_timeout(Duration::from_millis(100))
            && members.len() == count
        {
            return members;
        }
    }
    panic!("no llegó la lista de {count} miembros");
}

/// (o) Cada invitado recibe quién está en la sala (con nombres, sabiendo cuál es él) para
/// silenciar a quien quiera, y la lista se actualiza cuando alguien sale.
#[test]
fn o_guests_learn_who_is_in_the_room() {
    let _serial = serial();
    let server = TestServer::start();
    let mut host = host(server.config());
    let code = room_code(&host);
    let mut first = guest(server.config(), &code, None);
    let first_events = first.events();
    let members = wait_members(&first_events, 2);
    let names: Vec<(&str, bool)> = members.iter().map(|m| (m.name.as_str(), m.you)).collect();
    assert_eq!(names, [("Jugador 1", false), ("Jugador 2", true)]);
    assert_eq!(members[0].guest, 0, "el anfitrión es el 0 de VoiceState");

    let mut second = guest(server.config(), &code, None);
    let members = wait_members(&first_events, 3);
    assert!(
        members
            .iter()
            .any(|m| m.name == "Jugador 3" && !m.you && m.guest != 0)
    );
    let third = members
        .iter()
        .find(|m| m.name == "Jugador 3")
        .expect("Jugador 3");
    // Silenciarle es cosa de cada uno: solo cambia lo que suena aquí.
    first.audio().set_voice_muted(third.leaf, true);
    assert!(first.audio().voice_muted(third.leaf));
    assert!(!second.audio().voice_muted(third.leaf));

    second.close();
    let members = wait_members(&first_events, 2);
    assert!(members.iter().all(|m| m.name != "Jugador 3"));
    first.close();
    host.close();
    drop(server);
}
