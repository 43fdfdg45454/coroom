//! Medios sobre datagramas en `localhost`: sin pérdida todos los fotogramas llegan; a través de
//! `UdpLossy` (2 % de pérdida, 10 ± 5 ms) la FEC y los NACK recuperan y nunca pasan 100 ms sin
//! fotograma.

mod common;

use std::time::Duration;

use common::{Run, TIMEOUT, connected_pair, frame_payload, percentile, transmit};
use coroom_testkit::timing_assert;
use coroom_transport::EndpointOptions;
use coroom_transport::lossy::{LossyConfig, UdpLossy};

/// 2 s a 60 fps: sin pérdida basta para ver el orden y los huecos; con la del contrato (2 %),
/// son ~2000 datagramas y unas decenas de pérdidas que recuperar.
const FRAMES: u32 = 120;

/// Los tests de este fichero que transmiten a ritmo real miden pérdidas y huecos en `localhost`,
/// y uno no debe quitarle CPU ni búfer de socket a otro (en el runner de Windows, con dos núcleos,
/// se perdían objetos sin red de por medio). Con `cargo test` este cerrojo los pone en fila; con
/// nextest cada test es un proceso aparte y los pone en fila su grupo de `.config/nextest.toml`.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const SIZE: usize = 20 * 1024;

fn check_payloads(run: &Run) {
    for (frame, _) in &run.frames {
        assert_eq!(
            frame.payload,
            frame_payload(frame.group, SIZE),
            "grupo {}",
            frame.group
        );
    }
}

