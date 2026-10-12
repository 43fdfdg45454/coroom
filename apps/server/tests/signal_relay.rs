//! Servidor real en puertos efímeros: señalización por WebTransport (`/signal`) y relay, en el
//! modo desarrollo (`COROOM_DEV_TOKEN`, sin cuentas; las cuentas están en `accounts.rs`).
//!
//! - Dos clientes crean y se unen a una sala, reciben `Observed` (la dirección de su `Endpoint` de
//!   medios), intercambian `Mls` y `Candidates`, y el anfitrión ve salir al invitado.
//! - Relay: un anfitrión y dos invitados (uno por `Connector` con `--force-relay`); los datagramas
//!   del anfitrión llegan a los dos y un `Nack` se responde desde la caché sin llegar al anfitrión.

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use coroom_server::{Config, Server};
use coroom_transport::crypto::Identity;
use coroom_transport::proto::PROTOCOL_VERSION;
use coroom_transport::proto::control::{ControlMessage, Hello, Role};
use coroom_transport::proto::media::{ObjectHeader, Track};
use coroom_transport::proto::signal::{ErrorCode, HOST_PEER_ID, SignalMessage, is_valid_room_code};
use coroom_transport::{
    Connector, Endpoint, EndpointOptions, Path, RelayInfo, ServerTrust, SignalClient,
    TransportError, connect,
};
use tokio::sync::oneshot;

const TIMEOUT: Duration = Duration::from_secs(10);
const TOKEN: &str = "change-me";

struct Running {
    url: String,
    trust: ServerTrust,
    tcp: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<Result<(), coroom_server::ServerError>>,
    _dir: tempfile::TempDir,
}

impl Running {
    async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = tokio::time::timeout(TIMEOUT, &mut self.task).await;
    }
}

async fn start_server() -> Running {
    let dir = tempfile::tempdir().expect("directorio temporal");
    let any: SocketAddr = "127.0.0.1:0".parse().expect("dirección");
    let mut config = Config::self_signed(any, any, dir.path().to_owned());
    config.dev_token = Some(TOKEN.to_owned());
    config.max_guests_per_room = 2;
    // `/health` por HTTP/1.1 en claro (el TCP con TLS se prueba en `http3.rs`).
    config.tcp_plain = true;
    let server = Server::bind(&config).await.expect("arranque del servidor");
    let url = format!("https://127.0.0.1:{}", server.udp_addr().port());
    let trust = ServerTrust::Fingerprint(server.fingerprint().to_owned());
    let tcp = server.tcp_addr();
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let task = tokio::spawn(server.run(async move {
        let _ = stop_rx.await;
    }));
    Running {
        url,
        trust,
        tcp,
        stop: Some(stop_tx),
        task,
        _dir: dir,
    }
}

struct Client {
    identity: Identity,
    endpoint: Endpoint,
    signal: SignalClient,
}

async fn client(server: &Running) -> Client {
    let identity = Identity::generate().expect("identidad");
    let endpoint = Endpoint::bind(
        &identity,
        Some("127.0.0.1:0".parse().expect("dirección")),
        EndpointOptions::default(),
    )
    .expect("endpoint");
    let signal = tokio::time::timeout(
        TIMEOUT,
        SignalClient::connect(&server.url, TOKEN, &endpoint, &server.trust),
    )
    .await
    .expect("señalización a tiempo")
    .expect("señalización");
    Client {
        identity,
        endpoint,
        signal,
    }
}

async fn next(client: &mut Client) -> SignalMessage {
    tokio::time::timeout(TIMEOUT, client.signal.next_event())
        .await
        .expect("mensaje a tiempo")
        .expect("mensaje")
        .expect("sesión abierta")
}

/// `RelayToken` → datos de conexión al relay.
fn relay_info(message: SignalMessage, server: &Running, role: Role) -> RelayInfo {
    let SignalMessage::RelayToken { token, addr } = message else {
        panic!("se esperaba RelayToken: {message:?}");
    };
    RelayInfo {
        addr,
        server_name: "localhost".to_owned(),
        trust: server.trust.clone(),
        token,
        role,
    }
}

