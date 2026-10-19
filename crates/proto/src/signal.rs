//! Mensajes de señalización entre los clientes y el servidor, codificados con `postcard`.
//!
//! Viajan enmarcados por longitud ([`crate::codec`]) en el stream bidireccional de la sesión
//! WebTransport `/signal`. El primer mensaje del cliente es siempre [`SignalMessage::Auth`] con el
//! token de sesión de su cuenta y la clave pública de su dispositivo (el servidor solo la
//! acepta si está vinculada al usuario del token) o, en un servidor en modo desarrollo, con el
//! token `COROOM_DEV_TOKEN`; el servidor responde con [`SignalMessage::Observed`] o con
//! [`SignalMessage::Error`].
//!
//! Con cuentas, la identidad de `CreateRoom` y `JoinRoom` debe ser la del dispositivo
//! autenticado; el servidor añade el usuario, su nombre visible y la versión de su avatar a
//! `PeerJoined` y `Joined`, reparte la presencia de los amigos (`Presence`, y `Avatar` cuando uno
//! cambia de avatar) y las invitaciones a salas (`Invite` → `Invited`, solo
//! entre amigos), y avisa con `QuotaExceeded` o `RelayDisabled` cuando la sala no tiene relay.
//! A cada sesión de una cuenta le dice qué salas tiene abiertas esa cuenta (`OwnRooms`), para entrar
//! en la propia desde otro dispositivo, y a quien hospeda, el dispositivo de quien entra con una
//! cuenta que ya está en la sala (`PeerJoined::device`).
//!
//! El servidor enruta `Mls` y `Candidates` sin interpretarlos y rellena `from_peer` con el par que
//! los envió (el valor que pone el cliente se ignora).

use std::fmt;
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

/// Identificador de un par dentro de una sala. El anfitrión es siempre [`HOST_PEER_ID`].
pub type PeerId = u32;

/// Identificador de un usuario del servidor. `0` es el usuario anónimo del modo desarrollo.
pub type UserId = u64;

/// Identificador del anfitrión en su sala.
pub const HOST_PEER_ID: PeerId = 0;

/// Longitud de los códigos de sala.
pub const ROOM_CODE_LEN: usize = 6;

/// Alfabeto de los códigos de sala: mayúsculas y dígitos sin los ambiguos `0`, `O`, `1` e `I`
/// (32 símbolos, 5 bits por carácter).
pub const ROOM_CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Indica si `code` tiene la forma de un código de sala (no si la sala existe).
pub fn is_valid_room_code(code: &str) -> bool {
    code.len() == ROOM_CODE_LEN && code.bytes().all(|b| ROOM_CODE_ALPHABET.contains(&b))
}

/// Código de error de la señalización.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorCode {
    /// Falta el `Auth` inicial o el token no es válido.
    Unauthorized,
    /// La versión del protocolo del cliente no es la del servidor.
    ProtocolVersion,
    /// No existe una sala con ese código.
    RoomNotFound,
    /// La sala ya tiene el máximo de invitados.
    RoomFull,
    /// La sala se cerró (el anfitrión salió).
    RoomClosed,
    /// El destinatario no está en la sala.
    PeerNotFound,
    /// El anfitrión expulsó a este par.
    Kicked,
    /// Solo se puede invitar a una sala a un amigo.
    NotFriend,
    /// El usuario no está conectado a la señalización.
    UserOffline,
    /// El mensaje no tiene sentido en el estado actual de la sesión.
    BadRequest,
    /// Error interno del servidor.
    Internal,
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ErrorCode::Unauthorized => "no autorizado",
            ErrorCode::ProtocolVersion => "versión de protocolo incompatible",
            ErrorCode::RoomNotFound => "la sala no existe",
            ErrorCode::RoomFull => "la sala está llena",
            ErrorCode::RoomClosed => "la sala se ha cerrado",
            ErrorCode::PeerNotFound => "el par no está en la sala",
            ErrorCode::Kicked => "el anfitrión te ha expulsado",
            ErrorCode::NotFriend => "no sois amigos",
            ErrorCode::UserOffline => "el usuario no está conectado",
            ErrorCode::BadRequest => "petición no válida",
            ErrorCode::Internal => "error interno del servidor",
        })
    }
}

