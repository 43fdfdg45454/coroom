//! Latencia propia del audio, de la captura a lo que se oye, con las piezas reales de
//! `coroom-audio` y los dispositivos sintéticos (sin red ni cifrado):
//!
//! [`ToneSource`] (a ritmo real, marca exacta de la primera muestra de cada trama) → Opus
//! (`Profile::Game`) → [`JitterBuffer`] → Opus → [`Mixer`] (juego y una voz) → [`CaptureSink`] a
//! demanda (un hilo que hace de dispositivo, pide cada trama al empezar su hueco de 10 ms y la oye
//! su latencia de dispositivo, 10 ms, después: como la salida de PipeWire con
//! `node.latency = 480/48000`).
//!
//! Lo que se mide, trama a trama, es `suena − capturada`: la hora a la que el sumidero dice que
//! sonará la primera muestra menos la hora de captura de esa muestra, desglosado por etapas:
//! captura → recibida (los 10 ms que tarda en completarse la trama, porque la fuente la entrega
//! cuando tiene sus 480 muestras, más la codificación), recibida → decidida (la espera en el
//! buffer de jitter hasta que el dispositivo la pide: el margen del jitter medido más el desfase
//! entre los dos relojes, de 0 a una trama) y decidida → suena (la latencia del dispositivo
//! sintético). La red de la sesión (en `127.0.0.1`, décimas de ms) la miden los tests de
//! `apps/desktop/tests/audio.rs` con todo el camino.
//!
//! **Lo que se mide es el pipeline, no el planificador.** Los dos hilos (la captura y el
//! dispositivo) piden prioridad de tiempo real, como los de verdad, y anotan cuánto tarde
//! despertaron de cada sueño; si alguno llegó más de media trama tarde ([`STALL`]), esa medida es
//! del planificador de la máquina (una VM que para un hilo 20 ms encola tres tramas, que el
//! buffer tarda un rato en recortar) y se repite, hasta [`ATTEMPTS`] veces. Las primeras
//! [`WARMUP`] tramas no cuentan: la trama de margen con la que arranca el buffer de jitter se
//! recorta al pasar su ventana de un segundo.
//!
//! El presupuesto: la cota es el p95 ≤ [`BUDGET_MS`] y el suelo, lo que no se puede quitar (la
//! trama más el dispositivo: 20 ms), comprueba que se mide de verdad.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use coroom_audio::codec::{OpusDecoder, OpusEncoder, Profile};
use coroom_audio::jitter::{JitterBuffer, JitterStats, Pop};
use coroom_audio::mix::Mixer;
use coroom_audio::synthetic::{CaptureSink, ToneSource};
use coroom_audio::{
    AudioConfig, AudioFormat, AudioSink, AudioSource, DEVICE_LATENCY, FRAME_DURATION,
    MAX_FRAME_SAMPLES,
};

/// Tramas que se capturan en cada medida (4,5 s).
const FRAMES: u32 = 450;
/// Tramas del principio que no cuentan (1,5 s): el cebado, la ventana de recorte del buffer de
/// jitter (1 s) y un periodo de ajuste.
const WARMUP: usize = 150;
/// Veces que se repite una medida que el planificador estropeó (en una máquina de CI cargada,
/// tres seguidas no son raras).
const ATTEMPTS: usize = 5;
/// Retraso de un hilo al despertar a partir del cual la medida es del planificador: media trama.
const STALL: Duration = Duration::from_millis(5);
/// Cota del p95 de captura → suena, en ms. Medido en el equipo de referencia: 30,4/25,8/29,9 ms
/// (p50 = p95) para los desfases 0/5/9 ms; el peor, 30, es el suelo de 20 ms más la codificación
/// y un desfase de una trama entera (las llegadas y las peticiones del dispositivo coinciden: hace
/// falta una trama de cola). El margen hasta la cota es lo que puede llegar a retrasar un hilo sin
/// que la medida se repita ([`STALL`]).
const BUDGET_MS: f64 = 36.0;

