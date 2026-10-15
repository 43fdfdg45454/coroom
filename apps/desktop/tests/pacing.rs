//! Pacing con holgura **medida**, en bucle local: anfitrión con fuente sintética 1080p con ruido
//! (`NoisyCapturer`: el patrón liso comprime demasiado; con ruido en los bits bajos el codificador
//! gasta varios Mbit/s en fotogramas P, sin que el detector de cambio de escena de OpenH264 los
//! convierta en intra, como pasa con ruido puro) e invitado sin ventana por un servidor real en
//! `127.0.0.1` (ver `common`), con 10 Mbit/s de bitrate máximo. La sonda del enlace sale a
//! `probe_pace_kbps` (100 Mbit/s para ese máximo) y mide el bucle local, así que el pacing reparte
//! cada fotograma al 70 % de lo medido (70 Mbit/s) en vez de al máximo del usuario. Se mide en el
//! registro del anfitrión el tramo `cifrado → enviado` (la FEC y el pacing del fotograma; ver
//! `send_loop` de `coroom_session::remote::host`).
//!
//! Al pacing solo le llegan bytes y la cadencia, no la resolución, así que los dos perfiles
//! reproducen lo medido en la PC de referencia con 10 Mbit/s por los fotogramas que producen:
//!
//! - 60 fps (1080p60): fotogramas de ≥ 7,8 KB, que la regla anterior repartía a 1,25 × el máximo
//!   y recortaba siempre al 30 % del intervalo, 5 ms (medido: 4,3-5,2 ms de p50, dos tercios de los
//!   ~7 ms de captura a presentación); al ritmo medido, ~21 KB son 2,4 ms.
//! - 30 fps (720p30): fotogramas de ≥ 15,6 KB, recortados a 10 ms (medido con el paquete 1.6.15:
//!   10,4 de 11,7 ms); al ritmo medido, ~42 KB son 4,8 ms.
//!
//! Las cotas se calculan con lo medido en la propia sesión (bytes por fotograma y lo que midió la
//! sonda), no con cifras fijas, para que valgan con cualquier codificador y runner:
//!
//! 1. la regla anterior habría recortado el fotograma medio al 30 % del intervalo (los fotogramas
//!    son lo bastante grandes para que la prueba sea la que dice ser);
//! 2. el p50 queda por debajo de lo que tardan los bytes del fotograma medio al 70 % de lo que
//!    midió la sonda más [`TIMER_SLACK`]: el reparto va al ritmo medido;
//! 3. el p50 queda por debajo de lo que la regla anterior habría tardado con esos mismos bytes
//!    (sin contarle a ella la misma holgura de temporizadores, que también tenía).
//!
//! Además se comprueba que la sonda midió de verdad holgura (≥ 50 Mbit/s hacia el invitado). La
//! cota es sobre la mediana: los primeros fotogramas pueden salir antes de que llegue el resultado
//! de la sonda (todavía al ritmo del máximo), los intra se reparten en un intervalo y el runner
//! añade ruido a unos pocos.

mod common;

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use common::{Account, NoisyCapturer, TestServer};
use coroom_session::Session;
use coroom_session::capture::synthetic::SyntheticCapturer;
use coroom_session::codec::{DecoderSet, EncoderSet};
use coroom_session::latency::{LatencyLedger, SpanStats, Stage};
use coroom_session::loopback::{LoopbackConfig, NullSink};
use coroom_session::remote::{RemoteSession, SessionEvent};
use coroom_transport::media::{FRAME_PACING_SHARE, PACING_GAIN};
use coroom_transport::rate::PROBE_START_SHARE;

/// Bitrate máximo del usuario en los dos perfiles (el de la PC de referencia).
const MAX_KBPS: u32 = 10_000;
/// Tamaño de la imagen: 1080p, para que con el ruido de los bits bajos el codificador llegue a
/// fotogramas del tamaño que la regla anterior recortaba.
const CAPTURE: (u32, u32) = (1920, 1080);
/// Duración de cada sesión; el p50 se toma sobre todos sus fotogramas.
const SECONDS: u64 = 5;
/// Tiempo límite de una sesión.
const SESSION_LIMIT: Duration = Duration::from_secs(90);
/// Lo que el reparto puede tardar por encima de lo que dan los bytes al ritmo: los temporizadores
/// de `tokio` van en pasos de 1 ms y redondean cada espera hacia arriba (hasta 1 ms de más por
/// fotograma); con los cuatro núcleos del runner ocupados codificando y decodificando, el hilo que
/// despierta espera además su turno del planificador (otro milisegundo); y la FEC, los datagramas
/// y el paso del hilo `send` al runtime suman unos cientos de µs en debug. Medido en el runner:
/// 0,9 ms sobre el tiempo nominal a 30 fps y 1,3 ms a 60 fps.
const TIMER_SLACK: Duration = Duration::from_millis(2);

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

/// Un perfil: la cadencia del perfil medido; con el bitrate máximo fijo, los bytes por fotograma
/// (lo único que, con los fps, entra en el pacing) son del orden de los de ese perfil.
struct Profile {
    name: &'static str,
    fps: u32,
}

/// Lo medido en una sesión.
struct Measured {
    pacing: SpanStats,
    probe_kbps: u32,
    bitrate_kbps: f64,
    presented: u64,
}

