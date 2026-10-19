//! Arnés para el fuzzing (`fuzz/`, `feature` `fuzzing`): el despacho de rutas, la señalización y
//! las tramas del relay con un estado de servidor completo (SQLite en un directorio temporal,
//! certificado autofirmado, relay y cuotas) pero **sin red**, para que cada entrada del fuzzer se
//! resuelva en memoria. El único socket es uno UDP en localhost por el que el relay contesta y
//! reenvía (a direcciones de loopback donde no escucha nadie).
//!
//! No forma parte del servidor normal: solo se compila con la `feature` `fuzzing`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::Method;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use coroom_transport::proto::PROTOCOL_VERSION;
use coroom_transport::proto::codec;
use coroom_transport::proto::signal::SignalMessage;

use crate::auth::AuthState;
use crate::config::Config;
use crate::probe::UdpStatus;
use crate::quota::Quota;
use crate::relay::{Grant, Relay, Token};
use crate::routes::{Request, dispatch};
use crate::signal::{Hub, HubConfig};
use crate::tls::{CertResolver, TlsMaterial};
use crate::{ServerError, ServerState, open_database};
use coroom_transport::proto::control::Role;
use coroom_transport::proto::relay::Frame;
use coroom_transport::proto::signal::{HOST_PEER_ID, PeerId};
use coroom_transport::relay::{bind_mac, token_id};
use coroom_transport::socket::{HookedSocket, Segment, SegmentHook};

/// Token de desarrollo del arnés (ficticio).
pub const DEV_TOKEN: &str = "change-me";

/// Contraseña del administrador del arnés (ficticia).
pub const ADMIN_PASSWORD: &str = "change-me-please";

/// Métodos que elige el primer byte de una petición.
pub const METHODS: [Method; 5] = [
    Method::GET,
    Method::POST,
    Method::PATCH,
    Method::DELETE,
    Method::PUT,
];

/// Servidor sin red para el fuzzer.
pub struct Harness {
    runtime: tokio::runtime::Runtime,
    state: Arc<ServerState>,
    admin_token: String,
    relay_socket: Arc<HookedSocket<Relay>>,
    /// Tokens del anfitrión y del invitado de la sala del relay.
    relay_tokens: [Token; 2],
}

/// Sala del relay del arnés.
const RELAY_ROOM: u64 = 1;
/// Par del invitado de esa sala.
const RELAY_GUEST: PeerId = 1;

/// Dirección de origen `index` de las tramas del relay: cuatro en loopback, en el puerto de
/// descarte (nadie escucha, así que lo que el relay mande ahí se pierde).
pub fn relay_source(index: u8) -> SocketAddr {
    (Ipv4Addr::new(127, 0, 0, 2 + index % 4), 9).into()
}

impl std::fmt::Debug for Harness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Harness").finish_non_exhaustive()
    }
}

fn request(method: Method, path: &str, auth: Option<&str>, body: &[u8]) -> Request {
    let (path, query) = match path.split_once('?') {
        Some((path, query)) => (path.to_owned(), Some(query.to_owned())),
        None => (path.to_owned(), None),
    };
    Request {
        method,
        path,
        query,
        authorization: auth.map(|token| format!("Bearer {token}")),
        if_none_match: None,
        body: Bytes::copy_from_slice(body),
        remote: IpAddr::from(Ipv4Addr::LOCALHOST),
    }
}

