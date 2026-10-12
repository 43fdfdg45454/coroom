//! Hexadecimal en minúsculas para las claves, firmas, desafíos y tokens de la API.

const DIGITS: &[u8; 16] = b"0123456789abcdef";

/// `bytes` en hexadecimal en minúsculas.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Bytes de un texto hexadecimal (mayúsculas o minúsculas); `None` si no lo es.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&[high, low]| Some((nibble(high)? << 4) | nibble(low)?))
        .collect()
}

/// Exactamente `N` bytes de un texto hexadecimal.
pub fn decode_array<const N: usize>(text: &str) -> Option<[u8; N]> {
    decode(text)?.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        assert_eq!(encode(&[0x00, 0x7f, 0xab, 0xff]), "007fabff");
        assert_eq!(decode("007FABff"), Some(vec![0x00, 0x7f, 0xab, 0xff]));
        assert_eq!(decode("0"), None);
        assert_eq!(decode("zz"), None);
        assert_eq!(decode_array::<2>("abcd"), Some([0xab, 0xcd]));
        assert_eq!(decode_array::<3>("abcd"), None);
    }
}
