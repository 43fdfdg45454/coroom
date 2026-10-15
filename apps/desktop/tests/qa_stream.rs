//! QA de transmisión con los códecs **reales** de la máquina (los que da el sondeo: NVENC,
//! Vulkan Video, VA-API…), servidor real en proceso y fuente sintética, buscando que algo falle:
//!
//! - `qa_matrix`: resoluciones y fps de 64×64 a 8K y de 10 a 240 fps, tamaños impares;
//! - `qa_live_changes`: con un invitado viendo, cambios de fuente (resolución y fps), de códec y
//!   de bitrate en caliente, también en ráfaga;
//! - `qa_odd_cases`: entrar y salir varias veces, dos invitados con decodificadores distintos,
//!   expulsar, reconectar, cerrar la sala con el invitado dentro y pérdida de paquetes.
//!
//! Necesitan GPU y tardan minutos: `#[ignore]`. Se lanzan con
//! `cargo test --release -p coroom --test qa_stream -- --ignored --nocapture --test-threads=1`.

mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use common::{Account, TestServer};
use coroom_session::Session;
use coroom_session::capture::synthetic::SyntheticCapturer;
use coroom_session::codec::{Codec, DecoderBackend, DecoderSet, EncoderSet};
use coroom_session::latency::{LatencyLedger, LatencySummary};
use coroom_session::loopback::{DecodedFrame, FrameSink, LoopbackConfig, SinkError};
use coroom_session::remote::{RemoteSession, VideoChange};
use coroom_session::{GuestId, SessionState};
use coroom_transport::lossy::{LossyConfig, UdpLossy};

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

/// Lo que presenta el invitado: cuántos fotogramas y de qué tamaño fue el último.
#[derive(Clone, Default)]
struct Seen {
    frames: Arc<AtomicU64>,
    last: Arc<Mutex<(u32, u32)>>,
}

impl Seen {
    fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    fn size(&self) -> (u32, u32) {
        *self.last.lock().unwrap_or_else(|p| p.into_inner())
    }
}

struct SeenSink(Seen);

