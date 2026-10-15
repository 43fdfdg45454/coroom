//! Grupo MLS de la sala (RFC 9420, `openmls`).
//!
//! - Suite obligatoria `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`, proveedor
//!   `openmls_rust_crypto` con almacenamiento en memoria (la persistencia del grupo llega con las
//!   cuentas). Cada [`RoomGroup`] y cada [`KeyPackageBundle`] tienen su propio proveedor.
//! - La credencial es básica y contiene la clave pública Ed25519 de la [`Identity`]; la clave de
//!   firma MLS **es** la clave de la identidad: [`RoomGroup`] firma con la `SigningKey` de la
//!   identidad mediante el trait `Signer` de `openmls_traits` (en lugar de copiar los bytes de la
//!   clave privada a un `SignatureKeyPair` de `openmls_basic_credential`), así la huella de la
//!   identidad es la del miembro. Se rechaza cualquier `KeyPackage` cuya credencial no coincida con
//!   su clave de firma.
//! - Solo el anfitrión (quien crea el grupo) hace commits: los invitados rechazan commits de otros
//!   miembros ([`MlsError::UnauthorizedCommit`]). Por eso no hay commits concurrentes y el
//!   anfitrión fusiona el suyo en cuanto lo crea.
//! - Los commits viajan como `PublicMessage` firmados y con *membership tag*
//!   (`PURE_PLAINTEXT`; los mensajes de aplicación siempre van cifrados como `PrivateMessage`).
//!   Así un commit manipulado se rechaza sin gastar secretos del *secret tree*: con commits
//!   cifrados, una copia corrupta consumiría la generación y el commit bueno ya no se podría
//!   descifrar (`SecretReuseError`). Lo que el servidor ve de un commit (quién entra o sale, sus
//!   claves públicas) ya lo sabe porque enruta la sala; los secretos de ruta van cifrados con HPKE.
//! - El `Welcome` lleva el árbol (`ratchet_tree` como extensión), así un invitado no necesita nada
//!   más para entrar.
//!
//! El servidor es el *delivery service*: reenvía `commit` y `welcome` sin poder leerlos.

use std::fmt;

use ed25519_dalek::{Signer as _, SigningKey};
use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
use openmls::prelude::{
    BasicCredential, Ciphersuite, CredentialWithKey, KeyPackage, KeyPackageIn, LeafNodeIndex,
    MlsGroup, MlsGroupCreateConfig, MlsGroupJoinConfig, MlsMessageBodyIn, MlsMessageIn,
    MlsMessageOut, OpenMlsProvider, PURE_PLAINTEXT_WIRE_FORMAT_POLICY, ProcessedMessageContent,
    ProtocolVersion, Sender, StagedCommit, StagedWelcome,
};
use openmls_rust_crypto::OpenMlsRustCrypto;
use openmls_traits::signatures::{Signer, SignerError};
use openmls_traits::types::SignatureScheme;
use zeroize::Zeroizing;

use crate::chat::{CHAT_EXPORTER_LABEL, CHAT_KEY_LENGTH, ChatKey, chat_key_context};
use crate::identity::{Identity, PUBLIC_KEY_LENGTH};
use crate::sframe::{MEDIA_KEY_LENGTH, MediaKey, media_key_context};

/// Suite MLS de coroom (la obligatoria del RFC 9420).
pub const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

/// Etiqueta del exportador MLS para las claves SFrame por emisor y época.
pub const SFRAME_EXPORTER_LABEL: &str = "coroom sframe v1";

