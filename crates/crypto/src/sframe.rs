//! Cifrado de medios por fotograma o por paquete con SFrame (RFC 9605).
//!
//! - Suite `AES_256_GCM_SHA512_128` (valor `0x0005` del registro del RFC) sobre la crate `sframe`
//!   (sin `ring` ni OpenSSL): de ella se usan los tipos de clave con su derivación (§4.4.2, el KID
//!   en las etiquetas HKDF), la cabecera (§4.3) y la protección contra repeticiones (§9.3). El
//!   cifrado lo hace un **backend propio** enchufado por los traits `AeadEncrypt`/`AeadDecrypt`/
//!   `KeyDerivation` de la crate: HKDF-SHA512 de RustCrypto (`hkdf` + `sha2`) para derivar y
//!   AES-256-GCM de `aws-lc-rs` (el mismo proveedor que ya usa `rustls`) para cifrar. El backend
//!   RustCrypto de `sframe` (crate `aes-gcm`) da ~1,8 GB/s por sentido en la CPU de referencia, lo
//!   que deja 128 KB cifrar + descifrar en ~150 µs, justo en el umbral de `docs/ARQUITECTURA.md` §2.3; con `aws-lc-rs`
//!   (AES-NI + PCLMULQDQ/AVX en ensamblador) queda muy por debajo. La clave AES expandida se
//!   guarda en el secreto, así que no se rehace la expansión por paquete. El test
//!   `matches_the_rfc_9605_test_vector` comprueba el backend con el vector del apéndice A.
//! - **Claves**: cada emisor tiene una clave base por época, derivada del exportador MLS
//!   (`RoomGroup::media_keys`): `export_secret("coroom sframe v1", leaf_index (u32 BE) ||
//!   epoch (u64 BE), 32)`.
//! - **KID** = `(epoch & 0xFFFF) << 16 | leaf_index` (varint de la cabecera SFrame): único por
//!   emisor y época, y permite que convivan las dos últimas épocas durante una rotación.
//! - **Contador** (CTR) por emisor y época, automático y compartido por todos los
//!   [`FrameEncryptor`] del mismo anillo: (clave, KID, CTR) nunca se repite.
//! - **Pista**: viaja como *metadato* SFrame de un byte, en claro y autenticado. El formato en el
//!   cable es `cabecera SFrame || pista || texto cifrado || etiqueta (16 B)` y el AAD es
//!   `cabecera || pista`, exactamente el `header + metadata` del RFC 9605 §4.4.3. (El API de
//!   alto nivel de la crate pone el metadato *antes* de la cabecera en el AAD; por eso aquí se usa
//!   su API de claves y búferes, que sí sigue el orden del RFC.)
//! - **Sin asignaciones en caliente**: [`FrameEncryptor::encrypt_into`] y
//!   [`FrameDecryptor::decrypt_into`] escriben en un `Vec<u8>` del llamante que se reutiliza
//!   (solo crece la primera vez); cifran y descifran en sitio con una sola copia del contenido.
//!   Sirven igual por fotograma que por paquete en el hilo de envío.
//! - **Rotación**: [`MediaKeyRing`] se comparte (`Clone`) entre el hilo de control, que instala
//!   épocas nuevas, y los hilos de envío y recepción; cada cifrador o descifrador comprueba un
//!   contador de generación atómico por llamada y solo toma el cerrojo cuando cambió.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use ::sframe::CipherSuite;
use ::sframe::crypto::{
    AeadDecrypt, AeadEncrypt, DecryptionBufferView, EncryptionBufferView, KeyDerivation,
    get_hkdf_key_expand_label, get_hkdf_salt_expand_label,
};
use ::sframe::error::SframeError;
use ::sframe::frame::validation::{
    FrameValidation, ReplayAttackProtectionStore, Tolerance, UnvalidatedFrame,
};
use ::sframe::header::{KeyId, SframeHeader};
use ::sframe::key::{GenericDecryptionKey, GenericEncryptionKey};
use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use sha2::Sha512;
use zeroize::Zeroizing;

/// Suite SFrame de coroom.
pub const CIPHER_SUITE: CipherSuite = CipherSuite::AesGcm256Sha512;

/// Longitud de la clave base de un emisor (la que da el exportador MLS).
pub const MEDIA_KEY_LENGTH: usize = 32;

