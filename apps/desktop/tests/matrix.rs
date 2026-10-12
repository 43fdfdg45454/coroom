//! Matriz de red: una sesión completa en proceso (servidor
//! real con cuentas, anfitrión con vídeo sintético de 640×360 a 30 fps con OpenH264 y audio del
//! juego sintético, invitado con pantalla sintética de 60 Hz y salida de audio que graba, todo
//! cifrado) por cada perfil de red, con lo que mide el invitado:
//!
//! - **fps** presentados (fotogramas presentados / duración de la sesión);
//! - **pérdida**: la del enlace (proxy o netem) y la que ve el receptor (`loss_percent`);
//! - **bitrate** alcanzado (`target_kbps` del control de tasa al final);
//! - **hueco** máximo entre dos presentaciones;
//! - **|A−V|** p95 del audio del juego frente al vídeo.
//!
//! Dos tests:
//!
//! - `lossy_matrix`: siempre, con el proxy `UdpLossy` en proceso entre invitado y anfitrión, los
//!   perfiles `buena`, `media` y `mala` durante 10 s cada uno.
//! - `netem_matrix`: con `tc netem` real en `lo` (`sudo -n tools/netem.sh profile …`), los cinco
//!   perfiles durante 30 s cada uno; se salta si no hay `tc` o `sudo` sin contraseña (la CI de
//!   Linux sí lo tiene).
//!
//! Cotas por perfil ([`Bounds`]), derivadas del diseño y no de lo que salió una vez:
//!
//! - **fps**: la fuente da 30. Con pérdida `p` por datagrama, un fotograma de ~5 objetos con un
//!   30 % de reparación RaptorQ (la proporción que pide la pérdida medida, `media.rs`) solo se
//!   pierde si caen más de 2 de 7 datagramas: 0,4 % con `p` = 5 %; el `Nack` recupera el resto. Un
//!   fotograma que espera su reenvío retiene los que vienen detrás (la entrega es en orden), que
//!   llegan juntos cuando se completa y de los que solo se presenta el último (lo más nuevo, por
//!   latencia): cada espera cuesta unos RTT / intervalo fotogramas sin presentar. Se exige el 95 %
//!   (28,5 fps) en `buena` (RTT 30 ms, ~1 fotograma por espera), el 90 % (27 fps) en `media` (60 ms,
//!   ~2) y el 85 % (25,5 fps) en `mala` (120 ms, ~4, y algún intra).
//! - **hueco**: un fotograma con un objeto perdido espera el reenvío hasta su plazo máximo en
//!   vídeo, `max(4 intervalos, 3 RTT + 1 intervalo)` con 400 ms como mucho
//!   (`coroom_transport::media`). Si ni así llega, se descarta y el decodificador queda sin
//!   referencia hasta el intra que se pide entonces: un RTT más dos intervalos (pedirlo,
//!   codificarlo y enviarlo). Cota = plazo máximo + RTT + 67 ms + 2 × jitter, redondeada: 250 ms en
//!   `buena` (133 + 30 + 67 + 6), 400 ms en `media` (213 + 60 + 67 + 20) y 650 ms en `mala`
//!   (400 + 120 + 67 + 40). Es el peor caso (dos reenvíos seguidos perdidos); lo normal es no pasar
//!   de 2 o 3 intervalos. La puerta de 100 ms (2 %, 10 ± 5 ms) la sigue comprobando
//!   `tests/remote.rs`.
//! - **|A−V|** p95: la cota de `docs/ARQUITECTURA.md` §2.2 es 40 ms sin pérdida ni jitter; con jitter, el buffer
//!   de audio sube su objetivo con el p95 del jitter más una trama (`audio::jitter`), así que se
//!   suma el doble del jitter de un sentido: 50 ms en `buena` (± 3 ms), 60 ms en `media` (± 10 ms) y
//!   100 ms en `mala` (± 20 ms, con margen por las pérdidas que oculta el PLC).
//! - **pérdida** del enlace: la del perfil con un margen estadístico (±50 % o ±0,5 puntos), para
//!   comprobar que la prueba es la que dice ser.

