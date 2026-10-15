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
//! sonará la primera muestra menos la hora de captura de esa muestra. Incluye lo que tarda en
//! completarse la trama (10 ms: la fuente la entrega cuando tiene sus 480 muestras), la
//! codificación y la decodificación, la espera en el buffer de jitter hasta que el dispositivo la
//! pide (el desfase entre los dos relojes y la cola que mantiene su objetivo) y la latencia del
//! dispositivo sintético. La red de la sesión (en `127.0.0.1`, décimas de ms) la miden los tests
//! de `apps/desktop/tests/audio.rs` con todo el camino.
//!
//! El presupuesto es 30-45 ms: la cota es el p95 ≤ 45 ms y el suelo, lo que no se puede quitar
//! (la trama más el dispositivo: 20 ms), comprueba que se mide de verdad.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use coroom_audio::codec::{OpusDecoder, OpusEncoder, Profile};
use coroom_audio::jitter::{JitterBuffer, Pop};
use coroom_audio::mix::Mixer;
use coroom_audio::synthetic::{CaptureSink, ToneSource};
use coroom_audio::{
    AudioConfig, AudioFormat, AudioSink, AudioSource, DEVICE_LATENCY, FRAME_DURATION,
    MAX_FRAME_SAMPLES,
};

/// Tramas que se capturan en cada medida (2,5 s).
const FRAMES: u32 = 250;
/// Tramas del principio que no cuentan (cebado del buffer y arranque de los hilos).
const WARMUP: usize = 50;

/// Un paquete codificado camino del buffer de jitter (por valor, como en la sesión).
struct Packet {
    seq: u32,
    capture_us: u32,
    arrived: Instant,
    len: usize,
    data: [u8; 1500],
}

/// Lo que mide el dispositivo: (capturada, suena) de cada trama del juego que sonó.
type Heard = Arc<Mutex<Vec<(Instant, Instant)>>>;

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let rank = ((p / 100.0 * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

/// Captura → mezcla → reproducción: los milisegundos `suena − capturada` de cada trama, con el
/// dispositivo arrancando `phase` después de la fuente (el desfase entre sus relojes, que en un
/// equipo real es cualquiera).
fn measure(phase: Duration) -> Vec<f64> {
    let origin = Instant::now();
    let (tx, rx) = mpsc::sync_channel::<Packet>(64);
    // Captura: el tono del juego a ritmo real, codificado al momento.
    let capture = std::thread::spawn(move || {
        let mut source = ToneSource::new(440.0, 2);
        source.start(&AudioConfig::game()).expect("fuente");
        let mut encoder = OpusEncoder::new(Profile::Game).expect("codificador");
        let mut frame = Vec::with_capacity(MAX_FRAME_SAMPLES);
        for seq in 0..FRAMES {
            let captured = source.read(&mut frame).expect("trama");
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
    });

    // Reproducción a demanda: buffer de jitter, decodificador y mezcla en el hilo del dispositivo.
    let heard: Heard = Arc::default();
    let mut sink = CaptureSink::new(2);
    let recording = sink.recording();
    let mut jitter = JitterBuffer::new();
    let mut game = OpusDecoder::new(Profile::Game).expect("decodificador");
    let mut voice = OpusDecoder::new(Profile::Voice).expect("decodificador de voz");
    let mut mixer = Mixer::new(2);
    let mut pcm = [0i16; MAX_FRAME_SAMPLES * 2];
    let mut voice_pcm = [0i16; MAX_FRAME_SAMPLES];
    let device = Arc::clone(&heard);
    std::thread::sleep(phase);
    let render = Box::new(move |out: &mut [i16], played_at: Instant| {
        while let Ok(packet) = rx.try_recv() {
            jitter.push(
                packet.seq,
                packet.capture_us,
                0,
                &packet.data[..packet.len],
                packet.arrived,
            );
        }
        mixer.begin();
        let decided = Instant::now();
        let captured = match jitter.pop(decided) {
            Pop::Frame(frame) => {
                let samples = game
                    .decode_frames(Some(frame.data), 1, &mut pcm)
                    .expect("decodificar");
                mixer.add(0, &pcm[..samples], 2);
                Some(origin + Duration::from_micros(u64::from(frame.capture_us)))
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
        if let Some(captured) = captured {
            device.lock().expect("medidas").push((captured, played_at));
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
    assert!(
        recording.lock().expect("grabación").underruns == 0,
        "el dispositivo sintético no se quedó sin audio"
    );
    let heard = heard.lock().expect("medidas");
    heard
        .iter()
        .skip(WARMUP)
        .map(|(captured, played)| played.duration_since(*captured).as_secs_f64() * 1000.0)
        .collect()
}

#[test]
fn capture_to_ear_stays_within_the_audio_budget() {
    let floor = (FRAME_DURATION + DEVICE_LATENCY).as_secs_f64() * 1000.0;
    // El desfase entre la captura y el dispositivo va de 0 a una trama: al principio, a la mitad
    // y casi al final (el peor caso).
    for phase_ms in [0, 5, 9] {
        let mut latencies = measure(Duration::from_millis(phase_ms));
        assert!(
            latencies.len() as u32 > FRAMES * 9 / 10 - WARMUP as u32,
            "sonaron {} tramas de {FRAMES}",
            latencies.len()
        );
        latencies.sort_by(f64::total_cmp);
        let (p50, p95, max) = (
            percentile(&latencies, 50.0),
            percentile(&latencies, 95.0),
            latencies[latencies.len() - 1],
        );
        println!(
            "desfase {phase_ms} ms: captura → suena p50 {p50:.1} ms · p95 {p95:.1} ms · máx \
             {max:.1} ms ({} tramas; suelo {floor:.0} ms)",
            latencies.len()
        );
        assert!(
            latencies[0] >= floor - 0.5,
            "por debajo del suelo: {latencies:?}"
        );
        assert!(
            p95 <= 45.0,
            "desfase {phase_ms} ms: p95 {p95:.1} ms > 45 ms"
        );
    }
}
