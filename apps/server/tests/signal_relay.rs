//! Servidor real en puertos efímeros: señalización por WebTransport (`/signal`) y relay, en el
//! modo desarrollo (`COROOM_DEV_TOKEN`, sin cuentas; las cuentas están en `accounts.rs`).
//!
//! - Dos clientes crean y se unen a una sala, reciben `Observed` (la dirección de su `Endpoint` de
//!   medios), intercambian `Mls` y `Candidates`, y el anfitrión ve salir al invitado.
//! - Relay: un anfitrión y dos invitados (uno por `Connector` con `--force-relay`) se ligan al
//!   relay y abren sus conexiones de pares a través de él, de punta a punta; entre invitados no
//!   pasa nada, y al expulsar o cerrar la sala el relay suelta la ligadura.

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use coroom_server::{Config, Server};
use coroom_transport::connect::{Established, virtual_addr};
use coroom_transport::crypto::Identity;
use coroom_transport::proto::PROTOCOL_VERSION;
use coroom_transport::proto::control::{ControlMessage, Hello, Role};
use coroom_transport::proto::media::{ObjectHeader, Track};
use coroom_transport::proto::signal::{ErrorCode, HOST_PEER_ID, SignalMessage, is_valid_room_code};
use coroom_transport::relay::{BindingState, RelayBinding};
use coroom_transport::{
    Connector, Endpoint, EndpointOptions, Path, PeerConnection, RelayInfo, ServerTrust,
    SignalClient, TransportError,
};
use tokio::sync::oneshot;

const TIMEOUT: Duration = Duration::from_secs(10);
const TOKEN: &str = "change-me";

