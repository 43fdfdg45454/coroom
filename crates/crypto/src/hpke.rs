//! Cifrado y claves pareja a pareja con HPKE (RFC 9180), modo base.
//!
//! - Suite clásica: `DHKEM(X25519, HKDF-SHA256)`, `HKDF-SHA256`, `AES-256-GCM`. Las claves HPKE
//!   de cada par son las de su [`Identity`] convertidas a X25519 con la conversión estándar
//!   Ed25519 → X25519 (la que usan libsodium y age):
//!   - privada: los 32 bytes bajos de `SHA-512(semilla Ed25519)` sin reducir
//!     (`ed25519_dalek::SigningKey::to_scalar_bytes`); X25519 los recorta (*clamping*) igual que
//!     Ed25519, así que el escalar es el mismo que firma;
//!   - pública: el mapa birracional de Edwards a Montgomery, `u = (1 + y) / (1 − y)`
//!     (`ed25519_dalek::VerifyingKey::to_montgomery`).
//!
//!   Así basta conocer la clave de identidad de un par para sellarle algo. (Reusar la clave de
//!   firma para un KEM es seguro en este caso concreto: <https://eprint.iacr.org/2021/509>.)
//! - **X-Wing** (X25519 + ML-KEM-768, `draft-connolly-cfrg-xwing-kem`; identificador HPKE
//!   `0x647a` de `draft-ietf-hpke-pq`), tras la `feature` `pq` (activada por defecto): la crate
//!   `hpke` 0.14 lo trae como KEM (sobre las crates estables `x-wing` 0.1 y `ml-kem` 0.3), así que
//!   el API es el mismo. Como una clave pública ML-KEM no se puede derivar de una clave Ed25519,
//!   cada par publica su clave X-Wing (1216 B) **firmada con su identidad**
//!   ([`signed_pq_key`]); la semilla privada X-Wing se deriva de la identidad con
//!   `HKDF-SHA256(salt = "coroom xwing v1", ikm = semilla Ed25519)`, así que no hay nada más que
//!   guardar. Es un borrador IETF: el respaldo declarado (`docs/ARQUITECTURA.md` §3) es X25519 puro con
//!   el mismo código ([`RecipientKey::Identity`]), que se usa cuando el par no ha publicado su
//!   clave X-Wing o sin la `feature` `pq`.
//! - `info` siempre lleva delante una etiqueta de dominio de coroom.
//!
//! **Secreto pareja a pareja** ([`derive_pairwise_secret`]): con HPKE uno encapsula y el otro
//! desencapsula, así que hay un iniciador. Protocolo: el **invitado inicia** hacia el anfitrión
//! (cuya clave conoce por `Joined`, y su clave X-Wing firmada si la publicó), envía la
//! [`PairwiseOffer`] (encapsulación firmada con su identidad) por la señalización y ambos exportan
//! el mismo secreto con el exportador HPKE (`Context.Export`). El anfitrión la acepta con
//! [`accept_pairwise_secret`] tras verificar la firma con la identidad del invitado. La firma
//! impide que el servidor, que reenvía la oferta, se haga pasar por el invitado; el secreto queda
//! ligado a las dos identidades y a `info`.

use std::fmt;

use ::hpke::aead::AesGcm256;
use ::hpke::kdf::HkdfSha256;
use ::hpke::kem::X25519HkdfSha256;
use ::hpke::{Deserializable, Kem as _, OpModeR, OpModeS, Serializable};
use ed25519_dalek::VerifyingKey;
use zeroize::Zeroizing;

use crate::identity::{Identity, PUBLIC_KEY_LENGTH, SIGNATURE_LENGTH, verify_signature};

/// Longitud máxima de `info` y de `aad` admitida (HPKE limita `info` a menos de 64 KiB).
pub const MAX_INFO_LENGTH: usize = 4096;