/// Lo que hace un usuario conectado, para la lista de amigos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Activity {
    /// Conectado, sin sala.
    #[default]
    Idle,
    /// Tiene una sala abierta como anfitrión.
    Hosting,
    /// Está en la sala de otro.
    Playing,
}

impl Activity {
    /// Las tres, para los tests y las capturas.
    pub const ALL: [Activity; 3] = [Activity::Idle, Activity::Hosting, Activity::Playing];
}

/// Mensaje de señalización.
///
/// postcard codifica la variante por su posición: `Auth` (la primera) y `Error` no se mueven,
/// para que un cliente y un servidor de versiones distintas se entiendan al menos el
/// `Error { code: ProtocolVersion }`; las variantes nuevas van al final.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignalMessage {
    /// Cliente → servidor, primer mensaje: versión, token de sesión (o el de desarrollo,
    /// `COROOM_DEV_TOKEN`, si el servidor está en ese modo) y clave pública del dispositivo.
    Auth {
        /// [`crate::PROTOCOL_VERSION`] del cliente.
        protocol_version: u16,
        /// Token de sesión de `POST /api/v1/auth/login/complete` (o el de desarrollo).
        token: String,
        /// Clave pública Ed25519 de la identidad del dispositivo; debe ser la vinculada al token.
        device_pk: [u8; 32],
    },
    /// Cliente → servidor: crea una sala de la que es anfitrión.
    CreateRoom {
        /// Clave pública de la identidad del anfitrión.
        identity_pk: [u8; 32],
        /// Nombre visible de la sala.
        name: String,
    },
    /// Servidor → anfitrión: sala creada.
    RoomCreated {
        /// Código de [`ROOM_CODE_LEN`] caracteres de [`ROOM_CODE_ALPHABET`].
        code: String,
    },
    /// Cliente → servidor: entra en una sala como invitado.
    JoinRoom {
        /// Código de la sala.
        code: String,
        /// Clave pública de la identidad del invitado.
        identity_pk: [u8; 32],
        /// `KeyPackage` MLS serializado (el servidor no lo interpreta).
        key_package: Vec<u8>,
    },
    /// Servidor → invitado: ha entrado en la sala.
    Joined {
        /// Identificador interno de la sala.
        room_id: u64,
        /// Clave pública del anfitrión.
        host_pk: [u8; 32],
        /// Identificador del invitado en la sala.
        your_peer_id: PeerId,
        /// Usuario del anfitrión.
        host_user: UserId,
        /// Nombre visible del anfitrión.
        host_name: String,
        /// Versión del avatar del anfitrión (`crate::api::avatar_version`; `None`: no tiene).
        host_avatar: Option<u64>,
    },
    /// Servidor → anfitrión: un invitado ha entrado.
    PeerJoined {
        /// Identificador del invitado.
        peer_id: PeerId,
        /// Clave pública de su identidad.
        identity_pk: [u8; 32],
        /// Su `KeyPackage` MLS.
        key_package: Vec<u8>,
        /// Su usuario.
        user: UserId,
        /// Su nombre visible.
        name: String,
        /// Versión de su avatar (`None`: no tiene).
        avatar: Option<u64>,
        /// Nombre del dispositivo con el que entra, solo si su cuenta ya está en la sala (es la
        /// del anfitrión o la de otro invitado): con él se distinguen las dos filas.
        device: Option<String>,
    },
    /// Servidor → miembros: un par ha salido de la sala.
    PeerLeft {
        /// Par que salió.
        peer_id: PeerId,
    },
    /// Candidatos de red para la apertura de NAT, reenviados a `to_peer`.
    Candidates {
        /// Par que los envía (lo rellena el servidor).
        from_peer: PeerId,
        /// Destinatario.
        to_peer: PeerId,
        /// Direcciones UDP del `Endpoint` de medios del remitente.
        addrs: Vec<SocketAddr>,
    },
    /// Servidor → cliente: dirección de origen de la sesión de señalización (candidato reflejado).
    Observed {
        /// Dirección UDP tal como la ve el servidor.
        addr: SocketAddr,
    },
    /// Mensaje MLS opaco (commit, welcome, aplicación) reenviado por el servidor.
    Mls {
        /// Par que lo envía (lo rellena el servidor).
        from_peer: PeerId,
        /// Destinatario; `None` = todos los demás miembros de la sala.
        to_peer: Option<PeerId>,
        /// Mensaje MLS serializado.
        payload: Vec<u8>,
    },
    /// Servidor → cliente: credencial para el relay de la sala.
    RelayToken {
        /// Secreto con el que se liga al relay (`relay::Frame::Allocate` y `Bind`); nunca viaja
        /// tal cual, solo su id y firmas.
        token: Vec<u8>,
        /// Dirección UDP del relay.
        addr: SocketAddr,
        /// Límite del relay por invitado y sentido en kbit/s (`COROOM_RELAY_MAX_KBPS`; `0` = sin
        /// límite). Lo que pase se descarta, así que el anfitrión no apunta por encima.
        max_kbps: u32,
    },
    /// Anfitrión → servidor: expulsa a un invitado (cierra su señalización y su relay).
    Kick {
        /// Invitado expulsado.
        peer_id: PeerId,
    },
    /// Cliente → servidor: invita a un amigo conectado a la sala `code` (de la que el remitente es
    /// anfitrión).
    Invite {
        /// Usuario invitado.
        to_user: UserId,
        /// Código de la sala.
        code: String,
    },
    /// Servidor → cliente: un amigo te invita a su sala.
    Invited {
        /// Usuario que invita.
        from_user: UserId,
        /// Su nombre visible.
        from_name: String,
        /// Código de la sala.
        code: String,
    },
    /// Servidor → cliente: un amigo se ha conectado o desconectado de la señalización (al
    /// autenticarse llega una por cada amigo conectado).
    Presence {
        /// El amigo.
        user: UserId,
        /// Si está conectado.
        online: bool,
    },
    /// Servidor → cliente: qué hace un amigo conectado (al autenticarse llega una por cada amigo
    /// que no está simplemente disponible; después, cada vez que cambia).
    Activity {
        /// El amigo.
        user: UserId,
        /// Lo que hace.
        activity: Activity,
    },
    /// Cliente → servidor: pide entrar en la sala que un amigo tiene abierta. El servidor se lo
    /// hace llegar al anfitrión como `JoinRequested`; si acepta, invita (`Invite`) y si no,
    /// `JoinDecline`.
    JoinRequest {
        /// El amigo anfitrión.
        to_user: UserId,
    },
    /// Servidor → anfitrión: un amigo pide entrar en tu sala.
    JoinRequested {
        /// Quien pide entrar.
        from_user: UserId,
        /// Su nombre visible.
        from_name: String,
    },
    /// Cliente (anfitrión) → servidor: no deja entrar a quien lo pidió.
    JoinDecline {
        /// Quien pidió entrar.
        to_user: UserId,
    },
    /// Servidor → quien pidió entrar: el anfitrión no lo deja entrar ahora.
    JoinDeclined {
        /// El anfitrión.
        from_user: UserId,
        /// Su nombre visible.
        from_name: String,
    },
    /// Servidor → miembros de una sala: el anfitrión agotó su cuota mensual del relay; la sala
    /// sigue solo en directo (se envía en lugar de `RelayToken` o al cortar el relay).
    QuotaExceeded {
        /// Texto en español para mostrar.
        message: String,
    },
    /// Servidor → miembros de una sala: el servidor no tiene relay (`COROOM_RELAY_ENABLED=false`);
    /// se envía en lugar de `RelayToken`.
    RelayDisabled,
    /// Error; tras los de autenticación y expulsión el servidor cierra la sesión.
    Error {
        /// Código.
        code: ErrorCode,
        /// Texto en español para mostrar.
        message: String,
    },
    /// Servidor → cliente: un amigo, alguien con quien compartes sala u otra sesión tuya ha
    /// cambiado de avatar o lo ha quitado (`None`).
    Avatar {
        /// El usuario.
        user: UserId,
        /// Versión nueva (`crate::api::avatar_version`).
        avatar: Option<u64>,
    },
    /// Servidor → cliente: alguien acaba de pedirte amistad (`POST /friends/requests`); la
    /// lista de amigos se vuelve a pedir por la API.
    FriendRequested {
        /// Quien la pide.
        from_user: UserId,
        /// Su nombre visible.
        from_name: String,
    },
    /// Servidor → cada sesión de una cuenta: las salas que la cuenta tiene abiertas como
    /// anfitrión desde sus otros dispositivos (al autenticarse, aunque no haya ninguna, y cada vez
    /// que se abre o se cierra una; la que hospeda la propia sesión no va). Con ella se entra con
    /// un toque en la propia sala desde otro dispositivo.
    OwnRooms {
        /// Las salas, de la más antigua a la más nueva.
        rooms: Vec<OwnRoom>,
    },
}