/// Error del grupo MLS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MlsError {
    /// Los bytes no son un mensaje MLS válido.
    Malformed(String),
    /// El `KeyPackage` no es válido (firma, versión o suite).
    InvalidKeyPackage(String),
    /// La credencial no contiene la misma clave Ed25519 con la que firma el miembro.
    CredentialMismatch,
    /// Esa identidad ya es miembro del grupo.
    AlreadyMember,
    /// No hay ningún miembro en esa hoja.
    UnknownMember(u32),
    /// El anfitrión no puede expulsarse a sí mismo.
    CannotRemoveSelf,
    /// Un commit que no viene del anfitrión.
    UnauthorizedCommit {
        /// Hoja del emisor, si es miembro.
        sender: Option<u32>,
    },
    /// Se esperaba un `Welcome`.
    NotAWelcome,
    /// El mensaje no es de un tipo que la sala admita (propuestas sueltas, uniones externas).
    Unsupported(&'static str),
    /// Ya no somos miembros del grupo (nos expulsaron).
    Inactive,
    /// `openmls` rechazó la operación.
    Group(String),
}

impl fmt::Display for MlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MlsError::Malformed(err) => write!(f, "mensaje MLS mal formado: {err}"),
            MlsError::InvalidKeyPackage(err) => write!(f, "KeyPackage no válido: {err}"),
            MlsError::CredentialMismatch => {
                f.write_str("la credencial MLS no coincide con la clave de identidad que firma")
            }
            MlsError::AlreadyMember => f.write_str("esa identidad ya está en la sala"),
            MlsError::UnknownMember(leaf) => write!(f, "no hay ningún miembro en la hoja {leaf}"),
            MlsError::CannotRemoveSelf => {
                f.write_str("el anfitrión no puede expulsarse a sí mismo")
            }
            MlsError::UnauthorizedCommit { sender: Some(leaf) } => {
                write!(
                    f,
                    "commit rechazado: lo envía la hoja {leaf}, que no es el anfitrión"
                )
            }
            MlsError::UnauthorizedCommit { sender: None } => {
                f.write_str("commit rechazado: no lo envía el anfitrión")
            }
            MlsError::NotAWelcome => f.write_str("se esperaba un mensaje Welcome de MLS"),
            MlsError::Unsupported(what) => write!(f, "mensaje MLS no admitido en la sala: {what}"),
            MlsError::Inactive => f.write_str("ya no formas parte de la sala"),
            MlsError::Group(err) => write!(f, "el grupo MLS rechazó la operación: {err}"),
        }
    }
}

impl std::error::Error for MlsError {}

fn group_error(err: impl fmt::Debug) -> MlsError {
    MlsError::Group(format!("{err:?}"))
}

/// Miembro de la sala.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Member {
    /// Hoja en el árbol MLS; identifica al emisor en SFrame.
    pub leaf_index: u32,
    /// Clave pública Ed25519 de su identidad.
    pub public_key: [u8; PUBLIC_KEY_LENGTH],
}

/// Resultado de añadir o expulsar a un miembro (lo genera el anfitrión).
#[derive(Debug, Clone)]
pub struct MembershipChange {
    /// Commit para difundir a todos los miembros que ya estaban.
    pub commit: Vec<u8>,
    /// `Welcome` para el miembro nuevo (solo al añadir).
    pub welcome: Option<Vec<u8>>,
    /// Época nueva del grupo, ya fusionada en el anfitrión.
    pub epoch: u64,
    /// Miembro añadido (solo al añadir).
    pub added: Option<Member>,
}

/// Lo que produjo [`RoomGroup::process`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Processed {
    /// Commit del anfitrión aplicado: hay época nueva y hay que reinstalar las claves de medios.
    Commit {
        /// Época nueva.
        epoch: u64,
        /// Miembros añadidos por el commit.
        added: Vec<Member>,
        /// Hojas expulsadas por el commit.
        removed: Vec<u32>,
    },
    /// El commit nos expulsa: el grupo queda inactivo y no deriva más claves.
    Removed {
        /// Época en la que se nos expulsó.
        epoch: u64,
    },
    /// Mensaje de aplicación de otro miembro.
    Application {
        /// Hoja del emisor.
        sender_leaf: u32,
        /// Contenido descifrado.
        data: Vec<u8>,
    },
    /// Un mensaje propio que el servidor nos ha devuelto; se ignora.
    Own,
}

/// Firma MLS con la clave Ed25519 de la identidad.
struct IdentitySigner(SigningKey);

