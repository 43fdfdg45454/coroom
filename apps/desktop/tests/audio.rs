//! Audio del juego y voz de extremo a extremo en proceso: servidor real (`coroom-server`,
//! puertos efímeros), anfitrión con vídeo sintético (640×360 a 30 fps, OpenH264) y fuentes y
//! sumideros de audio **sintéticos** (tonos con marcas exactas y salidas que graban), todo en
//! `127.0.0.1` y cifrado con SFrame:
//!
//! - (a) audio del juego: tono de 440 Hz en el anfitrión; el invitado lo graba 5 s sin huecos de
//!   más de una trama y con |A−V| p95 < 40 ms frente al vídeo sintético (presentado en una
//!   pantalla sintética de 60 Hz, [`VsyncSink`]: con la presentación instantánea de `NullSink` el
//!   vídeo de 640×360 tarda unos 9 ms y |A−V| queda en p50 ≈ 33 ms, p95 36-44 ms según la carga);
//! - (b) voz: un invitado con permiso habla (660 Hz) y lo oyen el anfitrión y el segundo invitado,
//!   que oye además al anfitrión (550 Hz) en la mezcla; sin permiso, no llega nada;
//! - (c) con un proxy `UdpLossy` (2 %, 10 ± 5 ms): sin huecos de más de 60 ms y con las pérdidas
//!   ocultadas (PLC) y recuperadas (FEC en banda o DRED) contadas;
//! - (d) boca-oído de la voz (micrófono sintético del invitado → salida sintética del anfitrión)
//!   p95 < 80 ms, con el desglose por etapas de los dos extremos;
//! - (e) por el relay: (a) y (b) iguales.
//!
//! Los huecos son silencio en lo que se graba (≤ 1 trama); las rachas de tramas ocultadas con PLC
//! se acotan a 3 por los parones de la VM (ver `check_game_audio`).
//!
//! Cotas: |A−V| y boca-oído usan las marcas del registro (captura del emisor convertida con el
//! reloj de la sesión y hora estimada de reproducción del sumidero, que reproduce a demanda con
//! 10 ms de latencia de dispositivo, como la salida de PipeWire con `node.latency = 480/48000`). Los huecos se miden
//! dos veces: en el buffer de jitter (tramas seguidas sin paquete) y en la grabación (tramas de
//! 10 ms por debajo de −40 dBFS, cuando el tono está a −13 dBFS).
//!
//! Que el sonido llegue (tonos audibles, permisos, pérdidas ocultadas, cambios de dispositivo) es
//! funcional; las latencias, los huecos y las cuentas que dependen del tiempo de reloj van con
//! `timing_assert!`: avisan sin hacer fallar la prueba. Lo que se esperaba durmiendo un tiempo fijo
//! se espera mirando lo grabado o lo medido, con un plazo amplio.
//!
//! Para que no se roben CPU, con `cargo test` van de uno en uno (un cerrojo común); con nextest,
//! cada test es un proceso aparte y su grupo de `.config/nextest.toml` deja dos a la vez.

mod common;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use common::{Account, TestServer};
use coroom_session::audio::processing::ProcessingOptions;
use coroom_session::audio::synthetic::{CaptureSink, Recording, ToneSource};
use coroom_session::capture::synthetic::SyntheticCapturer;
use coroom_session::codec::{DecoderSet, EncoderSet};
use coroom_session::latency::{AudioSummary, AudioTrack, LatencyLedger, LatencySummary};
use coroom_session::loopback::{DecodedFrame, FrameSink, LoopbackConfig, SinkError};
use coroom_session::remote::{GuestAudio, HostAudio, RemoteSession, VoiceInput};
use coroom_session::{Permissions, Session};
use coroom_testkit::timing_assert;
use coroom_transport::lossy::{LossyConfig, UdpLossy};

