//! Cifrado de la entrada del invitado con la clave pareja a pareja.
//!
//! La entrada no va por SFrame (la clave de grupo MLS la tienen todos los invitados): va sellada
//! con el [`PairwiseSecret`] HPKE que el invitado y el anfitrión establecieron al entrar en la sala, así
//! que **ningún otro invitado ni el relay puede leerla ni falsificarla**.
//!
//! - **Claves direccionales**: `HKDF-SHA256(salt = "coroom input v1", ikm = secreto)` expandido con
//!   `"coroom input v1 guest→host"` y `"coroom input v1 host→guest"` (32 bytes cada una, AES-256-GCM
//!   de `aws-lc-rs`). Cada extremo sella con la de su dirección y abre con la otra
//!   ([`InputCipher::from_pairwise`] con su [`Role`]).
//! - **`key_id`**: 32 bits expandidos con `"coroom input v1 key id"`; iguales en los dos extremos,
//!   sirven para encontrar la clave (y el invitado) de un datagrama que llega por el relay.
//! - **Sellado** (`Sealed`): `canal (1 B) ‖ secuencia (u64 BE) ‖ texto cifrado ‖ etiqueta (16 B)`.
//!   Los 9 bytes en claro son el AAD. Nonce de 96 bits = `dirección (1 B) ‖ canal (1 B) ‖ 0 0 ‖
//!   secuencia (8 B)`: único mientras la secuencia de cada canal crezca, cosa que
//!   [`InputCipher::seal`] exige.
//! - **Anti-repetición**: ventana de [`REPLAY_WINDOW`] secuencias por canal (el stream y los
//!   datagramas no comparten numeración: un retraso del stream no hace viejos a los datagramas).

use std::fmt;

use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::hpke::PairwiseSecret;

/// Secuencias que recuerda la ventana anti-repetición de cada canal.
pub const REPLAY_WINDOW: u64 = 256;

/// Bytes en claro de un sellado (canal y secuencia).
pub const HEADER_LEN: usize = 9;

/// Etiqueta AES-GCM.
pub const TAG_LEN: usize = 16;

/// Bytes que añade el sellado al texto en claro.
pub const OVERHEAD: usize = HEADER_LEN + TAG_LEN;

pub(crate) const SALT: &[u8] = b"coroom input v1";
pub(crate) const GUEST_TO_HOST: &[u8] = "coroom input v1 guest→host".as_bytes();
pub(crate) const HOST_TO_GUEST: &[u8] = "coroom input v1 host→guest".as_bytes();
pub(crate) const KEY_ID: &[u8] = b"coroom input v1 key id";

/// Papel del extremo local.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Anfitrión: abre lo del invitado.
    Host,
    /// Invitado: sella hacia el anfitrión.
    Guest,
}

/// Canal de la entrada.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Channel {
    /// Stream fiable (cambios de estado).
    Stream = 0,
    /// Datagramas (movimiento continuo).
    Datagram = 1,
}

impl Channel {
    const fn index(self) -> usize {
        self as usize
    }

    fn from_byte(byte: u8) -> Option<Channel> {
        match byte {
            0 => Some(Channel::Stream),
            1 => Some(Channel::Datagram),
            _ => None,
        }
    }
}

/// Error del cifrado de entrada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputCryptoError {
    /// Faltan bytes o el canal no existe.
    Malformed,
    /// No abre: otra clave, otra dirección o datos manipulados.
    OpenFailed,
    /// Secuencia ya vista.
    Replayed(u64),
    /// Secuencia más antigua que la ventana.
    TooOld(u64),
    /// Se intentó sellar con una secuencia que no crece (reutilizaría el nonce).
    SequenceReused {
        /// Secuencia pedida.
        seq: u64,
        /// La siguiente admitida.
        next: u64,
    },
    /// Fallo interno.
    Crypto(String),
}