impl IdentitySigner {
    fn new(identity: &Identity) -> Self {
        Self(identity.signing_key().clone())
    }

    fn public_key(&self) -> [u8; PUBLIC_KEY_LENGTH] {
        self.0.verifying_key().to_bytes()
    }

    fn credential(&self) -> CredentialWithKey {
        let public_key = self.public_key();
        CredentialWithKey {
            credential: BasicCredential::new(public_key.to_vec()).into(),
            signature_key: public_key.to_vec().into(),
        }
    }
}

impl Signer for IdentitySigner {
    fn sign(&self, payload: &[u8]) -> Result<Vec<u8>, SignerError> {
        Ok(self.0.sign(payload).to_bytes().to_vec())
    }

    fn signature_scheme(&self) -> SignatureScheme {
        SignatureScheme::ED25519
    }
}

/// Clave pública de un miembro si su credencial básica coincide con su clave de firma.
fn member_key(credential_identity: &[u8], signature_key: &[u8]) -> Result<[u8; 32], MlsError> {
    if credential_identity != signature_key {
        return Err(MlsError::CredentialMismatch);
    }
    <[u8; PUBLIC_KEY_LENGTH]>::try_from(signature_key).map_err(|_| MlsError::CredentialMismatch)
}

fn credential_identity(credential: &openmls::prelude::Credential) -> Result<Vec<u8>, MlsError> {
    BasicCredential::try_from(credential.clone())
        .map(|basic| basic.identity().to_vec())
        .map_err(|_| MlsError::CredentialMismatch)
}

fn key_package_member_key(key_package: &KeyPackage) -> Result<[u8; 32], MlsError> {
    let leaf = key_package.leaf_node();
    member_key(
        &credential_identity(leaf.credential())?,
        leaf.signature_key().as_slice(),
    )
}

fn join_config() -> MlsGroupJoinConfig {
    MlsGroupJoinConfig::builder()
        .wire_format_policy(PURE_PLAINTEXT_WIRE_FORMAT_POLICY)
        .use_ratchet_tree_extension(true)
        .build()
}

fn serialize(message: &MlsMessageOut) -> Result<Vec<u8>, MlsError> {
    message.to_bytes().map_err(group_error)
}

fn deserialize(message: &[u8]) -> Result<MlsMessageIn, MlsError> {
    MlsMessageIn::tls_deserialize_exact(message).map_err(|err| MlsError::Malformed(err.to_string()))
}

/// `KeyPackage` de un invitado y su estado privado (claves de inicio y de hoja), necesario para
/// entrar con el `Welcome` que responda el anfitrión.
pub struct KeyPackageBundle {
    provider: OpenMlsRustCrypto,
    signer: IdentitySigner,
    key_package: Vec<u8>,
}

impl KeyPackageBundle {
    /// Genera un `KeyPackage` para `identity`. Devuelve el `KeyPackage` serializado (para enviarlo
    /// al anfitrión por la señalización) y el estado privado para [`RoomGroup::join`].
    pub fn generate(identity: &Identity) -> Result<(Vec<u8>, KeyPackageBundle), MlsError> {
        let provider = OpenMlsRustCrypto::default();
        let signer = IdentitySigner::new(identity);
        let bundle = KeyPackage::builder()
            .build(CIPHERSUITE, &provider, &signer, signer.credential())
            .map_err(group_error)?;
        let key_package = bundle
            .key_package()
            .tls_serialize_detached()
            .map_err(|err| MlsError::Malformed(err.to_string()))?;
        Ok((
            key_package.clone(),
            KeyPackageBundle {
                provider,
                signer,
                key_package,
            },
        ))
    }

    /// El `KeyPackage` serializado.
    pub fn key_package(&self) -> &[u8] {
        &self.key_package
    }
}

impl fmt::Debug for KeyPackageBundle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyPackageBundle").finish_non_exhaustive()
    }
}