const FPS: u32 = 30;
/// Segundos de vídeo (y de audio del juego) de las sesiones de (a) y (e).
const GAME_SECONDS: u64 = 5;
const GAME_HZ: f64 = 440.0;
const HOST_VOICE_HZ: f64 = 550.0;
const GUEST_VOICE_HZ: f64 = 660.0;
/// Nivel por debajo del cual una trama de 10 ms cuenta como hueco (el tono está a −13 dBFS).
const SILENCE_DBFS: f32 = -40.0;
/// Tiempo límite de cada sesión.
const LIMIT: Duration = Duration::from_secs(60);
/// Plazo de lo que se espera mirando lo grabado o lo medido.
const WAIT: Duration = Duration::from_secs(30);

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

fn wait_until(what: &str, limit: Duration, condition: impl FnMut() -> bool) {
    poll_until(what, limit, Duration::from_millis(20), condition);
}

/// Como [`wait_until`], mirando cada `period` (para lo que cuesta mirar: grabaciones, resúmenes).
fn poll_until(what: &str, limit: Duration, period: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "tiempo agotado esperando: {what}"
        );
        std::thread::sleep(period);
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

/// Pantalla sintética de 60 Hz: `present` vuelve en el siguiente vsync, como un presentador FIFO
/// (`NullSink` presenta al instante, sin la espera de la pantalla que sí cuenta el audio en su
/// sumidero sintético con latencia de dispositivo).
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

/// Micrófono sintético: un tono sin procesado (la supresión de ruido atenúa los tonos fijos).
fn tone_mic(freq: f64) -> VoiceInput {
    VoiceInput {
        processing: ProcessingOptions::NONE,
        ..VoiceInput::new(Box::new(ToneSource::new(freq, 1)))
    }
}

/// Grabación compartida de un sumidero sintético.
type Tape = Arc<Mutex<Recording>>;