impl fmt::Display for InputCryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InputCryptoError::Malformed => f.write_str("mensaje de entrada cifrado mal formado"),
            InputCryptoError::OpenFailed => f.write_str(
                "no se pudo abrir la entrada: otra clave, otra dirección o datos manipulados",
            ),
            InputCryptoError::Replayed(seq) => write!(f, "entrada repetida (secuencia {seq})"),
            InputCryptoError::TooOld(seq) => {
                write!(f, "entrada demasiado antigua (secuencia {seq})")
            }
            InputCryptoError::SequenceReused { seq, next } => write!(
                f,
                "secuencia de entrada {seq} reutilizada (la siguiente es {next})"
            ),
            InputCryptoError::Crypto(err) => write!(f, "fallo del cifrado de entrada: {err}"),
        }
    }
}

impl std::error::Error for InputCryptoError {}

/// Ventana anti-repetición: la secuencia más alta vista y un mapa de bits de las 256 anteriores.
#[derive(Debug, Clone, Default)]
struct ReplayWindow {
    highest: Option<u64>,
    /// Bit `i`: se vio `highest − i`.
    seen: [u64; (REPLAY_WINDOW / 64) as usize],
}

impl ReplayWindow {
    fn check(&self, seq: u64) -> Result<(), InputCryptoError> {
        let Some(highest) = self.highest else {
            return Ok(());
        };
        if seq > highest {
            return Ok(());
        }
        let age = highest - seq;
        if age >= REPLAY_WINDOW {
            return Err(InputCryptoError::TooOld(seq));
        }
        if self.seen[(age / 64) as usize] & (1 << (age % 64)) != 0 {
            return Err(InputCryptoError::Replayed(seq));
        }
        Ok(())
    }

    fn mark(&mut self, seq: u64) {
        let highest = match self.highest {
            None => {
                self.highest = Some(seq);
                self.seen = Default::default();
                self.seen[0] = 1;
                return;
            }
            Some(highest) => highest,
        };
        if seq > highest {
            let shift = seq - highest;
            self.shift(shift);
            self.highest = Some(seq);
            self.seen[0] |= 1;
        } else {
            let age = highest - seq;
            if age < REPLAY_WINDOW {
                self.seen[(age / 64) as usize] |= 1 << (age % 64);
            }
        }
    }

    /// Desplaza el mapa `shift` posiciones hacia lo antiguo.
    fn shift(&mut self, shift: u64) {
        if shift >= REPLAY_WINDOW {
            self.seen = Default::default();
            return;
        }
        let words = (shift / 64) as usize;
        let bits = (shift % 64) as u32;
        let len = self.seen.len();
        for i in (0..len).rev() {
            let source = i.checked_sub(words);
            let mut value = source.map_or(0, |s| self.seen[s] << bits);
            if bits > 0
                && let Some(lower) = source.and_then(|s| s.checked_sub(1))
            {
                value |= self.seen[lower] >> (64 - bits);
            }
            self.seen[i] = value;
        }
    }
}

/// Mensaje de entrada abierto.
#[derive(Debug, PartialEq, Eq)]
pub struct Opened<'a> {
    /// Canal.
    pub channel: Channel,
    /// Secuencia autenticada.
    pub seq: u64,
    /// Texto en claro.
    pub plaintext: &'a [u8],
}

/// Sellado y apertura de la entrada de un par de pares (anfitrión ↔ un invitado).
pub struct InputCipher {
    seal_key: LessSafeKey,
    open_key: LessSafeKey,
    seal_direction: u8,
    open_direction: u8,
    key_id: u32,
    next_seal: [u64; 2],
    windows: [ReplayWindow; 2],
}

impl fmt::Debug for InputCipher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputCipher")
            .field("key_id", &format_args!("{:#010x}", self.key_id))
            .field("next_seal", &self.next_seal)
            .finish_non_exhaustive()
    }
}

fn aes_key(hkdf: &hkdf::Hkdf<Sha256>, info: &[u8]) -> Result<LessSafeKey, InputCryptoError> {
    let mut key = Zeroizing::new([0u8; 32]);
    hkdf.expand(info, key.as_mut())
        .map_err(|err| InputCryptoError::Crypto(err.to_string()))?;
    let key = UnboundKey::new(&AES_256_GCM, key.as_slice())
        .map_err(|err| InputCryptoError::Crypto(err.to_string()))?;
    Ok(LessSafeKey::new(key))
}