async fn health(server: &Running) -> serde_json::Value {
    let stream = tokio::net::TcpStream::connect(server.tcp)
        .await
        .expect("conexión TCP");
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
            .await
            .expect("handshake HTTP/1.1");
    tokio::spawn(connection);
    let request = http::Request::get("/health")
        .header(http::header::HOST, "localhost")
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .expect("petición");
    let response = sender.send_request(request).await.expect("respuesta");
    let body = http_body_util::BodyExt::collect(response.into_body())
        .await
        .expect("cuerpo")
        .to_bytes();
    serde_json::from_slice(&body).expect("JSON")
}

/// Crea una sala con `host` y mete a `guest`; devuelve los `RelayToken` de los dos.
async fn room(
    host: &mut Client,
    guest: &mut Client,
) -> (String, SignalMessage, SignalMessage, u32) {
    host.signal
        .send(&SignalMessage::CreateRoom {
            identity_pk: host.identity.public_key(),
            name: "Sala de prueba".to_owned(),
        })
        .await
        .expect("CreateRoom");
    let SignalMessage::RoomCreated { code } = next(host).await else {
        panic!("se esperaba RoomCreated");
    };
    assert!(is_valid_room_code(&code), "{code}");
    let host_token = next(host).await;

    guest
        .signal
        .send(&SignalMessage::JoinRoom {
            code: code.to_lowercase(),
            identity_pk: guest.identity.public_key(),
            key_package: b"key-package".to_vec(),
        })
        .await
        .expect("JoinRoom");
    let SignalMessage::Joined {
        host_pk,
        your_peer_id,
        ..
    } = next(guest).await
    else {
        panic!("se esperaba Joined");
    };
    assert_eq!(host_pk, host.identity.public_key());
    assert_ne!(your_peer_id, HOST_PEER_ID);
    let guest_token = next(guest).await;
    assert_eq!(
        next(host).await,
        SignalMessage::PeerJoined {
            peer_id: your_peer_id,
            identity_pk: guest.identity.public_key(),
            key_package: b"key-package".to_vec(),
            user: 0,
            name: "desarrollo".to_owned(),
        }
    );
    (code, host_token, guest_token, your_peer_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signaling_creates_joins_and_relays_messages() {
    let server = start_server().await;
    let mut host = client(&server).await;
    let mut guest = client(&server).await;

    // `Observed` es la dirección del `Endpoint` de medios: la señalización sale del mismo socket.
    assert_eq!(
        host.signal.observed(),
        host.endpoint.local_addr().expect("dirección")
    );
    assert_eq!(
        guest.signal.observed(),
        guest.endpoint.local_addr().expect("dirección")
    );

    let (code, host_token, guest_token, guest_id) = room(&mut host, &mut guest).await;
    assert!(matches!(host_token, SignalMessage::RelayToken { .. }));
    assert!(matches!(guest_token, SignalMessage::RelayToken { .. }));
    assert_eq!(health(&server).await["rooms"], 1);

    // MLS: anfitrión → invitado y del invitado a todos (el anfitrión).
    host.signal
        .send(&SignalMessage::Mls {
            from_peer: 99,
            to_peer: Some(guest_id),
            payload: b"welcome".to_vec(),
        })
        .await
        .expect("Mls");
    assert_eq!(
        next(&mut guest).await,
        SignalMessage::Mls {
            from_peer: HOST_PEER_ID,
            to_peer: Some(guest_id),
            payload: b"welcome".to_vec(),
        }
    );
    guest
        .signal
        .send(&SignalMessage::Mls {
            from_peer: 99,
            to_peer: None,
            payload: b"commit".to_vec(),
        })
        .await
        .expect("Mls");
    assert_eq!(
        next(&mut host).await,
        SignalMessage::Mls {
            from_peer: guest_id,
            to_peer: None,
            payload: b"commit".to_vec(),
        }
    );

    // Candidatos en los dos sentidos.
    let host_candidates = vec![host.endpoint.local_addr().expect("dirección")];
    host.signal
        .send(&SignalMessage::Candidates {
            from_peer: 0,
            to_peer: guest_id,
            addrs: host_candidates.clone(),
        })
        .await
        .expect("Candidates");
    assert_eq!(
        next(&mut guest).await,
        SignalMessage::Candidates {
            from_peer: HOST_PEER_ID,
            to_peer: guest_id,
            addrs: host_candidates,
        }
    );
    guest
        .signal
        .send(&SignalMessage::Candidates {
            from_peer: 0,
            to_peer: HOST_PEER_ID,
            addrs: vec![guest.signal.observed()],
        })
        .await
        .expect("Candidates");
    let SignalMessage::Candidates { from_peer, .. } = next(&mut host).await else {
        panic!("se esperaba Candidates");
    };
    assert_eq!(from_peer, guest_id);

    // A un par que no existe: error, la sesión sigue.
    host.signal
        .send(&SignalMessage::Candidates {
            from_peer: 0,
            to_peer: 42,
            addrs: Vec::new(),
        })
        .await
        .expect("Candidates");
    assert!(matches!(
        next(&mut host).await,
        SignalMessage::Error {
            code: ErrorCode::PeerNotFound,
            ..
        }
    ));

    // La sala admite 2 invitados como mucho (configurado): el tercero no entra.
    let mut second = client(&server).await;
    let mut third = client(&server).await;
    for (joiner, expect_ok) in [(&mut second, true), (&mut third, false)] {
        joiner
            .signal
            .send(&SignalMessage::JoinRoom {
                code: code.clone(),
                identity_pk: joiner.identity.public_key(),
                key_package: Vec::new(),
            })
            .await
            .expect("JoinRoom");
        let reply = next(joiner).await;
        if expect_ok {
            assert!(matches!(reply, SignalMessage::Joined { .. }), "{reply:?}");
        } else {
            assert!(
                matches!(
                    reply,
                    SignalMessage::Error {
                        code: ErrorCode::RoomFull,
                        ..
                    }
                ),
                "{reply:?}"
            );
        }
    }

    // Cuando el invitado se va, el anfitrión recibe `PeerLeft`.
    guest.signal.close();
    loop {
        match next(&mut host).await {
            SignalMessage::PeerLeft { peer_id } if peer_id == guest_id => break,
            _ => continue,
        }
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_token_and_unknown_room_are_rejected() {
    let server = start_server().await;
    let identity = Identity::generate().expect("identidad");
    let endpoint = Endpoint::bind(
        &identity,
        Some("127.0.0.1:0".parse().expect("dirección")),
        EndpointOptions::default(),
    )
    .expect("endpoint");
    let result = SignalClient::connect(&server.url, "otro", &endpoint, &server.trust).await;
    assert!(
        matches!(
            result,
            Err(TransportError::Rejected {
                code: ErrorCode::Unauthorized,
                ..
            })
        ),
        "{result:?}"
    );
    // Con una huella equivocada ni siquiera se abre la conexión.
    let wrong = ServerTrust::Fingerprint("00".repeat(32));
    assert!(
        SignalClient::connect(&server.url, TOKEN, &endpoint, &wrong)
            .await
            .is_err()
    );

    let mut guest = client(&server).await;
    guest
        .signal
        .send(&SignalMessage::JoinRoom {
            code: "ABCDEF".to_owned(),
            identity_pk: guest.identity.public_key(),
            key_package: Vec::new(),
        })
        .await
        .expect("JoinRoom");
    assert!(matches!(
        next(&mut guest).await,
        SignalMessage::Error {
            code: ErrorCode::RoomNotFound,
            ..
        }
    ));
    server.stop().await;
}

fn object(group: u32, index: u16, count: u16) -> Bytes {
    let payload = [group as u8, index as u8, 0xAA];
    let header = ObjectHeader {
        track: Track::Video,
        flags: if index + 1 == count {
            ObjectHeader::LAST_OBJECT
        } else {
            0
        },
        group,
        object: index,
        object_count: count,
        payload_len: payload.len() as u16,
        epoch_hint: 1,
    };
    let mut bytes = header.to_bytes().to_vec();
    bytes.extend_from_slice(&payload);
    Bytes::from(bytes)
}

fn hello(identity: &Identity, role: Role) -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        identity_pk: identity.public_key(),
        role,
        relay_token: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_fans_out_and_answers_nacks_from_its_cache() {
    let server = start_server().await;
    let mut host = client(&server).await;
    let mut guest_a = client(&server).await;
    let mut guest_b = client(&server).await;

    let (code, host_token, token_a, _) = room(&mut host, &mut guest_a).await;
    guest_b
        .signal
        .send(&SignalMessage::JoinRoom {
            code,
            identity_pk: guest_b.identity.public_key(),
            key_package: Vec::new(),
        })
        .await
        .expect("JoinRoom");
    assert!(matches!(
        next(&mut guest_b).await,
        SignalMessage::Joined { .. }
    ));
    let token_b = next(&mut guest_b).await;

    let host_relay =
        connect::connect_relay(&host.endpoint, &relay_info(host_token, &server, Role::Host))
            .await
            .expect("anfitrión en el relay");
    assert_eq!(host_relay.path(), Path::Relayed);
    let relay_a = connect::connect_relay(
        &guest_a.endpoint,
        &relay_info(token_a, &server, Role::Guest),
    )
    .await
    .expect("invitado A en el relay");
    // El invitado B pasa por el `Connector` con `--force-relay`.
    let relay_b = Connector::new(guest_b.endpoint.clone())
        .force_relay(true)
        .connect(
            host.identity.public_key(),
            &[],
            Some(&relay_info(token_b, &server, Role::Guest)),
        )
        .await
        .expect("invitado B en el relay");
    assert_eq!(relay_b.path, Path::Relayed);
    let relay_b = relay_b.connection;

    // Controles: cada invitado abre el suyo y el anfitrión los acepta por su única conexión.
    let accept = {
        let host_relay = host_relay.clone();
        let host_hello = hello(&host.identity, Role::Host);
        tokio::spawn(async move {
            let first = host_relay.accept_control(host_hello.clone()).await?;
            let second = host_relay.accept_control(host_hello).await?;
            Ok::<_, TransportError>((first, second))
        })
    };
    let mut control_a = relay_a
        .open_control(hello(&guest_a.identity, Role::Guest))
        .await
        .expect("control A");
    let control_b = relay_b
        .open_control(hello(&guest_b.identity, Role::Guest))
        .await
        .expect("control B");
    assert_eq!(control_a.peer_hello().role, Role::Host);
    let (first, second) = tokio::time::timeout(TIMEOUT, accept)
        .await
        .expect("a tiempo")
        .expect("tarea")
        .expect("controles del anfitrión");
    let mut host_controls = [first, second];
    let mut seen: Vec<[u8; 32]> = host_controls
        .iter()
        .map(|control| control.peer_hello().identity_pk)
        .collect();
    seen.sort();
    let mut expected = vec![guest_a.identity.public_key(), guest_b.identity.public_key()];
    expected.sort();
    assert_eq!(
        seen, expected,
        "el relay fija la identidad de cada invitado"
    );

    // Abanico: un grupo de 3 objetos del anfitrión llega a los dos invitados.
    for index in 0..3 {
        host_relay
            .send_datagram(object(7, index, 3))
            .expect("datagrama");
    }
    for guest in [&relay_a, &relay_b] {
        let mut got = Vec::new();
        for _ in 0..3 {
            let datagram = tokio::time::timeout(TIMEOUT, guest.read_datagram())
                .await
                .expect("datagrama a tiempo")
                .expect("datagrama");
            got.push(ObjectHeader::parse(&datagram).expect("cabecera").object);
        }
        got.sort();
        assert_eq!(got, [0, 1, 2]);
    }

    // `Nack` de un grupo en caché: lo responde el relay y el anfitrión no lo ve.
    control_a
        .sender
        .send(&ControlMessage::Nack {
            track: Track::Video,
            group: 7,
            missing: vec![1],
        })
        .expect("Nack");
    let resent = tokio::time::timeout(TIMEOUT, relay_a.read_datagram())
        .await
        .expect("reenvío a tiempo")
        .expect("datagrama");
    assert_eq!(resent, object(7, 1, 3));
    // `Nack` de un grupo que no está: llega al anfitrión (por el control de A).
    control_a
        .sender
        .send(&ControlMessage::Nack {
            track: Track::Video,
            group: 99,
            missing: vec![0],
        })
        .expect("Nack");
    let index_a = host_controls
        .iter()
        .position(|control| control.peer_hello().identity_pk == guest_a.identity.public_key())
        .expect("control de A");
    let forwarded = tokio::time::timeout(TIMEOUT, host_controls[index_a].receiver.recv())
        .await
        .expect("a tiempo")
        .expect("mensaje");
    assert_eq!(
        forwarded,
        Some(ControlMessage::Nack {
            track: Track::Video,
            group: 99,
            missing: vec![0]
        }),
        "el Nack del grupo 7 no debe llegar al anfitrión, el del 99 sí"
    );

    // Control anfitrión → invitado y del invitado B al anfitrión.
    host_controls[index_a]
        .sender
        .send(&ControlMessage::TargetBitrate { kbps: 8000 })
        .expect("envío");
    assert_eq!(
        tokio::time::timeout(TIMEOUT, control_a.receiver.recv())
            .await
            .expect("a tiempo")
            .expect("mensaje"),
        Some(ControlMessage::TargetBitrate { kbps: 8000 })
    );
    control_b
        .sender
        .send(&ControlMessage::Ping { t: 5 })
        .expect("envío");
    let index_b = 1 - index_a;
    assert_eq!(
        tokio::time::timeout(TIMEOUT, host_controls[index_b].receiver.recv())
            .await
            .expect("a tiempo")
            .expect("mensaje"),
        Some(ControlMessage::Ping { t: 5 })
    );

    // Datagramas de un invitado al anfitrión.
    relay_b.send_datagram(object(1, 0, 1)).expect("datagrama");
    let upstream = tokio::time::timeout(TIMEOUT, host_relay.read_datagram())
        .await
        .expect("a tiempo")
        .expect("datagrama");
    assert_eq!(upstream, object(1, 0, 1));

    let health = health(&server).await;
    assert!(health["relay_bytes"].as_u64().unwrap_or(0) > 0, "{health}");
    assert_eq!(health["rooms"], 1);

    // Un token inventado no entra.
    let bogus = RelayInfo {
        token: vec![0; 32],
        ..relay_info(
            SignalMessage::RelayToken {
                token: Vec::new(),
                addr: host_relay.remote_address(),
            },
            &server,
            Role::Guest,
        )
    };
    assert!(
        connect::connect_relay(&guest_b.endpoint, &bogus)
            .await
            .is_err()
    );

    // Al cerrar el anfitrión su señalización, la sala se cierra y los invitados lo saben.
    host.signal.close();
    loop {
        match next(&mut guest_a).await {
            SignalMessage::Error {
                code: ErrorCode::RoomClosed,
                ..
            } => break,
            _ => continue,
        }
    }
    let closed = tokio::time::timeout(TIMEOUT, relay_a.closed()).await;
    assert!(closed.is_ok(), "el relay cierra las conexiones de la sala");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_host_can_kick_a_guest() {
    let server = start_server().await;
    let mut host = client(&server).await;
    let mut guest = client(&server).await;
    let (_, _, _, guest_id) = room(&mut host, &mut guest).await;

    // Un invitado no puede expulsar.
    guest
        .signal
        .send(&SignalMessage::Kick {
            peer_id: HOST_PEER_ID,
        })
        .await
        .expect("Kick");
    assert!(matches!(
        next(&mut guest).await,
        SignalMessage::Error {
            code: ErrorCode::BadRequest,
            ..
        }
    ));

    host.signal
        .send(&SignalMessage::Kick { peer_id: guest_id })
        .await
        .expect("Kick");
    assert_eq!(
        next(&mut host).await,
        SignalMessage::PeerLeft { peer_id: guest_id }
    );
    assert!(matches!(
        next(&mut guest).await,
        SignalMessage::Error {
            code: ErrorCode::Kicked,
            ..
        }
    ));
    // Y su sesión se cierra.
    let end = tokio::time::timeout(TIMEOUT, guest.signal.next_event()).await;
    assert!(
        matches!(end, Ok(Ok(None)) | Ok(Err(_))),
        "la sesión debe terminar: {end:?}"
    );
    server.stop().await;
}
