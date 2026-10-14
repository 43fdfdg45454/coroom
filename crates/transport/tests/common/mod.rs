//! Utilidades comunes de los tests de integración de `coroom-transport`.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use coroom_transport::crypto::Identity;
use coroom_transport::media::{
    CompleteFrame, MediaReceiver, MediaReceiverConfig, MediaSender, MediaSenderConfig,
};
use coroom_transport::proto::PROTOCOL_VERSION;
use coroom_transport::proto::control::{Hello, Role};
use coroom_transport::proto::media::Track;
use coroom_transport::rate::RateConfig;
use coroom_transport::{Endpoint, EndpointOptions, PeerConnection};

pub const TIMEOUT: Duration = Duration::from_secs(10);

/// `127.0.0.1:0`: los tests no dependen de que el sistema tenga IPv6.
pub fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().expect("dirección")
}

pub struct Peer {
    pub identity: Identity,
    pub endpoint: Endpoint,
}

pub fn peer(options: EndpointOptions) -> Peer {
    let identity = Identity::generate().expect("identidad");
    let endpoint = Endpoint::bind(&identity, Some(localhost()), options).expect("endpoint");
    Peer { identity, endpoint }
}

pub fn hello(peer: &Peer, role: Role) -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        identity_pk: peer.identity.public_key(),
        role,
        relay_token: None,
    }
}

/// Anfitrión (servidor QUIC) e invitado (cliente) conectados; el invitado llega por `via` si se
/// da (un proxy delante del anfitrión).
pub async fn connected_pair(
    options: EndpointOptions,
    via: Option<SocketAddr>,
) -> (Peer, PeerConnection, Peer, PeerConnection) {
    let host = peer(options);
    let guest = peer(options);
    host.endpoint.admit(guest.identity.public_key());
    let target = via.unwrap_or_else(|| host.endpoint.local_addr().expect("dirección"));
    let accept = {
        let endpoint = host.endpoint.clone();
        tokio::spawn(async move { endpoint.accept_peer().await })
    };
    let guest_conn = tokio::time::timeout(
        TIMEOUT,
        guest
            .endpoint
            .connect_peer(target, host.identity.public_key()),
    )
    .await
    .expect("conexión a tiempo")
    .expect("conexión");
    let host_conn = tokio::time::timeout(TIMEOUT, accept)
        .await
        .expect("aceptación a tiempo")
        .expect("tarea");
    (host, host_conn, guest, guest_conn)
}

/// Resultado de una transmisión de prueba.
#[derive(Debug)]
pub struct Run {
    pub frames: Vec<(CompleteFrame, Duration)>,
    pub max_gap: Duration,
    pub stats: coroom_transport::media::ReceiverStats,
    pub keyframe_requests: usize,
}

pub fn frame_payload(group: u32, len: usize) -> Vec<u8> {
    let mut payload: Vec<u8> = (0..len)
        .map(|i| (i as u32).wrapping_mul(2_654_435_761).wrapping_add(group) as u8)
        .collect();
    let n = len.min(4);
    payload[..n].copy_from_slice(&group.to_be_bytes()[..n]);
    payload
}

/// Envía `frames` fotogramas de `size` bytes a `fps` del anfitrión al invitado y devuelve lo
/// recibido con la latencia de cada fotograma (desde que empezó su envío).
pub async fn transmit(
    host_conn: PeerConnection,
    guest_conn: PeerConnection,
    host: &Peer,
    guest: &Peer,
    frames: u32,
    size: impl Fn(u32) -> usize + Send + 'static,
    fps: u32,
) -> Run {
    transmit_with(
        host_conn,
        guest_conn,
        host,
        guest,
        frames,
        size,
        fps,
        RateConfig::default(),
    )
    .await
}

