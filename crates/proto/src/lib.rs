//! Protocolo de coroom: cabeceras de medios y mensajes de control.
//!
//! Esta crate no hace IO ni depende de ninguna otra crate del workspace (`docs/ARQUITECTURA.md` §5). Fija
//! la versión del protocolo, los identificadores ALPN que comparte el único socket UDP del
//! servidor y los pares (`docs/ARQUITECTURA.md` §4), la cabecera de medios v2 con el modelo de Media over
//! QUIC ([`media::ObjectHeader`]) y los mensajes `postcard` de control ([`control`]) y de
//! señalización ([`signal`]), enmarcados por longitud con [`codec`], los eventos de entrada del
//! invitado ([`input`]), la cabecera de los paquetes de audio ([`audio`]) y los
//! tipos JSON de la API de cuentas del servidor ([`api`], con [`hex`]).

pub mod api;
pub mod audio;
pub mod codec;
pub mod control;
pub mod hex;
pub mod hid;
pub mod input;
pub mod media;
pub mod signal;

/// Versión del protocolo de coroom que hablan cliente y servidor.
///
/// Se publica en `GET /version` del servidor y se incrementa con cada cambio incompatible. La 2
/// introduce la cabecera de objetos v2, el control y la señalización; la 3, la
/// negociación de códec y el cursor por el control; la 4, la entrada del invitado
/// ([`input`]) y sus mensajes de control (`InputPermissions`, `HostFrameSize`, `InputReport`); la
/// 5, el audio del juego y la voz ([`audio`], `AudioConfig`, `VoicePermission` y `VoiceState`);
/// la 6, las cuentas en la señalización (`Auth` con token de sesión y clave del dispositivo,
/// usuario y nombre en `Joined` y `PeerJoined`, `Invite`, `Invited`, `Presence`, `QuotaExceeded`
/// y `RelayDisabled`); la 7, el formato del vídeo (espacio de color, croma y profundidad en
/// `CodecSelected`, 4:4:4 y HDR10 en `Capabilities`); la 8, los mandos tal cual (`pads` en los
/// paquetes y mensajes de entrada, `GamepadMode` y `GamepadOutput`); la 9, los miembros de la
/// sala con su nombre (`Members`), para que cada uno silencie a quien quiera; la 10, la sonda del
/// enlace (pista [`media::Track::Probe`] y `ProbeResult`), con la que el anfitrión arranca el
/// bitrate en lo que el camino entrega en vez de en una cifra fija; la 11, la entrada entera por
/// datagramas con el estado completo en cada paquete ([`input::InputState`], `queued_us`), el
/// stream solo para declarar mandos y el `InputReport` por etapas; la 12, el chat de la sala
/// (`Chat`, `ChatHistory` y `ChatClear`, cifrados de extremo a extremo con la clave de chat de la
/// época MLS: el relay los reenvía sin poder leerlos).
pub const PROTOCOL_VERSION: u16 = 12;

/// Identificadores ALPN (RFC 7301) que el socket QUIC usa para demultiplexar conexiones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Alpn {
    /// HTTP/3: API REST, administración y sesiones WebTransport de señalización.
    H3,
    /// Relay de medios de coroom, versión 1.
    Relay,
    /// QUIC directo entre pares, versión 1.
    P2p,
}

impl Alpn {
    /// Todos los ALPN que conoce esta versión del protocolo.
    pub const ALL: [Alpn; 3] = [Alpn::H3, Alpn::Relay, Alpn::P2p];

    /// Identificador ALPN como texto.
    pub const fn as_str(self) -> &'static str {
        match self {
            Alpn::H3 => "h3",
            Alpn::Relay => "coroom-relay/1",
            Alpn::P2p => "coroom-p2p/1",
        }
    }

    /// Identificador ALPN en bytes, tal como viaja en el `ClientHello` de TLS.
    pub const fn as_bytes(self) -> &'static [u8] {
        self.as_str().as_bytes()
    }

    /// Busca el ALPN que corresponde a los bytes negociados en TLS.
    pub fn from_bytes(bytes: &[u8]) -> Option<Alpn> {
        Alpn::ALL.into_iter().find(|alpn| alpn.as_bytes() == bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn alpn_identifiers_do_not_collide() {
        let unique: HashSet<&[u8]> = Alpn::ALL.iter().map(|alpn| alpn.as_bytes()).collect();
        assert_eq!(unique.len(), Alpn::ALL.len());
    }

    #[test]
    fn alpn_values_match_the_specification() {
        assert_eq!(Alpn::H3.as_bytes(), b"h3");
        assert_eq!(Alpn::Relay.as_bytes(), b"coroom-relay/1");
        assert_eq!(Alpn::P2p.as_bytes(), b"coroom-p2p/1");
    }

    #[test]
    fn alpn_round_trips_through_bytes() {
        for alpn in Alpn::ALL {
            assert_eq!(Alpn::from_bytes(alpn.as_bytes()), Some(alpn));
        }
        assert_eq!(Alpn::from_bytes(b"h2"), None);
    }

    #[test]
    fn alpn_fits_in_a_tls_protocol_name() {
        // RFC 7301: cada nombre de protocolo ocupa entre 1 y 255 bytes.
        for alpn in Alpn::ALL {
            assert!((1..=255).contains(&alpn.as_bytes().len()));
        }
    }
}
