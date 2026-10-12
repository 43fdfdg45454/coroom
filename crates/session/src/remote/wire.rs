//! Contenido de los mensajes `Mls { payload }` de la señalización.
//!
//! El servidor reenvía `payload` sin interpretarlo (es el *delivery service* de MLS). Además de los
//! mensajes MLS, por ahí viajan las dos piezas de HPKE del secreto pareja a pareja anfitrión ↔
//! invitado (la clave de entrada), así que el primer byte dice qué es:
//!
//! | Tipo | Valor | Cuerpo | Sentido |
//! |---|---|---|---|
//! | [`Payload::Commit`] | `0x01` | commit MLS (`PublicMessage` firmado, RFC 9420) | anfitrión → cada miembro |
//! | [`Payload::Welcome`] | `0x02` | `Welcome` MLS con el árbol | anfitrión → invitado nuevo |
//! | [`Payload::HostPqKey`] | `0x03` | clave X-Wing del anfitrión firmada con su identidad (`hpke::signed_pq_key`, 1216 + 64 B) | anfitrión → invitado nuevo |
//! | [`Payload::PairwiseOffer`] | `0x04` | `PairwiseOffer` HPKE firmada por el invitado (`PairwiseOffer::to_bytes`) | invitado → anfitrión |
//!
//! Así no hace falta tocar `proto::signal` ni el servidor: los cuatro son opacos para él. Ninguno
//! es secreto por sí solo (los secretos de ruta MLS y la encapsulación HPKE ya van cifrados) y
//! todos van firmados por su emisor, así que el servidor no puede falsificarlos.

use coroom_transport::crypto::hpke::{HpkeError, Kem, PairwiseOffer, RecipientKey};

/// Prefijo de un commit MLS.
pub const COMMIT: u8 = 0x01;
/// Prefijo de un `Welcome` MLS.
pub const WELCOME: u8 = 0x02;
/// Prefijo de la clave X-Wing firmada del anfitrión.
pub const HOST_PQ_KEY: u8 = 0x03;
/// Prefijo de la oferta HPKE del invitado.
pub const PAIRWISE_OFFER: u8 = 0x04;

/// Contenido de un `Mls { payload }` ya separado por su tipo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Payload<'a> {
    /// Commit MLS del anfitrión.
    Commit(&'a [u8]),
    /// `Welcome` MLS para el invitado nuevo.
    Welcome(&'a [u8]),
    /// Clave X-Wing firmada del anfitrión.
    HostPqKey(&'a [u8]),
    /// Oferta HPKE del invitado.
    PairwiseOffer(&'a [u8]),
}

impl<'a> Payload<'a> {
    /// Separa el tipo y el cuerpo; `None` si el tipo es desconocido o no hay cuerpo.
    pub fn decode(payload: &'a [u8]) -> Option<Payload<'a>> {
        let (&kind, body) = payload.split_first()?;
        if body.is_empty() {
            return None;
        }
        Some(match kind {
            COMMIT => Payload::Commit(body),
            WELCOME => Payload::Welcome(body),
            HOST_PQ_KEY => Payload::HostPqKey(body),
            PAIRWISE_OFFER => Payload::PairwiseOffer(body),
            _ => return None,
        })
    }

    /// Bytes con el prefijo de tipo, listos para `Mls { payload }`.
    pub fn encode(&self) -> Vec<u8> {
        let (kind, body) = match self {
            Payload::Commit(body) => (COMMIT, body),
            Payload::Welcome(body) => (WELCOME, body),
            Payload::HostPqKey(body) => (HOST_PQ_KEY, body),
            Payload::PairwiseOffer(body) => (PAIRWISE_OFFER, body),
        };
        let mut out = Vec::with_capacity(1 + body.len());
        out.push(kind);
        out.extend_from_slice(body);
        out
    }
}

/// Comprueba que la oferta pareja a pareja de un invitado use X-Wing. El anfitrión siempre publica
/// su clave X-Wing firmada, así que una oferta X25519 solo puede salir de una degradación en el
/// camino (`docs/seguridad.md`, hallazgo C-1).
pub fn require_pq_offer(offer: &PairwiseOffer) -> Result<(), HpkeError> {
    if offer.kem == Kem::XWing {
        Ok(())
    } else {
        Err(HpkeError::UnsupportedKem(offer.kem.id()))
    }
}

/// Destinatario X-Wing del anfitrión a partir de su clave firmada, **sin respaldo** a X25519: si
/// la firma no verifica, alguien la manipuló (hallazgo C-1) y no se establece la clave de entrada.
pub fn host_recipient(host_pk: [u8; 32], signed_key: &[u8]) -> Result<RecipientKey, HpkeError> {
    RecipientKey::from_signed_pq_key(host_pk, signed_key)
}

/// `info` HPKE del secreto pareja a pareja de una sala: lo liga al código de sala (las dos
/// identidades ya entran en el `info` que arma `coroom-crypto`).
pub fn pairwise_info(room_code: &str) -> Vec<u8> {
    let mut info = b"coroom input v1\0".to_vec();
    info.extend_from_slice(room_code.as_bytes());
    info
}

#[cfg(test)]
mod tests {
    use super::*;
    use coroom_transport::crypto::Identity;
    use coroom_transport::crypto::hpke::{derive_pairwise_secret, signed_pq_key};

    #[test]
    fn a_tampered_host_key_is_refused_without_falling_back_to_x25519() {
        let host = Identity::generate().expect("anfitrión");
        let guest = Identity::generate().expect("invitado");
        let signed = signed_pq_key(&host).expect("clave X-Wing");
        let recipient = host_recipient(host.public_key(), &signed).expect("clave buena");
        assert_eq!(recipient.kem(), Kem::XWing);
        let mut tampered = signed.clone();
        tampered[10] ^= 1;
        assert_eq!(
            host_recipient(host.public_key(), &tampered),
            Err(HpkeError::BadKeySignature)
        );
        assert!(host_recipient(host.public_key(), &signed[..100]).is_err());

        // El anfitrión acepta la oferta X-Wing y rechaza la X25519 de una degradación.
        let info = pairwise_info("ABC234");
        let (offer, _) = derive_pairwise_secret(&guest, &recipient, &info).expect("oferta");
        assert_eq!(require_pq_offer(&offer), Ok(()));
        let (downgraded, _) =
            derive_pairwise_secret(&guest, &RecipientKey::identity(host.public_key()), &info)
                .expect("oferta X25519");
        assert!(require_pq_offer(&downgraded).is_err());
    }

    #[test]
    fn payloads_round_trip() {
        for payload in [
            Payload::Commit(b"c"),
            Payload::Welcome(b"w"),
            Payload::HostPqKey(b"k"),
            Payload::PairwiseOffer(b"o"),
        ] {
            assert_eq!(Payload::decode(&payload.encode()), Some(payload));
        }
    }

    #[test]
    fn unknown_or_empty_payloads_are_rejected() {
        assert_eq!(Payload::decode(&[]), None);
        assert_eq!(Payload::decode(&[COMMIT]), None);
        assert_eq!(Payload::decode(&[0x7f, 1, 2]), None);
        // Un mensaje MLS sin prefijo empieza por la versión 0x0001: no se confunde con un tipo.
        assert_eq!(Payload::decode(&[0x00, 0x01, 0x00]), None);
    }

    #[test]
    fn pairwise_info_depends_on_the_room() {
        assert_ne!(pairwise_info("ABC234"), pairwise_info("ABC235"));
    }
}
