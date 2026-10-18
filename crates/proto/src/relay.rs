//! Tramas del relay de reenvío (sin IO).
//!
//! El relay no termina QUIC: el anfitrión y cada invitado hablan su conexión de pares
//! (`coroom-p2p/1`) de punta a punta y el relay solo copia los paquetes UDP de uno a otro, como un
//! servidor TURN (RFC 8656). Las tramas comparten el socket UDP del servidor con QUIC y se
//! distinguen por el primer byte: todo paquete QUIC lleva el bit fijo `0x40` (RFC 9000 §17; los
//! dos extremos desactivan su engrase, RFC 9287) y toda trama del relay empieza por `0b00xx_xxxx`.
//!
//! Para que el relay reenvíe, cada par se **liga** con el token que le dio la señalización sin
//! mandarlo nunca: [`Frame::Allocate`] con el identificador del token, el relay contesta con una
//! galleta atada a la dirección de origen ([`Frame::Challenge`], que prueba que el par recibe en
//! esa dirección), el par la firma con el token ([`Frame::Bind`]) y el relay confirma con su par
//! y su propia firma ([`Frame::Bound`]). Ligado, cada paquete QUIC viaja en un [`Frame::Data`] con
//! el par de destino (hacia el relay) o de origen (desde el relay): cinco bytes de más.

/// Primer byte de cada trama.
pub mod kind {
    /// Pedido de galleta.
    pub const ALLOCATE: u8 = 0x20;
    /// Galleta.
    pub const CHALLENGE: u8 = 0x21;
    /// Ligadura firmada.
    pub const BIND: u8 = 0x22;
    /// Ligadura aceptada.
    pub const BOUND: u8 = 0x23;
    /// Error.
    pub const ERROR: u8 = 0x24;
    /// Paquete QUIC de un par a otro.
    pub const DATA: u8 = 0x30;
    /// Sigue ligado.
    pub const REFRESH: u8 = 0x31;
    /// Respuesta a [`REFRESH`].
    pub const REFRESHED: u8 = 0x32;
}

/// Bytes del identificador de un token ([`Frame::Allocate`]).
pub const TOKEN_ID_LEN: usize = 16;
/// Bytes de una galleta.
pub const COOKIE_LEN: usize = 16;
/// Bytes de una firma.
pub const MAC_LEN: usize = 16;
/// Cabecera de un [`Frame::Data`]: tipo y par.
pub const DATA_HEADER: usize = 5;
/// Bytes de un [`Frame::Allocate`]: con relleno para no pedir más de lo que llega a contestarse
/// ([`Frame::Challenge`]): el relay nunca amplifica.
pub const ALLOCATE_LEN: usize = 1 + 2 + TOKEN_ID_LEN + COOKIE_LEN;

/// Si un paquete UDP es una trama del relay (y no QUIC).
pub fn is_relay_frame(packet: &[u8]) -> bool {
    packet.first().is_some_and(|byte| byte & 0xC0 == 0)
}

/// Por qué el relay rechaza una trama.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// Versión de protocolo distinta.
    Version,
    /// Token desconocido o retirado.
    UnknownToken,
    /// Galleta vencida o firma que no corresponde.
    BadSignature,
    /// La dirección no está ligada.
    NotBound,
    /// La sala agotó su cuota mensual.
    Quota,
    /// Desconocido (de una versión posterior).
    Other(u8),
}

impl ErrorCode {
    fn to_byte(self) -> u8 {
        match self {
            ErrorCode::Version => 1,
            ErrorCode::UnknownToken => 2,
            ErrorCode::BadSignature => 3,
            ErrorCode::NotBound => 4,
            ErrorCode::Quota => 5,
            ErrorCode::Other(code) => code,
        }
    }

    fn from_byte(byte: u8) -> Self {
        match byte {
            1 => ErrorCode::Version,
            2 => ErrorCode::UnknownToken,
            3 => ErrorCode::BadSignature,
            4 => ErrorCode::NotBound,
            5 => ErrorCode::Quota,
            other => ErrorCode::Other(other),
        }
    }
}