/// Como [`transmit`], con los límites del control de tasa del emisor.
#[allow(clippy::too_many_arguments)]
pub async fn transmit_with(
    host_conn: PeerConnection,
    guest_conn: PeerConnection,
    host: &Peer,
    guest: &Peer,
    frames: u32,
    size: impl Fn(u32) -> usize + Send + 'static,
    fps: u32,
    rate: RateConfig,
) -> Run {
    let interval = Duration::from_secs(1) / fps;
    let accept = {
        let host_conn = host_conn.clone();
        let host_hello = hello(host, Role::Host);
        tokio::spawn(async move { host_conn.accept_control(host_hello).await })
    };
    let guest_control = guest_conn
        .open_control(hello(guest, Role::Guest))
        .await
        .expect("control del invitado");
    let host_control = accept.await.expect("tarea").expect("control del anfitrión");
    assert_eq!(host_control.peer_hello().role, Role::Guest);
    assert_eq!(guest_control.peer_hello().role, Role::Host);

    let sender = Arc::new(MediaSender::new(
        host_conn,
        MediaSenderConfig {
            frame_interval: interval,
            rate,
        },
    ));
    let receiver = Arc::new(MediaReceiver::new(
        guest_conn,
        guest_control.sender.clone(),
        MediaReceiverConfig {
            frame_interval: interval,
        },
    ));

    // Control del anfitrión: Feedback, Nack y Ping al emisor; cuenta los KeyframeRequest.
    let (host_tx, mut host_rx, _) = host_control.split();
    let keyframes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let host_task = {
        let sender = Arc::clone(&sender);
        let keyframes = Arc::clone(&keyframes);
        tokio::spawn(async move {
            while let Ok(Some(message)) = host_rx.recv().await {
                if let Some(coroom_transport::proto::control::ControlMessage::KeyframeRequest {
                    ..
                }) = sender.handle_control(message, &host_tx)
                {
                    keyframes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        })
    };
    // Control del invitado: Pong al reloj del receptor.
    let (_guest_tx, mut guest_rx, _) = guest_control.split();
    let guest_task = {
        let receiver = Arc::clone(&receiver);
        tokio::spawn(async move {
            while let Ok(Some(message)) = guest_rx.recv().await {
                receiver.handle_control(message);
            }
        })
    };

    let start = Instant::now();
    let send_times = Arc::new(std::sync::Mutex::new(vec![start; frames as usize]));
    let send_task = {
        let sender = Arc::clone(&sender);
        let send_times = Arc::clone(&send_times);
        tokio::spawn(async move {
            let t0 = tokio::time::Instant::now();
            for group in 0..frames {
                tokio::time::sleep_until(t0 + interval * group).await;
                if let Ok(mut times) = send_times.lock() {
                    times[group as usize] = Instant::now();
                }
                let payload = frame_payload(group, size(group));
                sender
                    .send_frame(Track::Video, group, group == 0, &payload)
                    .await
                    .expect("envío");
            }
        })
    };

    let mut received = Vec::new();
    let mut last = Instant::now();
    let mut max_gap = Duration::ZERO;
    let deadline = start + interval * frames + Duration::from_secs(2);
    while let Ok(Ok(frame)) = tokio::time::timeout_at(deadline.into(), receiver.recv_frame()).await
    {
        let now = Instant::now();
        if !received.is_empty() {
            max_gap = max_gap.max(now - last);
        }
        last = now;
        let sent = send_times
            .lock()
            .map(|times| times[frame.group as usize])
            .unwrap_or(start);
        let group = frame.group;
        received.push((frame, now - sent));
        if group + 1 == frames {
            break;
        }
    }
    send_task.await.expect("envío completo");
    host_task.abort();
    guest_task.abort();
    Run {
        frames: received,
        max_gap,
        stats: receiver.stats(),
        keyframe_requests: keyframes.load(std::sync::atomic::Ordering::Relaxed),
    }
}

/// Percentil `p` (0..1) de una lista de duraciones.
pub fn percentile(values: &[Duration], p: f64) -> Duration {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let index = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[index]
}
