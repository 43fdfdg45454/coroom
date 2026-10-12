//! QUIC entre pares en `localhost`: claves públicas crudas, rechazo de identidades equivocadas,
//! stream de control y datagramas.

mod common;

use std::time::Duration;

use bytes::Bytes;
use common::{TIMEOUT, connected_pair, hello, peer};
use coroom_transport::proto::control::{ControlMessage, Role};
use coroom_transport::proto::media::ObjectHeader;
use coroom_transport::{EndpointOptions, Path};

#[tokio::test]
async fn peers_connect_with_raw_public_keys() {
    let (host, host_conn, guest, guest_conn) =
        connected_pair(EndpointOptions::default(), None).await;
    assert_eq!(guest_conn.peer_key(), Some(host.identity.public_key()));
    assert_eq!(host_conn.peer_key(), Some(guest.identity.public_key()));
    assert_eq!(guest_conn.path(), Path::Direct);

    // Claves crudas de verdad: la identidad TLS del par es su SPKI Ed25519, no un X.509.
    let certs = guest_conn
        .quinn()
        .peer_identity()
        .and_then(|identity| {
            identity
                .downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>()
                .ok()
        })
        .expect("identidad TLS");
    assert_eq!(
        certs[0].as_ref(),
        coroom_transport::tls::spki_from_ed25519_key(&host.identity.public_key()).as_slice()
    );

    // ALPN entre pares e intercambio híbrido poscuántico.
    let data = guest_conn
        .quinn()
        .handshake_data()
        .and_then(|data| data.downcast::<quinn::crypto::rustls::HandshakeData>().ok())
        .expect("datos del handshake");
    assert_eq!(data.protocol.as_deref(), Some(&b"coroom-p2p/1"[..]));

    // Control: el invitado abre, los dos se saludan y los mensajes llegan en orden.
    let accept = {
        let host_conn = host_conn.clone();
        let host_hello = hello(&host, Role::Host);
        tokio::spawn(async move { host_conn.accept_control(host_hello).await })
    };
    let mut guest_control = guest_conn
        .open_control(hello(&guest, Role::Guest))
        .await
        .expect("control");
    let mut host_control = accept.await.expect("tarea").expect("control");
    assert_eq!(
        host_control.peer_hello().identity_pk,
        guest.identity.public_key()
    );
    assert_eq!(
        guest_control.peer_hello().identity_pk,
        host.identity.public_key()
    );
    for kbps in 0..50 {
        guest_control
            .sender
            .send(&ControlMessage::TargetBitrate { kbps })
            .expect("envío");
    }
    for kbps in 0..50 {
        let message = tokio::time::timeout(TIMEOUT, host_control.receiver.recv())
            .await
            .expect("a tiempo")
            .expect("mensaje");
        assert_eq!(message, Some(ControlMessage::TargetBitrate { kbps }));
    }
    host_control
        .sender
        .send(&ControlMessage::Bye)
        .expect("envío");
    assert_eq!(
        guest_control.receiver.recv().await.expect("mensaje"),
        Some(ControlMessage::Bye)
    );

    // Datagramas de 1200 bytes: el MTU inicial de 1252 los permite desde el principio.
    assert!(
        guest_conn.max_packet() >= ObjectHeader::MAX_PACKET,
        "max_packet = {}",
        guest_conn.max_packet()
    );
    let datagram = Bytes::from(vec![7u8; ObjectHeader::MAX_PACKET]);
    host_conn
        .send_datagram(datagram.clone())
        .expect("datagrama");
    let received = tokio::time::timeout(TIMEOUT, guest_conn.read_datagram())
        .await
        .expect("a tiempo")
        .expect("datagrama");
    assert_eq!(received, datagram);
}

#[tokio::test]
async fn a_wrong_server_identity_is_rejected() {
    let host = peer(EndpointOptions::default());
    let guest = peer(EndpointOptions::default());
    let impostor = peer(EndpointOptions::default());
    host.endpoint.admit(guest.identity.public_key());
    let result = tokio::time::timeout(
        TIMEOUT,
        guest.endpoint.connect_peer(
            host.endpoint.local_addr().expect("dirección"),
            impostor.identity.public_key(),
        ),
    )
    .await
    .expect("a tiempo");
    assert!(result.is_err(), "debe rechazar una identidad inesperada");
}