/// Una sala abierta por la propia cuenta ([`SignalMessage::OwnRooms`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnRoom {
    /// Código de la sala.
    pub code: String,
    /// Nombre del dispositivo que la hospeda.
    pub device: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{FrameBuffer, decode, encode};
    use proptest::prelude::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    fn addr() -> impl Strategy<Value = SocketAddr> {
        prop_oneof![
            (any::<u32>(), any::<u16>())
                .prop_map(|(ip, port)| SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), port)),
            (any::<u128>(), any::<u16>())
                .prop_map(|(ip, port)| SocketAddr::new(IpAddr::V6(Ipv6Addr::from(ip)), port)),
        ]
    }

    fn bytes() -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(any::<u8>(), 0..256)
    }

    fn message() -> impl Strategy<Value = SignalMessage> {
        prop_oneof![
            (any::<u16>(), ".{0,40}", any::<[u8; 32]>()).prop_map(
                |(protocol_version, token, device_pk)| SignalMessage::Auth {
                    protocol_version,
                    token,
                    device_pk,
                }
            ),
            (any::<[u8; 32]>(), ".{0,40}")
                .prop_map(|(identity_pk, name)| SignalMessage::CreateRoom { identity_pk, name }),
            "[A-Z2-9]{6}".prop_map(|code| SignalMessage::RoomCreated { code }),
            ("[A-Z2-9]{6}", any::<[u8; 32]>(), bytes()).prop_map(
                |(code, identity_pk, key_package)| SignalMessage::JoinRoom {
                    code,
                    identity_pk,
                    key_package
                }
            ),
            (
                any::<u64>(),
                any::<[u8; 32]>(),
                any::<u32>(),
                any::<u64>(),
                ".{0,40}",
                prop::option::of(any::<u64>())
            )
                .prop_map(
                    |(room_id, host_pk, your_peer_id, host_user, host_name, host_avatar)| {
                        SignalMessage::Joined {
                            room_id,
                            host_pk,
                            your_peer_id,
                            host_user,
                            host_name,
                            host_avatar,
                        }
                    }
                ),
            (
                any::<u32>(),
                any::<[u8; 32]>(),
                bytes(),
                any::<u64>(),
                ".{0,40}",
                prop::option::of(any::<u64>()),
                prop::option::of(".{0,40}")
            )
                .prop_map(
                    |(peer_id, identity_pk, key_package, user, name, avatar, device)| {
                        SignalMessage::PeerJoined {
                            peer_id,
                            identity_pk,
                            key_package,
                            user,
                            name,
                            avatar,
                            device,
                        }
                    }
                ),
            prop::collection::vec(("[A-Z2-9]{6}", ".{0,40}"), 0..4).prop_map(|rooms| {
                SignalMessage::OwnRooms {
                    rooms: rooms
                        .into_iter()
                        .map(|(code, device)| OwnRoom { code, device })
                        .collect(),
                }
            }),
            (any::<u64>(), "[A-Z2-9]{6}")
                .prop_map(|(to_user, code)| SignalMessage::Invite { to_user, code }),
            (any::<u64>(), ".{0,40}", "[A-Z2-9]{6}").prop_map(|(from_user, from_name, code)| {
                SignalMessage::Invited {
                    from_user,
                    from_name,
                    code,
                }
            }),
            (any::<u64>(), any::<bool>())
                .prop_map(|(user, online)| SignalMessage::Presence { user, online }),
            (any::<u64>(), prop::option::of(any::<u64>()))
                .prop_map(|(user, avatar)| SignalMessage::Avatar { user, avatar }),
            (any::<u64>(), prop::sample::select(Activity::ALL.to_vec()))
                .prop_map(|(user, activity)| SignalMessage::Activity { user, activity }),
            any::<u64>().prop_map(|to_user| SignalMessage::JoinRequest { to_user }),
            (any::<u64>(), ".{0,40}").prop_map(|(from_user, from_name)| {
                SignalMessage::JoinRequested {
                    from_user,
                    from_name,
                }
            }),
            any::<u64>().prop_map(|to_user| SignalMessage::JoinDecline { to_user }),
            (any::<u64>(), ".{0,40}").prop_map(|(from_user, from_name)| {
                SignalMessage::JoinDeclined {
                    from_user,
                    from_name,
                }
            }),
            (any::<u64>(), ".{0,40}").prop_map(|(from_user, from_name)| {
                SignalMessage::FriendRequested {
                    from_user,
                    from_name,
                }
            }),
            ".{0,40}".prop_map(|message| SignalMessage::QuotaExceeded { message }),
            Just(SignalMessage::RelayDisabled),
            any::<u32>().prop_map(|peer_id| SignalMessage::PeerLeft { peer_id }),
            (
                any::<u32>(),
                any::<u32>(),
                prop::collection::vec(addr(), 0..8)
            )
                .prop_map(|(from_peer, to_peer, addrs)| SignalMessage::Candidates {
                    from_peer,
                    to_peer,
                    addrs
                }),
            addr().prop_map(|addr| SignalMessage::Observed { addr }),
            (any::<u32>(), prop::option::of(any::<u32>()), bytes()).prop_map(
                |(from_peer, to_peer, payload)| SignalMessage::Mls {
                    from_peer,
                    to_peer,
                    payload
                }
            ),
            (bytes(), addr(), any::<u32>()).prop_map(|(token, addr, max_kbps)| {
                SignalMessage::RelayToken {
                    token,
                    addr,
                    max_kbps,
                }
            }),
            any::<u32>().prop_map(|peer_id| SignalMessage::Kick { peer_id }),
            ".{0,40}".prop_map(|message| SignalMessage::Error {
                code: ErrorCode::RoomFull,
                message
            }),
        ]
    }

    proptest! {
        #[test]
        fn signal_messages_round_trip(message in message()) {
            let framed = encode(&message).expect("codificar");
            let mut buffer = FrameBuffer::new();
            buffer.extend(&framed);
            prop_assert_eq!(buffer.next_message::<SignalMessage>(), Ok(Some(message)));
        }

        #[test]
        fn garbage_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
            let _ = decode::<SignalMessage>(&bytes);
        }
    }

    /// `Auth` y `Error` conservan su posición entre versiones del protocolo: un cliente viejo
    /// tiene que poder leer el `Error { code: ProtocolVersion }` con el que se le rechaza.
    #[test]
    fn auth_and_error_keep_their_wire_index() {
        let error = SignalMessage::Error {
            code: ErrorCode::ProtocolVersion,
            message: "x".to_owned(),
        };
        assert_eq!(
            postcard::to_stdvec(&error).expect("codifica"),
            [22, 1, 1, b'x']
        );
        let auth = SignalMessage::Auth {
            protocol_version: 1,
            token: String::new(),
            device_pk: [0; 32],
        };
        assert_eq!(postcard::to_stdvec(&auth).expect("codifica")[0], 0);
    }

    #[test]
    fn room_code_alphabet_has_no_ambiguous_characters() {
        for ambiguous in *b"0O1I" {
            assert!(!ROOM_CODE_ALPHABET.contains(&ambiguous));
        }
        let unique: std::collections::HashSet<u8> = ROOM_CODE_ALPHABET.iter().copied().collect();
        assert_eq!(unique.len(), 32);
        assert!(is_valid_room_code("ABC234"));
        assert!(!is_valid_room_code("ABC23O"));
        assert!(!is_valid_room_code("ABC23"));
        assert!(!is_valid_room_code("abc234"));
    }
}