/// Grupo MLS de una sala; el mismo tipo para el anfitrión y los invitados.
pub struct RoomGroup {
    provider: OpenMlsRustCrypto,
    signer: IdentitySigner,
    group: MlsGroup,
    /// Hoja del anfitrión: el único miembro que puede hacer commits.
    host_leaf: u32,
}

impl RoomGroup {
    /// Crea el grupo con el anfitrión como único miembro (hoja 0, época 0).
    pub fn create(identity: &Identity) -> Result<RoomGroup, MlsError> {
        let provider = OpenMlsRustCrypto::default();
        let signer = IdentitySigner::new(identity);
        let config = MlsGroupCreateConfig::builder()
            .ciphersuite(CIPHERSUITE)
            .wire_format_policy(PURE_PLAINTEXT_WIRE_FORMAT_POLICY)
            .use_ratchet_tree_extension(true)
            .build();
        let group =
            MlsGroup::new(&provider, &signer, &config, signer.credential()).map_err(group_error)?;
        let host_leaf = group.own_leaf_index().u32();
        Ok(RoomGroup {
            provider,
            signer,
            group,
            host_leaf,
        })
    }

    /// Entra en el grupo con el `Welcome` del anfitrión y el estado privado del `KeyPackage`.
    pub fn join(welcome: &[u8], key_package: KeyPackageBundle) -> Result<RoomGroup, MlsError> {
        let MlsMessageBodyIn::Welcome(welcome) = deserialize(welcome)?.extract() else {
            return Err(MlsError::NotAWelcome);
        };
        let KeyPackageBundle {
            provider, signer, ..
        } = key_package;
        let staged = StagedWelcome::new_from_welcome(&provider, &join_config(), welcome, None)
            .map_err(group_error)?;
        let host_leaf = staged.welcome_sender_index().u32();
        let group = staged.into_group(&provider).map_err(group_error)?;
        let room = RoomGroup {
            provider,
            signer,
            group,
            host_leaf,
        };
        // Todos los miembros del árbol recibido deben cumplir la regla credencial = clave.
        room.checked_members()?;
        Ok(room)
    }

    /// Añade al invitado del `KeyPackage` (solo el anfitrión). El commit ya queda fusionado:
    /// difundir `commit` a los demás miembros y `welcome` al nuevo.
    pub fn add_member(&mut self, key_package: &[u8]) -> Result<MembershipChange, MlsError> {
        self.ensure_host()?;
        let key_package = KeyPackageIn::tls_deserialize_exact(key_package)
            .map_err(|err| MlsError::InvalidKeyPackage(err.to_string()))?
            .validate(self.provider.crypto(), ProtocolVersion::Mls10)
            .map_err(|err| MlsError::InvalidKeyPackage(format!("{err:?}")))?;
        if key_package.ciphersuite() != CIPHERSUITE {
            return Err(MlsError::InvalidKeyPackage(
                "la suite no es la de coroom".to_owned(),
            ));
        }
        let public_key = key_package_member_key(&key_package)?;
        if self.members().iter().any(|m| m.public_key == public_key) {
            return Err(MlsError::AlreadyMember);
        }
        let (commit, welcome, _group_info) = self
            .group
            .add_members(&self.provider, &self.signer, &[key_package])
            .map_err(group_error)?;
        self.group
            .merge_pending_commit(&self.provider)
            .map_err(group_error)?;
        let added = self
            .members()
            .into_iter()
            .find(|member| member.public_key == public_key);
        Ok(MembershipChange {
            commit: serialize(&commit)?,
            welcome: Some(serialize(&welcome)?),
            epoch: self.epoch(),
            added,
        })
    }

