//! Cifrado del chat de la sala con una clave de grupo de la época MLS.
//!
//! El chat viaja por el stream de control, que el relay reenvía (y podría leer): por eso cada
//! mensaje va sellado de extremo a extremo con una clave que solo tienen los miembros de la época.
//!
//! - **Clave**: `export_secret("coroom chat v1", epoch (u64 BE), 32)` del grupo MLS
//!   ([`RoomGroup::chat_key`](crate::RoomGroup::chat_key)), AES-256-GCM de `aws-lc-rs`. Una por
//!   época: quien sale de la sala no puede derivar la de la época nueva.
//! - **Nonce** de 96 bits = `hoja MLS de quien sella (u32 BE) ‖ contador (u64 BE)`: único mientras
//!   el contador de cada emisor crezca dentro de la época, cosa que [`ChatRing::seal`] garantiza
//!   (un contador por época, que vuelve a 0 con la clave nueva).
//! - **Datos asociados** = `época (u64 BE) ‖ tipo` ([`ChatKind`]): un historial no se puede hacer
//!   pasar por un mensaje ni por la orden de vaciar, ni un sellado de una época por otra.
//! - **Anti-repetición**: [`ChatRing::open`] solo acepta, por época y hoja, contadores mayores que
//!   el último abierto (el stream de control es fiable y ordenado) y lo anota solo tras autenticar.
//! - **Épocas que se guardan** ([`CHAT_EPOCHS_KEPT`]): al entrar alguien se conservan las
//!   anteriores (sus miembros siguen todos en la sala) para lo que ya venía de camino; al salir
//!   alguien se olvidan todas ([`ChatRing::install`] con `forget_older`), porque el que salió
//!   las conoce.
//!
//! La clave es de grupo: autentica que lo selló un miembro de la época, no cuál. En la sala en
//! estrella el anfitrión es quien fija autor y hora de cada mensaje (ver `docs/seguridad.md`).

use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use zeroize::Zeroizing;

/// Etiqueta del exportador MLS para la clave del chat de cada época.
pub const CHAT_EXPORTER_LABEL: &str = "coroom chat v1";

/// Bytes de la clave del chat.
pub const CHAT_KEY_LENGTH: usize = 32;

/// Etiqueta AES-GCM que añade cada sellado.
pub const TAG_LEN: usize = 16;

/// Épocas cuyas claves guarda un [`ChatRing`] como mucho.
pub const CHAT_EPOCHS_KEPT: usize = 4;

/// Qué lleva un sellado (va en los datos asociados).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatKind {
    /// Un mensaje.
    Message = 1,
    /// El historial para quien entra.
    History = 2,
    /// La orden de vaciar el chat.
    Clear = 3,
}

/// Error del cifrado del chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatCryptoError {
    /// No hay clave de esa época (todavía no se procesó el commit, o ya se olvidó).
    UnknownEpoch(u64),
    /// No hay ninguna clave instalada.
    NoKey,
    /// La época no es más nueva que la última instalada.
    StaleEpoch {
        /// Época que se quiso instalar.
        epoch: u64,
        /// La más nueva instalada.
        newest: u64,
    },
    /// Contador ya visto (o anterior) para esa época y hoja: repetición.
    Replayed {
        /// Época.
        epoch: u64,
        /// Hoja que selló.
        leaf: u32,
        /// Contador recibido.
        counter: u64,
    },
    /// El contador de la época se agotó (2⁶⁴ sellados).
    Exhausted,
    /// `aws-lc-rs` no pudo sellar.
    Seal,
    /// La autenticación falló: otra clave, otro tipo, otra época o bytes cambiados.
    Corrupt,
}

impl fmt::Display for ChatCryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChatCryptoError::UnknownEpoch(epoch) => {
                write!(f, "no hay clave del chat para la época {epoch}")
            }
            ChatCryptoError::NoKey => f.write_str("todavía no hay clave del chat"),
            ChatCryptoError::StaleEpoch { epoch, newest } => write!(
                f,
                "la época {epoch} del chat no es más nueva que la {newest} ya instalada"
            ),
            ChatCryptoError::Replayed {
                epoch,
                leaf,
                counter,
            } => write!(
                f,
                "mensaje del chat repetido (época {epoch}, hoja {leaf}, contador {counter})"
            ),
            ChatCryptoError::Exhausted => f.write_str("contador del chat agotado"),
            ChatCryptoError::Seal => f.write_str("no se pudo cifrar el mensaje del chat"),
            ChatCryptoError::Corrupt => f.write_str("el mensaje del chat no se pudo autenticar"),
        }
    }
}