/// Longitud del secreto pareja a pareja.
pub const PAIRWISE_SECRET_LENGTH: usize = 32;

/// Secreto pareja a pareja; se borra al soltarse.
pub type PairwiseSecret = Zeroizing<[u8; PAIRWISE_SECRET_LENGTH]>;

/// Longitud de la clave pública X-Wing.
#[cfg(feature = "pq")]
pub const XWING_PUBLIC_KEY_LENGTH: usize = 1216;

pub(crate) const SEAL_LABEL: &[u8] = b"coroom hpke v1\0";
pub(crate) const PAIRWISE_LABEL: &[u8] = b"coroom pairwise v1\0";
pub(crate) const PAIRWISE_EXPORT_CONTEXT: &[u8] = b"coroom pairwise secret";
#[cfg(feature = "pq")]
pub(crate) const XWING_SEED_SALT: &[u8] = b"coroom xwing v1";
#[cfg(feature = "pq")]
pub(crate) const XWING_KEY_SIGNATURE_LABEL: &[u8] = b"coroom xwing key v1\0";

/// Error de HPKE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HpkeError {
    /// La clave pública del par no es válida.
    InvalidPublicKey,
    /// La clave X-Wing publicada no está firmada por esa identidad.
    BadKeySignature,
    /// La oferta pareja a pareja no está firmada por el iniciador.
    BadOfferSignature,
    /// KEM desconocido o no compilado (`feature` `pq`).
    UnsupportedKem(u16),
    /// Bytes mal formados.
    Malformed,
    /// `info` o `aad` demasiado largos.
    TooLong,
    /// No se pudo abrir: otra identidad, otro `info`/`aad` o datos manipulados.
    OpenFailed,
    /// Fallo interno de HPKE.
    Crypto(String),
}

impl fmt::Display for HpkeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HpkeError::InvalidPublicKey => f.write_str("clave pública HPKE no válida"),
            HpkeError::BadKeySignature => {
                f.write_str("la clave X-Wing publicada no está firmada por esa identidad")
            }
            HpkeError::BadOfferSignature => {
                f.write_str("la oferta de clave pareja a pareja no está firmada por el par")
            }
            HpkeError::UnsupportedKem(id) => write!(f, "KEM HPKE no admitido: {id:#06x}"),
            HpkeError::Malformed => f.write_str("mensaje HPKE mal formado"),
            HpkeError::TooLong => write!(
                f,
                "info o aad de HPKE superan {MAX_INFO_LENGTH} bytes"
            ),
            HpkeError::OpenFailed => f.write_str(
                "no se pudo abrir el mensaje HPKE: otro destinatario, otro contexto o datos manipulados",
            ),
            HpkeError::Crypto(err) => write!(f, "fallo de HPKE: {err}"),
        }
    }
}

impl std::error::Error for HpkeError {}

fn crypto_error(err: ::hpke::HpkeError) -> HpkeError {
    HpkeError::Crypto(err.to_string())
}

/// KEM con el que se selló algo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kem {
    /// `DHKEM(X25519, HKDF-SHA256)` con la clave de identidad convertida.
    X25519,
    /// X-Wing (X25519 + ML-KEM-768).
    #[cfg(feature = "pq")]
    XWing,
}

impl Kem {
    /// Identificador HPKE del KEM.
    pub fn id(self) -> u16 {
        match self {
            Kem::X25519 => X25519HkdfSha256::KEM_ID,
            #[cfg(feature = "pq")]
            Kem::XWing => ::hpke::kem::XWing::KEM_ID,
        }
    }

    /// KEM a partir de su identificador.
    pub fn from_id(id: u16) -> Result<Kem, HpkeError> {
        if id == X25519HkdfSha256::KEM_ID {
            return Ok(Kem::X25519);
        }
        #[cfg(feature = "pq")]
        if id == ::hpke::kem::XWing::KEM_ID {
            return Ok(Kem::XWing);
        }
        Err(HpkeError::UnsupportedKem(id))
    }
}

