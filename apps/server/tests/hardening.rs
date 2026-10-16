//! Endurecimiento del servidor (`docs/seguridad.md`): tiempos límite de la pila TCP.
//!
//! - Hallazgo S-4: un cliente que manda las cabeceras de una petición y no las termina (o que
//!   deja la conexión abierta sin pedir nada) se queda sin conexión a los
//!   `HEADER_TIMEOUT` (10 s); una petición normal sigue funcionando.

mod common;

use std::time::{Duration, Instant};

use common::start;
use coroom_server::tcp_limits::HEADER_TIMEOUT;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_headers_are_cut_and_normal_requests_still_work() {
    // TCP sin TLS (`COROOM_TCP_PLAIN`, modo desarrollo) para escribir la petición a mano.
    let server = start(|config| config.tcp_plain = true).await;

    let mut normal = TcpStream::connect(server.tcp).await.expect("conexión");
    normal
        .write_all(b"GET /health HTTP/1.1\r\nHost: coroom.example.com\r\nConnection: close\r\n\r\n")
        .await
        .expect("petición");
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), normal.read_to_end(&mut response))
        .await
        .expect("respuesta a tiempo")
        .expect("respuesta");
    let text = String::from_utf8_lossy(&response);
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert!(
        text.to_ascii_lowercase()
            .contains("strict-transport-security")
    );

    let mut slow = TcpStream::connect(server.tcp).await.expect("conexión");
    slow.write_all(b"GET /health HTTP/1.1\r\nHost: coroom.example.com\r\n")
        .await
        .expect("cabeceras a medias");
    let started = Instant::now();
    let mut buffer = [0u8; 1024];
    let closed = tokio::time::timeout(HEADER_TIMEOUT + Duration::from_secs(20), async {
        loop {
            match slow.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "la conexión lenta sigue abierta");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= HEADER_TIMEOUT - Duration::from_secs(1),
        "se cerró antes de tiempo: {elapsed:?}"
    );
    println!("conexión con cabeceras a medias cerrada a los {elapsed:?}");
    server.stop().await;
}