/// Clave base SFrame de un emisor en una época; se borra al soltarse.
pub type MediaKey = Zeroizing<[u8; MEDIA_KEY_LENGTH]>;

/// Hoja más alta que cabe en el KID.
pub const MAX_LEAF_INDEX: u32 = 0xFFFF;

/// Contadores fuera de orden admitidos por KID antes de rechazar uno como repetición vieja.
pub const REPLAY_WINDOW: usize = 128;

/// Etiqueta de autenticación de AES-256-GCM.
pub const TAG_LENGTH: usize = 16;

/// Byte de pista que viaja como metadato.
pub const TRACK_LENGTH: usize = 1;

/// Cabecera SFrame más larga: byte de configuración + KID y CTR de hasta 8 bytes cada uno.
pub const MAX_HEADER_LENGTH: usize = 1 + 8 + 8;

/// Bytes que SFrame añade como máximo a cada fotograma o paquete.
pub const MAX_OVERHEAD: usize = MAX_HEADER_LENGTH + TRACK_LENGTH + TAG_LENGTH;

/// Épocas que conserva el anillo.
pub const RETAINED_EPOCHS: usize = 2;

/// Contexto del exportador MLS para la clave de `leaf_index` en `epoch`.
pub fn media_key_context(leaf_index: u32, epoch: u64) -> [u8; 12] {
    let mut context = [0u8; 12];
    context[..4].copy_from_slice(&leaf_index.to_be_bytes());
    context[4..].copy_from_slice(&epoch.to_be_bytes());
    context
}

/// KID SFrame de un emisor en una época.
pub fn key_id(epoch: u64, leaf_index: u32) -> u64 {
    ((epoch & 0xFFFF) << 16) | u64::from(leaf_index & MAX_LEAF_INDEX)
}

/// Error de cifrado de medios.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaError {
    /// La hoja no cabe en el KID.
    LeafOutOfRange(u32),
    /// Las épocas deben instalarse en orden creciente.
    EpochNotNewer {
        /// Época que se intentó instalar.
        epoch: u64,
        /// Época más nueva ya instalada.
        newest: u64,
    },
    /// La época nueva comparte los 16 bits bajos con la anterior y sus KID chocarían.
    EpochCollision(u64),
    /// No hay clave instalada para que esta hoja emita.
    NoSendingKey(u32),
    /// Se agotó el contador de esta época (hace falta una época nueva).
    CounterExhausted,
    /// El paquete no tiene una cabecera SFrame válida o es demasiado corto.
    Malformed,
    /// El KID no es de ninguna época instalada.
    UnknownKey(u64),
    /// El contador ya se recibió o es demasiado viejo para la ventana.
    Replay {
        /// KID del paquete.
        key_id: u64,
        /// Contador del paquete.
        counter: u64,
    },
    /// La etiqueta no cuadra: el paquete se manipuló o no es de esta clave.
    Authentication,
    /// Fallo interno de la crate `sframe`.
    Crypto(String),
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MediaError::LeafOutOfRange(leaf) => {
                write!(
                    f,
                    "la hoja {leaf} no cabe en el identificador de clave SFrame"
                )
            }
            MediaError::EpochNotNewer { epoch, newest } => write!(
                f,
                "la época {epoch} no es posterior a la instalada ({newest})"
            ),
            MediaError::EpochCollision(epoch) => write!(
                f,
                "la época {epoch} chocaría con la anterior en el identificador de clave SFrame"
            ),
            MediaError::NoSendingKey(leaf) => {
                write!(f, "no hay clave de medios para emitir desde la hoja {leaf}")
            }
            MediaError::CounterExhausted => {
                f.write_str("se agotó el contador SFrame; hace falta rotar la clave")
            }
            MediaError::Malformed => f.write_str("paquete SFrame mal formado"),
            MediaError::UnknownKey(kid) => {
                write!(
                    f,
                    "clave SFrame {kid:#x} desconocida o de una época retirada"
                )
            }
            MediaError::Replay { key_id, counter } => write!(
                f,
                "paquete SFrame repetido o demasiado viejo (clave {key_id:#x}, contador {counter})"
            ),
            MediaError::Authentication => {
                f.write_str("el paquete SFrame no se autentica: manipulado o de otra clave")
            }
            MediaError::Crypto(err) => write!(f, "fallo de SFrame: {err}"),
        }
    }
}