#[tokio::test]
async fn a_client_that_is_not_admitted_is_rejected() {
    let host = peer(EndpointOptions::default());
    let stranger = peer(EndpointOptions::default());
    let result = tokio::time::timeout(
        TIMEOUT,
        stranger.endpoint.connect_peer(
            host.endpoint.local_addr().expect("dirección"),
            host.identity.public_key(),
        ),
    )
    .await
    .expect("a tiempo");
    // El cliente puede completar su lado del handshake antes de recibir el rechazo: en ese caso
    // la conexión muere enseguida.
    if let Ok(connection) = result {
        let closed = tokio::time::timeout(Duration::from_secs(5), connection.closed()).await;
        assert!(closed.is_ok(), "el servidor debe cerrar la conexión");
    }
    // Y nunca llega al anfitrión.
    let accepted =
        tokio::time::timeout(Duration::from_millis(300), host.endpoint.accept_peer()).await;
    assert!(accepted.is_err());
}

#[tokio::test]
async fn revoked_peers_can_no_longer_connect() {
    let host = peer(EndpointOptions::default());
    let guest = peer(EndpointOptions::default());
    let key = guest.identity.public_key();
    host.endpoint.admit(key);
    host.endpoint.revoke(&key);
    let result = tokio::time::timeout(
        TIMEOUT,
        guest.endpoint.connect_peer(
            host.endpoint.local_addr().expect("dirección"),
            host.identity.public_key(),
        ),
    )
    .await
    .expect("a tiempo");
    if let Ok(connection) = result {
        let closed = tokio::time::timeout(Duration::from_secs(5), connection.closed()).await;
        assert!(closed.is_ok());
    }
}

#[tokio::test]
async fn input_datagrams_and_streams_are_kept_apart_from_media() {
    use coroom_transport::proto::input::{datagram_header, stream_preamble};

    let (_host, host_conn, _guest, guest_conn) =
        connected_pair(EndpointOptions::default(), None).await;
    // Medios, entrada y audio (juego y voz) mezclados en el cable; cada lector recibe
    // solo lo suyo y en orden.
    for i in 0..20u8 {
        let mut input = datagram_header(0xabcd).to_vec();
        input.push(i);
        guest_conn
            .send_datagram(Bytes::from(input))
            .expect("datagrama de entrada");
        guest_conn
            .send_datagram(Bytes::from(vec![0x02, 0x00, i]))
            .expect("datagrama de medios");
        let track = if i % 2 == 0 { 0x01 } else { 0x02 };
        guest_conn
            .send_datagram(Bytes::from(vec![0x02, track, i]))
            .expect("datagrama de audio");
    }
    for i in 0..20u8 {
        let input = tokio::time::timeout(TIMEOUT, host_conn.read_input_datagram())
            .await
            .expect("a tiempo")
            .expect("entrada");
        assert_eq!(input.last(), Some(&i));
        assert_eq!(input[1], 3, "pista de entrada");
        let media = tokio::time::timeout(TIMEOUT, host_conn.read_datagram())
            .await
            .expect("a tiempo")
            .expect("medios");
        assert_eq!(&media[..], &[0x02, 0x00, i]);
        let audio = tokio::time::timeout(TIMEOUT, host_conn.read_audio_datagram())
            .await
            .expect("a tiempo")
            .expect("audio");
        assert_eq!((audio[1], audio[2]), (if i % 2 == 0 { 1 } else { 2 }, i));
    }
    assert_eq!(host_conn.datagrams_dropped(), (0, 0));
    assert_eq!(host_conn.audio_datagrams_dropped(), 0);

    // El stream de entrada lo abre el invitado y lo acepta el anfitrión.
    let accept = {
        let host_conn = host_conn.clone();
        tokio::spawn(async move { host_conn.accept_input().await })
    };
    let (mut send, _recv) = guest_conn.open_input().await.expect("stream de entrada");
    send.write_all(&stream_preamble(0xabcd))
        .await
        .expect("preámbulo");
    let (_send, mut recv) = tokio::time::timeout(TIMEOUT, accept)
        .await
        .expect("a tiempo")
        .expect("tarea")
        .expect("stream");
    let mut preamble = [0u8; 5];
    recv.read_exact(&mut preamble).await.expect("preámbulo");
    assert_eq!(preamble, stream_preamble(0xabcd));

    // Al cerrar, los dos lectores terminan con error.
    guest_conn.close("fin");
    let closed = tokio::time::timeout(TIMEOUT, host_conn.read_input_datagram())
        .await
        .expect("a tiempo");
    assert!(closed.is_err());
    assert!(
        tokio::time::timeout(TIMEOUT, host_conn.read_datagram())
            .await
            .expect("a tiempo")
            .is_err()
    );
    assert!(
        tokio::time::timeout(TIMEOUT, host_conn.read_audio_datagram())
            .await
            .expect("a tiempo")
            .is_err()
    );
}