    /// Expulsa al miembro de la hoja `leaf_index` (solo el anfitrión). La época nueva ya no la
    /// puede derivar el expulsado. Difundir `commit` a todos (también al expulsado, para que
    /// sepa que está fuera).
    pub fn remove_member(&mut self, leaf_index: u32) -> Result<MembershipChange, MlsError> {
        self.ensure_host()?;
        if leaf_index == self.own_leaf_index() {
            return Err(MlsError::CannotRemoveSelf);
        }
        if self
            .group
            .member_at(LeafNodeIndex::new(leaf_index))
            .is_none()
        {
            return Err(MlsError::UnknownMember(leaf_index));
        }
        let (commit, _welcome, _group_info) = self
            .group
            .remove_members(
                &self.provider,
                &self.signer,
                &[LeafNodeIndex::new(leaf_index)],
            )
            .map_err(group_error)?;
        self.group
            .merge_pending_commit(&self.provider)
            .map_err(group_error)?;
        Ok(MembershipChange {
            commit: serialize(&commit)?,
            welcome: None,
            epoch: self.epoch(),
            added: None,
        })
    }

    /// Cifra un mensaje de aplicación para el resto del grupo (p. ej. claves de pista o avisos de
    /// control que el servidor no debe leer).
    pub fn create_message(&mut self, data: &[u8]) -> Result<Vec<u8>, MlsError> {
        self.ensure_active()?;
        let message = self
            .group
            .create_message(&self.provider, &self.signer, data)
            .map_err(group_error)?;
        serialize(&message)
    }

