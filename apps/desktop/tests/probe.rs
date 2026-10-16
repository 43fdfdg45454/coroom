//! Prueba de latencia en proceso: fuente sintética 1280×720 a 60 fps, 120 fotogramas, presentación
//! nula, H.264 (OpenH264). Comprueba que se presentan los 120 fotogramas y que todo fotograma
//! presentado tiene las ocho marcas; que ≥ 95 % de lo codificado llega a la pantalla y que el p95
//! total es < 150 ms son cotas de tiempo (`timing_assert!`: avisan sin hacer fallar la prueba).
//! Es una cota de regresión holgada para los runners de CI; la puerta real
//! (< 25 ms en el p95 a 1080p60) se mide a mano en el equipo de referencia con
//! `cargo run --release -p coroom -- probe --headless --frames 600`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use coroom_desktop::probe::{
    PresenterSink, ProbeOptions, Source, incomplete_presented, start_pipeline,
};
use coroom_render::NullPresenter;
use coroom_session::latency::{LatencyLedger, Stage};
use coroom_session::loopback::LoopbackConfig;
use coroom_testkit::timing_assert;

const FRAMES: u64 = 120;

#[test]
fn synthetic_720p60_pipeline_meets_the_regression_bounds() {
    let options = ProbeOptions::h264(
        Source::Synthetic,
        LoopbackConfig {
            width: 1280,
            height: 720,
            fps: 60,
            bitrate_kbps: 10_000,
            frames: Some(FRAMES),
            yuv444: false,
            hdr: false,
        },
    );
    let ledger = Arc::new(LatencyLedger::new());
    let sink = Box::new(PresenterSink::new(NullPresenter::new()));
    let pipeline = start_pipeline(&options, sink, Arc::clone(&ledger)).expect("arranque");
    let deadline = Instant::now() + Duration::from_secs(120);
    while !pipeline.wait(Duration::from_millis(250)) {
        assert!(
            Instant::now() < deadline,
            "la prueba no terminó en 2 minutos"
        );
    }
    assert_eq!(pipeline.failure(), None);
    let summary = pipeline.stop();
    println!("{summary}");

    assert!(summary.frames_presented >= FRAMES, "{summary}");

    // Los fotogramas que siguen en vuelo al parar no cuentan: ni se presentaron ni se descartaron.
    let presented = summary.frames_presented as f64;
    let drops_at = |stage: Stage| summary.drops[stage.index()].count as f64;
    // Entrega: de lo que sale del codificador, ≥ 95 % llega a la pantalla. Aquí caen las pérdidas
    // de transporte, reensamblado, decodificación y presentación (en el bucle local, cero).
    let lost_after_encoding: f64 = [
        Stage::Sent,
        Stage::Received,
        Stage::Decoded,
        Stage::Presented,
    ]
    .into_iter()
    .map(drops_at)
    .sum();
    let delivered = presented / (presented + lost_after_encoding);
    // Lo que se pierde por el camino depende de la carga (un hilo parado descarta en la cola); que
    // llegue más de la mitad prueba que el camino funciona.
    assert!(
        delivered > 0.5,
        "se presentó el {:.1} % de los fotogramas codificados\n{summary}",
        delivered * 100.0
    );
    timing_assert!(
        delivered >= 0.95,
        "se presentó el {:.1} % de los fotogramas codificados\n{summary}",
        delivered * 100.0
    );
    // Total: incluye los descartes de «gana el último» a la entrada del codificador, que miden la
    // capacidad de CPU del runner (los tres primeros se van siempre con el primer IDR). Cota
    // holgada: en un runner de 4 vCPU en debug se han visto entre el 1 y el 9 %.
    let overall = presented / (presented + summary.drops_total as f64);
    println!(
        "Presentados: {:.1} % de lo codificado, {:.1} % del total",
        delivered * 100.0,
        overall * 100.0
    );
    // Los runners de CI comparten CPU con otros trabajos: la cota es de regresión, no de calidad.
    timing_assert!(
        overall >= 0.70,
        "se presentó el {:.1} % de los fotogramas\n{summary}",
        overall * 100.0
    );

    let presented = ledger
        .timelines()
        .iter()
        .filter(|timeline| timeline.at(Stage::Presented).is_some())
        .count();
    assert!(presented as u64 >= FRAMES, "{summary}");
    assert_eq!(
        incomplete_presented(&ledger),
        0,
        "todo fotograma presentado tiene las ocho etapas"
    );
    for span in &summary.spans {
        assert!(span.samples as u64 >= FRAMES, "{span:?}");
    }

    // Cota de regresión holgada (en este entorno de 4 vCPU el p95 ronda los 37 ms); la puerta real
    // se mide en release en el equipo de referencia.
    let p95_ms = summary.total.p95_us as f64 / 1000.0;
    timing_assert!(p95_ms < 150.0, "p95 total de {p95_ms:.1} ms\n{summary}");
}

/// AV1 por software (SVT-AV1 → dav1d) a 640×360: el pipeline entero funciona con FFmpeg, el cursor
/// llega a la presentación con su forma y el resumen dice qué códec se usó.
#[test]
fn synthetic_av1_pipeline_presents_with_the_cursor_shape() {
    use coroom_session::codec::{Backend, Codec, DecoderBackend};
    use coroom_session::loopback::{DecodedFrame, FrameSink, SinkError};
    use std::sync::Mutex;

    /// Guarda el id de la forma del cursor que llega con cada fotograma.
    struct ShapeSink(Arc<Mutex<Vec<Option<u64>>>>);

    impl FrameSink for ShapeSink {
        fn present(&mut self, frame: DecodedFrame) -> Result<(), SinkError> {
            let mut seen = self.0.lock().unwrap_or_else(|p| p.into_inner());
            seen.push(frame.cursor_shape.map(|shape| shape.shape_id));
            Ok(())
        }
    }

    let mut options = ProbeOptions::h264(
        Source::Synthetic,
        LoopbackConfig {
            width: 640,
            height: 360,
            fps: 60,
            bitrate_kbps: 4_000,
            frames: Some(90),
            yuv444: false,
            hdr: false,
        },
    );
    options.codec = Codec::Av1;
    options.encoder = Some(Backend::Software);
    options.decoder = Some(DecoderBackend::Software);
    let ledger = Arc::new(LatencyLedger::new());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let pipeline = start_pipeline(
        &options,
        Box::new(ShapeSink(Arc::clone(&seen))),
        Arc::clone(&ledger),
    )
    .expect("arranque con AV1");
    let deadline = Instant::now() + Duration::from_secs(120);
    while !pipeline.wait(Duration::from_millis(250)) {
        assert!(Instant::now() < deadline, "la prueba AV1 no terminó");
    }
    assert_eq!(pipeline.failure(), None);
    let summary = pipeline.stop();
    println!("{summary}");
    assert!(summary.frames_presented >= 90, "{summary}");
    assert_eq!(
        summary.video.as_deref(),
        Some("AV1 · SVT-AV1 → dav1d"),
        "{summary}"
    );
    let seen = seen.lock().unwrap_or_else(|p| p.into_inner());
    let shape = coroom_session::capture::synthetic::CURSOR_SHAPE_ID;
    assert!(seen.iter().all(|id| *id == Some(shape)), "{seen:?}");
}