impl std::error::Error for ChatCryptoError {}

/// Contexto del exportador MLS para la clave del chat de `epoch`: la época en u64 BE.
pub fn chat_key_context(epoch: u64) -> [u8; 8] {
    epoch.to_be_bytes()
}

/// Nonce de un sellado: `sender_leaf (u32 BE) ‖ counter (u64 BE)`.
pub fn chat_nonce(sender_leaf: u32, counter: u64) -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    nonce[..4].copy_from_slice(&sender_leaf.to_be_bytes());
    nonce[4..].copy_from_slice(&counter.to_be_bytes());
    nonce
}

/// Datos asociados de un sellado: `epoch (u64 BE) ‖ kind`.
pub fn chat_aad(epoch: u64, kind: ChatKind) -> [u8; 9] {
    let mut aad = [0u8; 9];
    aad[..8].copy_from_slice(&epoch.to_be_bytes());
    aad[8] = kind as u8;
    aad
}

/// Clave del chat de una época.
#[derive(Clone)]
pub struct ChatKey {
    epoch: u64,
    secret: Zeroizing<[u8; CHAT_KEY_LENGTH]>,
}

impl fmt::Debug for ChatKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatKey")
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

impl ChatKey {
    /// Clave de `epoch` a partir de los [`CHAT_KEY_LENGTH`] bytes del exportador.
    pub fn new(epoch: u64, secret: [u8; CHAT_KEY_LENGTH]) -> ChatKey {
        ChatKey {
            epoch,
            secret: Zeroizing::new(secret),
        }
    }

    /// Época de la clave.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    fn aead(&self, error: ChatCryptoError) -> Result<LessSafeKey, ChatCryptoError> {
        UnboundKey::new(&AES_256_GCM, self.secret.as_ref())
            .map(LessSafeKey::new)
            .map_err(|_| error)
    }

    /// Sella `plaintext`. El nonce (`sender_leaf ‖ counter`) debe ser único para esta clave: lo
    /// garantiza quien llama ([`ChatRing::seal`] lo hace).
    pub fn seal(
        &self,
        kind: ChatKind,
        sender_leaf: u32,
        counter: u64,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, ChatCryptoError> {
        let mut body = plaintext.to_vec();
        self.aead(ChatCryptoError::Seal)?
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(chat_nonce(sender_leaf, counter)),
                Aad::from(chat_aad(self.epoch, kind)),
                &mut body,
            )
            .map_err(|_| ChatCryptoError::Seal)?;
        Ok(body)
    }

    /// Abre lo que selló [`ChatKey::seal`] con el mismo tipo, hoja y contador.
    pub fn open(
        &self,
        kind: ChatKind,
        sender_leaf: u32,
        counter: u64,
        ciphertext: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, ChatCryptoError> {
        if ciphertext.len() < TAG_LEN {
            return Err(ChatCryptoError::Corrupt);
        }
        let mut body = Zeroizing::new(ciphertext.to_vec());
        let len = self
            .aead(ChatCryptoError::Corrupt)?
            .open_in_place(
                Nonce::assume_unique_for_key(chat_nonce(sender_leaf, counter)),
                Aad::from(chat_aad(self.epoch, kind)),
                body.as_mut_slice(),
            )
            .map_err(|_| ChatCryptoError::Corrupt)?
            .len();
        body.truncate(len);
        Ok(body)
    }
}

/// Un sellado con lo que necesita quien lo abre (lo que viaja en `proto::control::SealedChat`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    /// Época de la clave.
    pub epoch: u64,
    /// Hoja que sella.
    pub sender_leaf: u32,
    /// Contador de esa hoja en la época.
    pub counter: u64,
    /// Texto cifrado con la etiqueta.
    pub ciphertext: Vec<u8>,
}

/// Claves del chat de las últimas épocas, el contador propio y la ventana anti-repetición.
#[derive(Default)]
pub struct ChatRing {
    /// Claves por época, de la más antigua a la más nueva.
    keys: VecDeque<ChatKey>,
    /// Siguiente contador propio en la época más nueva.
    next_counter: u64,
    /// Último contador abierto por época y hoja.
    seen: BTreeMap<(u64, u32), u64>,
}

impl fmt::Debug for ChatRing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatRing")
            .field("epochs", &self.epochs())
            .finish_non_exhaustive()
    }
}