mod common;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use common::{Account, TestServer};
use coroom_session::Session;
use coroom_session::audio::synthetic::{CaptureSink, ToneSource};
use coroom_session::capture::synthetic::SyntheticCapturer;
use coroom_session::codec::{DecoderSet, EncoderSet};
use coroom_session::latency::{LatencyLedger, LatencySummary, Stage};
use coroom_session::loopback::{DecodedFrame, FrameSink, LoopbackConfig, SinkError};
use coroom_session::remote::{GuestAudio, HostAudio, RemoteSession};
use coroom_transport::lossy::{LossyConfig, UdpLossy};

const FPS: u32 = 30;

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

/// Pantalla sintética de 60 Hz (como en `tests/audio.rs`).
struct VsyncSink {
    origin: Instant,
}

impl FrameSink for VsyncSink {
    fn present(&mut self, _frame: DecodedFrame) -> Result<(), SinkError> {
        let refresh = Duration::from_micros(16_667);
        let elapsed = self.origin.elapsed();
        let next = refresh * (elapsed.as_micros() / refresh.as_micros() + 1) as u32;
        std::thread::sleep(next.saturating_sub(elapsed));
        Ok(())
    }
}

fn host(account: Account) -> RemoteSession {
    let audio = HostAudio {
        game: Some(
            Box::new(ToneSource::new(440.0, 2)) as Box<dyn coroom_session::audio::AudioSource>
        ),
        game_bitrate: 0,
        frame_ms: 10,
        voice: None,
        speaker: None,
    };
    let mut session = RemoteSession::host(
        account.identity,
        account.config,
        Box::new(SyntheticCapturer::new(640, 360, FPS)),
        EncoderSet::openh264(),
        LoopbackConfig {
            width: 640,
            height: 360,
            fps: FPS,
            bitrate_kbps: 1_500,
            frames: None,
            yuv444: false,
            hdr: false,
        },
        Arc::new(LatencyLedger::new()),
    )
    .with_host_audio(audio);
    session.start().expect("arranque del anfitrión");
    session
}

fn guest(account: Account, code: &str, frames: u64) -> RemoteSession {
    let mut session = RemoteSession::guest(
        account.identity,
        account.config,
        code,
        DecoderSet::openh264(),
        Box::new(VsyncSink {
            origin: Instant::now(),
        }),
        Some(frames),
        Arc::new(LatencyLedger::new()),
    )
    .with_guest_audio(GuestAudio {
        speaker: Some(Box::new(CaptureSink::new(2))),
        voice: None,
        mute_game: false,
        frame_ms: 10,
    });
    session.start().expect("arranque del invitado");
    session
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

/// Fotogramas por segundo presentados (entre el primero y el último) y mayor hueco entre dos
/// presentaciones seguidas.
fn presentation(ledger: &LatencyLedger) -> (f64, Duration) {
    let mut presented: Vec<Instant> = ledger
        .timelines()
        .iter()
        .filter_map(|timeline| timeline.at(Stage::Presented))
        .collect();
    presented.sort_unstable();
    let gap = presented
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .max()
        .unwrap_or_default();
    let fps = match (presented.first(), presented.last()) {
        (Some(first), Some(last)) if last > first => {
            (presented.len() - 1) as f64 / (*last - *first).as_secs_f64()
        }
        _ => 0.0,
    };
    (fps, gap)
}

/// Cotas de un perfil (ver el módulo).
#[derive(Debug, Clone, Copy)]
struct Bounds {
    min_fps: f64,
    max_gap: Duration,
    max_av_p95_ms: f64,
}

fn bounds(profile: &str) -> Bounds {
    let ms = Duration::from_millis;
    match profile {
        "lan" | "buena" => Bounds {
            min_fps: 28.5,
            max_gap: ms(250),
            max_av_p95_ms: 50.0,
        },
        "media" | "cuello" => Bounds {
            min_fps: 27.0,
            max_gap: ms(400),
            max_av_p95_ms: 60.0,
        },
        _ => Bounds {
            min_fps: 25.5,
            max_gap: ms(650),
            max_av_p95_ms: 100.0,
        },
    }
}

/// Lo que se mide de una sesión.
#[derive(Debug, Clone)]
struct Row {
    profile: String,
    fps: f64,
    link_loss: f64,
    receiver_loss: f64,
    target_kbps: u32,
    gap: Duration,
    av_p95_ms: f64,
}

impl Row {
    fn markdown(&self) -> String {
        format!(
            "| {} | {:.1} | {:.1} % | {:.1} % | {} | {} | {:.1} |",
            self.profile,
            self.fps,
            self.link_loss,
            self.receiver_loss,
            self.target_kbps,
            self.gap.as_millis(),
            self.av_p95_ms
        )
    }
}

fn header() -> &'static str {
    "| Perfil | fps | Pérdida del enlace | Pérdida en el receptor | kbit/s objetivo | Hueco máx. (ms) | \
     |A−V| p95 (ms) |\n|---|---:|---:|---:|---:|---:|---:|"
}

