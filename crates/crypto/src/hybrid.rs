//! Firma híbrida de la identidad: Ed25519 + ML-DSA-65 (FIPS 204), tras la `feature` `pq-sign`.
//!
//! Evaluación (`docs/seguridad.md`, «Firmas poscuánticas»): `ml-dsa` 0.1 de
//! RustCrypto ya es estable (FIPS 204 final, sin auditoría independiente), así que la identidad
//! puede tener una mitad poscuántica sin guardar nada más:
//!
//! - **Clave ML-DSA-65**: su semilla de 32 bytes (`ξ` de `ML-DSA.KeyGen_internal`) se deriva de la
//!   semilla Ed25519 con `HKDF-SHA256(salt = "coroom ml-dsa-65 v1", ikm = semilla)`, como la de
//!   X-Wing ([`crate::hpke`]).
//! - **Firma híbrida** ([`Identity::sign_hybrid`]): las dos mitades firman el mismo mensaje
//!   `0x01 ‖ "coroom hybrid signature v1" ‖ 0x00 ‖ m` (ML-DSA, además, con el contexto FIPS 204
//!   `"coroom identity v1"`); [`verify_hybrid`] exige las dos. El prefijo empieza por `0x01`, un
//!   byte con el que no empieza ningún otro mensaje que firme la identidad
//!   ([`crate::domains`]), así que la mitad Ed25519 no sirve como firma clásica de otra cosa.
//! - **Ligado a la identidad**: [`HybridPublicKey`] lleva la clave ML-DSA firmada con Ed25519
//!   (`0x02 ‖ "coroom ml-dsa key v1" ‖ 0x00 ‖ Ed25519 ‖ ML-DSA`), para que quien solo conoce la
//!   huella Ed25519 (número de seguridad) pueda comprobarla, como la clave X-Wing.
//!
//! Ningún protocolo del cable la usa todavía (cambiaría el formato del `KeyPackage` MLS y del
//! desafío de dispositivo): queda lista para cuando la suite MLS con ML-DSA y su número de
//! seguridad estén definidos.

use std::fmt;

use ml_dsa::signature::Keypair;
use ml_dsa::{EncodedSignature, EncodedVerifyingKey, MlDsa65, Signature, SigningKey, VerifyingKey};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::identity::{
    Identity, IdentityError, PUBLIC_KEY_LENGTH, SIGNATURE_LENGTH, sha256_fingerprint,
    verify_signature,
};

/// Clave pública ML-DSA-65 codificada (FIPS 204).
pub const ML_DSA_PUBLIC_KEY_LENGTH: usize = 1952;

/// Firma ML-DSA-65 codificada (FIPS 204).
pub const ML_DSA_SIGNATURE_LENGTH: usize = 3309;

/// Firma híbrida: Ed25519 (64 B) seguida de ML-DSA-65.
pub const HYBRID_SIGNATURE_LENGTH: usize = SIGNATURE_LENGTH + ML_DSA_SIGNATURE_LENGTH;

/// Clave pública híbrida: Ed25519, ML-DSA-65 y la firma Ed25519 que las liga.
pub const HYBRID_PUBLIC_KEY_LENGTH: usize =
    PUBLIC_KEY_LENGTH + ML_DSA_PUBLIC_KEY_LENGTH + SIGNATURE_LENGTH;

/// Sal de HKDF de la semilla ML-DSA.
pub(crate) const ML_DSA_SEED_SALT: &[u8] = b"coroom ml-dsa-65 v1";

/// Prefijo de lo que firman las dos mitades.
pub(crate) const HYBRID_SIGNATURE_PREFIX: &[u8] = b"\x01coroom hybrid signature v1\x00";

/// Contexto FIPS 204 de la mitad ML-DSA.
pub(crate) const ML_DSA_CONTEXT: &[u8] = b"coroom identity v1";

/// Prefijo de la firma Ed25519 que liga la clave ML-DSA a la identidad.
pub(crate) const ML_DSA_KEY_BINDING_PREFIX: &[u8] = b"\x02coroom ml-dsa key v1\x00";

/// Clave de firma ML-DSA-65 derivada de una identidad; se borra al soltarse.
pub struct PqSigningKey(SigningKey<MlDsa65>);