impl ChatRing {
    /// Anillo vacío.
    pub fn new() -> ChatRing {
        ChatRing::default()
    }

    /// Instala la clave de una época más nueva que todas las que hay. Con `forget_older` (alguien
    /// salió de la sala, o es la primera clave de un grupo al que se acaba de entrar) se olvidan
    /// todas las anteriores; si no, se guardan hasta [`CHAT_EPOCHS_KEPT`].
    pub fn install(&mut self, key: ChatKey, forget_older: bool) -> Result<(), ChatCryptoError> {
        if let Some(newest) = self.epoch()
            && key.epoch <= newest
        {
            return Err(ChatCryptoError::StaleEpoch {
                epoch: key.epoch,
                newest,
            });
        }
        if forget_older {
            self.keys.clear();
        }
        self.keys.push_back(key);
        while self.keys.len() > CHAT_EPOCHS_KEPT {
            self.keys.pop_front();
        }
        self.next_counter = 0;
        let oldest = self.keys.front().map_or(0, ChatKey::epoch);
        self.seen.retain(|(epoch, _), _| *epoch >= oldest);
        Ok(())
    }

    /// Época de la clave más nueva.
    pub fn epoch(&self) -> Option<u64> {
        self.keys.back().map(ChatKey::epoch)
    }

    /// Épocas instaladas, de la más antigua a la más nueva.
    pub fn epochs(&self) -> Vec<u64> {
        self.keys.iter().map(ChatKey::epoch).collect()
    }

    /// Clave más nueva.
    pub fn newest(&self) -> Option<&ChatKey> {
        self.keys.back()
    }

    fn key(&self, epoch: u64) -> Option<&ChatKey> {
        self.keys.iter().find(|key| key.epoch == epoch)
    }

    /// Sella con la clave más nueva, como la hoja `own_leaf`, con el contador siguiente.
    pub fn seal(
        &mut self,
        kind: ChatKind,
        own_leaf: u32,
        plaintext: &[u8],
    ) -> Result<Sealed, ChatCryptoError> {
        let key = self.keys.back().ok_or(ChatCryptoError::NoKey)?;
        let counter = self.next_counter;
        let next = counter.checked_add(1).ok_or(ChatCryptoError::Exhausted)?;
        let ciphertext = key.seal(kind, own_leaf, counter, plaintext)?;
        // El contador solo avanza si se selló; nunca se repite.
        self.next_counter = next;
        Ok(Sealed {
            epoch: key.epoch,
            sender_leaf: own_leaf,
            counter,
            ciphertext,
        })
    }

