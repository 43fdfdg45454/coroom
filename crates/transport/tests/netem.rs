//! Tests con `tc netem` real en `lo` (2 % de pérdida, 10 ± 5 ms): se saltan si no está activo.
//!
//! ```text
//! sudo tools/netem.sh apply
//! cargo test -p coroom-transport -- netem
//! sudo tools/netem.sh clear
//! ```
//!
//! Sin netem, el mismo camino con pérdida se prueba en proceso con `UdpLossy` (`tests/media.rs`).

mod common;

use std::process::Command;
use std::time::Duration;

use common::{connected_pair, percentile, transmit};
use coroom_transport::EndpointOptions;

/// Indica si `lo` tiene una disciplina netem activa.
fn netem_active() -> bool {
    Command::new("tc")
        .args(["qdisc", "show", "dev", "lo"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).contains("netem"))
        .unwrap_or(false)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn netem_media_never_freezes_more_than_100_ms() {
    if !netem_active() {
        eprintln!("sin netem en lo: test saltado (ver tools/netem.sh)");
        return;
    }
    let (host, host_conn, guest, guest_conn) =
        connected_pair(EndpointOptions::default(), None).await;
    let run = transmit(host_conn, guest_conn, &host, &guest, 300, |_| 20 * 1024, 60).await;
    let latencies: Vec<Duration> = run.frames.iter().map(|(_, latency)| *latency).collect();
    eprintln!(
        "netem: {} de 300; latencia p50 {:?} p95 {:?}; hueco máx {:?}; {:?}",
        run.frames.len(),
        percentile(&latencies, 0.5),
        percentile(&latencies, 0.95),
        run.max_gap,
        run.stats
    );
    assert!(!run.frames.is_empty(), "no llegó ningún fotograma");
    coroom_testkit::timing_assert!(
        run.frames.len() >= 294,
        "entregados {} de 300",
        run.frames.len()
    );
    coroom_testkit::timing_assert!(
        run.max_gap <= Duration::from_millis(100),
        "hueco de {:?} sin fotograma",
        run.max_gap
    );
}