impl PqSigningKey {
    /// Clave pública ML-DSA-65 codificada.
    pub fn public_key(&self) -> Box<[u8; ML_DSA_PUBLIC_KEY_LENGTH]> {
        let mut out = Box::new([0u8; ML_DSA_PUBLIC_KEY_LENGTH]);
        out.copy_from_slice(self.0.verifying_key().encode().as_slice());
        out
    }

    /// Firma ML-DSA-65 determinista de `message` con el contexto de coroom.
    fn sign(&self, message: &[u8]) -> Result<Box<[u8; ML_DSA_SIGNATURE_LENGTH]>, IdentityError> {
        let signature = self
            .0
            .expanded_key()
            .sign_deterministic(message, ML_DSA_CONTEXT)
            .map_err(|_| IdentityError::Pkcs8("contexto ML-DSA demasiado largo".to_owned()))?;
        let mut out = Box::new([0u8; ML_DSA_SIGNATURE_LENGTH]);
        out.copy_from_slice(signature.encode().as_slice());
        Ok(out)
    }
}

impl fmt::Debug for PqSigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PqSigningKey").finish_non_exhaustive()
    }
}

/// Clave pública híbrida de una identidad, con la clave ML-DSA ligada por una firma Ed25519.
#[derive(Clone, PartialEq, Eq)]
pub struct HybridPublicKey {
    /// Clave Ed25519 (la identidad de siempre).
    pub ed25519: [u8; PUBLIC_KEY_LENGTH],
    /// Clave ML-DSA-65.
    pub ml_dsa: Box<[u8; ML_DSA_PUBLIC_KEY_LENGTH]>,
    /// Firma Ed25519 de `0x02 ‖ "coroom ml-dsa key v1" ‖ 0x00 ‖ ed25519 ‖ ml_dsa`.
    pub binding: [u8; SIGNATURE_LENGTH],
}

impl HybridPublicKey {
    /// `ed25519 ‖ ml_dsa ‖ binding` ([`HYBRID_PUBLIC_KEY_LENGTH`] bytes).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HYBRID_PUBLIC_KEY_LENGTH);
        out.extend_from_slice(&self.ed25519);
        out.extend_from_slice(self.ml_dsa.as_slice());
        out.extend_from_slice(&self.binding);
        out
    }

    /// Inverso de [`HybridPublicKey::to_bytes`]; comprueba que la firma que liga la clave
    /// ML-DSA sea de la clave Ed25519 y que la clave ML-DSA esté bien formada.
    pub fn from_bytes(bytes: &[u8]) -> Result<HybridPublicKey, IdentityError> {
        if bytes.len() != HYBRID_PUBLIC_KEY_LENGTH {
            return Err(IdentityError::InvalidPublicKey);
        }
        let (ed25519, rest) = bytes.split_at(PUBLIC_KEY_LENGTH);
        let (ml_dsa, binding) = rest.split_at(ML_DSA_PUBLIC_KEY_LENGTH);
        let ed25519: [u8; PUBLIC_KEY_LENGTH] = ed25519
            .try_into()
            .map_err(|_| IdentityError::InvalidPublicKey)?;
        let binding: [u8; SIGNATURE_LENGTH] = binding
            .try_into()
            .map_err(|_| IdentityError::InvalidPublicKey)?;
        verify_signature(&ed25519, &binding_message(&ed25519, ml_dsa), &binding)?;
        verifying_key(ml_dsa)?;
        let mut key = Box::new([0u8; ML_DSA_PUBLIC_KEY_LENGTH]);
        key.copy_from_slice(ml_dsa);
        Ok(HybridPublicKey {
            ed25519,
            ml_dsa: key,
            binding,
        })
    }

    /// Huella de la clave híbrida: SHA-256 de `ed25519 ‖ ml_dsa`.
    pub fn fingerprint(&self) -> String {
        let mut both = Vec::with_capacity(PUBLIC_KEY_LENGTH + ML_DSA_PUBLIC_KEY_LENGTH);
        both.extend_from_slice(&self.ed25519);
        both.extend_from_slice(self.ml_dsa.as_slice());
        sha256_fingerprint(&both)
    }
}