/// Una sesión de `seconds` a través de lo que haya en `lo` (con `proxy`, el anfitrión anuncia
/// solo el proxy y el invitado nada).
fn session(
    server: &TestServer,
    profile: &str,
    seconds: u64,
    proxy: Option<LossyConfig>,
) -> (LatencySummary, f64, Duration, f64) {
    let mut host_config = server.config();
    let mut guest_config = server.config();
    let proxy = proxy.map(|config| {
        let port = std::net::UdpSocket::bind("127.0.0.1:0")
            .and_then(|socket| socket.local_addr())
            .expect("puerto libre")
            .port();
        let host_addr: SocketAddr = (std::net::Ipv4Addr::LOCALHOST, port).into();
        let proxy = server
            .runtime
            .block_on(UdpLossy::start(host_addr, config))
            .expect("proxy");
        host_config.bind = Some(host_addr);
        host_config.advertise_candidates = Some(vec![proxy.addr()]);
        guest_config.advertise_candidates = Some(Vec::new());
        proxy
    });
    let mut host = host(host_config);
    let code = room_code(&host);
    let frames = u64::from(FPS) * seconds;
    let mut guest = guest(guest_config, &code, frames);
    let finished = guest.wait(Duration::from_secs(seconds * 4 + 30));
    assert!(
        finished,
        "{profile}: el invitado no terminó\n{}",
        guest.summary()
    );
    assert_eq!(guest.end_reason(), None, "{profile}\n{}", guest.summary());
    let summary = guest.summary();
    let (fps, gap) = presentation(guest.ledger());
    guest.close();
    host.close();
    let link_loss = proxy.as_ref().map_or(f64::NAN, |proxy| {
        let stats = proxy.stats();
        let total = stats.forwarded() + stats.dropped();
        100.0 * stats.dropped() as f64 / total.max(1) as f64
    });
    (summary, fps, gap, link_loss)
}

/// Una sesión del perfil y su fila de la tabla, con los fotogramas presentados y los esperados.
fn measure(
    server: &TestServer,
    profile: &str,
    seconds: u64,
    proxy: Option<LossyConfig>,
) -> (Row, u64, u64) {
    let (summary, fps, gap, link_loss) = session(server, profile, seconds, proxy);
    println!("--- {profile} ---\n{summary}");
    let link = summary.link.clone().expect("resumen del enlace");
    let av = summary
        .audio
        .as_ref()
        .and_then(|audio| audio.av)
        .unwrap_or_else(|| panic!("{profile}: sin |A−V|\n{summary}"));
    let frames = u64::from(FPS) * seconds;
    let presented = summary.frames_presented.min(frames);
    let row = Row {
        profile: profile.to_owned(),
        fps,
        link_loss,
        receiver_loss: link.loss_percent,
        target_kbps: link.target_kbps,
        gap,
        av_p95_ms: av.p95_ms,
    };
    (row, presented, frames)
}

/// Comprueba las cotas del perfil; devuelve la primera que no se cumple.
fn check(row: &Row, expected_loss: Option<f64>, presented: u64, frames: u64) -> Result<(), String> {
    let mut bounds = bounds(&row.profile);
    // Las cotas de tiempo de reloj escalan con la carga del entorno (`common::load_factor`).
    let factor = common::load_factor();
    bounds.max_gap = bounds.max_gap.mul_f64(factor);
    bounds.max_av_p95_ms *= factor;
    if presented < frames {
        return Err(format!("{}: {presented} de {frames}", row.profile));
    }
    if row.fps < bounds.min_fps {
        return Err(format!(
            "{}: {:.1} fps, cota {:.1}",
            row.profile, row.fps, bounds.min_fps
        ));
    }
    if row.gap > bounds.max_gap {
        return Err(format!(
            "{}: hueco de {:?}, cota {:?}",
            row.profile, row.gap, bounds.max_gap
        ));
    }
    if row.av_p95_ms > bounds.max_av_p95_ms {
        return Err(format!(
            "{}: |A−V| p95 {:.1} ms, cota {:.1} ms",
            row.profile, row.av_p95_ms, bounds.max_av_p95_ms
        ));
    }
    if let Some(expected) = expected_loss {
        let tolerance = (expected * 0.5).max(0.5);
        if (row.link_loss - expected).abs() > tolerance {
            return Err(format!(
                "{}: pérdida del enlace {:.2} %, perfil {expected} %",
                row.profile, row.link_loss
            ));
        }
    }
    Ok(())
}

