//! Prueba de larga duración: servidor real, un anfitrión con
//! vídeo sintético (640×360 a 30 fps, OpenH264) y audio del juego sintético, y **dos invitados**
//! en proceso que presentan y reproducen sin parar. Cada minuto se mide, del proceso entero:
//!
//! - memoria residente (`/proc/self/statm`, páginas residentes × tamaño de página);
//! - descriptores abiertos (`/proc/self/fd`);
//! - hilos (`Threads:` de `/proc/self/status`);
//!
//! y que los dos invitados siguen presentando. Falla si:
//!
//! - la regresión lineal de la memoria sobre los últimos 2/3 de la prueba (el primer tercio es el
//!   calentamiento: búferes, cachés del asignador, buffer de jitter) tiene pendiente positiva
//!   **significativa**: estadístico t > 3 y, a la vez, un crecimiento proyectado de más de
//!   64 MiB en 8 horas (más de ~136 KiB por minuto); las dos condiciones evitan que el ruido del
//!   asignador o una pendiente minúscula pero «significativa» fallen la prueba;
//! - los descriptores o los hilos crecen en esa ventana (último − primero > 2 descriptores o > 1
//!   hilo);
//! - algún invitado deja de presentar.
//!
//! Solo Linux (`/proc`). Es `#[ignore]`: por defecto dura 8 horas.
//!
//! Con glibc, se ejecuta con los umbrales del asignador fijos (`GLIBC_TUNABLES`, abajo). Con los de
//! fábrica, el umbral de `mmap` es dinámico (sube tras liberar un bloque grande, hasta 32 MiB) y
//! cada arena por hilo crece a su máximo histórico, así que la memoria residente sube a escalones
//! de 3-9 MB en momentos al azar (un intra mayor que los anteriores, una ráfaga tras un reenvío) y
//! luego se queda plana: no es una fuga, pero en 15 minutos la regresión la confunde con una. Con
//! los umbrales fijos se mide lo que retiene el programa (15 minutos, −10 KiB/min con
//! ellos; +1 206 KiB/min a escalones sin ellos).
//!
//! ```text
//! export GLIBC_TUNABLES=glibc.malloc.mmap_threshold=131072:glibc.malloc.trim_threshold=131072:glibc.malloc.arena_max=2
//! COROOM_SOAK_MINUTES=15 cargo test --release -p coroom --test soak -- --ignored --nocapture
//! cargo test --release -p coroom --test soak -- --ignored --nocapture     # 480 minutos
//! ```
//!
//! `COROOM_SOAK_INTERVAL_SECS` cambia el periodo de medida (60 por defecto; para probar el test).

mod common;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use common::{Account, TestServer};
use coroom_session::Session;
use coroom_session::audio::synthetic::{CaptureSink, Recording, ToneSource};
use coroom_session::audio::{AudioConfig, AudioError, AudioFormat, AudioSink, Played};
use coroom_session::capture::synthetic::SyntheticCapturer;
use coroom_session::codec::{DecoderSet, EncoderSet};
use coroom_session::latency::LatencyLedger;
use coroom_session::loopback::{LoopbackConfig, NullSink};
use coroom_session::remote::{GuestAudio, HostAudio, RemoteSession};

const FPS: u32 = 30;

/// Salida de audio a ritmo real que no guarda lo que suena (la de los tests lo graba todo).
struct DiscardSink {
    inner: CaptureSink,
    recording: Arc<Mutex<Recording>>,
}

impl DiscardSink {
    fn new() -> Self {
        let inner = CaptureSink::new(2);
        let recording = inner.recording();
        Self { inner, recording }
    }
}

impl AudioSink for DiscardSink {
    fn format(&self) -> AudioFormat {
        self.inner.format()
    }

    fn start(&mut self, config: &AudioConfig) -> Result<(), AudioError> {
        self.inner.start(config)
    }

    fn wait_ready(&mut self) -> Result<(), AudioError> {
        self.inner.wait_ready()
    }

    fn write(&mut self, samples: &[i16]) -> Result<Played, AudioError> {
        let played = self.inner.write(samples)?;
        let mut recording = self
            .recording
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        recording.samples.clear();
        recording.writes.clear();
        Ok(played)
    }

