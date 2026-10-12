//! Número de seguridad entre dos identidades.
//!
//! Dos amigos lo comparan en voz alta o por otro canal para comprobar que nadie se ha colado
//! entre ellos (TOFU + comparación, `docs/ARQUITECTURA.md` §4). Es simétrico: ambos ven el mismo.
//!
//! Derivación: `SHA-512("coroom safety number v1" || min(a, b) || max(a, b))`, con las claves
//! ordenadas byte a byte. Los 60 primeros bytes del resumen se parten en 12 bloques de 5 bytes;
//! cada bloque, leído como entero big-endian de 40 bits, da un grupo de 5 dígitos (`mod 100000`,
//! como hace Signal). El sesgo del módulo es despreciable (2⁴⁰ ≫ 10⁵).

use std::fmt;

use sha2::{Digest, Sha512};

use crate::identity::PUBLIC_KEY_LENGTH;

/// Grupos de dígitos del número.
pub const SAFETY_NUMBER_GROUPS: usize = 12;
/// Dígitos de cada grupo.
pub const SAFETY_NUMBER_GROUP_DIGITS: usize = 5;

pub(crate) const LABEL: &[u8] = b"coroom safety number v1";

/// Número de seguridad de 60 dígitos decimales entre dos claves públicas de identidad.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SafetyNumber {
    groups: [u32; SAFETY_NUMBER_GROUPS],
}

impl SafetyNumber {
    /// Número de seguridad entre las identidades `a` y `b` (el orden no importa).
    pub fn between(a: &[u8; PUBLIC_KEY_LENGTH], b: &[u8; PUBLIC_KEY_LENGTH]) -> SafetyNumber {
        let (low, high) = if a <= b { (a, b) } else { (b, a) };
        let digest = Sha512::new()
            .chain_update(LABEL)
            .chain_update(low)
            .chain_update(high)
            .finalize();
        let mut groups = [0u32; SAFETY_NUMBER_GROUPS];
        for (group, chunk) in groups.iter_mut().zip(digest.as_chunks::<5>().0) {
            let value = chunk
                .iter()
                .fold(0u64, |acc, &byte| (acc << 8) | u64::from(byte));
            // `value % 100_000` < 100 000 siempre cabe en u32.
            *group = u32::try_from(value % 100_000).unwrap_or(0);
        }
        SafetyNumber { groups }
    }

    /// Los 12 grupos de 5 dígitos como números (`0..=99_999`).
    pub fn groups(&self) -> [u32; SAFETY_NUMBER_GROUPS] {
        self.groups
    }

    /// Los 60 dígitos seguidos, sin espacios.
    pub fn digits(&self) -> String {
        self.groups
            .iter()
            .map(|group| format!("{group:05}"))
            .collect()
    }
}

impl fmt::Display for SafetyNumber {
    /// `12345 67890 …`: 12 grupos de 5 dígitos separados por espacios.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, group) in self.groups.iter().enumerate() {
            if index > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{group:05}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for SafetyNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SafetyNumber({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_symmetric_and_has_twelve_groups_of_five_digits() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let ab = SafetyNumber::between(&a, &b);
        assert_eq!(ab, SafetyNumber::between(&b, &a));
        let text = ab.to_string();
        let groups: Vec<&str> = text.split(' ').collect();
        assert_eq!(groups.len(), SAFETY_NUMBER_GROUPS);
        assert!(
            groups
                .iter()
                .all(|group| group.len() == SAFETY_NUMBER_GROUP_DIGITS
                    && group.chars().all(|c| c.is_ascii_digit()))
        );
        assert_eq!(ab.digits().len(), 60);
        assert_eq!(ab.digits(), text.replace(' ', ""));
    }

    #[test]
    fn differs_for_different_pairs() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let c = [3u8; 32];
        assert_ne!(SafetyNumber::between(&a, &b), SafetyNumber::between(&a, &c));
        assert_ne!(SafetyNumber::between(&a, &b), SafetyNumber::between(&b, &c));
    }

    #[test]
    fn is_stable_across_versions() {
        // Fija la derivación: si cambia, los usuarios dejan de reconocer números ya comparados.
        let number = SafetyNumber::between(&[0u8; 32], &[0xffu8; 32]);
        assert_eq!(number.to_string(), STABLE_ZERO_FF);
    }

    // Calculado aparte (Python, `hashlib.sha512`) con la derivación documentada arriba.
    const STABLE_ZERO_FF: &str =
        "49763 17516 19335 51288 83458 38183 72278 20773 54263 89490 00187 77551";
}
