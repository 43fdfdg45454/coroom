//! Criptografía de coroom: identidad, MLS, SFrame y HPKE (`docs/ARQUITECTURA.md` §6).
//!
//! Esta crate no hace IO de red ni depende de ninguna otra crate del workspace (`docs/ARQUITECTURA.md` §5).
//!
//! - [`identity`]: la [`Identity`] Ed25519 de cada par, su huella y dónde se guarda
//!   ([`KeyringStore`], [`FileStore`]).
//! - [`credentials`]: el token de sesión de cada servidor, [`CredentialStore`].
//! - [`safety`]: el [`SafetyNumber`] de 60 dígitos que comparan dos amigos.
//! - [`mls`]: el grupo MLS de la sala (RFC 9420), [`RoomGroup`].
//! - [`sframe`]: cifrado de medios por fotograma o paquete (RFC 9605), [`MediaKeyRing`].
//! - [`hpke`]: sellado y secretos pareja a pareja (RFC 9180), con X-Wing tras la `feature` `pq`.
//! - [`chat`]: cifrado del chat de la sala con la clave de chat de la época MLS ([`ChatRing`]).
//! - [`input`]: cifrado de la entrada del invitado con el secreto pareja a pareja ([`InputCipher`]).
//! - [`domains`]: el registro de todas las etiquetas de dominio y de los dominios de firma, con
//!   los tests de unicidad de la revisión de seguridad.
//! - `hybrid` (`feature` `pq-sign`): firma híbrida Ed25519 + ML-DSA-65 de la identidad.
//!
//! Los benches `criterion` (`benches/e2ee.rs`) y las puertas de `docs/ARQUITECTURA.md` §2.3
//! (`tests/gates.rs`, `cargo test --release -p coroom-crypto -- --ignored`) miden el coste.

pub mod chat;
pub mod credentials;
pub mod domains;
pub mod hpke;
#[cfg(feature = "pq-sign")]
pub mod hybrid;
pub mod identity;
pub mod input;
pub mod mls;
pub mod safety;
pub mod sealed;
pub mod sframe;

pub use chat::{ChatCryptoError, ChatKey, ChatKind, ChatRing};
pub use credentials::CredentialStore;
pub use identity::{
    FileStore, Identity, IdentityError, IdentityStore, KeyringStore, PUBLIC_KEY_LENGTH,
    SIGNATURE_LENGTH, StoreError, sha256_fingerprint, verify_signature,
};
pub use input::{InputCipher, InputCryptoError};
pub use mls::{KeyPackageBundle, Member, MembershipChange, MlsError, Processed, RoomGroup};
pub use safety::SafetyNumber;
pub use sealed::{SealKey, SealedFile};
pub use sframe::{
    DecryptedFrame, FrameDecryptor, FrameEncryptor, FrameInfo, MediaError, MediaKey, MediaKeyRing,
};