fn host(account: Account, profile: &Profile) -> RemoteSession {
    let mut session = RemoteSession::host(
        account.identity,
        account.config,
        Box::new(NoisyCapturer::new(SyntheticCapturer::new(
            CAPTURE.0,
            CAPTURE.1,
            profile.fps,
        ))),
        EncoderSet::openh264(),
        LoopbackConfig {
            width: CAPTURE.0,
            height: CAPTURE.1,
            fps: profile.fps,
            bitrate_kbps: MAX_KBPS,
            frames: None,
            yuv444: false,
            hdr: false,
        },
        Arc::new(LatencyLedger::new()),
    );
    session.start().expect("arranque del anfitrión");
    session
}

fn guest(account: Account, code: &str, frames: u64) -> RemoteSession {
    let mut session = RemoteSession::guest(
        account.identity,
        account.config,
        code,
        DecoderSet::openh264(),
        Box::new(NullSink),
        Some(frames),
        Arc::new(LatencyLedger::new()),
    );
    session.start().expect("arranque del invitado");
    session
}

fn room_code(host: &RemoteSession) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            !host.is_finished(),
            "el anfitrión terminó: {:?}",
            host.end_reason()
        );
        if let Some(code) = host.room_code() {
            return code;
        }
        assert!(deadline > std::time::Instant::now(), "sin código de sala");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn session(server: &TestServer, profile: &Profile) -> Measured {
    let mut host = host(server.config(), profile);
    let events = host.events();
    let code = room_code(&host);
    let frames = u64::from(profile.fps) * SECONDS;
    let mut guest = guest(server.config(), &code, frames);
    assert!(
        guest.wait(SESSION_LIMIT),
        "{}: el invitado no terminó\n{}",
        profile.name,
        guest.summary()
    );
    assert_eq!(
        guest.end_reason(),
        None,
        "{}\n{}",
        profile.name,
        guest.summary()
    );
    let guest_summary = guest.summary();
    let host_summary = host.summary();
    guest.close();
    host.close();
    let probe_kbps = events
        .try_iter()
        .filter_map(|event| match event {
            SessionEvent::LinkProbe { kbps, .. } => Some(kbps),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let pacing = host_summary
        .spans
        .iter()
        .copied()
        .find(|span| span.from == Stage::Encrypted && span.to == Stage::Sent)
        .expect("tramo cifrado → enviado");
    Measured {
        pacing,
        probe_kbps,
        bitrate_kbps: host_summary.bitrate_kbps,
        presented: guest_summary.frames_presented,
    }
}

/// Lo que tardan `bits` a `kbps`.
fn at(bits: f64, kbps: f64) -> Duration {
    Duration::from_secs_f64(bits / (kbps * 1000.0))
}

fn check(profile: &Profile) {
    let _serial = serial();
    let server = TestServer::start();
    let measured = session(&server, profile);
    drop(server);
    let frames = u64::from(profile.fps) * SECONDS;
    let interval = Duration::from_secs(1) / profile.fps;
    let p50 = Duration::from_micros(measured.pacing.p50_us);
    let p95 = Duration::from_micros(measured.pacing.p95_us);
    let mean_bits = measured.bitrate_kbps * 1000.0 / f64::from(profile.fps);
    // Lo que dan los bytes del fotograma medio al ritmo del pacing (70 % de la sonda).
    let nominal = at(
        mean_bits,
        PROBE_START_SHARE * f64::from(measured.probe_kbps),
    );
    // La regla anterior con esos bytes: a 1,25 × el máximo, recortado al share del intervalo.
    let clip = interval.mul_f64(FRAME_PACING_SHARE);
    let before = at(mean_bits, PACING_GAIN * f64::from(MAX_KBPS)).min(clip);
    println!(
        "{}: cifrado → enviado p50 {p50:?}, p95 {p95:?} ({} muestras); sonda {} kbit/s; \
         codificado {:.0} kbit/s (~{:.0} B por fotograma: nominal {nominal:?}, antes {before:?}); \
         presentados {} de {frames}",
        profile.name,
        measured.pacing.samples,
        measured.probe_kbps,
        measured.bitrate_kbps,
        mean_bits / 8.0,
        measured.presented
    );
    assert!(
        measured.probe_kbps >= 50_000,
        "{}: la sonda midió {} kbit/s, sin holgura sobre el máximo",
        profile.name,
        measured.probe_kbps
    );
    assert!(
        measured.pacing.samples as u64 >= frames / 2,
        "{}: solo {} muestras",
        profile.name,
        measured.pacing.samples
    );
    assert_eq!(
        before,
        clip,
        "{}: con ~{:.0} B por fotograma la regla anterior no recortaba al share",
        profile.name,
        mean_bits / 8.0
    );
    assert!(
        p50 < nominal + TIMER_SLACK,
        "{}: p50 {p50:?} no va al ritmo medido (nominal {nominal:?} + {TIMER_SLACK:?})",
        profile.name
    );
    assert!(
        p50 < before,
        "{}: p50 {p50:?} no mejora lo anterior ({before:?})",
        profile.name
    );
}

#[test]
fn a_60_fps_frames_leave_at_the_measured_rate_not_at_the_cap() {
    check(&Profile {
        name: "60 fps",
        fps: 60,
    });
}

#[test]
fn b_30_fps_frames_leave_at_the_measured_rate_not_at_the_cap() {
    check(&Profile {
        name: "30 fps",
        fps: 30,
    });
}