impl std::error::Error for MediaError {}

fn crypto_error(err: SframeError) -> MediaError {
    MediaError::Crypto(err.to_string())
}

// --- Backend: HKDF-SHA512 (RustCrypto) + AES-256-GCM (aws-lc-rs) -------------------------------

/// Secreto SFrame de un KID (`sframe_key` ya expandida para AES y `sframe_salt`).
struct SframeSecret {
    key: LessSafeKey,
    salt: Zeroizing<[u8; NONCE_LEN]>,
}

impl SframeSecret {
    /// Nonce del RFC 9605 §4.4.3: `sframe_salt XOR CTR` (contador alineado a la derecha).
    fn nonce(&self, counter: u64) -> Nonce {
        let mut nonce = *self.salt;
        for (byte, ctr) in nonce[NONCE_LEN - 8..].iter_mut().zip(counter.to_be_bytes()) {
            *byte ^= ctr;
        }
        Nonce::assume_unique_for_key(nonce)
    }
}

/// AEAD AES-256-GCM de `aws-lc-rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AwsLcAead;

impl TryFrom<CipherSuite> for AwsLcAead {
    type Error = SframeError;

    fn try_from(cipher_suite: CipherSuite) -> Result<Self, SframeError> {
        if cipher_suite == CIPHER_SUITE {
            Ok(AwsLcAead)
        } else {
            Err(SframeError::UnsupportedCipherSuite)
        }
    }
}

impl AeadEncrypt for AwsLcAead {
    type Secret = SframeSecret;

    fn encrypt<'a, B>(
        &self,
        secret: &SframeSecret,
        buffer: B,
        counter: u64,
    ) -> ::sframe::error::Result<()>
    where
        B: Into<EncryptionBufferView<'a>>,
    {
        let view = buffer.into();
        let tag = secret
            .key
            .seal_in_place_separate_tag(secret.nonce(counter), Aad::from(&*view.aad), view.data)
            .map_err(|_| SframeError::EncryptionFailure)?;
        if view.tag.len() != tag.as_ref().len() {
            return Err(SframeError::EncryptionFailure);
        }
        view.tag.copy_from_slice(tag.as_ref());
        Ok(())
    }
}

impl AeadDecrypt for AwsLcAead {
    type Secret = SframeSecret;

    fn decrypt<'a, B>(
        &self,
        secret: &SframeSecret,
        buffer: B,
        counter: u64,
    ) -> ::sframe::error::Result<()>
    where
        B: Into<DecryptionBufferView<'a>>,
    {
        let view = buffer.into();
        secret
            .key
            .open_in_place(secret.nonce(counter), Aad::from(&*view.aad), view.data)
            .map(|_| ())
            .map_err(|_| SframeError::DecryptionFailure)
    }
}

/// HKDF-SHA512 del RFC 9605 §4.4.2: `HKDF-Extract("", base_key)` y `HKDF-Expand` con las
/// etiquetas `"SFrame 1.0 Secret key " || KID || suite` y `"SFrame 1.0 Secret salt " || …`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HkdfSha512;

impl KeyDerivation for HkdfSha512 {
    type Secret = SframeSecret;

    fn expand_from<M, K>(
        cipher_suite: CipherSuite,
        key_material: M,
        key_id: K,
    ) -> ::sframe::error::Result<SframeSecret>
    where
        M: AsRef<[u8]>,
        K: Into<KeyId>,
    {
        if cipher_suite != CIPHER_SUITE {
            return Err(SframeError::UnsupportedCipherSuite);
        }
        let key_id = key_id.into();
        let hkdf = hkdf::Hkdf::<Sha512>::new(None, key_material.as_ref());
        let mut key = Zeroizing::new([0u8; 32]);
        hkdf.expand(
            &get_hkdf_key_expand_label(key_id, cipher_suite),
            key.as_mut(),
        )
        .map_err(|_| SframeError::KeyDerivationFailure)?;
        let mut salt = Zeroizing::new([0u8; NONCE_LEN]);
        hkdf.expand(
            &get_hkdf_salt_expand_label(key_id, cipher_suite),
            salt.as_mut(),
        )
        .map_err(|_| SframeError::KeyDerivationFailure)?;
        let key = UnboundKey::new(&AES_256_GCM, key.as_slice())
            .map_err(|_| SframeError::KeyDerivationFailure)?;
        Ok(SframeSecret {
            key: LessSafeKey::new(key),
            salt,
        })
    }
}