impl InputCipher {
    /// Claves de entrada derivadas del secreto pareja a pareja, vistas desde `role`.
    pub fn from_pairwise(
        secret: &PairwiseSecret,
        role: Role,
    ) -> Result<InputCipher, InputCryptoError> {
        let hkdf = hkdf::Hkdf::<Sha256>::new(Some(SALT), secret.as_slice());
        let guest_to_host = aes_key(&hkdf, GUEST_TO_HOST)?;
        let host_to_guest = aes_key(&hkdf, HOST_TO_GUEST)?;
        let mut id = [0u8; 4];
        hkdf.expand(KEY_ID, &mut id)
            .map_err(|err| InputCryptoError::Crypto(err.to_string()))?;
        let (seal_key, open_key, seal_direction, open_direction) = match role {
            Role::Guest => (guest_to_host, host_to_guest, 0, 1),
            Role::Host => (host_to_guest, guest_to_host, 1, 0),
        };
        Ok(InputCipher {
            seal_key,
            open_key,
            seal_direction,
            open_direction,
            key_id: u32::from_be_bytes(id),
            next_seal: [0; 2],
            windows: Default::default(),
        })
    }

    /// Identificador público de la clave (igual en los dos extremos).
    pub fn key_id(&self) -> u32 {
        self.key_id
    }

    /// Siguiente secuencia que admite [`InputCipher::seal`] en `channel`.
    pub fn next_seq(&self, channel: Channel) -> u64 {
        self.next_seal[channel.index()]
    }

    fn nonce(direction: u8, channel: Channel, seq: u64) -> Nonce {
        let mut nonce = [0u8; NONCE_LEN];
        nonce[0] = direction;
        nonce[1] = channel as u8;
        nonce[4..].copy_from_slice(&seq.to_be_bytes());
        Nonce::assume_unique_for_key(nonce)
    }