    /// Abre un sellado de una época instalada si su contador es nuevo para su hoja; solo entonces
    /// lo anota.
    pub fn open(
        &mut self,
        kind: ChatKind,
        sealed: &Sealed,
    ) -> Result<Zeroizing<Vec<u8>>, ChatCryptoError> {
        let key = self
            .key(sealed.epoch)
            .ok_or(ChatCryptoError::UnknownEpoch(sealed.epoch))?;
        let slot = (sealed.epoch, sealed.sender_leaf);
        if self
            .seen
            .get(&slot)
            .is_some_and(|last| sealed.counter <= *last)
        {
            return Err(ChatCryptoError::Replayed {
                epoch: sealed.epoch,
                leaf: sealed.sender_leaf,
                counter: sealed.counter,
            });
        }
        let plaintext = key.open(kind, sealed.sender_leaf, sealed.counter, &sealed.ciphertext)?;
        self.seen.insert(slot, sealed.counter);
        Ok(plaintext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Identity, KeyPackageBundle, Processed, RoomGroup};

    fn key(epoch: u64, byte: u8) -> ChatKey {
        ChatKey::new(epoch, [byte; CHAT_KEY_LENGTH])
    }

    #[test]
    fn sealed_messages_open_with_the_same_kind_leaf_counter_and_epoch() {
        let key = key(7, 1);
        let sealed = key
            .seal(ChatKind::Message, 3, 9, "hola 👋".as_bytes())
            .expect("sellar");
        assert_eq!(sealed.len(), "hola 👋".len() + TAG_LEN);
        assert!(!sealed.windows(4).any(|w| w == b"hola"), "va cifrado");
        assert_eq!(
            key.open(ChatKind::Message, 3, 9, &sealed)
                .expect("abre")
                .as_slice(),
            "hola 👋".as_bytes()
        );
        // Otro tipo, otra hoja, otro contador, otra época u otra clave: no abre.
        assert_eq!(
            key.open(ChatKind::History, 3, 9, &sealed),
            Err(ChatCryptoError::Corrupt)
        );
        assert!(key.open(ChatKind::Message, 4, 9, &sealed).is_err());
        assert!(key.open(ChatKind::Message, 3, 10, &sealed).is_err());
        assert!(
            ChatKey::new(8, [1; CHAT_KEY_LENGTH])
                .open(ChatKind::Message, 3, 9, &sealed)
                .is_err(),
            "la época va en los datos asociados"
        );
        assert!(
            self::key(7, 2)
                .open(ChatKind::Message, 3, 9, &sealed)
                .is_err()
        );
        let mut tampered = sealed.clone();
        tampered[0] ^= 1;
        assert!(key.open(ChatKind::Message, 3, 9, &tampered).is_err());
        assert!(
            key.open(ChatKind::Message, 3, 9, &sealed[..TAG_LEN - 1])
                .is_err()
        );
        assert!(format!("{key:?}").starts_with("ChatKey"));
        assert!(!format!("{key:?}").contains("secret"));
    }

    #[test]
    fn nonces_are_unique_per_sender_and_counter() {
        let mut ring = ChatRing::new();
        assert_eq!(
            ring.seal(ChatKind::Message, 0, b"x").unwrap_err(),
            ChatCryptoError::NoKey
        );
        ring.install(key(1, 1), true).expect("instalar");
        let mut nonces = std::collections::HashSet::new();
        for leaf in [0, 1, 2] {
            // Cada emisor tiene su propio anillo con la misma clave.
            let mut sender = ChatRing::new();
            sender.install(key(1, 1), true).expect("instalar");
            for _ in 0..100 {
                let sealed = sender.seal(ChatKind::Message, leaf, b"x").expect("sellar");
                assert!(nonces.insert(chat_nonce(sealed.sender_leaf, sealed.counter)));
            }
        }
        assert_eq!(nonces.len(), 300);
        // El contador vuelve a 0 con la clave nueva (otra clave: el nonce puede repetirse).
        let first = ring.seal(ChatKind::Message, 0, b"a").expect("sellar");
        let second = ring.seal(ChatKind::Message, 0, b"b").expect("sellar");
        assert_eq!((first.counter, second.counter), (0, 1));
        ring.install(key(2, 2), false).expect("instalar");
        let third = ring.seal(ChatKind::Message, 0, b"c").expect("sellar");
        assert_eq!((third.epoch, third.counter), (2, 0));
    }

    #[test]
    fn replays_and_unknown_epochs_are_rejected() {
        let mut sender = ChatRing::new();
        let mut receiver = ChatRing::new();
        sender.install(key(5, 9), true).expect("instalar");
        receiver.install(key(5, 9), true).expect("instalar");
        let first = sender.seal(ChatKind::Message, 2, b"uno").expect("sellar");
        let second = sender.seal(ChatKind::Message, 2, b"dos").expect("sellar");
        assert_eq!(
            receiver
                .open(ChatKind::Message, &second)
                .expect("abre")
                .as_slice(),
            b"dos"
        );
        // Lo anterior o lo mismo ya no se acepta.
        assert!(matches!(
            receiver.open(ChatKind::Message, &first),
            Err(ChatCryptoError::Replayed { counter: 0, .. })
        ));
        assert!(receiver.open(ChatKind::Message, &second).is_err());
        // Uno manipulado no consume su contador.
        let third = sender.seal(ChatKind::Message, 2, b"tres").expect("sellar");
        let mut bad = third.clone();
        bad.ciphertext[0] ^= 0x80;
        assert_eq!(
            receiver.open(ChatKind::Message, &bad),
            Err(ChatCryptoError::Corrupt)
        );
        assert!(receiver.open(ChatKind::Message, &third).is_ok());
        // Época que no está.
        let future = Sealed {
            epoch: 6,
            ..third.clone()
        };
        assert_eq!(
            receiver.open(ChatKind::Message, &future),
            Err(ChatCryptoError::UnknownEpoch(6))
        );
    }

    #[test]
    fn rings_keep_epochs_after_joins_and_forget_them_after_leaves() {
        let mut ring = ChatRing::new();
        ring.install(key(1, 1), true).expect("1");
        assert_eq!(
            ring.install(key(1, 1), false),
            Err(ChatCryptoError::StaleEpoch {
                epoch: 1,
                newest: 1
            })
        );
        for epoch in 2..=6 {
            ring.install(key(epoch, epoch as u8), false).expect("entra");
        }
        assert_eq!(ring.epochs(), [3, 4, 5, 6], "como mucho CHAT_EPOCHS_KEPT");
        ring.install(key(7, 7), true).expect("sale alguien");
        assert_eq!(ring.epochs(), [7]);
        assert_eq!(ring.epoch(), Some(7));
        assert_eq!(ring.newest().map(ChatKey::epoch), Some(7));
    }

    /// Anfitrión y dos invitados con la clave del chat derivada del exportador MLS: todos la
    /// comparten, cambia con cada época y el expulsado no puede abrir lo que se escribe después.
    #[test]
    fn a_removed_member_cannot_read_what_is_written_after() {
        let host_identity = Identity::generate().expect("anfitrión");
        let mut host = RoomGroup::create(&host_identity).expect("grupo");
        let mut guests: Vec<RoomGroup> = Vec::new();
        for _ in 0..2 {
            let identity = Identity::generate().expect("invitado");
            let (key_package, bundle) = KeyPackageBundle::generate(&identity).expect("KeyPackage");
            let change = host.add_member(&key_package).expect("alta");
            for guest in &mut guests {
                guest.process(&change.commit).expect("commit");
            }
            guests.push(
                RoomGroup::join(change.welcome.as_ref().expect("Welcome"), bundle).expect("entrar"),
            );
        }
        let before = host.chat_key().expect("clave");
        assert_eq!(before.epoch(), host.epoch());
        let mut host_ring = ChatRing::new();
        host_ring.install(before.clone(), true).expect("instalar");
        let mut rings: Vec<ChatRing> = guests
            .iter()
            .map(|group| {
                let mut ring = ChatRing::new();
                ring.install(group.chat_key().expect("clave"), true)
                    .expect("instalar");
                ring
            })
            .collect();
        let hello = host_ring
            .seal(ChatKind::Message, host.own_leaf_index(), b"hola a los dos")
            .expect("sellar");
        for ring in &mut rings {
            assert_eq!(
                ring.open(ChatKind::Message, &hello)
                    .expect("abre")
                    .as_slice(),
                b"hola a los dos"
            );
        }
        // La clave del chat no es la de SFrame ni la de ninguna otra etiqueta.
        let other = host
            .export_secret("coroom sframe v1", &chat_key_context(host.epoch()), 32)
            .expect("exportador");
        assert_ne!(other.as_slice(), before.secret.as_ref());

        // Expulsión del segundo invitado: época nueva y clave nueva.
        let removed_leaf = guests[1].own_leaf_index();
        let change = host.remove_member(removed_leaf).expect("expulsión");
        let after = host.chat_key().expect("clave");
        assert_eq!(after.epoch(), before.epoch() + 1);
        assert_ne!(after.secret.as_ref(), before.secret.as_ref());
        host_ring.install(after, true).expect("instalar");
        assert_eq!(host_ring.epochs(), [change.epoch]);
        let secret = host_ring
            .seal(ChatKind::Message, host.own_leaf_index(), b"ya no lo lees")
            .expect("sellar");

        let processed = guests[0].process(&change.commit).expect("commit");
        assert!(matches!(processed, Processed::Commit { .. }));
        rings[0]
            .install(guests[0].chat_key().expect("clave"), true)
            .expect("instalar");
        assert_eq!(
            rings[0]
                .open(ChatKind::Message, &secret)
                .expect("abre")
                .as_slice(),
            b"ya no lo lees"
        );

        // El expulsado: fuera del grupo, sin exportador y sin clave de la época nueva.
        assert_eq!(
            guests[1].process(&change.commit).expect("commit"),
            Processed::Removed {
                epoch: change.epoch
            }
        );
        assert!(guests[1].chat_key().is_err());
        assert_eq!(
            rings[1].open(ChatKind::Message, &secret),
            Err(ChatCryptoError::UnknownEpoch(change.epoch))
        );
        // Ni con su clave vieja haciéndola pasar por la nueva.
        assert_eq!(
            before.open(
                ChatKind::Message,
                secret.sender_leaf,
                secret.counter,
                &secret.ciphertext
            ),
            Err(ChatCryptoError::Corrupt)
        );
        let relabelled = ChatKey::new(change.epoch, *before.secret);
        assert!(
            relabelled
                .open(
                    ChatKind::Message,
                    secret.sender_leaf,
                    secret.counter,
                    &secret.ciphertext
                )
                .is_err()
        );
    }
}