/// Clave de cifrado SFrame de un KID.
type EncryptionKey = GenericEncryptionKey<AwsLcAead, HkdfSha512>;
/// Clave de descifrado SFrame de un KID.
type DecryptionKey = GenericDecryptionKey<AwsLcAead, HkdfSha512>;

/// Claves de un emisor en una época.
struct SenderKeys {
    leaf_index: u32,
    key_id: u64,
    encryption: EncryptionKey,
    decryption: DecryptionKey,
    /// Próximo contador para cuando emitimos nosotros con esta clave.
    next_counter: AtomicU64,
}

/// Claves de todos los emisores de una época.
struct EpochKeys {
    epoch: u64,
    senders: Vec<SenderKeys>,
}

impl EpochKeys {
    fn sender(&self, leaf_index: u32) -> Option<&SenderKeys> {
        self.senders.iter().find(|s| s.leaf_index == leaf_index)
    }
}

struct Shared {
    generation: AtomicU64,
    epochs: Mutex<VecDeque<Arc<EpochKeys>>>,
}

/// Anillo de claves de medios: las dos últimas épocas instaladas, con una clave por emisor.
///
/// Es barato de clonar y todas las copias comparten estado.
#[derive(Clone)]
pub struct MediaKeyRing {
    shared: Arc<Shared>,
}

impl MediaKeyRing {
    /// Anillo vacío.
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                generation: AtomicU64::new(0),
                epochs: Mutex::new(VecDeque::with_capacity(RETAINED_EPOCHS + 1)),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, VecDeque<Arc<EpochKeys>>> {
        // Si otro hilo entró en pánico con el cerrojo, el estado sigue siendo coherente: cada
        // instalación sustituye entradas completas.
        self.shared
            .epochs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Instala las claves base de `epoch` (una por hoja, de `RoomGroup::media_keys`). Debe ser
    /// posterior a la última instalada; se retira la más vieja si ya había dos.
    pub fn install_epoch(&self, epoch: u64, members: &[(u32, MediaKey)]) -> Result<(), MediaError> {
        let mut senders = Vec::with_capacity(members.len());
        for (leaf_index, base_key) in members {
            let leaf_index = *leaf_index;
            if leaf_index > MAX_LEAF_INDEX {
                return Err(MediaError::LeafOutOfRange(leaf_index));
            }
            let kid = key_id(epoch, leaf_index);
            senders.push(SenderKeys {
                leaf_index,
                key_id: kid,
                encryption: EncryptionKey::derive_from(CIPHER_SUITE, kid, base_key.as_slice())
                    .map_err(crypto_error)?,
                decryption: DecryptionKey::derive_from(CIPHER_SUITE, kid, base_key.as_slice())
                    .map_err(crypto_error)?,
                next_counter: AtomicU64::new(0),
            });
        }
        let mut epochs = self.lock();
        if let Some(newest) = epochs.back() {
            if epoch <= newest.epoch {
                return Err(MediaError::EpochNotNewer {
                    epoch,
                    newest: newest.epoch,
                });
            }
            if epoch & 0xFFFF == newest.epoch & 0xFFFF {
                return Err(MediaError::EpochCollision(epoch));
            }
        }
        epochs.push_back(Arc::new(EpochKeys { epoch, senders }));
        while epochs.len() > RETAINED_EPOCHS {
            epochs.pop_front();
        }
        self.shared.generation.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// Épocas instaladas, de la más vieja a la más nueva.
    pub fn epochs(&self) -> Vec<u64> {
        self.lock().iter().map(|keys| keys.epoch).collect()
    }

    /// Cifrador para emitir como `own_leaf` (con la época más nueva en la que tenga clave).
    pub fn encryptor(&self, own_leaf: u32) -> FrameEncryptor {
        FrameEncryptor {
            ring: self.clone(),
            own_leaf,
            generation: u64::MAX,
            current: None,
        }
    }

    /// Descifrador de los paquetes de todos los emisores de las épocas instaladas.
    pub fn decryptor(&self) -> FrameDecryptor {
        FrameDecryptor {
            ring: self.clone(),
            generation: u64::MAX,
            installed: Installed(Vec::new()),
            replay: ReplayAttackProtectionStore::new(Tolerance::new(REPLAY_WINDOW)),
            aad: [0u8; MAX_HEADER_LENGTH + TRACK_LENGTH],
        }
    }

    fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }

    fn snapshot(&self) -> (u64, Vec<Arc<EpochKeys>>) {
        let epochs = self.lock();
        (self.generation(), epochs.iter().cloned().collect())
    }
}

impl Default for MediaKeyRing {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for MediaKeyRing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaKeyRing")
            .field("epochs", &self.epochs())
            .finish_non_exhaustive()
    }
}