/// Trama del relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame<'a> {
    /// Par → relay: pide una galleta para el token `token_id`.
    Allocate {
        /// Versión de protocolo del par.
        version: u16,
        /// Identificador del token.
        token_id: [u8; TOKEN_ID_LEN],
    },
    /// Relay → par: galleta para `token_id` en la dirección de la que vino el pedido.
    Challenge {
        /// Identificador del token pedido.
        token_id: [u8; TOKEN_ID_LEN],
        /// Galleta.
        cookie: [u8; COOKIE_LEN],
    },
    /// Par → relay: la galleta firmada con el token.
    Bind {
        /// Versión de protocolo del par.
        version: u16,
        /// Identificador del token.
        token_id: [u8; TOKEN_ID_LEN],
        /// Galleta del [`Frame::Challenge`].
        cookie: [u8; COOKIE_LEN],
        /// Firma.
        mac: [u8; MAC_LEN],
    },
    /// Relay → par: ligado como `peer`; la firma prueba que contesta el relay del token.
    Bound {
        /// Galleta de la ligadura.
        cookie: [u8; COOKIE_LEN],
        /// Par dentro de la sala.
        peer: u32,
        /// Segundos sin tráfico tras los que el relay suelta la ligadura.
        lifetime_s: u16,
        /// Firma.
        mac: [u8; MAC_LEN],
    },
    /// Relay → par: rechazo.
    Error(ErrorCode),
    /// Un paquete QUIC: hacia el relay, `peer` es el destino; desde el relay, el origen.
    Data {
        /// Par de destino u origen.
        peer: u32,
        /// Paquete QUIC.
        packet: &'a [u8],
    },
    /// Par → relay: sigue ligado aunque no mande datos.
    Refresh,
    /// Relay → par: respuesta a [`Frame::Refresh`].
    Refreshed,
}

fn array<const N: usize>(bytes: &[u8], at: usize) -> Option<[u8; N]> {
    bytes.get(at..at + N)?.try_into().ok()
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(array(bytes, at)?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(array(bytes, at)?))
}

impl<'a> Frame<'a> {
    /// Lee una trama; `None` si no lo es o está truncada.
    pub fn parse(bytes: &'a [u8]) -> Option<Self> {
        let first = *bytes.first()?;
        Some(match first {
            kind::ALLOCATE if bytes.len() >= ALLOCATE_LEN => Frame::Allocate {
                version: u16_at(bytes, 1)?,
                token_id: array(bytes, 3)?,
            },
            kind::CHALLENGE => Frame::Challenge {
                token_id: array(bytes, 1)?,
                cookie: array(bytes, 1 + TOKEN_ID_LEN)?,
            },
            kind::BIND => Frame::Bind {
                version: u16_at(bytes, 1)?,
                token_id: array(bytes, 3)?,
                cookie: array(bytes, 3 + TOKEN_ID_LEN)?,
                mac: array(bytes, 3 + TOKEN_ID_LEN + COOKIE_LEN)?,
            },
            kind::BOUND => Frame::Bound {
                cookie: array(bytes, 1)?,
                peer: u32_at(bytes, 1 + COOKIE_LEN)?,
                lifetime_s: u16_at(bytes, 5 + COOKIE_LEN)?,
                mac: array(bytes, 7 + COOKIE_LEN)?,
            },
            kind::ERROR => Frame::Error(ErrorCode::from_byte(*bytes.get(1)?)),
            kind::DATA => Frame::Data {
                peer: u32_at(bytes, 1)?,
                packet: bytes.get(DATA_HEADER..)?,
            },
            kind::REFRESH => Frame::Refresh,
            kind::REFRESHED => Frame::Refreshed,
            _ => return None,
        })
    }

    /// Escribe la trama al final de `out`.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        match *self {
            Frame::Allocate { version, token_id } => {
                out.push(kind::ALLOCATE);
                out.extend_from_slice(&version.to_be_bytes());
                out.extend_from_slice(&token_id);
                out.extend_from_slice(&[0; COOKIE_LEN]);
            }
            Frame::Challenge { token_id, cookie } => {
                out.push(kind::CHALLENGE);
                out.extend_from_slice(&token_id);
                out.extend_from_slice(&cookie);
            }
            Frame::Bind {
                version,
                token_id,
                cookie,
                mac,
            } => {
                out.push(kind::BIND);
                out.extend_from_slice(&version.to_be_bytes());
                out.extend_from_slice(&token_id);
                out.extend_from_slice(&cookie);
                out.extend_from_slice(&mac);
            }
            Frame::Bound {
                cookie,
                peer,
                lifetime_s,
                mac,
            } => {
                out.push(kind::BOUND);
                out.extend_from_slice(&cookie);
                out.extend_from_slice(&peer.to_be_bytes());
                out.extend_from_slice(&lifetime_s.to_be_bytes());
                out.extend_from_slice(&mac);
            }
            Frame::Error(code) => {
                out.push(kind::ERROR);
                out.push(code.to_byte());
            }
            Frame::Data { peer, packet } => {
                out.extend_from_slice(&data_header(peer));
                out.extend_from_slice(packet);
            }
            Frame::Refresh => out.push(kind::REFRESH),
            Frame::Refreshed => out.push(kind::REFRESHED),
        }
    }

    /// La trama en un `Vec` nuevo.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }
}