/// Destinatario de un sellado: siempre una identidad, con su clave X-Wing si la publicó.
#[derive(Clone, PartialEq, Eq)]
pub enum RecipientKey {
    /// Solo la clave de identidad: KEM X25519 (respaldo clásico).
    Identity([u8; PUBLIC_KEY_LENGTH]),
    /// Identidad y su clave X-Wing ya verificada.
    #[cfg(feature = "pq")]
    XWing {
        /// Clave de identidad Ed25519.
        identity: [u8; PUBLIC_KEY_LENGTH],
        /// Clave pública X-Wing firmada por esa identidad.
        key: Box<[u8; XWING_PUBLIC_KEY_LENGTH]>,
    },
}

impl RecipientKey {
    /// Destinatario por su clave de identidad (KEM X25519).
    pub fn identity(public_key: [u8; PUBLIC_KEY_LENGTH]) -> RecipientKey {
        RecipientKey::Identity(public_key)
    }

    /// Destinatario X-Wing a partir de la clave firmada que publicó la identidad `identity`
    /// ([`signed_pq_key`]). Falla si la firma no es de esa identidad.
    #[cfg(feature = "pq")]
    pub fn from_signed_pq_key(
        identity: [u8; PUBLIC_KEY_LENGTH],
        signed: &[u8],
    ) -> Result<RecipientKey, HpkeError> {
        if signed.len() != XWING_PUBLIC_KEY_LENGTH + SIGNATURE_LENGTH {
            return Err(HpkeError::Malformed);
        }
        let (key, signature) = signed.split_at(XWING_PUBLIC_KEY_LENGTH);
        let signature: [u8; SIGNATURE_LENGTH] =
            signature.try_into().map_err(|_| HpkeError::Malformed)?;
        verify_signature(&identity, &pq_key_message(&identity, key), &signature)
            .map_err(|_| HpkeError::BadKeySignature)?;
        // Valida que sea una clave X-Wing bien formada.
        <::hpke::kem::XWing as ::hpke::Kem>::PublicKey::from_bytes(key)
            .map_err(|_| HpkeError::InvalidPublicKey)?;
        let mut boxed = Box::new([0u8; XWING_PUBLIC_KEY_LENGTH]);
        boxed.copy_from_slice(key);
        Ok(RecipientKey::XWing {
            identity,
            key: boxed,
        })
    }

    /// Clave de identidad del destinatario.
    pub fn identity_key(&self) -> &[u8; PUBLIC_KEY_LENGTH] {
        match self {
            RecipientKey::Identity(identity) => identity,
            #[cfg(feature = "pq")]
            RecipientKey::XWing { identity, .. } => identity,
        }
    }

    /// KEM que se usará con este destinatario.
    pub fn kem(&self) -> Kem {
        match self {
            RecipientKey::Identity(_) => Kem::X25519,
            #[cfg(feature = "pq")]
            RecipientKey::XWing { .. } => Kem::XWing,
        }
    }
}

impl fmt::Debug for RecipientKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecipientKey")
            .field("kem", &self.kem())
            .field(
                "identity",
                &crate::identity::sha256_fingerprint(self.identity_key()),
            )
            .finish()
    }
}

/// Mensaje sellado: encapsulación + texto cifrado (con etiqueta).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    /// KEM usado.
    pub kem: Kem,
    /// Clave encapsulada.
    pub enc: Vec<u8>,
    /// Texto cifrado con su etiqueta.
    pub ciphertext: Vec<u8>,
}