    /// Procesa un commit del anfitrión o un mensaje de aplicación de otro miembro. Los mensajes
    /// corruptos, de otra época o de otro grupo devuelven error y no cambian el estado.
    pub fn process(&mut self, message: &[u8]) -> Result<Processed, MlsError> {
        self.ensure_active()?;
        let message = deserialize(message)?
            .try_into_protocol_message()
            .map_err(|err| MlsError::Malformed(format!("{err:?}")))?;
        let processed = self
            .group
            .process_message(&self.provider, message)
            .map_err(group_error)?;
        let sender = match processed.sender() {
            Sender::Member(leaf) => Some(leaf.u32()),
            _ => None,
        };
        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(message) => {
                let sender_leaf = sender.ok_or(MlsError::Unsupported("aplicación externa"))?;
                Ok(Processed::Application {
                    sender_leaf,
                    data: message.into_bytes(),
                })
            }
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                if sender != Some(self.host_leaf) {
                    return Err(MlsError::UnauthorizedCommit { sender });
                }
                self.merge_commit(*staged)
            }
            ProcessedMessageContent::OwnPendingCommit
            | ProcessedMessageContent::OwnPrivateMessage => Ok(Processed::Own),
            ProcessedMessageContent::ProposalMessage(_) => {
                Err(MlsError::Unsupported("propuesta suelta"))
            }
            _ => Err(MlsError::Unsupported("unión externa")),
        }
    }

    fn merge_commit(&mut self, staged: StagedCommit) -> Result<Processed, MlsError> {
        let mut added = Vec::new();
        for add in staged.add_proposals() {
            let public_key = key_package_member_key(add.add_proposal().key_package())?;
            added.push(public_key);
        }
        let removed: Vec<u32> = staged
            .remove_proposals()
            .map(|remove| remove.remove_proposal().removed().u32())
            .collect();
        let self_removed = staged.self_removed();
        self.group
            .merge_staged_commit(&self.provider, staged)
            .map_err(group_error)?;
        let epoch = self.group.epoch().as_u64();
        if self_removed {
            return Ok(Processed::Removed { epoch });
        }
        let members = self.members();
        let added = added
            .into_iter()
            .filter_map(|key| members.iter().copied().find(|m| m.public_key == key))
            .collect();
        Ok(Processed::Commit {
            epoch,
            added,
            removed,
        })
    }

    /// Época actual del grupo.
    pub fn epoch(&self) -> u64 {
        self.group.epoch().as_u64()
    }

    /// `false` si nos han expulsado.
    pub fn is_active(&self) -> bool {
        self.group.is_active()
    }

    /// Miembros actuales, por hoja.
    pub fn members(&self) -> Vec<Member> {
        self.group
            .members()
            .filter_map(|member| {
                let identity = credential_identity(&member.credential).ok()?;
                let public_key = member_key(&identity, &member.signature_key).ok()?;
                Some(Member {
                    leaf_index: member.index.u32(),
                    public_key,
                })
            })
            .collect()
    }

    fn checked_members(&self) -> Result<Vec<Member>, MlsError> {
        self.group
            .members()
            .map(|member| {
                let identity = credential_identity(&member.credential)?;
                Ok(Member {
                    leaf_index: member.index.u32(),
                    public_key: member_key(&identity, &member.signature_key)?,
                })
            })
            .collect()
    }

    /// Hoja propia.
    pub fn own_leaf_index(&self) -> u32 {
        self.group.own_leaf_index().u32()
    }

    /// Hoja del anfitrión.
    pub fn host_leaf_index(&self) -> u32 {
        self.host_leaf
    }

    /// Clave pública de nuestra identidad.
    pub fn own_public_key(&self) -> [u8; PUBLIC_KEY_LENGTH] {
        self.signer.public_key()
    }

    /// Exportador MLS (RFC 9420 §8.5) de la época actual.
    pub fn export_secret(
        &self,
        label: &str,
        context: &[u8],
        len: usize,
    ) -> Result<Zeroizing<Vec<u8>>, MlsError> {
        self.ensure_active()?;
        self.group
            .export_secret(self.provider.crypto(), label, context, len)
            .map(Zeroizing::new)
            .map_err(group_error)
    }

    /// Clave SFrame base de cada miembro en la época actual, lista para
    /// [`MediaKeyRing::install_epoch`](crate::sframe::MediaKeyRing::install_epoch):
    /// `export_secret("coroom sframe v1", leaf_index (u32 BE) || epoch (u64 BE), 32)`.
    pub fn media_keys(&self) -> Result<Vec<(u32, MediaKey)>, MlsError> {
        let epoch = self.epoch();
        self.members()
            .into_iter()
            .map(|member| {
                let context = media_key_context(member.leaf_index, epoch);
                let secret =
                    self.export_secret(SFRAME_EXPORTER_LABEL, &context, MEDIA_KEY_LENGTH)?;
                let mut key = MediaKey::new([0u8; MEDIA_KEY_LENGTH]);
                key.copy_from_slice(&secret);
                Ok((member.leaf_index, key))
            })
            .collect()
    }

    /// Clave del chat de la época actual ([`crate::chat`]):
    /// `export_secret("coroom chat v1", epoch (u64 BE), 32)`.
    pub fn chat_key(&self) -> Result<ChatKey, MlsError> {
        let epoch = self.epoch();
        let secret = self.export_secret(
            CHAT_EXPORTER_LABEL,
            &chat_key_context(epoch),
            CHAT_KEY_LENGTH,
        )?;
        let mut key = [0u8; CHAT_KEY_LENGTH];
        key.copy_from_slice(&secret);
        let key = ChatKey::new(epoch, key);
        Ok(key)
    }

    fn ensure_active(&self) -> Result<(), MlsError> {
        if self.group.is_active() {
            Ok(())
        } else {
            Err(MlsError::Inactive)
        }
    }

    fn ensure_host(&self) -> Result<(), MlsError> {
        self.ensure_active()?;
        if self.own_leaf_index() == self.host_leaf {
            Ok(())
        } else {
            Err(MlsError::UnauthorizedCommit {
                sender: Some(self.own_leaf_index()),
            })
        }
    }
}