/// Lo que dice un paquete descifrado sobre sí mismo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameInfo {
    /// Hoja MLS del emisor.
    pub sender_leaf: u32,
    /// Época de la clave.
    pub epoch: u64,
    /// Pista (byte de metadato autenticado).
    pub track: u8,
    /// Contador SFrame del paquete.
    pub counter: u64,
}

/// Paquete descifrado con su contenido en un búfer propio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecryptedFrame {
    /// Hoja MLS del emisor.
    pub sender_leaf: u32,
    /// Época de la clave.
    pub epoch: u64,
    /// Pista.
    pub track: u8,
    /// Contenido en claro.
    pub payload: Vec<u8>,
}

/// Cifrador de un emisor. Un contador por (emisor, época) compartido con el anillo.
pub struct FrameEncryptor {
    ring: MediaKeyRing,
    own_leaf: u32,
    generation: u64,
    current: Option<(Arc<EpochKeys>, usize)>,
}

impl FrameEncryptor {
    fn refresh(&mut self) {
        let generation = self.ring.generation();
        if generation == self.generation {
            return;
        }
        let (generation, epochs) = self.ring.snapshot();
        self.generation = generation;
        self.current = epochs.into_iter().rev().find_map(|keys| {
            let index = keys
                .senders
                .iter()
                .position(|s| s.leaf_index == self.own_leaf)?;
            Some((keys, index))
        });
    }

    /// Época con la que cifra ahora, si tiene clave.
    pub fn epoch(&mut self) -> Option<u64> {
        self.refresh();
        self.current.as_ref().map(|(keys, _)| keys.epoch)
    }

    /// Cifra `frame` de la pista `track` en `out` (se sustituye su contenido y se reutiliza su
    /// capacidad). Devuelve el contador usado.
    pub fn encrypt_into(
        &mut self,
        track: u8,
        frame: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<u64, MediaError> {
        self.refresh();
        let (keys, index) = self
            .current
            .as_ref()
            .ok_or(MediaError::NoSendingKey(self.own_leaf))?;
        let sender = &keys.senders[*index];
        let counter = sender.next_counter.fetch_add(1, Ordering::Relaxed);
        if counter == u64::MAX {
            return Err(MediaError::CounterExhausted);
        }
        seal(
            &sender.encryption,
            sender.key_id,
            counter,
            &[track],
            frame,
            out,
        )?;
        Ok(counter)
    }

    /// Como [`FrameEncryptor::encrypt_into`], con un búfer nuevo.
    pub fn encrypt(&mut self, track: u8, frame: &[u8]) -> Result<Vec<u8>, MediaError> {
        let mut out = Vec::with_capacity(frame.len() + MAX_OVERHEAD);
        self.encrypt_into(track, frame, &mut out)?;
        Ok(out)
    }
}

impl fmt::Debug for FrameEncryptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameEncryptor")
            .field("own_leaf", &self.own_leaf)
            .finish_non_exhaustive()
    }
}

/// Cifra en `out`: `header || metadata || ciphertext || tag`, con AAD = `header || metadata`.
fn seal(
    key: &EncryptionKey,
    kid: u64,
    counter: u64,
    metadata: &[u8],
    plaintext: &[u8],
    out: &mut Vec<u8>,
) -> Result<(), MediaError> {
    let header = SframeHeader::new(kid, counter);
    let aad_len = header.len() + metadata.len();
    out.clear();
    out.resize(aad_len, 0);
    header.serialize(&mut out[..]).map_err(crypto_error)?;
    out[header.len()..aad_len].copy_from_slice(metadata);
    out.extend_from_slice(plaintext);
    out.resize(aad_len + plaintext.len() + TAG_LENGTH, 0);
    let (aad, rest) = out.split_at_mut(aad_len);
    let (data, tag) = rest.split_at_mut(plaintext.len());
    key.encrypt(EncryptionBufferView { aad, data, tag }, counter)
        .map_err(crypto_error)
}