impl Sealed {
    /// `kem_id (u16 BE) || len(enc) (u16 BE) || enc || ciphertext`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + self.enc.len() + self.ciphertext.len());
        out.extend_from_slice(&self.kem.id().to_be_bytes());
        out.extend_from_slice(
            &u16::try_from(self.enc.len())
                .unwrap_or(u16::MAX)
                .to_be_bytes(),
        );
        out.extend_from_slice(&self.enc);
        out.extend_from_slice(&self.ciphertext);
        out
    }

    /// Inverso de [`Sealed::to_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Sealed, HpkeError> {
        let (kem, enc, rest) = split_kem_enc(bytes)?;
        Ok(Sealed {
            kem,
            enc: enc.to_vec(),
            ciphertext: rest.to_vec(),
        })
    }
}

fn split_kem_enc(bytes: &[u8]) -> Result<(Kem, &[u8], &[u8]), HpkeError> {
    if bytes.len() < 4 {
        return Err(HpkeError::Malformed);
    }
    let kem = Kem::from_id(u16::from_be_bytes([bytes[0], bytes[1]]))?;
    let enc_len = usize::from(u16::from_be_bytes([bytes[2], bytes[3]]));
    let rest = &bytes[4..];
    if rest.len() < enc_len {
        return Err(HpkeError::Malformed);
    }
    let (enc, rest) = rest.split_at(enc_len);
    Ok((kem, enc, rest))
}

fn labeled(label: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(label.len() + parts.iter().map(|p| p.len()).sum::<usize>());
    out.extend_from_slice(label);
    for part in parts {
        out.extend_from_slice(part);
    }
    out
}

fn check_len(data: &[u8]) -> Result<(), HpkeError> {
    if data.len() > MAX_INFO_LENGTH {
        Err(HpkeError::TooLong)
    } else {
        Ok(())
    }
}

// --- Claves ------------------------------------------------------------------------------------

type X25519Kem = X25519HkdfSha256;

fn x25519_private(
    identity: &Identity,
) -> Result<<X25519Kem as ::hpke::Kem>::PrivateKey, HpkeError> {
    let scalar = Zeroizing::new(identity.signing_key().to_scalar_bytes());
    <X25519Kem as ::hpke::Kem>::PrivateKey::from_bytes(scalar.as_slice()).map_err(crypto_error)
}

fn x25519_public(
    identity: &[u8; PUBLIC_KEY_LENGTH],
) -> Result<<X25519Kem as ::hpke::Kem>::PublicKey, HpkeError> {
    let montgomery = VerifyingKey::from_bytes(identity)
        .map_err(|_| HpkeError::InvalidPublicKey)?
        .to_montgomery();
    <X25519Kem as ::hpke::Kem>::PublicKey::from_bytes(montgomery.as_bytes())
        .map_err(|_| HpkeError::InvalidPublicKey)
}

/// Clave pública X25519 (Montgomery) que corresponde a una identidad Ed25519.
pub fn x25519_public_key(identity: &[u8; PUBLIC_KEY_LENGTH]) -> Result<[u8; 32], HpkeError> {
    Ok(VerifyingKey::from_bytes(identity)
        .map_err(|_| HpkeError::InvalidPublicKey)?
        .to_montgomery()
        .to_bytes())
}

#[cfg(feature = "pq")]
type XWingKem = ::hpke::kem::XWing;

#[cfg(feature = "pq")]
fn xwing_private(identity: &Identity) -> Result<<XWingKem as ::hpke::Kem>::PrivateKey, HpkeError> {
    let mut seed = Zeroizing::new([0u8; 32]);
    hkdf::Hkdf::<sha2::Sha256>::new(Some(XWING_SEED_SALT), identity.signing_key().as_bytes())
        .expand(&[], seed.as_mut())
        .map_err(|err| HpkeError::Crypto(err.to_string()))?;
    <XWingKem as ::hpke::Kem>::PrivateKey::from_bytes(seed.as_slice()).map_err(crypto_error)
}

#[cfg(feature = "pq")]
pub(crate) fn pq_key_message(identity: &[u8; PUBLIC_KEY_LENGTH], key: &[u8]) -> Vec<u8> {
    labeled(XWING_KEY_SIGNATURE_LABEL, &[identity, key])
}