    /// Añade a `out` el sellado de `plaintext` con la secuencia `seq` de `channel`, que debe ser al
    /// menos [`InputCipher::next_seq`] (si no, el nonce se repetiría).
    pub fn seal(
        &mut self,
        channel: Channel,
        seq: u64,
        plaintext: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(), InputCryptoError> {
        let next = self.next_seal[channel.index()];
        if seq < next || seq == u64::MAX {
            return Err(InputCryptoError::SequenceReused { seq, next });
        }
        let start = out.len();
        out.push(channel as u8);
        out.extend_from_slice(&seq.to_be_bytes());
        out.extend_from_slice(plaintext);
        let (header, body) = out[start..].split_at_mut(HEADER_LEN);
        let tag = self
            .seal_key
            .seal_in_place_separate_tag(
                Self::nonce(self.seal_direction, channel, seq),
                Aad::from(&*header),
                body,
            )
            .map_err(|_| InputCryptoError::Crypto("AES-GCM no pudo sellar".into()))?;
        out.extend_from_slice(tag.as_ref());
        self.next_seal[channel.index()] = seq + 1;
        Ok(())
    }

    /// Abre un sellado del otro extremo en `out` (que se vacía antes) y anota su secuencia en la
    /// ventana anti-repetición de su canal.
    pub fn open<'a>(
        &mut self,
        data: &[u8],
        out: &'a mut Vec<u8>,
    ) -> Result<Opened<'a>, InputCryptoError> {
        if data.len() < OVERHEAD {
            return Err(InputCryptoError::Malformed);
        }
        let (header, body) = data.split_at(HEADER_LEN);
        let channel = Channel::from_byte(header[0]).ok_or(InputCryptoError::Malformed)?;
        let mut seq_bytes = [0u8; 8];
        seq_bytes.copy_from_slice(&header[1..]);
        let seq = u64::from_be_bytes(seq_bytes);
        self.windows[channel.index()].check(seq)?;
        out.clear();
        out.extend_from_slice(body);
        let len = self
            .open_key
            .open_in_place(
                Self::nonce(self.open_direction, channel, seq),
                Aad::from(header),
                out,
            )
            .map_err(|_| InputCryptoError::OpenFailed)?
            .len();
        out.truncate(len);
        self.windows[channel.index()].mark(seq);
        Ok(Opened {
            channel,
            seq,
            plaintext: out.as_slice(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (InputCipher, InputCipher) {
        let secret = PairwiseSecret::new([7u8; 32]);
        (
            InputCipher::from_pairwise(&secret, Role::Guest).unwrap(),
            InputCipher::from_pairwise(&secret, Role::Host).unwrap(),
        )
    }

    #[test]
    fn round_trip_on_both_channels() {
        let (mut guest, mut host) = pair();
        assert_eq!(guest.key_id(), host.key_id());
        for channel in [Channel::Stream, Channel::Datagram] {
            for seq in 0..10 {
                let mut sealed = Vec::new();
                guest
                    .seal(channel, seq, b"pulsar la W", &mut sealed)
                    .unwrap();
                assert_eq!(sealed.len(), b"pulsar la W".len() + OVERHEAD);
                let mut out = Vec::new();
                let opened = host.open(&sealed, &mut out).unwrap();
                assert_eq!(opened.channel, channel);
                assert_eq!(opened.seq, seq);
                assert_eq!(opened.plaintext, b"pulsar la W");
            }
        }
    }

    #[test]
    fn seal_appends_after_a_prefix() {
        let (mut guest, mut host) = pair();
        let mut datagram = b"cabecera".to_vec();
        guest
            .seal(Channel::Datagram, 3, b"x", &mut datagram)
            .unwrap();
        let mut out = Vec::new();
        assert_eq!(host.open(&datagram[8..], &mut out).unwrap().plaintext, b"x");
    }

    #[test]
    fn replays_and_old_sequences_are_rejected() {
        let (mut guest, mut host) = pair();
        let mut sealed = Vec::new();
        guest.seal(Channel::Datagram, 5, b"a", &mut sealed).unwrap();
        let mut out = Vec::new();
        host.open(&sealed, &mut out).unwrap();
        assert_eq!(
            host.open(&sealed, &mut out),
            Err(InputCryptoError::Replayed(5))
        );
        // Fuera de orden dentro de la ventana: vale una vez.
        let mut late = Vec::new();
        let mut later = Vec::new();
        let mut far = Vec::new();
        let mut guest_b = pair().0;
        guest_b.seal(Channel::Datagram, 2, b"b", &mut late).unwrap();
        guest_b
            .seal(Channel::Datagram, 300, b"c", &mut later)
            .unwrap();
        guest_b
            .seal(Channel::Datagram, 301, b"d", &mut far)
            .unwrap();
        assert!(host.open(&late, &mut out).is_ok());
        assert_eq!(
            host.open(&late, &mut out),
            Err(InputCryptoError::Replayed(2))
        );
        assert!(host.open(&later, &mut out).is_ok());
        assert_eq!(host.open(&late, &mut out), Err(InputCryptoError::TooOld(2)));
        assert!(host.open(&far, &mut out).is_ok());
        // Los canales tienen ventanas propias.
        let mut stream = Vec::new();
        guest.seal(Channel::Stream, 0, b"e", &mut stream).unwrap();
        assert!(host.open(&stream, &mut out).is_ok());
    }

    #[test]
    fn the_cross_direction_is_rejected() {
        let (mut guest, mut host) = pair();
        let mut sealed = Vec::new();
        guest
            .seal(Channel::Stream, 0, b"hola", &mut sealed)
            .unwrap();
        // El propio invitado no puede abrir lo que selló (sería la dirección cruzada) y un
        // anfitrión que reenvíe al invitado lo suyo tampoco pasa.
        let mut out = Vec::new();
        assert_eq!(
            guest.open(&sealed, &mut out),
            Err(InputCryptoError::OpenFailed)
        );
        let mut reflected = Vec::new();
        host.seal(Channel::Stream, 0, b"hola", &mut reflected)
            .unwrap();
        assert_eq!(
            host.open(&reflected, &mut out),
            Err(InputCryptoError::OpenFailed)
        );
        assert!(guest.open(&reflected, &mut out).is_ok());
    }

    #[test]
    fn another_secret_or_tampering_is_rejected() {
        let (mut guest, _) = pair();
        let mut other =
            InputCipher::from_pairwise(&PairwiseSecret::new([8u8; 32]), Role::Host).unwrap();
        assert_ne!(other.key_id(), guest.key_id());
        let mut sealed = Vec::new();
        guest
            .seal(Channel::Datagram, 0, b"secreto", &mut sealed)
            .unwrap();
        let mut out = Vec::new();
        assert_eq!(
            other.open(&sealed, &mut out),
            Err(InputCryptoError::OpenFailed)
        );
        let (_, mut host) = pair();
        for byte in 0..sealed.len() {
            let mut tampered = sealed.clone();
            tampered[byte] ^= 0x01;
            assert!(host.open(&tampered, &mut out).is_err(), "byte {byte}");
        }
        assert!(host.open(&sealed, &mut out).is_ok());
        assert_eq!(
            host.open(&sealed[..OVERHEAD - 1], &mut out),
            Err(InputCryptoError::Malformed)
        );
    }

    #[test]
    fn sequences_must_grow_when_sealing() {
        let (mut guest, _) = pair();
        let mut out = Vec::new();
        guest.seal(Channel::Stream, 4, b"a", &mut out).unwrap();
        assert_eq!(guest.next_seq(Channel::Stream), 5);
        assert_eq!(guest.next_seq(Channel::Datagram), 0);
        assert_eq!(
            guest.seal(Channel::Stream, 4, b"a", &mut out),
            Err(InputCryptoError::SequenceReused { seq: 4, next: 5 })
        );
        assert!(guest.seal(Channel::Datagram, 0, b"a", &mut out).is_ok());
    }

    #[test]
    fn the_window_survives_large_jumps() {
        let mut window = ReplayWindow::default();
        for seq in [0u64, 1, 2, 70, 69, 200, 455, 456] {
            window.check(seq).unwrap();
            window.mark(seq);
        }
        for seq in [455u64, 456] {
            assert_eq!(window.check(seq), Err(InputCryptoError::Replayed(seq)));
        }
        // 456 − 200 = 256: justo fuera de la ventana.
        assert_eq!(window.check(200), Err(InputCryptoError::TooOld(200)));
        assert_eq!(window.check(70), Err(InputCryptoError::TooOld(70)));
        assert!(window.check(201).is_ok());
        window.mark(10_000);
        assert!(window.check(9_999).is_ok());
        assert_eq!(window.check(456), Err(InputCryptoError::TooOld(456)));
    }

    #[test]
    fn the_window_matches_a_naive_model() {
        // Generador lineal congruente: secuencias que avanzan y retroceden al azar.
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            state >> 33
        };
        let mut window = ReplayWindow::default();
        let mut seen = std::collections::BTreeSet::new();
        let mut highest: Option<u64> = None;
        let mut cursor = 1_000u64;
        for _ in 0..20_000 {
            let step = next() % 400;
            cursor = if next() % 3 == 0 {
                cursor.saturating_sub(step)
            } else {
                cursor + step / 4
            };
            let expected = match highest {
                Some(h) if cursor <= h && h - cursor >= REPLAY_WINDOW => {
                    Err(InputCryptoError::TooOld(cursor))
                }
                _ if seen.contains(&cursor) => Err(InputCryptoError::Replayed(cursor)),
                _ => Ok(()),
            };
            assert_eq!(window.check(cursor), expected, "secuencia {cursor}");
            if expected.is_ok() {
                window.mark(cursor);
                seen.insert(cursor);
                highest = Some(highest.map_or(cursor, |h| h.max(cursor)));
            }
        }
    }
}