struct Running {
    url: String,
    udp: SocketAddr,
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

/// Límite por invitado del relay de estos tests.
const RELAY_MAX_KBPS: u32 = 400_000;

async fn start_server() -> Running {
    let dir = tempfile::tempdir().expect("directorio temporal");
    let any: SocketAddr = "127.0.0.1:0".parse().expect("dirección");
    let mut config = Config::self_signed(any, any, dir.path().to_owned());
    config.dev_token = Some(TOKEN.to_owned());
    config.max_guests_per_room = 2;
    // Holgado: los tests de medios no lo rozan, y el `RelayToken` tiene que anunciarlo.
    config.relay_max_kbps = RELAY_MAX_KBPS;
    // `/health` por HTTP/1.1 en claro (el TCP con TLS se prueba en `http3.rs`).
    config.tcp_plain = true;
    let server = Server::bind(&config).await.expect("arranque del servidor");
    let udp = server.udp_addr();
    let url = format!("https://127.0.0.1:{}", udp.port());
    let trust = ServerTrust::Fingerprint(server.fingerprint().to_owned());
    let tcp = server.tcp_addr();
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let task = tokio::spawn(server.run(async move {
        let _ = stop_rx.await;
    }));
    Running {
        url,
        udp,
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

/// `RelayToken` → datos de la ligadura con el relay, que está donde `client` llegó al servidor.
fn relay_info(client: &Client, message: SignalMessage) -> RelayInfo {
    let SignalMessage::RelayToken { token, max_kbps } = message else {
        panic!("se esperaba RelayToken: {message:?}");
    };
    RelayInfo {
        addr: client.signal.server_addr(),
        token,
        max_kbps,
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
            avatar: None,
            device: None,
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
    for token in [&host_token, &guest_token] {
        assert!(
            matches!(
                token,
                SignalMessage::RelayToken {
                    max_kbps: RELAY_MAX_KBPS,
                    ..
                }
            ),
            "el relay anuncia su límite: {token:?}"
        );
    }
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
    }
}

/// Liga `client` al relay con su `RelayToken`.
async fn bind(client: &Client, token: SignalMessage) -> RelayBinding {
    tokio::time::timeout(
        TIMEOUT,
        client
            .endpoint
            .bind_relay(relay_info(client, token).params()),
    )
    .await
    .expect("ligadura a tiempo")
    .expect("ligadura")
}

/// Conecta `host` con el invitado `peer` por el relay, abriendo desde los dos lados a la vez
/// como las sesiones; el invitado con `force_relay` y los candidatos directos del anfitrión.
async fn relayed(
    host: &Client,
    guest: &Client,
    peer: u32,
    force_relay: bool,
) -> (PeerConnection, Established) {
    let host_candidates = vec![host.endpoint.local_addr().expect("dirección")];
    let host_connector = Connector::new(host.endpoint.clone());
    let from_host =
        host_connector.connect(guest.identity.public_key(), &[], Some(virtual_addr(peer)));
    let guest_connector = Connector::new(guest.endpoint.clone()).force_relay(force_relay);
    let candidates: &[SocketAddr] = if force_relay { &host_candidates } else { &[] };
    let from_guest = guest_connector.connect(
        host.identity.public_key(),
        candidates,
        Some(virtual_addr(HOST_PEER_ID)),
    );
    let (host_side, guest_side) =
        tokio::time::timeout(TIMEOUT, async { tokio::join!(from_host, from_guest) })
            .await
            .expect("conexiones a tiempo");
    let host_side = host_side.expect("el anfitrión conecta por el relay");
    let guest_side = guest_side.expect("el invitado conecta por el relay");
    assert_eq!(host_side.path, Path::Relayed);
    assert_eq!(guest_side.path, Path::Relayed);
    (host_side.connection, guest_side)
}

async fn datagram(connection: &PeerConnection) -> Bytes {
    tokio::time::timeout(TIMEOUT, connection.read_datagram())
        .await
        .expect("datagrama a tiempo")
        .expect("datagrama")
}

/// Espera a que el relay suelte la ligadura de `binding`; `poke` manda algo por ella para que
/// el relay conteste que no la conoce (sin tráfico se vería al refrescarla, a los 10 s).
async fn lost(binding: &RelayBinding, poke: impl Fn()) -> BindingState {
    let mut watch = binding.watch();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let state = *watch.borrow_and_update();
        if matches!(state, BindingState::Lost(_)) {
            return state;
        }
        poke();
        let _ = tokio::time::timeout_at(
            deadline.min(tokio::time::Instant::now() + Duration::from_millis(100)),
            watch.changed(),
        )
        .await;
        assert!(
            tokio::time::Instant::now() < deadline,
            "el relay debería soltar la ligadura"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_relay_forwards_end_to_end_connections_without_terminating_them() {
    let server = start_server().await;
    let mut host = client(&server).await;
    let mut guest_a = client(&server).await;
    let mut guest_b = client(&server).await;

    let (code, host_token, token_a, peer_a) = room(&mut host, &mut guest_a).await;
    guest_b
        .signal
        .send(&SignalMessage::JoinRoom {
            code,
            identity_pk: guest_b.identity.public_key(),
            key_package: Vec::new(),
        })
        .await
        .expect("JoinRoom");
    let SignalMessage::Joined {
        your_peer_id: peer_b,
        ..
    } = next(&mut guest_b).await
    else {
        panic!("B no entró");
    };
    assert_ne!(peer_a, peer_b);
    let token_b = next(&mut guest_b).await;

    // Cada uno se liga con su token y el relay le dice qué par es.
    let host_binding = bind(&host, host_token).await;
    let binding_a = bind(&guest_a, token_a).await;
    let binding_b = bind(&guest_b, token_b).await;
    assert_eq!(
        host_binding.state(),
        BindingState::Bound { peer: HOST_PEER_ID }
    );
    assert_eq!(binding_a.state(), BindingState::Bound { peer: peer_a });
    assert_eq!(binding_b.state(), BindingState::Bound { peer: peer_b });
    let relay_addr = host_binding.relay().expect("dirección del relay");
    assert_eq!(relay_addr.port(), server.udp.port());

    // A por la carrera normal (sin candidatos directos) y B con `--force-relay`, aunque tenga
    // los candidatos directos del anfitrión.
    let (host_a, guest_a_side) = relayed(&host, &guest_a, peer_a, false).await;
    let (host_b, guest_b_side) = relayed(&host, &guest_b, peer_b, true).await;
    assert_eq!(
        guest_b_side.relay_reason.as_deref(),
        Some("se pidió usar el relay")
    );
    let (relay_a, relay_b) = (guest_a_side.connection, guest_b_side.connection);
    // La conexión es de punta a punta: cada extremo autentica al otro con su identidad, y el
    // relay, que no tiene sus claves, no puede hacerse pasar por ninguno.
    assert_eq!(host_a.peer_key(), Some(guest_a.identity.public_key()));
    assert_eq!(host_b.peer_key(), Some(guest_b.identity.public_key()));
    assert_eq!(relay_a.peer_key(), Some(host.identity.public_key()));
    assert_eq!(relay_a.remote_address(), relay_addr);
    assert_eq!(host_a.remote_address(), relay_addr);

    // Control: cada invitado abre el suyo y el anfitrión lo acepta por la conexión de ese invitado.
    let accept_a = {
        let (host_a, host_hello) = (host_a.clone(), hello(&host.identity, Role::Host));
        tokio::spawn(async move { host_a.accept_control(host_hello).await })
    };
    let accept_b = {
        let (host_b, host_hello) = (host_b.clone(), hello(&host.identity, Role::Host));
        tokio::spawn(async move { host_b.accept_control(host_hello).await })
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
    let host_control_a = tokio::time::timeout(TIMEOUT, accept_a)
        .await
        .expect("a tiempo")
        .expect("tarea")
        .expect("control de A en el anfitrión");
    let mut host_control_b = tokio::time::timeout(TIMEOUT, accept_b)
        .await
        .expect("a tiempo")
        .expect("tarea")
        .expect("control de B en el anfitrión");
    assert_eq!(
        host_control_a.peer_hello().identity_pk,
        guest_a.identity.public_key()
    );

    // Datagramas: cada invitado recibe solo lo que el anfitrión le manda a él.
    for index in 0..3 {
        host_a
            .send_datagram(object(7, index, 3))
            .expect("datagrama a A");
    }
    host_b
        .send_datagram(object(8, 0, 1))
        .expect("datagrama a B");
    let mut got = Vec::new();
    for _ in 0..3 {
        got.push(ObjectHeader::parse(&datagram(&relay_a).await).expect("cabecera"));
    }
    got.sort_by_key(|header| header.object);
    assert!(got.iter().all(|header| header.group == 7), "{got:?}");
    assert_eq!(datagram(&relay_b).await, object(8, 0, 1));
    relay_b.send_datagram(object(1, 0, 1)).expect("datagrama");
    assert_eq!(datagram(&host_b).await, object(1, 0, 1));

    // Control en los dos sentidos, de punta a punta.
    host_control_a
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
    assert_eq!(
        tokio::time::timeout(TIMEOUT, host_control_b.receiver.recv())
            .await
            .expect("a tiempo")
            .expect("mensaje"),
        Some(ControlMessage::Ping { t: 5 })
    );
    // Un stream grande pasa entero (el relay no corta los paquetes de tamaño completo).
    let big: Vec<u8> = (0..400_000u32).map(|i| i as u8).collect();
    let receiver = {
        let relay_a = relay_a.clone();
        tokio::spawn(async move {
            let mut stream = relay_a.quinn().accept_uni().await.expect("stream");
            stream.read_to_end(1 << 20).await.expect("lectura")
        })
    };
    let mut stream = host_a.quinn().open_uni().await.expect("stream");
    stream.write_all(&big).await.expect("escritura");
    stream.finish().expect("fin");
    let received = tokio::time::timeout(TIMEOUT, receiver)
        .await
        .expect("a tiempo")
        .expect("tarea");
    assert!(received == big, "el stream llega entero");

    // Entre invitados no pasa nada: el relay solo une al anfitrión con los suyos.
    guest_b.endpoint.admit(guest_a.identity.public_key());
    let between = tokio::time::timeout(
        Duration::from_secs(2),
        guest_a
            .endpoint
            .connect_peer(virtual_addr(peer_b), guest_b.identity.public_key()),
    )
    .await;
    assert!(
        !matches!(between, Ok(Ok(_))),
        "un invitado no debe llegar a otro por el relay"
    );

    let health = health(&server).await;
    assert!(
        health["relay_bytes"].as_u64().unwrap_or(0) > 400_000,
        "{health}"
    );
    assert_eq!(health["rooms"], 1);

    // Un token inventado no se liga.
    let bogus = coroom_transport::relay::RelayParams {
        addr: relay_addr,
        token: vec![0; 32],
    };
    let other = client(&server).await;
    assert!(other.endpoint.bind_relay(bogus).await.is_err());

    // Al expulsar a B, el relay suelta su ligadura y deja de pasarle nada.
    host.signal
        .send(&SignalMessage::Kick { peer_id: peer_b })
        .await
        .expect("Kick");
    let state = lost(&binding_b, || {
        let _ = relay_b.send_datagram(object(2, 0, 1));
    })
    .await;
    assert!(matches!(state, BindingState::Lost(_)), "{state:?}");
    let _ = host_b.send_datagram(object(9, 0, 1));
    let after = tokio::time::timeout(Duration::from_millis(500), relay_b.read_datagram()).await;
    assert!(
        after.is_err(),
        "nada llega a B tras la expulsión: {after:?}"
    );
    // A sigue.
    host_a.send_datagram(object(10, 0, 1)).expect("datagrama");
    assert_eq!(datagram(&relay_a).await, object(10, 0, 1));

    // Al cerrar el anfitrión su señalización, la sala se cierra y el relay suelta a A.
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
    let state = lost(&binding_a, || {
        let _ = relay_a.send_datagram(object(3, 0, 1));
    })
    .await;
    assert!(matches!(state, BindingState::Lost(_)), "{state:?}");
    drop(host_binding);
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

/// Vídeo del anfitrión con `payload` bytes de carga: objeto `index` de un grupo de `count`.
fn video_object(group: u32, index: u16, count: u16, payload: usize) -> Bytes {
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
        payload_len: payload as u16,
        epoch_hint: 1,
    };
    let mut bytes = header.to_bytes().to_vec();
    bytes.resize(bytes.len() + payload, 0x5A);
    Bytes::from(bytes)
}

/// MEDICIÓN: cuánto vídeo reenvía el relay de un anfitrión a un invitado en localhost.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "medición"]
async fn relay_forwarding_rate() {
    const RATE_KBPS: u64 = 300_000;
    const PAYLOAD: usize = 1100;
    const SECONDS: u64 = 3;
    let server = start_server().await;
    let mut host = client(&server).await;
    let mut guest = client(&server).await;
    let (_code, host_token, guest_token, peer) = room(&mut host, &mut guest).await;
    let _host_binding = bind(&host, host_token).await;
    let _guest_binding = bind(&guest, guest_token).await;
    let (host_relay, guest_side) = relayed(&host, &guest, peer, false).await;
    let guest_relay = guest_side.connection;
    host_relay.set_target_kbps(RATE_KBPS as u32);
    guest_relay.set_target_kbps(RATE_KBPS as u32);
    let datagram_len = video_object(0, 0, 1, PAYLOAD).len() as u64;
    let per_ms = (RATE_KBPS * 1000 / 8 / 1000 / datagram_len).max(1);
    let reader = {
        let guest_relay = guest_relay.clone();
        tokio::spawn(async move {
            let mut bytes = 0u64;
            let mut first = None;
            let mut last = std::time::Instant::now();
            while let Ok(Ok(datagram)) =
                tokio::time::timeout(Duration::from_millis(500), guest_relay.read_datagram()).await
            {
                first.get_or_insert_with(std::time::Instant::now);
                last = std::time::Instant::now();
                bytes += datagram.len() as u64;
            }
            (bytes, first.map(|first| last - first))
        })
    };
    let start = std::time::Instant::now();
    let mut sent = 0u64;
    let mut dropped = 0u64;
    let mut tick = tokio::time::interval(Duration::from_millis(1));
    let mut group = 0u32;
    while start.elapsed() < Duration::from_secs(SECONDS) {
        tick.tick().await;
        for index in 0..per_ms {
            let datagram = video_object(group, index as u16, per_ms as u16, PAYLOAD);
            match host_relay.send_datagram(datagram) {
                Ok(()) => sent += datagram_len,
                Err(_) => dropped += 1,
            }
        }
        group += 1;
    }
    let elapsed = start.elapsed().as_secs_f64();
    let (received, span) = reader.await.expect("lector");
    let span = span.map_or(elapsed, |span| span.as_secs_f64());
    println!(
        "enviado {:.1} Mbit/s ({dropped} sin enviar), recibido {:.1} Mbit/s ({:.1} % de lo enviado)",
        sent as f64 * 8.0 / elapsed / 1e6,
        received as f64 * 8.0 / span / 1e6,
        received as f64 * 100.0 / sent.max(1) as f64
    );
    server.stop().await;
}