/// Clave pública X-Wing de `identity` seguida de su firma Ed25519
/// (`clave (1216 B) || firma (64 B)`), para publicarla; el par la verifica con
/// [`RecipientKey::from_signed_pq_key`].
#[cfg(feature = "pq")]
pub fn signed_pq_key(identity: &Identity) -> Result<Vec<u8>, HpkeError> {
    let private = xwing_private(identity)?;
    let public = XWingKem::sk_to_pk(&private).to_bytes();
    let signature = identity.sign(&pq_key_message(&identity.public_key(), &public));
    let mut out = Vec::with_capacity(XWING_PUBLIC_KEY_LENGTH + SIGNATURE_LENGTH);
    out.extend_from_slice(&public);
    out.extend_from_slice(&signature);
    Ok(out)
}

// --- Sellar y abrir ----------------------------------------------------------------------------

/// Sella `plaintext` para `recipient` (HPKE modo base, un solo mensaje).
pub fn seal(
    recipient: &RecipientKey,
    info: &[u8],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Sealed, HpkeError> {
    check_len(info)?;
    check_len(aad)?;
    let info = labeled(SEAL_LABEL, &[info]);
    let (enc, ciphertext) = match recipient {
        RecipientKey::Identity(identity) => {
            let pk = x25519_public(identity)?;
            let (enc, ct) = ::hpke::single_shot_seal::<AesGcm256, HkdfSha256, X25519Kem>(
                &OpModeS::Base,
                &pk,
                &info,
                plaintext,
                aad,
            )
            .map_err(crypto_error)?;
            (enc.to_bytes().to_vec(), ct)
        }
        #[cfg(feature = "pq")]
        RecipientKey::XWing { key, .. } => {
            let pk = <XWingKem as ::hpke::Kem>::PublicKey::from_bytes(key.as_slice())
                .map_err(|_| HpkeError::InvalidPublicKey)?;
            let (enc, ct) = ::hpke::single_shot_seal::<AesGcm256, HkdfSha256, XWingKem>(
                &OpModeS::Base,
                &pk,
                &info,
                plaintext,
                aad,
            )
            .map_err(crypto_error)?;
            (enc.to_bytes().to_vec(), ct)
        }
    };
    Ok(Sealed {
        kem: recipient.kem(),
        enc,
        ciphertext,
    })
}

/// Abre lo que se selló para `identity` con el mismo `info` y `aad`.
pub fn open(
    identity: &Identity,
    info: &[u8],
    aad: &[u8],
    sealed: &Sealed,
) -> Result<Zeroizing<Vec<u8>>, HpkeError> {
    check_len(info)?;
    check_len(aad)?;
    let info = labeled(SEAL_LABEL, &[info]);
    let plaintext = match sealed.kem {
        Kem::X25519 => {
            let sk = x25519_private(identity)?;
            let enc = <X25519Kem as ::hpke::Kem>::EncappedKey::from_bytes(&sealed.enc)
                .map_err(|_| HpkeError::Malformed)?;
            ::hpke::single_shot_open::<AesGcm256, HkdfSha256, X25519Kem>(
                &OpModeR::Base,
                &sk,
                &enc,
                &info,
                &sealed.ciphertext,
                aad,
            )
        }
        #[cfg(feature = "pq")]
        Kem::XWing => {
            let sk = xwing_private(identity)?;
            let enc = <XWingKem as ::hpke::Kem>::EncappedKey::from_bytes(&sealed.enc)
                .map_err(|_| HpkeError::Malformed)?;
            ::hpke::single_shot_open::<AesGcm256, HkdfSha256, XWingKem>(
                &OpModeR::Base,
                &sk,
                &enc,
                &info,
                &sealed.ciphertext,
                aad,
            )
        }
    };
    plaintext
        .map(Zeroizing::new)
        .map_err(|_| HpkeError::OpenFailed)
}

// --- Secreto pareja a pareja -------------------------------------------------------------------

/// Oferta del iniciador: encapsulación HPKE hacia el otro par, firmada con su identidad.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairwiseOffer {
    /// KEM usado.
    pub kem: Kem,
    /// Clave encapsulada.
    pub enc: Vec<u8>,
    /// Firma Ed25519 del iniciador sobre la oferta.
    pub signature: [u8; SIGNATURE_LENGTH],
}