impl Harness {
    /// Arnés con los datos en `dir`: crea el primer administrador e inicia su sesión web.
    pub fn new(dir: &Path) -> Result<Harness, ServerError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| ServerError::Config(err.to_string()))?;
        let any: SocketAddr = (Ipv4Addr::LOCALHOST, 4433).into();
        let mut config = Config::self_signed(any, any, dir.to_owned());
        config.dev_token = Some(DEV_TOKEN.to_owned());
        config.public_host = "coroom.example.com".to_owned();
        let state = runtime.block_on(async {
            let db = open_database(&config).await?;
            let material = TlsMaterial::load(&config)?;
            let certs = Arc::new(CertResolver::new(&material)?);
            let quota = Arc::new(Quota::new(db.clone(), config.quota_bytes()));
            let relay = Relay::with_limits(Some(Arc::clone(&quota)), config.relay_max_kbps);
            let hub = Hub::with_accounts(
                HubConfig {
                    dev_token: config.dev_token.clone(),
                    max_guests: config.max_guests_per_room,
                    relay_enabled: true,
                },
                Arc::clone(&relay),
                db.clone(),
                Arc::clone(&quota),
            );
            Ok::<_, ServerError>(Arc::new(ServerState {
                config: config.clone(),
                db,
                auth: AuthState::default(),
                hub,
                relay,
                quota,
                certs,
                udp: Arc::new(UdpStatus::default()),
                udp_addr: any,
                tcp_addr: any,
            }))
        })?;
        let relay_socket = {
            let _guard = runtime.enter();
            let socket = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
                .and_then(|socket| HookedSocket::new(socket, Arc::clone(&state.relay)))
                .map_err(|err| ServerError::Config(err.to_string()))?;
            socket
        };
        let grant = |peer, role| Grant {
            room: RELAY_ROOM,
            peer,
            role,
            identity_pk: [0; 32],
            host_user: None,
        };
        let (Some(host), Some(guest)) = (
            state.relay.grant(grant(HOST_PEER_ID, Role::Host)),
            state.relay.grant(grant(RELAY_GUEST, Role::Guest)),
        ) else {
            return Err(ServerError::Config("sin aleatoriedad".to_owned()));
        };
        let mut harness = Harness {
            runtime,
            state,
            admin_token: String::new(),
            relay_socket,
            relay_tokens: [host, guest],
        };
        let setup = format!(r#"{{"username":"admin","password":"{ADMIN_PASSWORD}"}}"#);
        harness.request(&Method::POST, "/api/v1/setup", None, setup.as_bytes());
        let (status, body) =
            harness.request(&Method::POST, "/api/v1/auth/login", None, setup.as_bytes());
        if status == 200
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body)
            && let Some(token) = value.get("token").and_then(|token| token.as_str())
        {
            harness.admin_token = token.to_owned();
        }
        Ok(harness)
    }

    /// Salas abiertas en la señalización.
    pub fn open_rooms(&self) -> usize {
        self.state.hub.rooms()
    }

    /// Token de la sesión web del administrador.
    pub fn admin_token(&self) -> &str {
        &self.admin_token
    }

    /// Resuelve una petición y devuelve el estado y el cuerpo.
    pub fn request(
        &self,
        method: &Method,
        path: &str,
        auth: Option<&str>,
        body: &[u8],
    ) -> (u16, Vec<u8>) {
        let request = request(method.clone(), path, auth, body);
        let response = self.runtime.block_on(dispatch(&self.state, request));
        (response.status.as_u16(), response.body)
    }

    /// Una sesión de señalización que recibe `bytes` (con un `Auth` de desarrollo válido delante si
    /// `authenticate`) y se cierra; devuelve los mensajes que respondió el servidor.
    pub fn signal(&self, authenticate: bool, bytes: &[u8]) -> Vec<SignalMessage> {
        let hub = Arc::clone(&self.state.hub);
        let remote: SocketAddr = (Ipv4Addr::LOCALHOST, 50_000).into();
        self.runtime.block_on(async move {
            let (client, server) = tokio::io::duplex(1 << 16);
            let session = tokio::spawn(hub.run(server, remote));
            let (mut reader, mut writer) = tokio::io::split(client);
            let reading = tokio::spawn(async move {
                let mut out = Vec::new();
                let _ = reader.read_to_end(&mut out).await;
                out
            });
            if authenticate
                && let Ok(auth) = codec::encode(&SignalMessage::Auth {
                    protocol_version: PROTOCOL_VERSION,
                    token: DEV_TOKEN.to_owned(),
                    device_pk: [0; 32],
                })
            {
                let _ = writer.write_all(&auth).await;
            }
            let _ = writer.write_all(bytes).await;
            let _ = writer.shutdown().await;
            drop(writer);
            let _ = tokio::time::timeout(Duration::from_secs(5), session).await;
            let replies = tokio::time::timeout(Duration::from_secs(5), reading)
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default();
            let mut buffer = codec::FrameBuffer::new();
            buffer.extend(&replies);
            let mut messages = Vec::new();
            while let Ok(Some(message)) = buffer.next_message::<SignalMessage>() {
                messages.push(message);
            }
            messages
        })
    }

    /// Una trama del relay que llega de [`relay_source`]`(from)`, como si viniese de la red;
    /// `true` si era del relay (si no, iría a QUIC).
    pub fn relay_frame(&self, from: u8, bytes: &[u8]) -> bool {
        let _guard = self.runtime.enter();
        let io = self.relay_socket.io();
        let mut datagram = bytes.to_vec();
        let consumed = !matches!(
            self.state
                .relay
                .segment(io, relay_source(from), None, &mut datagram),
            Segment::Quic
        );
        self.state.relay.flush(io);
        consumed
    }

    /// Liga [`relay_source`]`(from)` como el anfitrión (`guest == false`) o el invitado de la
    /// sala, con un `Bind` válido (la galleta de la época actual y la firma con su token).
    pub fn relay_bind(&self, from: u8, guest: bool) {
        let token = &self.relay_tokens[usize::from(guest)];
        let token_id = token_id(token);
        let cookie = self
            .state
            .relay
            .current_cookie(relay_source(from), &token_id);
        let bind = Frame::Bind {
            version: PROTOCOL_VERSION,
            token_id,
            cookie,
            mac: bind_mac(token, &token_id, &cookie),
        };
        self.relay_frame(from, &bind.encode());
    }

    /// Pares ligados al relay.
    pub fn relay_bound(&self) -> usize {
        self.state.relay.bound_peers()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_harness_serves_routes_and_signalling_without_sockets() {
        let dir = tempfile::tempdir().expect("directorio temporal");
        let harness = Harness::new(dir.path()).expect("arnés");
        assert!(
            !harness.admin_token().is_empty(),
            "sesión del administrador"
        );
        let (status, _) =
            harness.request(&Method::GET, "/api/v1/me", Some(harness.admin_token()), b"");
        assert_eq!(status, 200);
        let (status, _) = harness.request(&Method::GET, "/api/v1/me", Some("00ff"), b"");
        assert_eq!(status, 401);
        let create = codec::encode(&SignalMessage::CreateRoom {
            identity_pk: [1; 32],
            name: "Sala".to_owned(),
        })
        .expect("codificar");
        let replies = harness.signal(true, &create);
        assert!(
            replies
                .iter()
                .any(|message| matches!(message, SignalMessage::RoomCreated { .. })),
            "{replies:?}"
        );
        assert_eq!(harness.open_rooms(), 0, "la sala se cierra con la sesión");
        let replies = harness.signal(false, &create);
        assert!(
            matches!(replies.first(), Some(SignalMessage::Error { .. })),
            "{replies:?}"
        );

        // Relay: las tramas que no son suyas van a QUIC; un `Bind` válido liga, uno inventado no.
        assert!(!harness.relay_frame(0, &[0x40, 1, 2, 3]));
        assert!(harness.relay_frame(0, &[0x30, 0, 0, 0, 1, 0x40]));
        harness.relay_bind(0, false);
        harness.relay_bind(1, true);
        assert_eq!(harness.relay_bound(), 2);
        assert!(harness.relay_frame(0, &[0x30, 0, 0, 0, 1, 0x40, 0x41]));
        let forged = Frame::Bind {
            version: PROTOCOL_VERSION,
            token_id: [7; 16],
            cookie: [0; 16],
            mac: [0; 16],
        };
        harness.relay_frame(2, &forged.encode());
        assert_eq!(harness.relay_bound(), 2);
    }
}