/// Mide un perfil y comprueba sus cotas; si no se cumplen, repite la sesión una vez. Una sesión
/// de pocos segundos es sensible a la carga del equipo (un intra grande en mal momento basta para
/// perder un fotograma de la cota de fps); dos seguidas fuera de cota son un fallo real.
fn measure_checked(
    server: &TestServer,
    profile: &str,
    seconds: u64,
    proxy: Option<LossyConfig>,
    expected_loss: Option<f64>,
) -> Row {
    let (row, presented, frames) = measure(server, profile, seconds, proxy);
    println!("{}", row.markdown());
    match check(&row, expected_loss, presented, frames) {
        Ok(()) => row,
        Err(first) => {
            println!("{first}: se repite la sesión de «{profile}» una vez");
            let (row, presented, frames) = measure(server, profile, seconds, proxy);
            println!("{}", row.markdown());
            if let Err(second) = check(&row, expected_loss, presented, frames) {
                panic!("{profile}: fuera de cota dos veces: {first}; {second}");
            }
            row
        }
    }
}

#[test]
fn lossy_matrix() {
    let _serial = serial();
    common::load_factor();
    let server = TestServer::start();
    let seconds = 10;
    let mut rows = Vec::new();
    for profile in ["buena", "media", "mala"] {
        let config = LossyConfig::profile(profile).expect("perfil");
        let row = measure_checked(
            &server,
            profile,
            seconds,
            Some(config),
            Some(config.loss * 100.0),
        );
        rows.push(row);
    }
    println!(
        "\nMatriz con UdpLossy ({seconds} s por perfil):\n{}",
        header()
    );
    for row in &rows {
        println!("{}", row.markdown());
    }
    drop(server);
}

/// `tc` y `sudo -n` disponibles (o root).
fn can_use_netem() -> bool {
    let tc = std::process::Command::new("tc")
        .arg("-V")
        .output()
        .is_ok_and(|output| output.status.success());
    let sudo = std::process::Command::new("sudo")
        .args(["-n", "true"])
        .output()
        .is_ok_and(|output| output.status.success());
    tc && sudo
}

fn netem(args: &[&str]) -> bool {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/netem.sh");
    std::process::Command::new("sudo")
        .arg("-n")
        .arg(script)
        .args(args)
        .status()
        .is_ok_and(|status| status.success())
}

/// Quita netem al terminar, también si el test falla.
struct NetemGuard;

impl Drop for NetemGuard {
    fn drop(&mut self) {
        let _ = netem(&["clear"]);
    }
}

#[test]
#[ignore = "necesita tc netem y sudo -n; la CI lo ejecuta en su paso «Matriz de red (netem)»"]
fn netem_matrix() {
    if !can_use_netem() {
        eprintln!("sin `tc` o sin `sudo -n`: netem_matrix saltado (ver tools/netem.sh)");
        return;
    }
    let _serial = serial();
    common::load_factor();
    let server = TestServer::start();
    let seconds = 30;
    let mut rows = Vec::new();
    for profile in ["lan", "buena", "media", "mala", "cuello"] {
        let _guard = NetemGuard;
        if !netem(&["profile", profile]) {
            eprintln!("el kernel no tiene sch_netem: netem_matrix saltado");
            return;
        }
        let row = measure_checked(&server, profile, seconds, None, None);
        rows.push(row);
    }
    println!(
        "\nMatriz con tc netem ({seconds} s por perfil):\n{}",
        header()
    );
    for row in &rows {
        println!("{}", row.markdown());
    }
    drop(server);
}

#[test]
fn the_load_factor_is_measured() {
    let _serial = serial();
    let elapsed = common::encode_calibration();
    let factor = common::load_factor();
    println!(
        "codificar {} fotogramas: {elapsed:?}; factor {factor:.2}",
        common::CALIBRATION_FRAMES
    );
    assert!((1.0..=3.0).contains(&factor));
}