impl PairwiseOffer {
    /// `kem_id (u16 BE) || len(enc) (u16 BE) || enc || firma (64 B)`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + self.enc.len() + SIGNATURE_LENGTH);
        out.extend_from_slice(&self.kem.id().to_be_bytes());
        out.extend_from_slice(
            &u16::try_from(self.enc.len())
                .unwrap_or(u16::MAX)
                .to_be_bytes(),
        );
        out.extend_from_slice(&self.enc);
        out.extend_from_slice(&self.signature);
        out
    }

    /// Inverso de [`PairwiseOffer::to_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> Result<PairwiseOffer, HpkeError> {
        let (kem, enc, rest) = split_kem_enc(bytes)?;
        let signature: [u8; SIGNATURE_LENGTH] =
            rest.try_into().map_err(|_| HpkeError::Malformed)?;
        Ok(PairwiseOffer {
            kem,
            enc: enc.to_vec(),
            signature,
        })
    }
}

pub(crate) fn pairwise_info(
    initiator: &[u8; PUBLIC_KEY_LENGTH],
    responder: &[u8; PUBLIC_KEY_LENGTH],
    info: &[u8],
) -> Vec<u8> {
    labeled(PAIRWISE_LABEL, &[initiator, responder, info])
}

pub(crate) fn offer_message(kem: Kem, hpke_info: &[u8], enc: &[u8]) -> Vec<u8> {
    labeled(&kem.id().to_be_bytes(), &[hpke_info, enc])
}

/// Lado iniciador (el invitado): encapsula hacia `peer`, firma la oferta y exporta el secreto.
/// Enviar la [`PairwiseOffer`] al par, que obtiene el mismo secreto con
/// [`accept_pairwise_secret`].
pub fn derive_pairwise_secret(
    identity: &Identity,
    peer: &RecipientKey,
    info: &[u8],
) -> Result<(PairwiseOffer, PairwiseSecret), HpkeError> {
    check_len(info)?;
    let hpke_info = pairwise_info(&identity.public_key(), peer.identity_key(), info);
    let mut secret = PairwiseSecret::new([0u8; PAIRWISE_SECRET_LENGTH]);
    let enc = match peer {
        RecipientKey::Identity(peer) => {
            let pk = x25519_public(peer)?;
            let (enc, ctx) = ::hpke::setup_sender::<AesGcm256, HkdfSha256, X25519Kem>(
                &OpModeS::Base,
                &pk,
                &hpke_info,
            )
            .map_err(crypto_error)?;
            ctx.export(PAIRWISE_EXPORT_CONTEXT, secret.as_mut())
                .map_err(crypto_error)?;
            enc.to_bytes().to_vec()
        }
        #[cfg(feature = "pq")]
        RecipientKey::XWing { key, .. } => {
            let pk = <XWingKem as ::hpke::Kem>::PublicKey::from_bytes(key.as_slice())
                .map_err(|_| HpkeError::InvalidPublicKey)?;
            let (enc, ctx) = ::hpke::setup_sender::<AesGcm256, HkdfSha256, XWingKem>(
                &OpModeS::Base,
                &pk,
                &hpke_info,
            )
            .map_err(crypto_error)?;
            ctx.export(PAIRWISE_EXPORT_CONTEXT, secret.as_mut())
                .map_err(crypto_error)?;
            enc.to_bytes().to_vec()
        }
    };
    let kem = peer.kem();
    let signature = identity.sign(&offer_message(kem, &hpke_info, &enc));
    Ok((
        PairwiseOffer {
            kem,
            enc,
            signature,
        },
        secret,
    ))
}