impl fmt::Debug for RoomGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RoomGroup")
            .field("epoch", &self.epoch())
            .field("own_leaf_index", &self.own_leaf_index())
            .field("members", &self.group.members().count())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Guest {
        identity: Identity,
        group: Option<RoomGroup>,
    }

    fn exporter(group: &RoomGroup) -> Vec<u8> {
        group.export_secret("test", b"ctx", 32).unwrap().to_vec()
    }

    /// Anfitrión + `n` invitados, todos en la última época.
    fn room(n: usize) -> (Identity, RoomGroup, Vec<Guest>) {
        let host_identity = Identity::generate().unwrap();
        let mut host = RoomGroup::create(&host_identity).unwrap();
        let mut guests: Vec<Guest> = Vec::new();
        for _ in 0..n {
            let identity = Identity::generate().unwrap();
            let (key_package, bundle) = KeyPackageBundle::generate(&identity).unwrap();
            let change = host.add_member(&key_package).unwrap();
            for guest in &mut guests {
                let processed = guest
                    .group
                    .as_mut()
                    .unwrap()
                    .process(&change.commit)
                    .unwrap();
                assert!(
                    matches!(processed, Processed::Commit { epoch, .. } if epoch == change.epoch)
                );
            }
            let group = RoomGroup::join(change.welcome.as_ref().unwrap(), bundle).unwrap();
            assert_eq!(change.added.unwrap().public_key, identity.public_key());
            assert_eq!(group.own_leaf_index(), change.added.unwrap().leaf_index);
            guests.push(Guest {
                identity,
                group: Some(group),
            });
        }
        (host_identity, host, guests)
    }

    #[test]
    fn host_and_three_guests_share_epoch_and_exporter() {
        let (host_identity, host, guests) = room(3);
        assert_eq!(host.epoch(), 3);
        assert_eq!(host.members().len(), 4);
        assert_eq!(host.members()[0].public_key, host_identity.public_key());
        for guest in &guests {
            let group = guest.group.as_ref().unwrap();
            assert_eq!(group.epoch(), host.epoch());
            assert_eq!(exporter(group), exporter(&host));
            assert_eq!(group.members(), host.members());
            assert_eq!(group.host_leaf_index(), host.own_leaf_index());
            assert!(
                group
                    .members()
                    .iter()
                    .any(|m| m.public_key == guest.identity.public_key())
            );
        }
        // Las claves de medios coinciden en todos y son distintas por emisor.
        let keys = host.media_keys().unwrap();
        assert_eq!(keys.len(), 4);
        assert_ne!(*keys[0].1, *keys[1].1);
        for guest in &guests {
            let theirs = guest.group.as_ref().unwrap().media_keys().unwrap();
            assert_eq!(theirs.len(), keys.len());
            for ((a_leaf, a_key), (b_leaf, b_key)) in keys.iter().zip(theirs.iter()) {
                assert_eq!(a_leaf, b_leaf);
                assert_eq!(**a_key, **b_key);
            }
        }
    }

    #[test]
    fn removed_guest_cannot_derive_the_new_exporter() {
        let (_host_identity, mut host, mut guests) = room(3);
        let old = exporter(&host);
        let removed_leaf = guests[1].group.as_ref().unwrap().own_leaf_index();
        let change = host.remove_member(removed_leaf).unwrap();
        assert!(change.welcome.is_none());
        assert_eq!(change.epoch, 4);
        let new = exporter(&host);
        assert_ne!(old, new);

        for (index, guest) in guests.iter_mut().enumerate() {
            let group = guest.group.as_mut().unwrap();
            let processed = group.process(&change.commit).unwrap();
            if index == 1 {
                assert_eq!(processed, Processed::Removed { epoch: 4 });
                assert!(!group.is_active());
                assert_eq!(
                    group.export_secret("test", b"ctx", 32),
                    Err(MlsError::Inactive)
                );
            } else {
                assert_eq!(
                    processed,
                    Processed::Commit {
                        epoch: 4,
                        added: Vec::new(),
                        removed: vec![removed_leaf],
                    }
                );
                assert_eq!(exporter(group), new);
            }
        }
        assert_eq!(host.members().len(), 3);
    }

    #[test]
    fn processing_a_foreign_commit_advances_the_epoch() {
        let (_host_identity, mut host, mut guests) = room(1);
        let identity = Identity::generate().unwrap();
        let (key_package, _bundle) = KeyPackageBundle::generate(&identity).unwrap();
        let change = host.add_member(&key_package).unwrap();
        let group = guests[0].group.as_mut().unwrap();
        assert_eq!(group.epoch(), 1);
        let processed = group.process(&change.commit).unwrap();
        assert_eq!(group.epoch(), 2);
        let Processed::Commit {
            epoch,
            added,
            removed,
        } = processed
        else {
            panic!("se esperaba un commit");
        };
        assert_eq!(epoch, 2);
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].public_key, identity.public_key());
        assert!(removed.is_empty());
    }

    #[test]
    fn application_messages_reach_other_members() {
        let (_host_identity, mut host, mut guests) = room(2);
        let message = host.create_message(b"hola sala").unwrap();
        for guest in &mut guests {
            let processed = guest.group.as_mut().unwrap().process(&message).unwrap();
            assert_eq!(
                processed,
                Processed::Application {
                    sender_leaf: host.own_leaf_index(),
                    data: b"hola sala".to_vec(),
                }
            );
        }
    }

    #[test]
    fn corrupt_messages_return_errors_not_panics() {
        let (_host_identity, mut host, mut guests) = room(1);
        let identity = Identity::generate().unwrap();
        let (key_package, _bundle) = KeyPackageBundle::generate(&identity).unwrap();
        let change = host.add_member(&key_package).unwrap();
        let group = guests[0].group.as_mut().unwrap();

        assert!(group.process(&[]).is_err());
        assert!(group.process(b"basura").is_err());
        for position in [0, 5, change.commit.len() / 2, change.commit.len() - 1] {
            let mut corrupt = change.commit.clone();
            corrupt[position] ^= 0x40;
            assert!(group.process(&corrupt).is_err(), "byte {position}");
        }
        assert!(
            group
                .process(&change.commit[..change.commit.len() - 3])
                .is_err()
        );
        // El estado no cambió: el commit bueno sigue aplicándose.
        assert_eq!(group.epoch(), 1);
        let r = group.process(&change.commit);
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(group.epoch(), 2);

        assert!(host.add_member(b"no es un key package").is_err());
        let (_kp, bundle) = KeyPackageBundle::generate(&identity).unwrap();
        assert!(RoomGroup::join(b"basura", bundle).is_err());
        let (_kp, bundle) = KeyPackageBundle::generate(&identity).unwrap();
        assert_eq!(
            RoomGroup::join(&change.commit, bundle).unwrap_err(),
            MlsError::NotAWelcome
        );
    }

    #[test]
    fn rejects_duplicates_self_removal_and_guest_commits() {
        let (_host_identity, mut host, mut guests) = room(1);
        let guest_identity = &guests[0].identity;
        let (again, _bundle) = KeyPackageBundle::generate(guest_identity).unwrap();
        assert_eq!(
            host.add_member(&again).unwrap_err(),
            MlsError::AlreadyMember
        );
        assert_eq!(
            host.remove_member(host.own_leaf_index()).unwrap_err(),
            MlsError::CannotRemoveSelf
        );
        assert_eq!(
            host.remove_member(42).unwrap_err(),
            MlsError::UnknownMember(42)
        );

        let other = Identity::generate().unwrap();
        let (key_package, _bundle) = KeyPackageBundle::generate(&other).unwrap();
        let group = guests[0].group.as_mut().unwrap();
        assert!(matches!(
            group.add_member(&key_package),
            Err(MlsError::UnauthorizedCommit { .. })
        ));
    }

    #[test]
    fn welcome_needs_the_matching_key_package_state() {
        let host_identity = Identity::generate().unwrap();
        let mut host = RoomGroup::create(&host_identity).unwrap();
        let guest = Identity::generate().unwrap();
        let (key_package, _bundle) = KeyPackageBundle::generate(&guest).unwrap();
        let change = host.add_member(&key_package).unwrap();
        // Otro estado privado (otro KeyPackage) no abre ese Welcome.
        let (_other, other_bundle) = KeyPackageBundle::generate(&guest).unwrap();
        assert!(RoomGroup::join(change.welcome.as_ref().unwrap(), other_bundle).is_err());
    }
}