/// Claves de las épocas instaladas, buscadas por KID.
struct Installed(Vec<Arc<EpochKeys>>);

impl Installed {
    fn find(&self, kid: u64) -> Option<(u64, &SenderKeys)> {
        let leaf = u32::try_from(kid & u64::from(MAX_LEAF_INDEX)).ok()?;
        self.0.iter().rev().find_map(|keys| {
            let sender = keys.sender(leaf)?;
            (sender.key_id == kid).then_some((keys.epoch, sender))
        })
    }
}

/// Descifrador de los paquetes de todos los emisores, con ventana anti-repetición por KID.
pub struct FrameDecryptor {
    ring: MediaKeyRing,
    generation: u64,
    installed: Installed,
    replay: ReplayAttackProtectionStore,
    aad: [u8; MAX_HEADER_LENGTH + TRACK_LENGTH],
}

impl FrameDecryptor {
    fn refresh(&mut self) {
        let generation = self.ring.generation();
        if generation == self.generation {
            return;
        }
        let (generation, epochs) = self.ring.snapshot();
        self.generation = generation;
        self.installed = Installed(epochs);
        let installed = &self.installed;
        self.replay.retain(|kid| {
            installed
                .0
                .iter()
                .any(|keys| keys.senders.iter().any(|s| s.key_id == kid))
        });
    }

    /// Descifra `data` en `out` (se sustituye su contenido y se reutiliza su capacidad) y
    /// devuelve quién lo envió y el contenido, que es `out` entero.
    pub fn decrypt_into<'o>(
        &mut self,
        data: &[u8],
        out: &'o mut Vec<u8>,
    ) -> Result<(FrameInfo, &'o [u8]), MediaError> {
        self.refresh();
        let header = SframeHeader::deserialize(data).map_err(|_| MediaError::Malformed)?;
        let header_len = header.len();
        let aad_len = header_len + TRACK_LENGTH;
        if data.len() < aad_len + TAG_LENGTH {
            return Err(MediaError::Malformed);
        }
        let track = data[header_len];
        let kid = header.key_id();
        let counter = header.counter();
        let (epoch, sender) = self
            .installed
            .find(kid)
            .ok_or(MediaError::UnknownKey(kid))?;
        let token = self
            .replay
            .screen(UnvalidatedFrame::new(&header, &data[header_len..aad_len]))
            .map_err(|_| MediaError::Replay {
                key_id: kid,
                counter,
            })?;

        self.aad[..aad_len].copy_from_slice(&data[..aad_len]);
        out.clear();
        out.extend_from_slice(&data[aad_len..]);
        sender
            .decryption
            .decrypt(
                DecryptionBufferView {
                    aad: &mut self.aad[..aad_len],
                    data: &mut out[..],
                },
                counter,
            )
            .map_err(|_| MediaError::Authentication)?;
        out.truncate(out.len() - TAG_LENGTH);
        let info = FrameInfo {
            sender_leaf: sender.leaf_index,
            epoch,
            track,
            counter,
        };
        // Solo se apunta el contador cuando el paquete se ha autenticado.
        self.replay.record(token);
        Ok((info, &out[..]))
    }

    /// Como [`FrameDecryptor::decrypt_into`], con un búfer nuevo.
    pub fn decrypt(&mut self, data: &[u8]) -> Result<DecryptedFrame, MediaError> {
        let mut payload = Vec::with_capacity(data.len());
        let (info, _) = self.decrypt_into(data, &mut payload)?;
        Ok(DecryptedFrame {
            sender_leaf: info.sender_leaf,
            epoch: info.epoch,
            track: info.track,
            payload,
        })
    }
}