/// Cabecera de un [`Frame::Data`] para `peer`.
pub fn data_header(peer: u32) -> [u8; DATA_HEADER] {
    let peer = peer.to_be_bytes();
    [kind::DATA, peer[0], peer[1], peer[2], peer[3]]
}

/// El par de un [`Frame::Data`] (sin comprobar el resto).
pub fn data_peer(frame: &[u8]) -> Option<u32> {
    (frame.first() == Some(&kind::DATA))
        .then(|| u32_at(frame, 1))
        .flatten()
}

/// Cambia el par de un [`Frame::Data`] en su sitio (el relay lo reescribe al reenviar).
pub fn set_data_peer(frame: &mut [u8], peer: u32) {
    if let Some(field) = frame.get_mut(1..DATA_HEADER) {
        field.copy_from_slice(&peer.to_be_bytes());
    }
}

/// Lo que firma el par en un [`Frame::Bind`].
pub fn bind_message(token_id: &[u8; TOKEN_ID_LEN], cookie: &[u8; COOKIE_LEN]) -> Vec<u8> {
    let mut message = b"coroom relay bind".to_vec();
    message.extend_from_slice(token_id);
    message.extend_from_slice(cookie);
    message
}

/// Lo que firma el relay en un [`Frame::Bound`].
pub fn bound_message(cookie: &[u8; COOKIE_LEN], peer: u32, lifetime_s: u16) -> Vec<u8> {
    let mut message = b"coroom relay bound".to_vec();
    message.extend_from_slice(cookie);
    message.extend_from_slice(&peer.to_be_bytes());
    message.extend_from_slice(&lifetime_s.to_be_bytes());
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn round_trip(frame: Frame<'_>) {
        let bytes = frame.encode();
        assert!(is_relay_frame(&bytes), "{frame:?}");
        assert_eq!(Frame::parse(&bytes), Some(frame));
    }

    proptest! {
        #[test]
        fn every_frame_round_trips(
            version in any::<u16>(),
            token_id in any::<[u8; TOKEN_ID_LEN]>(),
            cookie in any::<[u8; COOKIE_LEN]>(),
            mac in any::<[u8; MAC_LEN]>(),
            peer in any::<u32>(),
            lifetime_s in any::<u16>(),
            code in any::<u8>().prop_filter("conocido o no", |code| *code != 0),
            packet in proptest::collection::vec(any::<u8>(), 0..1500),
        ) {
            round_trip(Frame::Allocate { version, token_id });
            round_trip(Frame::Challenge { token_id, cookie });
            round_trip(Frame::Bind { version, token_id, cookie, mac });
            round_trip(Frame::Bound { cookie, peer, lifetime_s, mac });
            round_trip(Frame::Error(ErrorCode::from_byte(code)));
            round_trip(Frame::Data { peer, packet: &packet });
            round_trip(Frame::Refresh);
            round_trip(Frame::Refreshed);
        }

        #[test]
        fn parsing_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
            let _ = Frame::parse(&bytes);
        }
    }

    #[test]
    fn quic_packets_are_never_relay_frames() {
        // Cabecera larga (0x80) o corta con el bit fijo (0x40): todo paquete QUIC lo lleva.
        for first in 0x40..=0xFFu8 {
            assert!(!is_relay_frame(&[first, 0, 0]));
        }
        assert!(!is_relay_frame(&[]));
    }

    #[test]
    fn the_relay_never_answers_more_than_it_is_asked() {
        let allocate = Frame::Allocate {
            version: 1,
            token_id: [7; TOKEN_ID_LEN],
        }
        .encode();
        let challenge = Frame::Challenge {
            token_id: [7; TOKEN_ID_LEN],
            cookie: [9; COOKIE_LEN],
        }
        .encode();
        assert_eq!(allocate.len(), ALLOCATE_LEN);
        assert!(challenge.len() <= allocate.len());
        // Truncado, no se acepta: sin relleno no hay galleta.
        assert_eq!(Frame::parse(&allocate[..allocate.len() - 1]), None);
    }

    #[test]
    fn the_data_peer_is_rewritten_in_place() {
        let mut frame = Frame::Data {
            peer: 3,
            packet: &[0x40, 1, 2],
        }
        .encode();
        assert_eq!(data_peer(&frame), Some(3));
        set_data_peer(&mut frame, 0);
        assert_eq!(
            Frame::parse(&frame),
            Some(Frame::Data {
                peer: 0,
                packet: &[0x40, 1, 2]
            })
        );
    }
}