impl FrameSink for SeenSink {
    fn present(&mut self, frame: DecodedFrame) -> Result<(), SinkError> {
        *self.0.last.lock().unwrap_or_else(|p| p.into_inner()) =
            (frame.frame.width, frame.frame.height);
        self.0.frames.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

fn config(width: u32, height: u32, fps: u32, kbps: u32) -> LoopbackConfig {
    LoopbackConfig {
        width,
        height,
        fps,
        bitrate_kbps: kbps,
        frames: None,
        yuv444: false,
        hdr: false,
    }
}

fn host(
    account: Account,
    width: u32,
    height: u32,
    fps: u32,
    encoders: EncoderSet,
) -> RemoteSession {
    let mut session = RemoteSession::host(
        account.identity,
        account.config,
        Box::new(SyntheticCapturer::new(width, height, fps)),
        encoders,
        config(width, height, fps, 20_000),
        Arc::new(LatencyLedger::new()),
    );
    session.start().expect("arranque del anfitrión");
    session
}

/// Como [`host`], pero devuelve el error de arranque en vez de fallar.
fn try_host(account: Account, width: u32, height: u32, fps: u32) -> Result<RemoteSession, String> {
    let mut session = RemoteSession::host(
        account.identity,
        account.config,
        Box::new(SyntheticCapturer::new(width, height, fps)),
        EncoderSet::probe(),
        config(width, height, fps, 20_000),
        Arc::new(LatencyLedger::new()),
    );
    session
        .start()
        .map_err(|err| format!("arranque del anfitrión: {err}"))?;
    Ok(session)
}

fn guest(
    account: Account,
    code: &str,
    frames: Option<u64>,
    decoders: DecoderSet,
    seen: &Seen,
) -> RemoteSession {
    let mut session = RemoteSession::guest(
        account.identity,
        account.config,
        code,
        decoders,
        Box::new(SeenSink(seen.clone())),
        frames,
        Arc::new(LatencyLedger::new()),
    );
    session.start().expect("arranque del invitado");
    session
}

fn room_code(host: &RemoteSession) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(code) = host.room_code() {
            return Ok(code);
        }
        if host.is_finished() {
            return Err(format!("el anfitrión terminó: {:?}", host.end_reason()));
        }
        if Instant::now() > deadline {
            return Err("sin código de sala en 20 s".to_owned());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Espera a que el invitado presente `more` fotogramas más (y, si se da, de ese tamaño); `Err`
/// con lo que pasó si no llega en `limit` o alguno de los dos termina.
fn advance(
    host: &RemoteSession,
    guest: &RemoteSession,
    seen: &Seen,
    more: u64,
    size: Option<(u32, u32)>,
    limit: Duration,
) -> Result<Duration, String> {
    let started = Instant::now();
    let mut base = seen.frames();
    let mut matched = size.is_none();
    loop {
        if host.is_finished() {
            return Err(format!("el anfitrión terminó: {:?}", host.end_reason()));
        }
        if guest.is_finished() {
            return Err(format!("el invitado terminó: {:?}", guest.end_reason()));
        }
        if !matched && Some(seen.size()) == size {
            matched = true;
            base = seen.frames();
        }
        if matched && seen.frames() >= base + more {
            return Ok(started.elapsed());
        }
        if started.elapsed() > limit {
            return Err(format!(
                "en {limit:?} solo {} fotogramas más, último tamaño {:?} (esperado {size:?})",
                seen.frames().saturating_sub(base),
                seen.size()
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Anota el resultado de un paso: lo imprime y, si falló, lo añade a `failures`.
fn check(failures: &mut Vec<String>, what: &str, result: Result<Duration, String>) {
    match result {
        Ok(took) => println!("{what}: ok en {took:?}"),
        Err(err) => {
            println!("{what}: FALLO {err}");
            failures.push(format!("{what}: {err}"));
        }
    }
}

fn ms(us: u64) -> f64 {
    us as f64 / 1000.0
}

fn line(summary: &LatencySummary) -> String {
    format!(
        "{} · {:.1} fps · {:.0} kbit/s · total p50 {:.1} / p95 {:.1} / máx {:.1} ms · descartes {} · errores de decodificación {}",
        summary.video.as_deref().unwrap_or("?"),
        summary.fps,
        summary.bitrate_kbps,
        ms(summary.total.p50_us),
        ms(summary.total.p95_us),
        ms(summary.total.max_us),
        summary.drops_total,
        summary.decode_errors
    )
}

/// Resoluciones y fps, una sala por caso: 5 s de vídeo (como mucho 300 fotogramas) con los
/// codificadores y decodificadores del sondeo. Imprime una línea por caso y falla al final si
/// alguno no llegó a presentar el 90 % de lo esperado o terminó con error.
#[test]
#[ignore = "necesita GPU; QA manual"]
fn qa_matrix() {
    let _serial = serial();
    let cases: [(u32, u32, u32); 18] = [
        (64, 64, 10),
        (320, 180, 15),
        (640, 360, 30),
        (1001, 701, 50),
        (1280, 720, 60),
        (1280, 720, 144),
        (1366, 768, 75),
        (1920, 1080, 24),
        (1920, 1080, 60),
        (1920, 1080, 120),
        (1920, 1080, 240),
        (2560, 1080, 100),
        (2560, 1440, 60),
        (2560, 1440, 165),
        (3440, 1440, 100),
        (3840, 2160, 60),
        (3840, 2160, 120),
        (7680, 4320, 30),
    ];
    let server = TestServer::start();
    let mut failures = Vec::new();
    for (width, height, fps) in cases {
        let name = format!("{width}×{height}@{fps}");
        // `QA_ONLY=1920×1080@240` repite solo ese caso.
        if std::env::var("QA_ONLY").is_ok_and(|only| only != name) {
            continue;
        }
        let frames = u64::from(fps * 5).min(300);
        let seen = Seen::default();
        let host = match try_host(server.config(), width, height, fps) {
            Ok(host) => host,
            Err(err) => {
                println!("{name}: FALLO {err}");
                failures.push(format!("{name}: {err}"));
                continue;
            }
        };
        let code = match room_code(&host) {
            Ok(code) => code,
            Err(err) => {
                println!("{name}: FALLO {err}");
                failures.push(format!("{name}: {err}"));
                continue;
            }
        };
        let mut guest = guest(
            server.config(),
            &code,
            Some(frames),
            DecoderSet::probe(),
            &seen,
        );
        let limit = Duration::from_secs(u64::from(frames as u32 / fps.max(1)) + 30);
        let finished = guest.wait(limit);
        let summary = guest.summary();
        let reason = guest.end_reason();
        let host_reason = host.end_reason();
        let presented = summary.frames_presented;
        let status = if !finished {
            format!("FALLO: no terminó en {limit:?} ({presented} de {frames})")
        } else if reason.is_some() || host_reason.is_some() {
            format!("FALLO: invitado {reason:?}, anfitrión {host_reason:?}")
        } else if presented * 10 < frames * 9 {
            format!("FALLO: {presented} de {frames} presentados")
        } else {
            "ok".to_owned()
        };
        println!(
            "{name}: {status} · tamaño visto {:?} · {}",
            seen.size(),
            line(&summary)
        );
        if status != "ok" {
            failures.push(format!("{name}: {status}"));
        }
        guest.close();
        let mut host = host;
        host.close();
    }
    assert!(
        failures.is_empty(),
        "casos con fallo:\n{}",
        failures.join("\n")
    );
}

/// Un invitado viendo y el anfitrión cambiando todo en caliente: resolución y fps (fuente
/// nueva), códec, bitrate y luego todo en ráfaga. Después de cada paso el invitado tiene que
/// seguir presentando (y con el tamaño nuevo, recortado a par: una ventana de 1199×808 llega
/// como 1198×808) sin que nadie salga de la sala.
#[test]
#[ignore = "necesita GPU; QA manual"]
fn qa_live_changes() {
    let _serial = serial();
    let server = TestServer::start();
    let mut host = host(server.config(), 1920, 1080, 60, EncoderSet::probe());
    let code = room_code(&host).expect("sala");
    let seen = Seen::default();
    let mut guest = guest(server.config(), &code, None, DecoderSet::probe(), &seen);
    let step = Duration::from_secs(15);
    let mut failures: Vec<String> = Vec::new();
    check(
        &mut failures,
        "arranque 1920×1080@60",
        advance(&host, &guest, &seen, 60, Some((1920, 1080)), step),
    );
    for (width, height, fps) in [
        (1280, 720, 30),
        (3840, 2160, 60),
        (2560, 1440, 144),
        (640, 360, 15),
        (1366, 768, 75),
        (1199, 808, 60),
        (1285, 807, 60),
        (1920, 1080, 240),
        (1920, 1080, 60),
    ] {
        let what = format!("fuente → {width}×{height}@{fps}");
        let switched =
            host.switch_capture(Box::new(SyntheticCapturer::new(width, height, fps)), None);
        check(
            &mut failures,
            &what,
            switched
                .map_err(|err| format!("switch_capture: {err}"))
                .and_then(|()| {
                    advance(
                        &host,
                        &guest,
                        &seen,
                        u64::from(fps),
                        Some((width & !1, height & !1)),
                        step,
                    )
                }),
        );
    }
    for (label, codec) in [
        ("H.264", Some(Codec::H264)),
        ("HEVC", Some(Codec::Hevc)),
        ("AV1", Some(Codec::Av1)),
        ("auto", None),
    ] {
        let what = format!("códec → {label}");
        let changed = host.set_video(VideoChange {
            encoders: Some(EncoderSet::probe().only_codec(codec)),
            max_kbps: None,
        });
        check(
            &mut failures,
            &what,
            changed
                .map_err(|err| format!("set_video: {err}"))
                .and_then(|()| advance(&host, &guest, &seen, 120, None, step)),
        );
        println!(
            "  vídeo ahora: {:?} / invitado {:?}",
            host.summary().video,
            guest.summary().video
        );
    }
    for kbps in [300, 500_000, 1_000, 50_000] {
        let what = format!("bitrate máximo → {kbps} kbit/s");
        let changed = host.set_video(VideoChange {
            encoders: None,
            max_kbps: Some(kbps),
        });
        check(
            &mut failures,
            &what,
            changed
                .map_err(|err| format!("set_video: {err}"))
                .and_then(|()| advance(&host, &guest, &seen, 60, None, step)),
        );
    }
    // Ráfaga: 20 cambios cada 150 ms alternando fuente y códec, sin esperar a que asienten.
    let burst_started = Instant::now();
    for i in 0..20u32 {
        let result = if i % 2 == 0 {
            let (width, height, fps) = if i % 4 == 0 {
                (1280, 720, 120)
            } else {
                (1920, 1080, 60)
            };
            host.switch_capture(Box::new(SyntheticCapturer::new(width, height, fps)), None)
        } else {
            let codec = [Some(Codec::H264), Some(Codec::Av1), Some(Codec::Hevc), None]
                [(i as usize / 2) % 4];
            host.set_video(VideoChange {
                encoders: Some(EncoderSet::probe().only_codec(codec)),
                max_kbps: Some(5_000 + i * 1_000),
            })
        };
        if let Err(err) = result {
            failures.push(format!("ráfaga {i}: {err}"));
            println!("ráfaga {i}: FALLO {err}");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    println!("ráfaga enviada en {:?}", burst_started.elapsed());
    check(
        &mut failures,
        "tras la ráfaga (1920×1080)",
        advance(&host, &guest, &seen, 120, Some((1920, 1080)), step),
    );
    println!("invitado: {}", line(&guest.summary()));
    println!("anfitrión: {:?}", host.summary().video);
    guest.close();
    host.close();
    assert!(
        failures.is_empty(),
        "pasos con fallo:\n{}",
        failures.join("\n")
    );
}

/// Casos raros con los códecs reales.
#[test]
#[ignore = "necesita GPU; QA manual"]
fn qa_odd_cases() {
    let _serial = serial();
    let server = TestServer::start();
    let step = Duration::from_secs(15);
    let mut failures: Vec<String> = Vec::new();
    let mut note = |what: &str, result: Result<(), String>| match result {
        Ok(()) => println!("{what}: ok"),
        Err(err) => {
            println!("{what}: FALLO {err}");
            failures.push(format!("{what}: {err}"));
        }
    };

    // 1. Entrar y salir cinco veces seguidas en la misma sala.
    let mut host1 = host(server.config(), 1920, 1080, 60, EncoderSet::probe());
    let code = room_code(&host1).expect("sala");
    for round in 1..=5 {
        let seen = Seen::default();
        let mut visitor = guest(server.config(), &code, None, DecoderSet::probe(), &seen);
        note(
            &format!("entrar y salir, vuelta {round}"),
            advance(&host1, &visitor, &seen, 30, None, step).map(|_| ()),
        );
        visitor.close();
    }

    // 2. Dos invitados a la vez: uno con el decodificador del sondeo y otro por software.
    let (seen_a, seen_b) = (Seen::default(), Seen::default());
    let mut a = guest(server.config(), &code, None, DecoderSet::probe(), &seen_a);
    let mut b = guest(
        server.config(),
        &code,
        None,
        DecoderSet::probe().only_backend(Some(DecoderBackend::Software)),
        &seen_b,
    );
    note(
        "dos invitados: el del sondeo",
        advance(&host1, &a, &seen_a, 120, None, step).map(|_| ()),
    );
    note(
        "dos invitados: el de software",
        advance(&host1, &b, &seen_b, 120, None, step).map(|_| ()),
    );
    println!("  A: {}\n  B: {}", line(&a.summary()), line(&b.summary()));

    // 3. Expulsar a B: A sigue viendo, B termina con motivo.
    let b_id = host1
        .guests()
        .iter()
        .map(|status| status.guest)
        .max_by_key(|id: &GuestId| id.0);
    if let Some(id) = b_id {
        let kicked = host1.kick(id).map_err(|err| err.to_string());
        let finished = b.wait(Duration::from_secs(10));
        note(
            "expulsar a B",
            kicked.and_then(|()| {
                if finished {
                    Ok(())
                } else {
                    Err("B sigue en la sala 10 s después".to_owned())
                }
            }),
        );
        println!("  B terminó con: {:?}", b.end_reason());
    }
    note(
        "A sigue tras expulsar a B",
        advance(&host1, &a, &seen_a, 60, None, step).map(|_| ()),
    );

    // 4. A se reconecta dos veces seguidas.
    let first = a.reconnect().map_err(|err| err.to_string());
    let second = a.reconnect().map_err(|err| err.to_string());
    note(
        "dos reconexiones seguidas",
        first.and(second).and_then(|()| {
            advance(&host1, &a, &seen_a, 120, None, Duration::from_secs(30)).map(|_| ())
        }),
    );

    // 5. El anfitrión cierra la sala con A dentro: A termina, sin colgarse.
    host1.close();
    let ended = a.wait(Duration::from_secs(15));
    note(
        "cerrar la sala con el invitado dentro",
        if ended {
            Ok(())
        } else {
            Err("el invitado no terminó en 15 s".to_owned())
        },
    );
    println!(
        "  A terminó con: {:?} (estado {:?})",
        a.end_reason(),
        a.state()
    );
    a.close();
    b.close();

    // 6. Pérdida: 10 % de pérdida y 20 ± 10 ms entre anfitrión e invitado, 1080p60 real.
    let port = std::net::UdpSocket::bind("127.0.0.1:0")
        .and_then(|socket| socket.local_addr())
        .expect("puerto libre")
        .port();
    let host_addr: std::net::SocketAddr = (std::net::Ipv4Addr::LOCALHOST, port).into();
    let proxy = server
        .runtime
        .block_on(UdpLossy::start(
            host_addr,
            LossyConfig {
                loss: 0.10,
                delay: Duration::from_millis(20),
                jitter: Duration::from_millis(10),
                rate_bps: None,
                queue_limit: Duration::from_millis(200),
                seed: 7,
                outages: None,
            },
        ))
        .expect("proxy");
    let mut host_config = server.config();
    host_config.bind = Some(host_addr);
    host_config.advertise_candidates = Some(vec![proxy.addr()]);
    let mut lossy_host = host(host_config, 1920, 1080, 60, EncoderSet::probe());
    let code = room_code(&lossy_host).expect("sala con pérdida");
    let mut guest_config = server.config();
    guest_config.advertise_candidates = Some(Vec::new());
    let seen = Seen::default();
    let mut lossy_guest = guest(guest_config, &code, Some(600), DecoderSet::probe(), &seen);
    let finished = lossy_guest.wait(Duration::from_secs(60));
    let summary = lossy_guest.summary();
    println!("  con 10 % de pérdida: {}", line(&summary));
    note(
        "10 % de pérdida, 600 fotogramas",
        if !finished {
            Err(format!(
                "no terminó: {} presentados",
                summary.frames_presented
            ))
        } else if let Some(reason) = lossy_guest.end_reason() {
            Err(format!("terminó con {reason:?}"))
        } else {
            Ok(())
        },
    );
    lossy_guest.close();
    lossy_host.close();
    drop(proxy);
    assert_eq!(lossy_host.state(), SessionState::Closed);
    assert!(
        failures.is_empty(),
        "casos con fallo:\n{}",
        failures.join("\n")
    );
}
