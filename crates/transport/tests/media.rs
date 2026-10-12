//! Medios sobre datagramas en `localhost`: sin pérdida todos los fotogramas llegan; a través de
//! `UdpLossy` (2 % de pérdida, 10 ± 5 ms) la FEC y los NACK recuperan y nunca pasan 100 ms sin
//! fotograma.

mod common;

use std::time::Duration;

use common::{Run, connected_pair, frame_payload, percentile, transmit};
use coroom_transport::EndpointOptions;
use coroom_transport::lossy::{LossyConfig, UdpLossy};

const FRAMES: u32 = 300;
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
    let (host, host_conn, guest, guest_conn) =
        connected_pair(EndpointOptions::default(), None).await;
    let run = transmit(host_conn, guest_conn, &host, &guest, FRAMES, |_| SIZE, 60).await;
    summary("sin pérdida", &run);
    assert_eq!(run.frames.len(), FRAMES as usize);
    let groups: Vec<u32> = run.frames.iter().map(|(frame, _)| frame.group).collect();
    assert_eq!(groups, (0..FRAMES).collect::<Vec<_>>(), "en orden");
    assert_eq!(run.stats.dropped, 0);
    check_payloads(&run);
    assert!(
        run.max_gap < Duration::from_millis(100),
        "{:?}",
        run.max_gap
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fec_and_nack_survive_loss_and_jitter() {
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
    let guest_conn = guest
        .endpoint
        .connect_peer(proxy.addr(), host.identity.public_key())
        .await
        .expect("conexión a través del proxy");
    let host_conn = accept.await.expect("tarea");

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
    assert!(delivered >= 0.98, "entregados {:.1} %", delivered * 100.0);
    assert!(
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
    let (host, host_conn, guest, guest_conn) =
        connected_pair(EndpointOptions::default(), None).await;
    // 0 bytes, 1 byte, justo un objeto, un objeto + 1, y un intra de 200 KB.
    let sizes = [1, 2, 1180, 1181, 200_000, 4];
    let run = transmit(
        host_conn,
        guest_conn,
        &host,
        &guest,
        sizes.len() as u32,
        move |group| sizes[group as usize],
        30,
    )
    .await;
    assert_eq!(run.frames.len(), sizes.len());
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