impl fmt::Debug for FrameDecryptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameDecryptor").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> MediaKey {
        MediaKey::new([byte; MEDIA_KEY_LENGTH])
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    /// RFC 9605, apéndice A, vector de `AES_256_GCM_SHA512_128` (con metadato).
    #[test]
    fn matches_the_rfc_9605_test_vector() {
        let base_key = hex("000102030405060708090a0b0c0d0e0f");
        let kid = 0x123;
        let ctr = 0x4567;
        let metadata = hex("4945544620534672616d65205747");
        let plaintext = hex("64726166742d696574662d736672616d652d656e63");
        let expected = hex(
            "990123456794f509d36e9beacb0e261d99c7d1e972f1fed787d4049f17ca21353c1cc24d56ceabced279",
        );
        let encryption = EncryptionKey::derive_from(CIPHER_SUITE, kid, &base_key).unwrap();
        let mut out = Vec::new();
        seal(&encryption, kid, ctr, &metadata, &plaintext, &mut out).unwrap();
        // En el vector, el metadato no viaja con el SFrame: se quita del cable para comparar.
        let header_len = 5;
        let mut sframe = out[..header_len].to_vec();
        sframe.extend_from_slice(&out[header_len + metadata.len()..]);
        assert_eq!(sframe, expected);
    }

    #[test]
    fn round_trip_with_three_senders_and_two_overlapping_epochs() {
        let ring = MediaKeyRing::new();
        ring.install_epoch(7, &[(0, key(1)), (1, key(2)), (2, key(3))])
            .unwrap();
        let mut old: Vec<FrameEncryptor> = (0..3).map(|leaf| ring.encryptor(leaf)).collect();
        let old_frames: Vec<Vec<u8>> = old
            .iter_mut()
            .enumerate()
            .map(|(leaf, enc)| enc.encrypt(leaf as u8, &[leaf as u8; 100]).unwrap())
            .collect();

        ring.install_epoch(8, &[(0, key(4)), (1, key(5)), (2, key(6))])
            .unwrap();
        let new_frames: Vec<Vec<u8>> = old
            .iter_mut()
            .enumerate()
            .map(|(leaf, enc)| {
                assert_eq!(enc.epoch(), Some(8));
                enc.encrypt(10 + leaf as u8, &[0x80 | leaf as u8; 2000])
                    .unwrap()
            })
            .collect();

        let mut decryptor = ring.decryptor();
        for (leaf, frame) in old_frames.iter().enumerate() {
            let decrypted = decryptor.decrypt(frame).unwrap();
            assert_eq!(decrypted.sender_leaf, leaf as u32);
            assert_eq!(decrypted.epoch, 7);
            assert_eq!(decrypted.track, leaf as u8);
            assert_eq!(decrypted.payload, vec![leaf as u8; 100]);
        }
        for (leaf, frame) in new_frames.iter().enumerate() {
            let decrypted = decryptor.decrypt(frame).unwrap();
            assert_eq!(decrypted.sender_leaf, leaf as u32);
            assert_eq!(decrypted.epoch, 8);
            assert_eq!(decrypted.track, 10 + leaf as u8);
            assert_eq!(decrypted.payload, vec![0x80 | leaf as u8; 2000]);
        }
    }

    #[test]
    fn tampering_any_byte_fails() {
        let ring = MediaKeyRing::new();
        ring.install_epoch(1, &[(0, key(9))]).unwrap();
        let frame = ring.encryptor(0).encrypt(3, b"fotograma secreto").unwrap();
        for position in 0..frame.len() {
            let mut tampered = frame.clone();
            tampered[position] ^= 0x01;
            assert!(
                ring.decryptor().decrypt(&tampered).is_err(),
                "byte {position}"
            );
        }
        assert!(ring.decryptor().decrypt(&frame[..frame.len() - 1]).is_err());
        assert_eq!(ring.decryptor().decrypt(&[]), Err(MediaError::Malformed));
        assert!(ring.decryptor().decrypt(&frame).is_ok());
    }

    #[test]
    fn replays_are_rejected() {
        let ring = MediaKeyRing::new();
        ring.install_epoch(1, &[(0, key(9))]).unwrap();
        let mut encryptor = ring.encryptor(0);
        let mut decryptor = ring.decryptor();
        let frames: Vec<Vec<u8>> = (0..200)
            .map(|i| encryptor.encrypt(0, &[i as u8; 32]).unwrap())
            .collect();
        assert!(decryptor.decrypt(&frames[0]).is_ok());
        assert!(matches!(
            decryptor.decrypt(&frames[0]),
            Err(MediaError::Replay { counter: 0, .. })
        ));
        // Fuera de orden dentro de la ventana: se acepta una vez.
        assert!(decryptor.decrypt(&frames[150]).is_ok());
        assert!(decryptor.decrypt(&frames[100]).is_ok());
        assert!(decryptor.decrypt(&frames[100]).is_err());
        // Más de 128 contadores por detrás del más nuevo: demasiado viejo.
        assert!(matches!(
            decryptor.decrypt(&frames[5]),
            Err(MediaError::Replay { counter: 5, .. })
        ));
        // Un paquete manipulado no consume su contador.
        let mut tampered = frames[160].clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xff;
        assert_eq!(
            decryptor.decrypt(&tampered),
            Err(MediaError::Authentication)
        );
        assert!(decryptor.decrypt(&frames[160]).is_ok());
    }

    #[test]
    fn rotation_keeps_the_previous_epoch_and_drops_older_ones() {
        let ring = MediaKeyRing::new();
        let mut encryptor = ring.encryptor(1);
        assert_eq!(encryptor.encrypt(0, b"x"), Err(MediaError::NoSendingKey(1)));

        ring.install_epoch(1, &[(0, key(1)), (1, key(2))]).unwrap();
        let first = encryptor.encrypt(0, b"epoca 1").unwrap();
        ring.install_epoch(2, &[(0, key(3)), (1, key(4))]).unwrap();
        let second = encryptor.encrypt(0, b"epoca 2").unwrap();

        let mut decryptor = ring.decryptor();
        ring.install_epoch(3, &[(0, key(5)), (1, key(6))]).unwrap();
        let third = encryptor.encrypt(0, b"epoca 3").unwrap();
        assert_eq!(ring.epochs(), vec![2, 3]);

        assert!(matches!(
            decryptor.decrypt(&first),
            Err(MediaError::UnknownKey(kid)) if kid == key_id(1, 1)
        ));
        assert_eq!(decryptor.decrypt(&second).unwrap().epoch, 2);
        assert_eq!(decryptor.decrypt(&third).unwrap().payload, b"epoca 3");
    }

    #[test]
    fn epochs_must_increase_and_leaves_must_fit() {
        let ring = MediaKeyRing::new();
        ring.install_epoch(5, &[(0, key(1))]).unwrap();
        assert_eq!(
            ring.install_epoch(5, &[(0, key(1))]),
            Err(MediaError::EpochNotNewer {
                epoch: 5,
                newest: 5
            })
        );
        assert_eq!(
            ring.install_epoch(5 + 0x1_0000, &[(0, key(1))]),
            Err(MediaError::EpochCollision(5 + 0x1_0000))
        );
        assert_eq!(
            ring.install_epoch(6, &[(0x1_0000, key(1))]),
            Err(MediaError::LeafOutOfRange(0x1_0000))
        );
        assert_eq!(ring.epochs(), vec![5]);
    }

    #[test]
    fn encryptors_of_the_same_sender_never_reuse_a_counter() {
        let ring = MediaKeyRing::new();
        ring.install_epoch(1, &[(0, key(1))]).unwrap();
        let mut a = ring.encryptor(0);
        let mut b = ring.encryptor(0);
        let mut out = Vec::new();
        assert_eq!(a.encrypt_into(0, b"1", &mut out).unwrap(), 0);
        assert_eq!(b.encrypt_into(0, b"2", &mut out).unwrap(), 1);
        assert_eq!(a.encrypt_into(0, b"3", &mut out).unwrap(), 2);
    }

    #[test]
    fn buffers_are_reused() {
        let ring = MediaKeyRing::new();
        ring.install_epoch(1, &[(0, key(1))]).unwrap();
        let mut encryptor = ring.encryptor(0);
        let mut decryptor = ring.decryptor();
        let frame = vec![0x5a; 17 * 1024];
        let mut sealed = Vec::with_capacity(frame.len() + MAX_OVERHEAD);
        let mut opened = Vec::with_capacity(frame.len() + TAG_LENGTH);
        let (sealed_ptr, opened_ptr) = (sealed.as_ptr(), opened.as_ptr());
        for _ in 0..10 {
            encryptor.encrypt_into(0, &frame, &mut sealed).unwrap();
            assert!(sealed.len() <= frame.len() + MAX_OVERHEAD);
            let (info, payload) = decryptor.decrypt_into(&sealed, &mut opened).unwrap();
            assert_eq!(info.track, 0);
            assert_eq!(payload, &frame[..]);
        }
        assert_eq!(sealed.as_ptr(), sealed_ptr);
        assert_eq!(opened.as_ptr(), opened_ptr);
    }
}