impl fmt::Debug for HybridPublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HybridPublicKey")
            .field("ed25519", &sha256_fingerprint(&self.ed25519))
            .field("fingerprint", &self.fingerprint())
            .finish()
    }
}

/// Firma híbrida: las dos mitades sobre el mismo mensaje con prefijo.
#[derive(Clone, PartialEq, Eq)]
pub struct HybridSignature {
    /// Mitad Ed25519.
    pub ed25519: [u8; SIGNATURE_LENGTH],
    /// Mitad ML-DSA-65.
    pub ml_dsa: Box<[u8; ML_DSA_SIGNATURE_LENGTH]>,
}

impl HybridSignature {
    /// `ed25519 ‖ ml_dsa` ([`HYBRID_SIGNATURE_LENGTH`] bytes).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HYBRID_SIGNATURE_LENGTH);
        out.extend_from_slice(&self.ed25519);
        out.extend_from_slice(self.ml_dsa.as_slice());
        out
    }

    /// Inverso de [`HybridSignature::to_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> Result<HybridSignature, IdentityError> {
        if bytes.len() != HYBRID_SIGNATURE_LENGTH {
            return Err(IdentityError::InvalidPublicKey);
        }
        let (ed25519, ml_dsa) = bytes.split_at(SIGNATURE_LENGTH);
        let ed25519: [u8; SIGNATURE_LENGTH] = ed25519
            .try_into()
            .map_err(|_| IdentityError::InvalidPublicKey)?;
        let mut pq = Box::new([0u8; ML_DSA_SIGNATURE_LENGTH]);
        pq.copy_from_slice(ml_dsa);
        Ok(HybridSignature {
            ed25519,
            ml_dsa: pq,
        })
    }
}

impl fmt::Debug for HybridSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HybridSignature").finish_non_exhaustive()
    }
}

fn binding_message(ed25519: &[u8; PUBLIC_KEY_LENGTH], ml_dsa: &[u8]) -> Vec<u8> {
    let mut message =
        Vec::with_capacity(ML_DSA_KEY_BINDING_PREFIX.len() + PUBLIC_KEY_LENGTH + ml_dsa.len());
    message.extend_from_slice(ML_DSA_KEY_BINDING_PREFIX);
    message.extend_from_slice(ed25519);
    message.extend_from_slice(ml_dsa);
    message
}

/// Lo que firman las dos mitades de una firma híbrida de `message`.
pub(crate) fn hybrid_message(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HYBRID_SIGNATURE_PREFIX.len() + message.len());
    out.extend_from_slice(HYBRID_SIGNATURE_PREFIX);
    out.extend_from_slice(message);
    out
}

fn verifying_key(bytes: &[u8]) -> Result<VerifyingKey<MlDsa65>, IdentityError> {
    let encoded = EncodedVerifyingKey::<MlDsa65>::try_from(bytes)
        .map_err(|_| IdentityError::InvalidPublicKey)?;
    Ok(VerifyingKey::decode(&encoded))
}

impl Identity {
    /// Clave ML-DSA-65 derivada de la identidad (siempre la misma para la misma identidad).
    pub fn pq_signing_key(&self) -> Result<PqSigningKey, IdentityError> {
        let mut seed = Zeroizing::new([0u8; 32]);
        hkdf::Hkdf::<Sha256>::new(Some(ML_DSA_SEED_SALT), self.signing_key().as_bytes())
            .expand(&[], seed.as_mut())
            .map_err(|_| IdentityError::Pkcs8("HKDF no pudo derivar la semilla ML-DSA".into()))?;
        let xi = ml_dsa::Seed::from(*seed);
        Ok(PqSigningKey(SigningKey::from_seed(&xi)))
    }

    /// Clave pública híbrida de la identidad, con la clave ML-DSA ligada por Ed25519.
    pub fn hybrid_public_key(&self) -> Result<HybridPublicKey, IdentityError> {
        let ml_dsa = self.pq_signing_key()?.public_key();
        let ed25519 = self.public_key();
        let binding = self.sign(&binding_message(&ed25519, ml_dsa.as_slice()));
        Ok(HybridPublicKey {
            ed25519,
            ml_dsa,
            binding,
        })
    }