/// Un paquete codificado camino del buffer de jitter (por valor, como en la sesión).
struct Packet {
    seq: u32,
    capture_us: u32,
    arrived: Instant,
    len: usize,
    data: [u8; 1500],
}

/// Una trama del juego que sonó: capturada, recibida (codificada), decidida (sacada del buffer y
/// decodificada) y cuándo suena.
#[derive(Clone, Copy)]
struct Sample {
    captured: Instant,
    arrived: Instant,
    decided: Instant,
    played: Instant,
}

/// Lo que mide el dispositivo.
type Heard = Arc<Mutex<Vec<Sample>>>;

/// Una medida: las tramas que sonaron, lo más tarde que despertó cada hilo y los contadores del
/// buffer de jitter al final.
struct Run {
    samples: Vec<Sample>,
    capture_late: Duration,
    device_late: Duration,
    device_underruns: u64,
    jitter: JitterStats,
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let rank = ((p / 100.0 * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Anota en `max` (µs) un retraso si supera el guardado.
fn note_late(max: &AtomicU64, late: Duration) {
    max.fetch_max(late.as_micros() as u64, Ordering::Relaxed);
}

/// Captura → mezcla → reproducción con el dispositivo arrancando `phase` después de la fuente (el
/// desfase entre sus relojes, que en un equipo real es cualquiera).
fn measure(phase: Duration) -> Run {
    let origin = Instant::now();
    let (tx, rx) = mpsc::sync_channel::<Packet>(64);
    let capture_late = Arc::new(AtomicU64::new(0));
    // Captura: el tono del juego a ritmo real, codificado al momento.
    let capture = {
        let late = Arc::clone(&capture_late);
        std::thread::spawn(move || {
            let _ = coroom_audio::realtime::promote_current_thread();
            let mut source = ToneSource::new(440.0, 2);
            source.start(&AudioConfig::game()).expect("fuente");
            let mut encoder = OpusEncoder::new(Profile::Game).expect("codificador");
            let mut frame = Vec::with_capacity(MAX_FRAME_SAMPLES);
            for seq in 0..FRAMES {
                let captured = source.read(&mut frame).expect("trama");
                // La trama está lista una trama después de su primera muestra: lo que pase de ahí
                // es lo que tardó este hilo en despertar. Solo cuenta en régimen: el
                // calentamiento se descarta también de la medida.
                if seq as usize >= WARMUP {
                    let ready = captured.captured_at + FRAME_DURATION;
                    note_late(&late, Instant::now().saturating_duration_since(ready));
                }
                let mut packet = Packet {
                    seq,
                    capture_us: captured.captured_at.duration_since(origin).as_micros() as u32,
                    arrived: captured.captured_at,
                    len: 0,
                    data: [0; 1500],
                };
                packet.len = encoder
                    .encode(&frame[..captured.samples], &mut packet.data)
                    .expect("codificar");
                packet.arrived = Instant::now();
                if tx.send(packet).is_err() {
                    break;
                }
            }
        })
    };

    // Reproducción a demanda: buffer de jitter, decodificador y mezcla en el hilo del dispositivo.
    let heard: Heard = Arc::default();
    let jitter_stats: Arc<Mutex<JitterStats>> = Arc::default();
    let device_late = Arc::new(AtomicU64::new(0));
    let mut sink = CaptureSink::new(2);
    let recording = sink.recording();
    let mut jitter = JitterBuffer::new();
    let mut game = OpusDecoder::new(Profile::Game).expect("decodificador");
    let mut voice = OpusDecoder::new(Profile::Voice).expect("decodificador de voz");
    let mut mixer = Mixer::new(2);
    let mut pcm = [0i16; MAX_FRAME_SAMPLES * 2];
    let mut voice_pcm = [0i16; MAX_FRAME_SAMPLES];
    let device = Arc::clone(&heard);
    let stats = Arc::clone(&jitter_stats);
    let late = Arc::clone(&device_late);
    let mut promoted = false;
    let mut pops = 0usize;
    std::thread::sleep(phase);
    let render = Box::new(move |out: &mut [i16], played_at: Instant| {
        // La primera llamada sube el hilo del dispositivo sintético a tiempo real, como el de un
        // backend real; lo que tarde eso (una ida y vuelta por el bus si hay que pedirlo al
        // portal) no es un retraso del dispositivo.
        let promoting = !promoted;
        if promoting {
            promoted = true;
            let _ = coroom_audio::realtime::promote_current_thread();
        }
        while let Ok(packet) = rx.try_recv() {
            // La última trama avisa de que el emisor calla (DTX): que la cola se vacíe después
            // no es una pérdida.
            let flags = if packet.seq == FRAMES - 1 {
                coroom_audio::jitter::DTX
            } else {
                0
            };
            jitter.push(
                packet.seq,
                packet.capture_us,
                flags,
                &packet.data[..packet.len],
                packet.arrived,
            );
        }
        mixer.begin();
        let decided = Instant::now();
        // El hueco de esta trama empezó una latencia de dispositivo antes de que suene: lo que
        // pase de ahí es lo que tardó el hilo del dispositivo en despertar. Solo en régimen (ni
        // la llamada que promueve el hilo ni el calentamiento).
        pops += 1;
        if !promoting && pops > WARMUP {
            note_late(
                &late,
                decided.saturating_duration_since(played_at - DEVICE_LATENCY),
            );
        }
        let frame = match jitter.pop(decided) {
            Pop::Frame(frame) => {
                let samples = game
                    .decode_frames(Some(frame.data), 1, &mut pcm)
                    .expect("decodificar");
                mixer.add(0, &pcm[..samples], 2);
                Some((
                    origin + Duration::from_micros(u64::from(frame.capture_us)),
                    frame.arrived,
                ))
            }
            Pop::Plc(_) => {
                let samples = game.decode(None, &mut pcm).expect("PLC");
                mixer.add(0, &pcm[..samples], 2);
                None
            }
            Pop::Wait => None,
        };
        // Una voz en silencio (PLC del decodificador mono) en la mezcla, como en la sala.
        if let Ok(samples) = voice.decode(None, &mut voice_pcm) {
            mixer.add(1, &voice_pcm[..samples], 1);
        }
        mixer.finish(out);
        *stats.lock().expect("contadores") = jitter.stats();
        if let Some((captured, arrived)) = frame {
            device.lock().expect("medidas").push(Sample {
                captured,
                arrived,
                decided,
                played: played_at,
            });
        }
    });
    sink.start_pull(
        &AudioConfig {
            format: AudioFormat::GAME,
            device: None,
            latency: DEVICE_LATENCY,
        },
        render,
    )
    .expect("dispositivo");
    capture.join().expect("captura");
    // Lo que queda en el buffer termina de sonar.
    std::thread::sleep(FRAME_DURATION * 10);
    sink.stop();
    let samples = heard.lock().expect("medidas").clone();
    Run {
        samples,
        capture_late: Duration::from_micros(capture_late.load(Ordering::Relaxed)),
        device_late: Duration::from_micros(device_late.load(Ordering::Relaxed)),
        device_underruns: recording.lock().expect("grabación").underruns,
        jitter: *jitter_stats.lock().expect("contadores"),
    }
}

/// Mide con el desfase `phase` hasta que el planificador deje una medida limpia.
fn clean_measure(phase_ms: u64) -> Run {
    let mut last = None;
    for attempt in 1..=ATTEMPTS {
        let run = measure(Duration::from_millis(phase_ms));
        let stall = run.capture_late.max(run.device_late);
        if stall <= STALL && run.device_underruns == 0 {
            return run;
        }
        println!(
            "desfase {phase_ms} ms, intento {attempt}: la captura despertó hasta {:.1} ms tarde y \
             el dispositivo {:.1} ms ({} faltas): es el planificador, se repite",
            ms(run.capture_late),
            ms(run.device_late),
            run.device_underruns
        );
        last = Some(run);
    }
    let run = last.expect("al menos un intento");
    panic!(
        "la máquina no deja medir: en {ATTEMPTS} intentos algún hilo despertó más de {} ms tarde \
         (captura {:.1} ms, dispositivo {:.1} ms, {} faltas del dispositivo)",
        ms(STALL),
        ms(run.capture_late),
        ms(run.device_late),
        run.device_underruns
    );
}

/// p50 y p95 de una etapa, en ms.
fn stage(samples: &[Sample], pick: impl Fn(&Sample) -> Duration) -> (f64, f64) {
    let mut values: Vec<f64> = samples.iter().map(|s| ms(pick(s))).collect();
    values.sort_by(f64::total_cmp);
    (percentile(&values, 50.0), percentile(&values, 95.0))
}

#[test]
fn capture_to_ear_stays_within_the_audio_budget() {
    let floor = ms(FRAME_DURATION + DEVICE_LATENCY);
    // El desfase entre la captura y el dispositivo va de 0 a una trama: al principio, a la mitad
    // y casi al final.
    for phase_ms in [0, 5, 9] {
        let run = clean_measure(phase_ms);
        let samples = &run.samples[WARMUP.min(run.samples.len())..];
        assert!(
            samples.len() as u32 > FRAMES * 9 / 10 - WARMUP as u32,
            "sonaron {} tramas de {FRAMES}",
            run.samples.len()
        );
        let mut latencies: Vec<f64> = samples
            .iter()
            .map(|s| ms(s.played.duration_since(s.captured)))
            .collect();
        latencies.sort_by(f64::total_cmp);
        let (p50, p95, max) = (
            percentile(&latencies, 50.0),
            percentile(&latencies, 95.0),
            latencies[latencies.len() - 1],
        );
        let received = stage(samples, |s| s.arrived.saturating_duration_since(s.captured));
        let decided = stage(samples, |s| s.decided.saturating_duration_since(s.arrived));
        let played = stage(samples, |s| s.played.saturating_duration_since(s.decided));
        println!(
            "desfase {phase_ms} ms: captura → suena p50 {p50:.1} ms · p95 {p95:.1} ms · máx \
             {max:.1} ms ({} tramas; suelo {floor:.0} ms)\n  captura → recibida p50 {:.1} · p95 \
             {:.1} ms; recibida → decidida p50 {:.1} · p95 {:.1} ms; decidida → suena p50 {:.1} · \
             p95 {:.1} ms\n  hilos: captura hasta {:.2} ms tarde, dispositivo hasta {:.2} ms; \
             buffer de jitter: objetivo {:.1} ms, jitter p99 {:.2} ms, {} recortes, {} \
             estiramientos, {} ocultadas",
            samples.len(),
            received.0,
            received.1,
            decided.0,
            decided.1,
            played.0,
            played.1,
            ms(run.capture_late),
            ms(run.device_late),
            run.jitter.target_ms,
            run.jitter.jitter_p99_ms,
            run.jitter.trimmed,
            run.jitter.stretched,
            run.jitter.concealed,
        );
        assert!(
            latencies[0] >= floor - 0.5,
            "por debajo del suelo: {latencies:?}"
        );
        assert!(
            p95 <= BUDGET_MS,
            "desfase {phase_ms} ms: p95 {p95:.1} ms > {BUDGET_MS} ms"
        );
        // En régimen el buffer no inserta ni quita nada: el margen del arranque se recortó una
        // vez y el resto del tiempo el adelanto se mantuvo en su banda.
        assert!(
            run.jitter.concealed == 0 && run.jitter.stretched <= 1 && run.jitter.trimmed <= 3,
            "{:?}",
            run.jitter
        );
    }
}