fn tape(tape: &Tape) -> Recording {
    tape.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// Los últimos `seconds` de la grabación: copiarla entera cada poco con el cerrojo del sumidero
/// tomado deja sin sitio a la reproducción en un runner lento.
fn tail(tape: &Tape, seconds: usize) -> Recording {
    let recording = tape.lock().unwrap_or_else(|p| p.into_inner());
    let keep = seconds * 48_000 * usize::from(recording.channels.max(1));
    let from = recording.samples.len().saturating_sub(keep);
    Recording {
        channels: recording.channels,
        samples: recording.samples[from..].to_vec(),
        writes: Vec::new(),
        underruns: recording.underruns,
    }
}

struct Host {
    session: RemoteSession,
    speaker: Option<Tape>,
}

fn host(account: Account, game: bool, voice: Option<f64>, speaker: bool) -> Host {
    let sink = CaptureSink::new(1);
    let tape = speaker.then(|| sink.recording());
    let audio = HostAudio {
        game: game.then(|| {
            Box::new(ToneSource::new(GAME_HZ, 2)) as Box<dyn coroom_session::audio::AudioSource>
        }),
        game_bitrate: 0,
        frame_ms: 10,
        voice: voice.map(tone_mic),
        speaker: speaker.then(|| Box::new(sink) as Box<dyn coroom_session::audio::AudioSink>),
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
    Host {
        session,
        speaker: tape,
    }
}

struct Guest {
    session: RemoteSession,
    speaker: Tape,
}

fn guest(account: Account, code: &str, frames: Option<u64>, voice: Option<f64>) -> Guest {
    let sink = CaptureSink::new(2);
    let speaker = sink.recording();
    let mut session = RemoteSession::guest(
        account.identity,
        account.config,
        code,
        DecoderSet::openh264(),
        Box::new(VsyncSink {
            origin: Instant::now(),
        }),
        frames,
        Arc::new(LatencyLedger::new()),
    )
    .with_guest_audio(GuestAudio {
        speaker: Some(Box::new(sink)),
        voice: voice.map(tone_mic),
        mute_game: false,
        frame_ms: 10,
    });
    session.start().expect("arranque del invitado");
    Guest { session, speaker }
}

/// Espera a que el anfitrión tenga `count` invitados conectados y les da el permiso de voz.
fn allow_voice(host: &mut RemoteSession, count: usize, voice: bool) {
    wait_until("los invitados", Duration::from_secs(20), || {
        host.guests().iter().filter(|g| g.path.is_some()).count() >= count
    });
    for status in host.guests() {
        assert!(status.permissions.voice, "entra con la voz permitida");
        let permissions = Permissions {
            voice,
            ..status.permissions
        };
        host.set_permissions(status.guest, permissions)
            .expect("permisos");
    }
}

fn audio(summary: &LatencySummary) -> AudioSummary {
    summary
        .audio
        .clone()
        .unwrap_or_else(|| panic!("sin audio:\n{summary}"))
}

/// Amplitud de pico (0-1) de `freq` en el canal 0 de las últimas `seconds` de `recording`: la
/// mediana de la de cada bloque de 100 ms (un recorte o un estiramiento del buffer de jitter rompe
/// la fase del tono, que no es un fallo).
fn tone_level(recording: &Recording, freq: f64, seconds: f64) -> f64 {
    let channels = usize::from(recording.channels.max(1));
    let samples: Vec<f64> = recording
        .samples
        .iter()
        .step_by(channels)
        .map(|s| f64::from(*s) / 32_768.0)
        .collect();
    let len = ((seconds * 48_000.0) as usize).min(samples.len());
    let mut levels: Vec<f64> = samples[samples.len() - len..]
        .as_chunks::<4_800>()
        .0
        .iter()
        .map(|block| {
            let (mut re, mut im) = (0.0, 0.0);
            for (n, v) in block.iter().enumerate() {
                let phase = 2.0 * std::f64::consts::PI * freq * n as f64 / 48_000.0;
                re += v * phase.cos();
                im += v * phase.sin();
            }
            2.0 * (re * re + im * im).sqrt() / block.len() as f64
        })
        .collect();
    levels.sort_by(f64::total_cmp);
    levels.get(levels.len() / 2).copied().unwrap_or(0.0)
}

fn print_audio(who: &str, summary: &LatencySummary) {
    match &summary.audio {
        Some(audio) => println!("--- {who} ---\n{audio}"),
        None => println!("--- {who} --- sin audio"),
    }
}

/// (a) y (e): el invitado graba el tono del juego sin huecos y en sincronía con el vídeo durante
/// `seconds`.
fn check_game_audio(guest: &Guest, summary: &LatencySummary, seconds: u64) {
    let audio = audio(summary);
    let game = audio
        .track(AudioTrack::Game)
        .unwrap_or_else(|| panic!("sin audio del juego:\n{summary}"));
    let recording = tape(&guest.speaker);
    let silence = recording.longest_silence(SILENCE_DBFS, true);
    println!(
        "juego: {} tramas reproducidas, mayor racha sin paquete {} tramas, mayor silencio grabado \
         {silence} tramas",
        game.counters.played, game.counters.max_gap_frames
    );
    // 100 tramas de 10 ms por segundo; el 80 % basta (el principio y el final de la sesión).
    let played = 80 * seconds;
    assert!(game.counters.played > 100, "suena el juego: {game:?}");
    timing_assert!(
        game.counters.played > played,
        "{} tramas reproducidas en {seconds} s (cota {played}): {game:?}",
        game.counters.played
    );
    // Hueco = silencio en lo que se oye: como mucho una trama. Las rachas sin paquete que oculta el
    // PLC (sin silencio) pueden llegar a 2 o 3 tramas cuando la VM para un hilo 20-50 ms (el máximo
    // de «captura → recibido» lo enseña): se acotan a 3. Los dos son tiempo de reloj (parones del
    // hilo); que el tono suena lo prueba su nivel.
    timing_assert!(silence <= 1, "hueco de {silence} tramas en la grabación");
    // Las rachas escalan con la carga.
    let max_run = (3.0 * common::load_factor()).ceil() as u32;
    timing_assert!(
        game.counters.max_gap_frames <= max_run,
        "racha de {} tramas sin paquete (cota {max_run}): {game:?}",
        game.counters.max_gap_frames
    );
    assert!(tone_level(&recording, GAME_HZ, 2.0) > 0.15, "el tono suena");
    // Latencia propia del audio: de la marca de captura del anfitrión a la hora en que el
    // altavoz del invitado lo reproduce (`played_at` del dispositivo, latencia del motor incluida).
    // Se compone de la trama que se acumula (10 ms), codificar y enviar, la espera en el buffer de
    // jitter (su margen, el p99 del jitter, más la fase entre la llegada y la petición del
    // altavoz, de 0 a una trama) y la latencia del dispositivo. Medido en el equipo de
    // referencia: mediana de 29 ms (10,7 + 8,6 + 9,9) y p95 de 39 ms; el p95 admite una trama
    // más, la de la fase, que deriva entre los relojes del anfitrión y del invitado. Las cotas
    // escalan con la carga, porque el margen del buffer sale del jitter medido y la carga lo
    // aumenta.
    let total = game.total;
    let (p50, p95) = (total.p50_us as f64 / 1000.0, total.p95_us as f64 / 1000.0);
    println!(
        "juego: captura → suena p50 {p50:.1} ms, p95 {p95:.1} ms ({} tramas)",
        total.samples
    );
    assert!(total.samples > 100, "{total:?}");
    let load = common::load_factor();
    timing_assert!(
        p50 < 40.0 * load,
        "captura → suena p50 {p50:.1} ms (cota {:.1} ms)",
        40.0 * load
    );
    // Si el hilo del altavoz se paró (racha de más de una trama sin paquete), la cola se llena
    // durante el parón y el p95 mide cuánto tarda en recortarse, no el camino del audio.
    if game.counters.max_gap_frames <= 1 {
        timing_assert!(
            p95 < 50.0 * load,
            "captura → suena p95 {p95:.1} ms (cota {:.1} ms)",
            50.0 * load
        );
    }
    let av = audio.av.unwrap_or_else(|| panic!("sin |A−V|:\n{audio}"));
    println!(
        "|A−V|: p50 {:.1} ms, p95 {:.1} ms ({} muestras), ajuste {} ms",
        av.p50_ms, av.p95_ms, av.samples, av.adjust_ms
    );
    assert!(av.samples > 100, "{av:?}");
    // 40 ms en el equipo de referencia, por el factor de carga del entorno. En Windows el
    // planificador de las máquinas virtuales de la CI retrasa la salida hasta una trama de audio
    // (20 ms) más, aunque la codificación vaya a la velocidad de referencia.
    let reference = if cfg!(windows) { 60.0 } else { 40.0 };
    let bound = reference * common::load_factor();
    timing_assert!(
        av.p95_ms < bound,
        "|A−V| p95 {:.1} ms (cota {bound:.1} ms)",
        av.p95_ms
    );
}

#[test]
fn a_game_audio_plays_in_sync_without_gaps() {
    let _serial = serial();
    common::load_factor();
    let server = TestServer::start();
    let mut host = host(server.config(), true, None, false);
    let code = room_code(&host.session);
    let frames = u64::from(FPS) * GAME_SECONDS;
    let mut guest = guest(server.config(), &code, Some(frames), None);
    assert!(guest.session.wait(LIMIT), "{}", guest.session.summary());
    assert_eq!(guest.session.end_reason(), None);
    let summary = guest.session.summary();
    guest.session.close();
    print_audio("invitado", &summary);
    check_game_audio(&guest, &summary, GAME_SECONDS);
    host.session.close();
    let host_summary = host.session.summary();
    print_audio("anfitrión", &host_summary);
    let sent = audio(&host_summary)
        .track(AudioTrack::Game)
        .map_or(0, |game| game.counters.sent);
    assert!(sent > 0, "el anfitrión envía el juego\n{host_summary}");
    timing_assert!(
        sent > 80 * GAME_SECONDS,
        "{sent} tramas del juego enviadas en {GAME_SECONDS} s"
    );
    drop(server);
}

/// (b) y (e): voz de un invitado con permiso al anfitrión y al otro invitado.
fn voice_round(server: &TestServer, relay: bool, permitted: bool) {
    let mut host_config = server.config();
    host_config.force_relay = relay;
    let mut host = host(host_config, false, Some(HOST_VOICE_HZ), true);
    let code = room_code(&host.session);
    let mut speaker_account = server.config();
    speaker_account.force_relay = relay;
    let mut listener_account = server.config();
    listener_account.force_relay = relay;
    let speaker = guest(speaker_account, &code, None, Some(GUEST_VOICE_HZ));
    let listener = guest(listener_account, &code, None, None);
    allow_voice(&mut host.session, 2, true);
    if !permitted {
        let id = host
            .session
            .guests()
            .iter()
            .find(|status| status.fingerprint == speaker.session.fingerprint())
            .map(|status| status.guest)
            .expect("el que habla");
        host.session
            .set_permissions(id, Permissions::VIEW_ONLY)
            .expect("sin voz");
    }
    let changed = Instant::now();
    let host_tape = host.speaker.as_ref().expect("salida del anfitrión");
    let level = |recording: &Recording, freq| tone_level(recording, freq, 2.0);
    // El tono de cada uno llega a unos 0,3 de pico; el que no debería estar, a ruido de fondo.
    const HEARD: f64 = 0.1;
    const ABSENT: f64 = 0.01;
    // Se espera a oír lo que tiene que oírse (la mediana de los bloques de los últimos 2 s: al
    // menos 1 s de tono) y, con permiso, a que el que habla sepa que habla el anfitrión y el
    // anfitrión que habla el invitado. Sin permiso, además, a que los 2 s que se miden sean todos
    // posteriores al cambio de permiso (la voz salió permitida hasta entonces). Se comprueba lo
    // mismo que se miró al salir de la espera.
    let mut taken = None;
    poll_until(
        "que la voz llegue",
        WAIT,
        Duration::from_millis(250),
        || {
            let at_host = tail(host_tape, 2);
            let at_speaker = tail(&speaker.speaker, 2);
            let at_listener = tail(&listener.speaker, 2);
            let speaker_summary = speaker.session.summary();
            let heard = level(&at_listener, HOST_VOICE_HZ) > HEARD
                && level(&at_speaker, HOST_VOICE_HZ) > HEARD;
            let ready = if permitted {
                heard
                    && level(&at_host, GUEST_VOICE_HZ) > HEARD
                    && level(&at_listener, GUEST_VOICE_HZ) > HEARD
                    && speaker_summary
                        .audio
                        .as_ref()
                        .is_some_and(|audio| audio.speaking.iter().any(|who| who == "anfitrión"))
                    && host.session.audio().talkers().iter().any(|who| *who != 0)
            } else {
                heard && changed.elapsed() >= Duration::from_millis(2_500)
            };
            taken = Some((at_host, at_speaker, at_listener, speaker_summary));
            ready
        },
    );
    let (at_host, at_speaker, at_listener, speaker_summary) = taken.expect("lo medido");
    let host_summary = host.session.summary();
    let listener_summary = listener.session.summary();
    println!(
        "relay {relay}, permiso {permitted}: anfitrión oye 660 Hz a {:.3}; el que habla oye 550 Hz \
         a {:.3} y 660 Hz a {:.3}; el otro oye 550 Hz a {:.3} y 660 Hz a {:.3}",
        level(&at_host, GUEST_VOICE_HZ),
        level(&at_speaker, HOST_VOICE_HZ),
        level(&at_speaker, GUEST_VOICE_HZ),
        level(&at_listener, HOST_VOICE_HZ),
        level(&at_listener, GUEST_VOICE_HZ),
    );
    print_audio("anfitrión", &host_summary);
    print_audio("invitado que habla", &speaker_summary);
    print_audio("invitado que escucha", &listener_summary);
    // El anfitrión siempre llega al que escucha (tiene permiso) y nadie se oye a sí mismo.
    assert!(
        level(&at_listener, HOST_VOICE_HZ) > HEARD,
        "el otro oye al anfitrión"
    );
    assert!(
        level(&at_speaker, GUEST_VOICE_HZ) < ABSENT,
        "nadie se oye a sí mismo"
    );
    if permitted {
        assert!(
            level(&at_host, GUEST_VOICE_HZ) > HEARD,
            "el anfitrión oye al invitado"
        );
        assert!(
            level(&at_listener, GUEST_VOICE_HZ) > HEARD,
            "el otro invitado oye al primero (reenviado)"
        );
        assert!(
            level(&at_speaker, HOST_VOICE_HZ) > HEARD,
            "el que habla oye al anfitrión"
        );
        let talkers = speaker_summary
            .audio
            .as_ref()
            .map(|audio| audio.speaking.clone())
            .unwrap_or_default();
        assert!(talkers.contains(&"anfitrión".to_owned()), "{talkers:?}");
        assert!(
            host.session.audio().talkers().iter().any(|who| *who != 0),
            "el anfitrión sabe que el invitado habla"
        );
    } else {
        assert!(
            level(&at_host, GUEST_VOICE_HZ) < ABSENT,
            "sin permiso no llega nada"
        );
        assert!(
            level(&at_listener, GUEST_VOICE_HZ) < ABSENT,
            "ni se reenvía"
        );
        assert!(
            level(&at_speaker, HOST_VOICE_HZ) > HEARD,
            "pero oye a la sala igual: el permiso es para hablar"
        );
        assert!(!speaker.session.audio().voice_allowed());
    }
    let (mut speaker, mut listener) = (speaker, listener);
    speaker.session.close();
    listener.session.close();
    host.session.close();
}

#[test]
fn b_voice_reaches_permitted_members_and_mixes() {
    let _serial = serial();
    let server = TestServer::start();
    voice_round(&server, false, true);
    voice_round(&server, false, false);
    drop(server);
}

#[test]
fn c_lossy_link_conceals_audio_losses() {
    let _serial = serial();
    let server = TestServer::start();
    // Como en `tests/remote.rs`: el anfitrión anuncia solo el proxy y el invitado nada, así que la
    // conexión es invitado → proxy (2 %, 10 ± 5 ms, en los dos sentidos) → anfitrión.
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
    let mut host = host(host_config, true, None, true);
    let code = room_code(&host.session);
    let mut guest_config = server.config();
    guest_config.advertise_candidates = Some(Vec::new());
    let mut guest = guest(guest_config, &code, None, Some(GUEST_VOICE_HZ));
    allow_voice(&mut host.session, 1, true);
    // Con un 2 % de pérdida, en unos segundos hay tramas ocultadas en las dos pistas y alguna
    // recuperada por la FEC en banda o DRED; se espera a verlo (y al menos 3 s, para que los huecos
    // y el silencio se midan sobre algo).
    let started = Instant::now();
    let counters = |summary: &LatencySummary, track| {
        summary
            .audio
            .as_ref()
            .and_then(|audio| audio.track(track))
            .map(|track| track.counters)
    };
    poll_until(
        "pérdidas ocultadas y recuperadas",
        WAIT,
        Duration::from_millis(250),
        || {
            let game = counters(&guest.session.summary(), AudioTrack::Game);
            let voice = counters(&host.session.summary(), AudioTrack::Voice);
            started.elapsed() >= Duration::from_secs(3)
                && proxy.stats().dropped() > 0
                && game.is_some_and(|game| game.concealed > 0)
                && voice.is_some_and(|voice| {
                    voice.concealed > 0 && voice.recovered_fec + voice.recovered_dred > 0
                })
        },
    );
    let guest_summary = guest.session.summary();
    let host_summary = host.session.summary();
    println!(
        "proxy: {} reenviados, {} perdidos",
        proxy.stats().forwarded(),
        proxy.stats().dropped()
    );
    print_audio("invitado", &guest_summary);
    print_audio("anfitrión", &host_summary);
    assert!(proxy.stats().dropped() > 0, "el proxy perdió datagramas");
    let game = audio(&guest_summary)
        .track(AudioTrack::Game)
        .map(|track| track.counters)
        .expect("juego");
    let voice = audio(&host_summary)
        .track(AudioTrack::Voice)
        .map(|track| track.counters)
        .expect("voz");
    println!(
        "juego: pérdida {:.1} %, {} ocultadas, racha máxima {} tramas · voz: pérdida {:.1} %, {} \
         ocultadas ({} FEC, {} DRED), racha máxima {} tramas",
        game.loss_percent(),
        game.concealed,
        game.max_gap_frames,
        voice.loss_percent(),
        voice.concealed,
        voice.recovered_fec,
        voice.recovered_dred,
        voice.max_gap_frames
    );
    assert!(
        game.concealed > 0 && voice.concealed > 0,
        "hubo pérdidas que ocultar"
    );
    assert!(
        voice.recovered_fec + voice.recovered_dred > 0,
        "la FEC en banda o DRED recuperó tramas de voz"
    );
    // 60 ms = 6 tramas de 10 ms.
    timing_assert!(
        game.max_gap_frames <= 6,
        "racha de {} tramas en el juego: {game:?}",
        game.max_gap_frames
    );
    timing_assert!(
        voice.max_gap_frames <= 6,
        "racha de {} tramas en la voz: {voice:?}",
        voice.max_gap_frames
    );
    let silence = tape(&guest.speaker).longest_silence(SILENCE_DBFS, true);
    timing_assert!(silence <= 6, "silencio de {silence} tramas en el juego");
    guest.session.close();
    host.session.close();
    drop(server);
}

#[test]
fn d_mouth_to_ear_stays_under_80_ms() {
    let _serial = serial();
    let server = TestServer::start();
    let mut host = host(server.config(), false, None, true);
    let code = room_code(&host.session);
    let mut guest = guest(server.config(), &code, None, Some(GUEST_VOICE_HZ));
    allow_voice(&mut host.session, 1, true);
    // 300 tramas de voz (3 s) bastan para el p95.
    poll_until(
        "300 tramas de voz en el anfitrión",
        WAIT,
        Duration::from_millis(250),
        || {
            host.session
                .summary()
                .audio
                .as_ref()
                .and_then(|audio| audio.track(AudioTrack::Voice))
                .is_some_and(|voice| voice.total.samples > 300)
        },
    );
    let host_summary = host.session.summary();
    let guest_summary = guest.session.summary();
    let heard = audio(&host_summary);
    let voice = heard.track(AudioTrack::Voice).expect("voz en el anfitrión");
    let sent = audio(&guest_summary);
    let sender = sent.track(AudioTrack::Voice).expect("voz en el invitado");
    println!("Desglose del emisor (invitado):");
    for span in &sender.spans {
        println!(
            "  {:<26} p50 {:>6.2} ms · p95 {:>6.2} ms",
            format!("{} → {}", span.from.label(), span.to.label()),
            span.p50_us as f64 / 1000.0,
            span.p95_us as f64 / 1000.0
        );
    }
    println!("Desglose del receptor (anfitrión):");
    for span in &voice.spans {
        println!(
            "  {:<26} p50 {:>6.2} ms · p95 {:>6.2} ms",
            format!("{} → {}", span.from.label(), span.to.label()),
            span.p50_us as f64 / 1000.0,
            span.p95_us as f64 / 1000.0
        );
    }
    let (p50, p95) = (
        voice.total.p50_us as f64 / 1000.0,
        voice.total.p95_us as f64 / 1000.0,
    );
    println!(
        "boca-oído: p50 {p50:.1} ms · p95 {p95:.1} ms ({} tramas)",
        voice.total.samples
    );
    assert!(voice.total.samples > 300, "{voice:?}");
    timing_assert!(p95 < 80.0, "boca-oído p95 {p95:.1} ms");
    guest.session.close();
    host.session.close();
    drop(server);
}

#[test]
fn e_relay_carries_game_audio_and_voice() {
    let _serial = serial();
    let server = TestServer::start();
    {
        let mut host_config = server.config();
        host_config.force_relay = true;
        let mut host = host(host_config, true, None, false);
        let code = room_code(&host.session);
        let mut config = server.config();
        config.force_relay = true;
        let mut guest = guest(config, &code, Some(u64::from(FPS) * GAME_SECONDS), None);
        assert!(guest.session.wait(LIMIT), "{}", guest.session.summary());
        assert_eq!(guest.session.end_reason(), None);
        let summary = guest.session.summary();
        guest.session.close();
        assert_eq!(
            summary.link.as_ref().map(|link| link.path.as_str()),
            Some("relay")
        );
        print_audio("invitado (relay)", &summary);
        check_game_audio(&guest, &summary, GAME_SECONDS);
        host.session.close();
    }
    voice_round(&server, true, true);
    voice_round(&server, true, false);
    drop(server);
}

/// (f) Con la sala abierta: el anfitrión cambia de micrófono (otro tono) y baja el volumen de su
/// voz, y el invitado cambia de salida; nadie reconecta y el sonido sigue llegando. Cada cambio se
/// espera mirando lo grabado (con un plazo amplio: en un runner lento, el buffer de jitter tarda
/// más en vaciarse de lo de antes).
#[test]
fn f_devices_and_the_voice_volume_change_live() {
    const OTHER_MIC_HZ: f64 = 770.0;
    const HEARD: f64 = 0.1;
    const ABSENT: f64 = 0.01;
    // Solo la cola de la grabación ([`tail`]), y sin prisa.
    let wait = |what: &str, condition: Box<dyn FnMut() -> bool + '_>| {
        poll_until(what, WAIT, Duration::from_millis(250), condition);
    };
    let _serial = serial();
    let server = TestServer::start();
    let mut host = host(server.config(), false, Some(HOST_VOICE_HZ), false);
    let code = room_code(&host.session);
    let mut listener = guest(server.config(), &code, None, None);
    allow_voice(&mut host.session, 1, true);
    let level = |tape_: &Tape, freq| tone_level(&tail(tape_, 1), freq, 1.0);
    wait(
        "la voz del anfitrión",
        Box::new(|| level(&listener.speaker, HOST_VOICE_HZ) > HEARD),
    );
    // Otro micrófono en marcha: llega el nuevo y deja de llegar el anterior.
    assert_eq!(
        host.session
            .audio()
            .replace_mic(Box::new(ToneSource::new(OTHER_MIC_HZ, 1))),
        Ok(()),
        "lo adopta el hilo de la voz"
    );
    wait(
        "el micrófono nuevo",
        Box::new(|| {
            level(&listener.speaker, OTHER_MIC_HZ) > HEARD
                && level(&listener.speaker, HOST_VOICE_HZ) < ABSENT
        }),
    );
    // El control automático de ganancia del procesado se adapta al micrófono nuevo durante unos
    // segundos: la referencia se toma cuando el nivel deja de moverse, no al primer oírlo.
    let mut full = level(&listener.speaker, OTHER_MIC_HZ);
    wait(
        "que se asiente el micrófono nuevo",
        Box::new(|| {
            std::thread::sleep(Duration::from_millis(250));
            let now = level(&listener.speaker, OTHER_MIC_HZ);
            let settled = (now / full - 1.0).abs() < 0.05;
            full = now;
            settled
        }),
    );
    println!("tras cambiar de micrófono: {OTHER_MIC_HZ} Hz a {full:.3}");
    // La voz a la mitad: llega a la mitad.
    host.session.audio().set_mic_gain(0.5);
    wait(
        "que baje la voz",
        Box::new(|| level(&listener.speaker, OTHER_MIC_HZ) / full < 0.65),
    );
    // Lo que queda en la ventana de antes del cambio sale en un segundo: se espera a que el nivel
    // quede en la mitad dos veces seguidas (no un bloque de paso entre los dos niveles).
    let mut half = 0.0;
    let mut in_range = 0;
    wait(
        "la voz a la mitad",
        Box::new(|| {
            half = level(&listener.speaker, OTHER_MIC_HZ);
            if (0.35..0.65).contains(&(half / full)) {
                in_range += 1;
            } else {
                in_range = 0;
            }
            in_range >= 2
        }),
    );
    println!("a la mitad: {half:.3} (antes {full:.3})");
    // El invitado cambia de salida: lo nuevo suena en la nueva.
    let audio = listener.session.audio();
    let format = audio.speaker_format().expect("formato de la salida en uso");
    assert_eq!(format.channels, 2);
    let sink = CaptureSink::new(2);
    let new_tape = sink.recording();
    assert_eq!(audio.replace_speaker(Box::new(sink)), Ok(()), "abre");
    wait(
        "la salida nueva",
        Box::new(|| level(&new_tape, OTHER_MIC_HZ) > HEARD / 2.0),
    );
    println!("en la salida nueva: {:.3}", level(&new_tape, OTHER_MIC_HZ));
    assert_eq!(listener.session.end_reason(), None, "sin reconectar");
    listener.session.close();
    host.session.close();
    drop(server);
}