    fn stop(&mut self) {
        self.inner.stop();
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

fn guest(account: Account, code: &str) -> RemoteSession {
    let mut session = RemoteSession::guest(
        account.identity,
        account.config,
        code,
        DecoderSet::openh264(),
        Box::new(NullSink),
        None,
        Arc::new(LatencyLedger::new()),
    )
    .with_guest_audio(GuestAudio {
        speaker: Some(Box::new(DiscardSink::new())),
        voice: None,
        mute_game: false,
        frame_ms: 10,
    });
    session.start().expect("arranque del invitado");
    session
}

/// Una medida del proceso.
#[derive(Debug, Clone, Copy)]
struct Sample {
    minute: f64,
    rss_bytes: u64,
    fds: usize,
    threads: usize,
    presented: [u64; 2],
}

/// Tamaño de página del núcleo (`KernelPageSize` de la primera región de `/proc/self/smaps`):
/// 4 KiB en x86-64, 4, 16 o 64 KiB en aarch64.
fn page_size() -> u64 {
    use std::io::BufRead;
    let file = std::fs::File::open("/proc/self/smaps").expect("/proc/self/smaps");
    std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .find_map(|line| {
            let kb = line.strip_prefix("KernelPageSize:")?;
            kb.trim().trim_end_matches("kB").trim().parse::<u64>().ok()
        })
        .map_or(4096, |kb| kb * 1024)
}

fn rss_bytes(page_size: u64) -> u64 {
    let statm = std::fs::read_to_string("/proc/self/statm").expect("/proc/self/statm");
    let resident: u64 = statm
        .split_whitespace()
        .nth(1)
        .and_then(|pages| pages.parse().ok())
        .expect("páginas residentes");
    resident * page_size
}

fn open_fds() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("/proc/self/fd")
        .count()
}

fn threads() -> usize {
    std::fs::read_to_string("/proc/self/status")
        .expect("/proc/self/status")
        .lines()
        .find_map(|line| line.strip_prefix("Threads:"))
        .and_then(|count| count.trim().parse().ok())
        .expect("hilos")
}

/// Pendiente (por minuto) de la regresión lineal de `points` y su error típico.
fn regression(points: &[(f64, f64)]) -> (f64, f64) {
    let n = points.len() as f64;
    let mean_x = points.iter().map(|(x, _)| x).sum::<f64>() / n;
    let mean_y = points.iter().map(|(_, y)| y).sum::<f64>() / n;
    let sxx: f64 = points.iter().map(|(x, _)| (x - mean_x).powi(2)).sum();
    let sxy: f64 = points
        .iter()
        .map(|(x, y)| (x - mean_x) * (y - mean_y))
        .sum();
    let slope = sxy / sxx;
    let intercept = mean_y - slope * mean_x;
    let residual: f64 = points
        .iter()
        .map(|(x, y)| (y - (intercept + slope * x)).powi(2))
        .sum();
    let se = (residual / (n - 2.0).max(1.0) / sxx).sqrt();
    (slope, se)
}

/// Crecimiento máximo admitido en 8 horas.
const MAX_GROWTH_8H: f64 = 64.0 * 1024.0 * 1024.0;

#[test]
#[ignore = "larga duración: COROOM_SOAK_MINUTES (480 por defecto)"]
fn soak() {
    if cfg!(target_env = "gnu") && std::env::var_os("GLIBC_TUNABLES").is_none() {
        println!(
            "aviso: sin GLIBC_TUNABLES, los escalones del asignador pueden parecer una fuga \
             (ver la documentación del módulo)"
        );
    }
    let minutes: u64 = std::env::var("COROOM_SOAK_MINUTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(480);
    let interval = Duration::from_secs(
        std::env::var("COROOM_SOAK_INTERVAL_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(60),
    );
    assert!(minutes >= 3, "hacen falta al menos 3 minutos");
    let server = TestServer::start();
    let mut host = host(server.config());
    let mut code = None;
    let deadline = Instant::now() + Duration::from_secs(15);
    while code.is_none() && Instant::now() < deadline {
        code = host.room_code();
        std::thread::sleep(Duration::from_millis(50));
    }
    let code = code.expect("código de sala");
    let mut guests = [guest(server.config(), &code), guest(server.config(), &code)];
    let deadline = Instant::now() + Duration::from_secs(30);
    while guests
        .iter()
        .any(|guest| guest.summary().frames_presented == 0)
    {
        assert!(Instant::now() < deadline, "los invitados no presentan");
        std::thread::sleep(Duration::from_millis(100));
    }

    let page = page_size();
    let started = Instant::now();
    let total = Duration::from_secs(minutes * 60);
    let mut samples: Vec<Sample> = Vec::new();
    println!("minuto | memoria (MiB) | descriptores | hilos | presentados");
    loop {
        let presented = [
            guests[0].summary().frames_presented,
            guests[1].summary().frames_presented,
        ];
        let sample = Sample {
            minute: started.elapsed().as_secs_f64() / 60.0,
            rss_bytes: rss_bytes(page),
            fds: open_fds(),
            threads: threads(),
            presented,
        };
        println!(
            "{:6.1} | {:13.1} | {:12} | {:5} | {} / {}",
            sample.minute,
            sample.rss_bytes as f64 / 1_048_576.0,
            sample.fds,
            sample.threads,
            sample.presented[0],
            sample.presented[1]
        );
        if let Some(previous) = samples.last() {
            for i in 0..2 {
                assert!(
                    sample.presented[i] > previous.presented[i],
                    "el invitado {i} dejó de presentar en el minuto {:.1}",
                    sample.minute
                );
            }
        }
        assert!(
            !host.is_finished(),
            "el anfitrión terminó: {:?}",
            host.end_reason()
        );
        samples.push(sample);
        if started.elapsed() >= total {
            break;
        }
        std::thread::sleep(interval.min(total.saturating_sub(started.elapsed())));
    }
    for guest in &mut guests {
        guest.close();
    }
    host.close();
    drop(server);

    let window = &samples[samples.len() / 3..];
    let points: Vec<(f64, f64)> = window
        .iter()
        .map(|sample| (sample.minute, sample.rss_bytes as f64))
        .collect();
    let (slope, se) = regression(&points);
    let t = if se > 0.0 {
        slope / se
    } else {
        f64::INFINITY * slope.signum()
    };
    let growth_8h = slope * 480.0;
    let first = window.first().expect("ventana");
    let last = window.last().expect("ventana");
    println!(
        "\nResumen ({minutes} min, {} medidas; ventana de los últimos 2/3: {} medidas):\n\
         - memoria: {:.1} → {:.1} MiB, pendiente {:+.1} KiB/min (t = {t:.2}), proyección a 8 h \
         {:+.1} MiB\n- descriptores: {} → {} (máx. {})\n- hilos: {} → {} (máx. {})",
        samples.len(),
        window.len(),
        first.rss_bytes as f64 / 1_048_576.0,
        last.rss_bytes as f64 / 1_048_576.0,
        slope / 1024.0,
        growth_8h / 1_048_576.0,
        first.fds,
        last.fds,
        samples.iter().map(|s| s.fds).max().unwrap_or(0),
        first.threads,
        last.threads,
        samples.iter().map(|s| s.threads).max().unwrap_or(0),
    );
    assert!(
        !(t > 3.0 && growth_8h > MAX_GROWTH_8H),
        "la memoria crece {:+.1} KiB/min (t = {t:.2}): {:+.1} MiB en 8 h",
        slope / 1024.0,
        growth_8h / 1_048_576.0
    );
    assert!(
        last.fds <= first.fds + 2,
        "los descriptores crecen: {} → {}",
        first.fds,
        last.fds
    );
    assert!(
        last.threads <= first.threads + 1,
        "los hilos crecen: {} → {}",
        first.threads,
        last.threads
    );
}

#[test]
fn the_regression_finds_a_leak_and_ignores_noise() {
    // 100 KiB por minuto de fuga con ruido de ±2 MiB: pendiente cercana a la real.
    let leak: Vec<(f64, f64)> = (0..300)
        .map(|i| {
            let noise = if i % 2 == 0 { 2.0 } else { -2.0 } * 1_048_576.0;
            (f64::from(i), 400e6 + f64::from(i) * 102_400.0 + noise)
        })
        .collect();
    let (slope, se) = regression(&leak);
    assert!((slope - 102_400.0).abs() < 5_000.0, "{slope}");
    assert!(slope / se > 3.0);
    // Solo ruido: sin pendiente significativa.
    let flat: Vec<(f64, f64)> = (0..300)
        .map(|i| (f64::from(i), 400e6 + if i % 3 == 0 { 1e6 } else { -5e5 }))
        .collect();
    let (slope, se) = regression(&flat);
    assert!(slope.abs() * 480.0 < MAX_GROWTH_8H, "{slope} ± {se}");
}