fn summary(label: &str, run: &Run) {
    let latencies: Vec<Duration> = run.frames.iter().map(|(_, latency)| *latency).collect();
    eprintln!(
        "{label}: {} de {FRAMES} fotogramas; latencia p50 {:?} p95 {:?} máx {:?}; hueco máx {:?}; \
         FEC {} NACK {} ({} enviados) descartados {} intra pedidos {}; objetos perdidos {}; no válidos {} tardíos {}",
        run.frames.len(),
        percentile(&latencies, 0.5),
        percentile(&latencies, 0.95),
        percentile(&latencies, 1.0),
        run.max_gap,
        run.stats.recovered_fec,
        run.stats.recovered_nack,
        run.stats.nacks,
        run.stats.dropped,
        run.keyframe_requests,
        run.stats.lost_objects,
        run.stats.invalid,
        run.stats.late,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_frame_arrives_without_loss() {
    let _serial = SERIAL.lock().await;
    let (host, host_conn, guest, guest_conn) =
        connected_pair(EndpointOptions::default(), None).await;
    let run = transmit(host_conn, guest_conn, &host, &guest, FRAMES, |_| SIZE, 60).await;
    summary("sin pérdida", &run);
    let groups: Vec<u32> = run.frames.iter().map(|(frame, _)| frame.group).collect();
    // Un parón del proceso puede costar algún fotograma (hasta el último), pero no la mitad ni el
    // final de la transmisión.
    assert!(
        run.frames.len() >= FRAMES as usize / 2,
        "{} de {FRAMES} fotogramas: {groups:?}",
        run.frames.len()
    );
    assert!(
        groups.last() >= Some(&(FRAMES - 3)),
        "llega hasta casi el último fotograma: {groups:?}"
    );
    timing_assert!(
        groups.last() == Some(&(FRAMES - 1)),
        "no llegó el último fotograma: {:?}",
        groups.last()
    );
    assert!(groups.is_sorted(), "en orden: {groups:?}");
    check_payloads(&run);
    // Sin red de por medio solo se pierde un fotograma si el proceso se queda parado más que el
    // plazo del receptor (1,5 intervalos más los NACK): el runner de Windows, con dos núcleos, a
    // veces se para 80 ms.
    timing_assert!(
        run.stats.dropped <= 1 && run.frames.len() + run.stats.dropped as usize == FRAMES as usize,
        "{} de {FRAMES} fotogramas, {} descartados",
        run.frames.len(),
        run.stats.dropped
    );
    timing_assert!(
        run.max_gap < Duration::from_millis(100),
        "hueco de {:?} sin fotograma",
        run.max_gap
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fec_and_nack_survive_loss_and_jitter() {
    let _serial = SERIAL.lock().await;
    let host_options = EndpointOptions::default();
    // El proxy se pone delante del anfitrión: hace falta su dirección antes de conectar.
    let host = common::peer(host_options);
    let proxy = UdpLossy::start(
        host.endpoint.local_addr().expect("dirección"),
        LossyConfig::contract(),
    )
    .await
    .expect("proxy");
    let guest = common::peer(host_options);
    host.endpoint.admit(guest.identity.public_key());
    let accept = {
        let endpoint = host.endpoint.clone();
        tokio::spawn(async move { endpoint.accept_peer().await })
    };
    let guest_conn = tokio::time::timeout(
        TIMEOUT,
        guest
            .endpoint
            .connect_peer(proxy.addr(), host.identity.public_key()),
    )
    .await
    .expect("conexión a tiempo")
    .expect("conexión a través del proxy");
    let host_conn = tokio::time::timeout(TIMEOUT, accept)
        .await
        .expect("aceptación a tiempo")
        .expect("tarea");

    let run = transmit(host_conn, guest_conn, &host, &guest, FRAMES, |_| SIZE, 60).await;
    summary("2 % / 10 ± 5 ms", &run);
    eprintln!(
        "proxy: {} reenviados, {} perdidos",
        proxy.stats().forwarded(),
        proxy.stats().dropped()
    );
    let got: std::collections::HashSet<u32> = run.frames.iter().map(|(f, _)| f.group).collect();
    let missing: Vec<u32> = (0..FRAMES).filter(|g| !got.contains(g)).collect();
    eprintln!("grupos perdidos: {missing:?}");
    assert!(proxy.stats().dropped() > 0, "el proxy debe perder algo");
    check_payloads(&run);
    let delivered = run.frames.len() as f64 / f64::from(FRAMES);
    // Que la recuperación funciona lo prueba `recovered > 0` (abajo). Este mínimo solo dice que la
    // transmisión no se cae: sin recuperar nada, el 2 % de pérdida sobre ~18 datagramas por
    // fotograma aún entrega ~70 %; el 98 % depende de que el proceso no se pare más que el plazo
    // del receptor.
    assert!(delivered >= 0.5, "entregados {:.1} %", delivered * 100.0);
    timing_assert!(delivered >= 0.98, "entregados {:.1} %", delivered * 100.0);
    timing_assert!(
        run.max_gap <= Duration::from_millis(100),
        "hueco de {:?} sin fotograma",
        run.max_gap
    );
    assert!(
        run.stats.recovered_fec + run.stats.recovered_nack > 0,
        "algo se tuvo que recuperar: {:?}",
        run.stats
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tiny_and_huge_frames_round_trip() {
    let _serial = SERIAL.lock().await;
    let (host, host_conn, guest, guest_conn) =
        connected_pair(EndpointOptions::default(), None).await;
    // 0 bytes, 1 byte, justo un objeto, un objeto + 1, y un intra de 200 KB. A 10 fps el
    // receptor espera 150 ms a cada uno antes de darlo por perdido.
    let sizes = [1, 2, 1180, 1181, 200_000, 4];
    let run = transmit(
        host_conn,
        guest_conn,
        &host,
        &guest,
        sizes.len() as u32,
        move |group| sizes[group as usize],
        10,
    )
    .await;
    // Un parón puede costar como mucho un fotograma, y nunca el intra de 200 KB (grupo 4), que es
    // el que prueba la fragmentación grande.
    assert!(
        run.frames.iter().any(|(frame, _)| frame.group == 4),
        "no llegó el fotograma de 200 KB"
    );
    assert!(
        run.frames.len() >= sizes.len() - 1,
        "{} de {} fotogramas",
        run.frames.len(),
        sizes.len()
    );
    timing_assert!(
        run.frames.len() == sizes.len(),
        "{} de {} fotogramas",
        run.frames.len(),
        sizes.len()
    );
    for (frame, _) in &run.frames {
        assert_eq!(
            frame.payload,
            frame_payload(frame.group, sizes[frame.group as usize])
        );
    }
}

/// Un emisor hacia el relay sirve a varios invitados: cada uno informa con su propio `Feedback`, el
/// objetivo es el del más lento y al olvidarlo manda el siguiente.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shared_sender_follows_the_slowest_receiver() {
    use coroom_transport::media::{MediaSender, MediaSenderConfig};
    use coroom_transport::proto::control::Feedback;
    use coroom_transport::proto::media::Track;

    let (_host, host_conn, _guest, _guest_conn) =
        connected_pair(EndpointOptions::default(), None).await;
    let sender = MediaSender::new(host_conn, MediaSenderConfig::default());
    let start = sender.target_kbps();
    for group in 0..10 {
        sender
            .send_frame(Track::Video, group, group == 0, &frame_payload(group, SIZE))
            .await
            .expect("envío");
    }
    let report = |lost_objects| Feedback {
        highest_group: 9,
        received_bytes: 0,
        lost_objects,
        owd_samples: Vec::new(),
        ecn_ce: 0,
    };
    // El invitado 1 pierde 40 objetos de los ~180 enviados; el 2, ninguno.
    sender.on_feedback_from(1, &report(40));
    let target = sender.on_feedback_from(2, &report(0));
    let (slow, fast) = (
        sender.source_target_kbps(1).expect("invitado 1"),
        sender.source_target_kbps(2).expect("invitado 2"),
    );
    assert!(slow < start && start <= fast, "{slow} < {start} <= {fast}");
    assert_eq!(target, slow);
    assert_eq!(sender.target_kbps(), slow);
    assert!(sender.repair_ratio() > 0.0, "la FEC sigue al que pierde");

    sender.forget_source(1);
    assert_eq!(sender.source_target_kbps(1), None);
    assert_eq!(sender.target_kbps(), fast);
}

/// Caudal sostenido: 80 Mbit/s (60 fotogramas de 166 KB por segundo, durante 1 s) a través de un
/// proxy sin pérdida con ~1 ms y ~30 ms de RTT llegan casi enteros. Con la ventana de `target × RTT × 2` y
/// suelo de 64 KB, un ACK retrasado dejaba una LAN en ~17 Mbit/s.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eighty_megabits_flow_at_lan_and_internet_rtt() {
    let _serial = SERIAL.lock().await;
    use coroom_transport::rate::RateConfig;

    const KBPS: u32 = 80_000;
    const FPS: u32 = 60;
    const SECONDS: u32 = 1;
    let size = (KBPS as usize * 1000 / 8) / FPS as usize;
    for one_way in [Duration::from_micros(500), Duration::from_millis(15)] {
        let options = EndpointOptions {
            initial_target_kbps: KBPS,
            ..EndpointOptions::default()
        };
        let host = common::peer(options);
        let proxy = UdpLossy::start(
            host.endpoint.local_addr().expect("dirección"),
            LossyConfig {
                loss: 0.0,
                delay: one_way,
                jitter: Duration::ZERO,
                rate_bps: None,
                queue_limit: Duration::from_millis(200),
                seed: 1,
                outages: None,
            },
        )
        .await
        .expect("proxy");
        let guest = common::peer(options);
        host.endpoint.admit(guest.identity.public_key());
        let accept = {
            let endpoint = host.endpoint.clone();
            tokio::spawn(async move { endpoint.accept_peer().await })
        };
        let guest_conn = tokio::time::timeout(
            TIMEOUT,
            guest
                .endpoint
                .connect_peer(proxy.addr(), host.identity.public_key()),
        )
        .await
        .expect("conexión a tiempo")
        .expect("conexión a través del proxy");
        let host_conn = tokio::time::timeout(TIMEOUT, accept)
            .await
            .expect("aceptación a tiempo")
            .expect("tarea");
        let started = std::time::Instant::now();
        let run = common::transmit_with(
            host_conn,
            guest_conn,
            &host,
            &guest,
            FPS * SECONDS,
            move |_| size,
            FPS,
            RateConfig {
                min_kbps: KBPS,
                max_kbps: KBPS,
                start_kbps: KBPS,
            },
        )
        .await;
        let elapsed = started.elapsed();
        let bytes: usize = run
            .frames
            .iter()
            .map(|(frame, _)| frame.payload.len())
            .sum();
        let mbps = bytes as f64 * 8.0 / elapsed.as_secs_f64() / 1e6;
        let latencies: Vec<Duration> = run.frames.iter().map(|(_, latency)| *latency).collect();
        eprintln!(
            "RTT ~{:?}: {} de {} fotogramas, {mbps:.1} Mbit/s en {elapsed:?}, latencia p50 {:?} \
             p95 {:?}, descartados {}",
            one_way * 2,
            run.frames.len(),
            FPS * SECONDS,
            percentile(&latencies, 0.5),
            percentile(&latencies, 0.95),
            run.stats.dropped,
        );
        assert!(
            !run.frames.is_empty(),
            "no llegó ningún fotograma con RTT ~{:?}",
            one_way * 2
        );
        for (frame, _) in &run.frames {
            assert_eq!(
                frame.payload,
                frame_payload(frame.group, size),
                "grupo {}",
                frame.group
            );
        }
        let delivered = run.frames.len() as f64 / f64::from(FPS * SECONDS);
        timing_assert!(
            delivered >= 0.97,
            "entregados {:.1} % con RTT ~{:?}",
            delivered * 100.0,
            one_way * 2
        );
        // El tiempo incluye la espera final del último fotograma: ≥ 70 de los 80 Mbit/s.
        timing_assert!(mbps >= 70.0, "{mbps:.1} Mbit/s con RTT ~{:?}", one_way * 2);
        let p95 = percentile(&latencies, 0.95);
        timing_assert!(
            p95 < Duration::from_millis(60) + one_way * 4,
            "la cola crece: latencia p95 {p95:?} con RTT ~{:?}",
            one_way * 2
        );
    }
}

/// El ritmo del pacing sigue a lo que midió la sonda aunque supere el bitrate máximo del usuario:
/// con un tren a `probe_pace_kbps` (100 Mbit/s para 10 Mbit/s de máximo) que midió el bucle local,
/// el control de tasa arranca acotado al máximo (10 Mbit/s) pero el pacing va al 70 % de lo medido
/// (70 Mbit/s), con holgura para los intras. Sin sonda, `gain × objetivo`; y al bajar el máximo en
/// marcha el objetivo lo sigue mientras la sonda sigue mandando en el ritmo.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_pacing_follows_the_probe_above_the_bitrate_cap() {
    use coroom_transport::media::{
        DEFAULT_SOURCE, MediaSender, MediaSenderConfig, PROBE_MAX_PACE_KBPS, PROBE_PACKETS,
        ProbeOutcome, probe_pace_kbps,
    };
    use coroom_transport::rate::RateConfig;

    let (_host, host_conn, _guest, _guest_conn) =
        connected_pair(EndpointOptions::default(), None).await;
    let sender = MediaSender::new(
        host_conn,
        MediaSenderConfig {
            frame_interval: Duration::from_micros(16_667),
            rate: RateConfig {
                min_kbps: 1_000,
                max_kbps: 10_000,
                start_kbps: 8_000,
            },
            pacing: Default::default(),
        },
    );
    // Sin sonda ni estimación: 1,25 × 8 Mbit/s de arranque.
    assert_eq!(sender.pacing_plan(), (10_000, false));

    let pace = probe_pace_kbps(10_000);
    assert_eq!(pace, PROBE_MAX_PACE_KBPS);
    let train = sender.send_probe(pace).await.expect("tren de sonda");
    // Lo que informaría el receptor de un tren entero que llegó a 100 Mbit/s: 64 × 1200 B,
    // 63 de ellos en 6,048 ms.
    let outcome = ProbeOutcome {
        train,
        sent: PROBE_PACKETS,
        received: PROBE_PACKETS,
        bytes: 76_800,
        span: Duration::from_micros(6_048),
    };
    assert_eq!(outcome.kbps(), Some(100_000));
    assert_eq!(
        sender.on_probe_result(DEFAULT_SOURCE, outcome),
        Some(100_000)
    );
    assert_eq!(
        sender.target_kbps(),
        10_000,
        "el objetivo respeta el máximo"
    );
    assert_eq!(sender.pacing_plan(), (70_000, true), "el pacing, lo medido");

    sender.set_max_kbps(4_000);
    assert_eq!(sender.target_kbps(), 4_000);
    assert_eq!(sender.pacing_plan(), (70_000, true));
    sender.set_max_kbps(20_000);
    assert_eq!(
        sender.target_kbps(),
        4_000,
        "subir el máximo no sube el objetivo de golpe"
    );
    assert_eq!(sender.pacing_plan(), (70_000, true));
}