/// Lado receptor (el anfitrión): verifica que la oferta la firmó `initiator` y obtiene el mismo
/// secreto que él.
pub fn accept_pairwise_secret(
    identity: &Identity,
    initiator: &[u8; PUBLIC_KEY_LENGTH],
    offer: &PairwiseOffer,
    info: &[u8],
) -> Result<PairwiseSecret, HpkeError> {
    check_len(info)?;
    let hpke_info = pairwise_info(initiator, &identity.public_key(), info);
    verify_signature(
        initiator,
        &offer_message(offer.kem, &hpke_info, &offer.enc),
        &offer.signature,
    )
    .map_err(|_| HpkeError::BadOfferSignature)?;
    let mut secret = PairwiseSecret::new([0u8; PAIRWISE_SECRET_LENGTH]);
    match offer.kem {
        Kem::X25519 => {
            let sk = x25519_private(identity)?;
            let enc = <X25519Kem as ::hpke::Kem>::EncappedKey::from_bytes(&offer.enc)
                .map_err(|_| HpkeError::Malformed)?;
            let ctx = ::hpke::setup_receiver::<AesGcm256, HkdfSha256, X25519Kem>(
                &OpModeR::Base,
                &sk,
                &enc,
                &hpke_info,
            )
            .map_err(|_| HpkeError::OpenFailed)?;
            ctx.export(PAIRWISE_EXPORT_CONTEXT, secret.as_mut())
                .map_err(crypto_error)?;
        }
        #[cfg(feature = "pq")]
        Kem::XWing => {
            let sk = xwing_private(identity)?;
            let enc = <XWingKem as ::hpke::Kem>::EncappedKey::from_bytes(&offer.enc)
                .map_err(|_| HpkeError::Malformed)?;
            let ctx = ::hpke::setup_receiver::<AesGcm256, HkdfSha256, XWingKem>(
                &OpModeR::Base,
                &sk,
                &enc,
                &hpke_info,
            )
            .map_err(|_| HpkeError::OpenFailed)?;
            ctx.export(PAIRWISE_EXPORT_CONTEXT, secret.as_mut())
                .map_err(crypto_error)?;
        }
    }
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipients(identity: &Identity) -> Vec<RecipientKey> {
        #[allow(unused_mut)]
        let mut out = vec![RecipientKey::identity(identity.public_key())];
        #[cfg(feature = "pq")]
        out.push(
            RecipientKey::from_signed_pq_key(
                identity.public_key(),
                &signed_pq_key(identity).unwrap(),
            )
            .unwrap(),
        );
        out
    }

    #[test]
    fn ed25519_to_x25519_conversion_is_consistent() {
        let identity = Identity::generate().unwrap();
        let private = x25519_private(&identity).unwrap();
        let derived = X25519Kem::sk_to_pk(&private).to_bytes();
        assert_eq!(
            derived.as_slice(),
            x25519_public_key(&identity.public_key()).unwrap()
        );
    }

    #[test]
    fn seal_and_open() {
        let bob = Identity::generate().unwrap();
        for recipient in recipients(&bob) {
            let sealed = seal(&recipient, b"clave de sala", b"sala 1", b"secreto").unwrap();
            assert_eq!(sealed.kem, recipient.kem());
            let round = Sealed::from_bytes(&sealed.to_bytes()).unwrap();
            assert_eq!(round, sealed);
            let opened = open(&bob, b"clave de sala", b"sala 1", &round).unwrap();
            assert_eq!(opened.as_slice(), b"secreto");
        }
    }

    #[test]
    fn opening_with_another_identity_or_context_fails() {
        let bob = Identity::generate().unwrap();
        let eve = Identity::generate().unwrap();
        for recipient in recipients(&bob) {
            let sealed = seal(&recipient, b"info", b"aad", b"secreto").unwrap();
            assert_eq!(
                open(&eve, b"info", b"aad", &sealed),
                Err(HpkeError::OpenFailed)
            );
            assert_eq!(
                open(&bob, b"otra", b"aad", &sealed),
                Err(HpkeError::OpenFailed)
            );
            assert_eq!(
                open(&bob, b"info", b"otro", &sealed),
                Err(HpkeError::OpenFailed)
            );
            let mut tampered = sealed.clone();
            tampered.ciphertext[0] ^= 1;
            assert_eq!(
                open(&bob, b"info", b"aad", &tampered),
                Err(HpkeError::OpenFailed)
            );
        }
        assert!(Sealed::from_bytes(&[0, 0x20, 0xff]).is_err());
        assert_eq!(
            Sealed::from_bytes(&[0x12, 0x34, 0, 0]),
            Err(HpkeError::UnsupportedKem(0x1234))
        );
    }

    #[test]
    fn pairwise_secret_matches_on_both_sides() {
        let guest = Identity::generate().unwrap();
        let host = Identity::generate().unwrap();
        for recipient in recipients(&host) {
            let (offer, guest_secret) =
                derive_pairwise_secret(&guest, &recipient, b"entrada").unwrap();
            let offer = PairwiseOffer::from_bytes(&offer.to_bytes()).unwrap();
            let host_secret =
                accept_pairwise_secret(&host, &guest.public_key(), &offer, b"entrada").unwrap();
            assert_eq!(*guest_secret, *host_secret);

            // Otro info, otro iniciador o una oferta manipulada no dan el secreto.
            assert_eq!(
                accept_pairwise_secret(&host, &guest.public_key(), &offer, b"otro"),
                Err(HpkeError::BadOfferSignature)
            );
            let impostor = Identity::generate().unwrap();
            assert_eq!(
                accept_pairwise_secret(&host, &impostor.public_key(), &offer, b"entrada"),
                Err(HpkeError::BadOfferSignature)
            );
            let mut tampered = offer.clone();
            tampered.enc[0] ^= 1;
            assert!(
                accept_pairwise_secret(&host, &guest.public_key(), &tampered, b"entrada").is_err()
            );
            // Un tercero no obtiene el mismo secreto aunque acepte la oferta.
            let eve = Identity::generate().unwrap();
            if let Ok(secret) =
                accept_pairwise_secret(&eve, &guest.public_key(), &offer, b"entrada")
            {
                assert_ne!(*secret, *guest_secret);
            }
            // Dos ofertas dan secretos distintos (encapsulación efímera).
            let (_, again) = derive_pairwise_secret(&guest, &recipient, b"entrada").unwrap();
            assert_ne!(*again, *guest_secret);
        }
    }

    #[cfg(feature = "pq")]
    #[test]
    fn pq_key_must_be_signed_by_its_identity() {
        let host = Identity::generate().unwrap();
        let other = Identity::generate().unwrap();
        let signed = signed_pq_key(&host).unwrap();
        assert_eq!(signed.len(), XWING_PUBLIC_KEY_LENGTH + SIGNATURE_LENGTH);
        assert_eq!(signed, signed_pq_key(&host).unwrap(), "determinista");
        assert_eq!(
            RecipientKey::from_signed_pq_key(other.public_key(), &signed),
            Err(HpkeError::BadKeySignature)
        );
        let mut tampered = signed.clone();
        tampered[10] ^= 1;
        assert_eq!(
            RecipientKey::from_signed_pq_key(host.public_key(), &tampered),
            Err(HpkeError::BadKeySignature)
        );
        assert_eq!(
            RecipientKey::from_signed_pq_key(host.public_key(), &signed[1..]),
            Err(HpkeError::Malformed)
        );
    }
}