    /// Firma híbrida de `message` (Ed25519 + ML-DSA-65); ver el módulo.
    pub fn sign_hybrid(&self, message: &[u8]) -> Result<HybridSignature, IdentityError> {
        let signed = hybrid_message(message);
        Ok(HybridSignature {
            ed25519: self.sign(&signed),
            ml_dsa: self.pq_signing_key()?.sign(&signed)?,
        })
    }
}

/// Comprueba una firma híbrida: **las dos** mitades deben ser válidas.
pub fn verify_hybrid(
    key: &HybridPublicKey,
    message: &[u8],
    signature: &HybridSignature,
) -> Result<(), IdentityError> {
    let signed = hybrid_message(message);
    verify_signature(&key.ed25519, &signed, &signature.ed25519)?;
    let pq_key = verifying_key(key.ml_dsa.as_slice())?;
    let encoded = EncodedSignature::<MlDsa65>::try_from(signature.ml_dsa.as_slice())
        .map_err(|_| IdentityError::InvalidPublicKey)?;
    let pq_signature =
        Signature::<MlDsa65>::decode(&encoded).ok_or(IdentityError::InvalidPublicKey)?;
    if pq_key.verify_with_context(&signed, ML_DSA_CONTEXT, &pq_signature) {
        Ok(())
    } else {
        Err(IdentityError::InvalidPublicKey)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pq_key_is_derived_deterministically_from_the_identity() {
        let identity = Identity::generate().expect("identidad");
        let again =
            Identity::from_pkcs8_der(&identity.to_pkcs8_der().expect("PKCS#8")).expect("recargar");
        let a = identity.pq_signing_key().expect("ML-DSA").public_key();
        let b = again.pq_signing_key().expect("ML-DSA").public_key();
        assert_eq!(a, b);
        let other = Identity::generate().expect("otra");
        assert_ne!(a, other.pq_signing_key().expect("ML-DSA").public_key());
        assert_eq!(a.len(), ML_DSA_PUBLIC_KEY_LENGTH);
    }

    #[test]
    fn hybrid_signatures_need_both_halves() {
        let identity = Identity::generate().expect("identidad");
        let key = identity.hybrid_public_key().expect("clave híbrida");
        let key = HybridPublicKey::from_bytes(&key.to_bytes()).expect("ida y vuelta");
        let signature = identity.sign_hybrid(b"hola").expect("firma");
        assert_eq!(signature.to_bytes().len(), HYBRID_SIGNATURE_LENGTH);
        let signature = HybridSignature::from_bytes(&signature.to_bytes()).expect("ida y vuelta");
        assert!(verify_hybrid(&key, b"hola", &signature).is_ok());
        assert!(verify_hybrid(&key, "adiós".as_bytes(), &signature).is_err());

        // Una mitad de otra identidad, o de otro mensaje, invalida el conjunto.
        let other = Identity::generate().expect("otra");
        let forged = other.sign_hybrid(b"hola").expect("firma");
        let mixed = HybridSignature {
            ed25519: signature.ed25519,
            ml_dsa: forged.ml_dsa.clone(),
        };
        assert!(verify_hybrid(&key, b"hola", &mixed).is_err());
        let mixed = HybridSignature {
            ed25519: forged.ed25519,
            ml_dsa: signature.ml_dsa.clone(),
        };
        assert!(verify_hybrid(&key, b"hola", &mixed).is_err());

        // La mitad Ed25519 no es una firma clásica del mismo mensaje.
        assert!(verify_signature(&key.ed25519, b"hola", &signature.ed25519).is_err());
    }

    #[test]
    fn a_pq_key_not_bound_to_the_identity_is_rejected() {
        let identity = Identity::generate().expect("identidad");
        let other = Identity::generate().expect("otra");
        let mut key = identity.hybrid_public_key().expect("clave");
        key.ml_dsa = other.pq_signing_key().expect("ML-DSA").public_key();
        assert!(HybridPublicKey::from_bytes(&key.to_bytes()).is_err());
        assert!(HybridPublicKey::from_bytes(&[0u8; 10]).is_err());
        assert!(HybridSignature::from_bytes(&[0u8; 10]).is_err());
    }
}
