//! Apertura simultánea en `localhost`: los dos pares llaman a `Connector::connect` a la vez y
//! acaban en la misma conexión directa; sin candidatos útiles ni relay, `Unreachable`.

mod common;

use std::time::Duration;

use common::{TIMEOUT, hello, peer};
use coroom_transport::proto::control::{ControlMessage, Role};
use coroom_transport::{Connector, EndpointOptions, Path, TransportError};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_open_agrees_on_one_direct_connection() {
    for _ in 0..5 {
        let host = peer(EndpointOptions::default());
        let guest = peer(EndpointOptions::default());
        let host_addr = host.endpoint.local_addr().expect("dirección");
        let guest_addr = guest.endpoint.local_addr().expect("dirección");
        let host_key = host.identity.public_key();
        let guest_key = guest.identity.public_key();

        let host_side = {
            let connector = Connector::new(host.endpoint.clone());
            tokio::spawn(async move { connector.connect(guest_key, &[guest_addr], None).await })
        };
        let guest_side = {
            let connector = Connector::new(guest.endpoint.clone());
            tokio::spawn(async move { connector.connect(host_key, &[host_addr], None).await })
        };
        let host_conn = tokio::time::timeout(TIMEOUT, host_side)
            .await
            .expect("a tiempo")
            .expect("tarea")
            .expect("conexión del anfitrión");
        let guest_conn = tokio::time::timeout(TIMEOUT, guest_side)
            .await
            .expect("a tiempo")
            .expect("tarea")
            .expect("conexión del invitado");
        assert_eq!(host_conn.path, Path::Direct);
        assert_eq!(guest_conn.path, Path::Direct);
        assert_eq!(host_conn.connection.peer_key(), Some(guest_key));
        assert_eq!(guest_conn.connection.peer_key(), Some(host_key));

        // Es la misma conexión: el control abierto por el invitado llega al anfitrión.
        let accept = {
            let connection = host_conn.connection.clone();
            let host_hello = hello(&host, Role::Host);
            tokio::spawn(async move { connection.accept_control(host_hello).await })
        };
        let control = tokio::time::timeout(
            TIMEOUT,
            guest_conn
                .connection
                .open_control(hello(&guest, Role::Guest)),
        )
        .await
        .expect("a tiempo")
        .expect("control");
        let mut host_control = tokio::time::timeout(TIMEOUT, accept)
            .await
            .expect("a tiempo")
            .expect("tarea")
            .expect("control del anfitrión");
        control
            .sender
            .send(&ControlMessage::Ping { t: 7 })
            .expect("envío");
        assert_eq!(
            host_control.receiver.recv().await.expect("mensaje"),
            Some(ControlMessage::Ping { t: 7 })
        );
        // Y sigue viva pasada la gracia en la que se cierran las duplicadas.
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(host_conn.connection.quinn().close_reason().is_none());
        assert!(guest_conn.connection.quinn().close_reason().is_none());
    }
}

#[tokio::test]
async fn without_candidates_or_relay_the_peer_is_unreachable() {
    let lonely = peer(EndpointOptions::default());
    let absent = peer(EndpointOptions::default());
    let key = absent.identity.public_key();
    // Un puerto UDP sin nadie escuchando.
    let dead = {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("socket");
        socket.local_addr().expect("dirección")
    };
    let result = Connector::new(lonely.endpoint.clone())
        .with_timeout(Duration::from_millis(300))
        .connect(key, &[dead], None)
        .await;
    assert!(
        matches!(result, Err(TransportError::Unreachable)),
        "{result:?}"
    );
}
