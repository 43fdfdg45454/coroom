//! Spike del controlador de congestión: el controlador de medios propio
//! (ventana `target × RTT × 2`, nunca < 64 KB) frente al BBR de `quinn`, a través de `UdpLossy`.
//!
//! Mide la latencia de cada fotograma (del inicio de su envío a su reensamblado) por encima del
//! retardo base del camino: eso es la cola que añaden QUIC y su controlador. Se ejecuta aparte:
//!
//! ```text
//! cargo test --release -p coroom-transport --test spike -- --ignored --nocapture
//! ```

mod common;

use std::time::Duration;

use common::{percentile, transmit};
use coroom_transport::lossy::{LossyConfig, UdpLossy};
use coroom_transport::{Congestion, EndpointOptions};

const FRAMES: u32 = 600;
const FPS: u32 = 60;

/// 20 KB por fotograma (≈ 9,8 Mbit/s) y un intra de 100 KB cada segundo.
fn size(group: u32) -> usize {
    if group.is_multiple_of(FPS) {
        100 * 1024
    } else {
        20 * 1024
    }
}

struct Row {
    label: String,
    delivered: usize,
    p50: Duration,
    p95: Duration,
    p99: Duration,
    max: Duration,
    gap: Duration,
}

async fn measure(congestion: Congestion, link: LossyConfig, label: &str) -> Row {
    let options = EndpointOptions {
        congestion,
        ..EndpointOptions::default()
    };
    let host = common::peer(options);
    let proxy = UdpLossy::start(host.endpoint.local_addr().expect("dirección"), link)
        .await
        .expect("proxy");
    let guest = common::peer(options);
    host.endpoint.admit(guest.identity.public_key());
    let accept = {
        let endpoint = host.endpoint.clone();
        tokio::spawn(async move { endpoint.accept_peer().await })
    };
    let guest_conn = guest
        .endpoint
        .connect_peer(proxy.addr(), host.identity.public_key())
        .await
        .expect("conexión");
    let host_conn = accept.await.expect("tarea");
    let run = transmit(host_conn, guest_conn, &host, &guest, FRAMES, size, FPS).await;
    // Cola = latencia − retardo base de ida.
    let queue: Vec<Duration> = run
        .frames
        .iter()
        .map(|(_, latency)| latency.saturating_sub(link.delay))
        .collect();
    Row {
        label: format!("{label} {congestion:?}"),
        delivered: run.frames.len(),
        p50: percentile(&queue, 0.50),
        p95: percentile(&queue, 0.95),
        p99: percentile(&queue, 0.99),
        max: percentile(&queue, 1.0),
        gap: run.max_gap,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spike de medida; ejecutar en release con --ignored --nocapture"]
async fn spike_media_controller_versus_bbr() {
    let contract = LossyConfig::contract();
    let bottleneck = LossyConfig {
        rate_bps: Some(20_000_000),
        ..LossyConfig::contract()
    };
    let mut rows = Vec::new();
    for (link, label) in [
        (contract, "2 % 10±5 ms"),
        (bottleneck, "2 % 10±5 ms 20 Mbit/s"),
    ] {
        for congestion in [Congestion::Media, Congestion::Bbr] {
            rows.push(measure(congestion, link, label).await);
        }
    }
    eprintln!("| Enlace y controlador | Entregados | Cola p50 | p95 | p99 | máx | Hueco máx |");
    eprintln!("|---|---:|---:|---:|---:|---:|---:|");
    for row in &rows {
        eprintln!(
            "| {} | {}/{FRAMES} | {:.1} ms | {:.1} ms | {:.1} ms | {:.1} ms | {:.1} ms |",
            row.label,
            row.delivered,
            row.p50.as_secs_f64() * 1e3,
            row.p95.as_secs_f64() * 1e3,
            row.p99.as_secs_f64() * 1e3,
            row.max.as_secs_f64() * 1e3,
            row.gap.as_secs_f64() * 1e3,
        );
        assert!(
            row.delivered > 0,
            "{}: no llegó ningún fotograma",
            row.label
        );
        coroom_testkit::timing_assert!(
            row.delivered as f64 >= f64::from(FRAMES) * 0.9,
            "{}: entregados {} de {FRAMES}",
            row.label,
            row.delivered
        );
    }
}
