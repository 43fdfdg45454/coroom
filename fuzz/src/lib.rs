//! Utilidades comunes de los objetivos de fuzzing de coroom (`fuzz_targets/`).
//!
//! Los formatos de entrada de cada objetivo están en `fuzz/README.md`; las semillas
//! (`fuzz/seeds/<objetivo>/`) las genera el test `fuzz_seeds` de `crates/session`.

/// Lector de los bytes del fuzzer: cuando se acaban, todo devuelve ceros o vacío.
#[derive(Debug, Clone, Copy)]
pub struct Bytes<'a> {
    data: &'a [u8],
}

impl<'a> Bytes<'a> {
    /// Lector sobre `data`.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    /// `true` si no queda nada.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Un byte.
    pub fn u8(&mut self) -> u8 {
        match self.data.split_first() {
            Some((&byte, rest)) => {
                self.data = rest;
                byte
            }
            None => 0,
        }
    }

    /// Un `u16` big-endian.
    pub fn u16(&mut self) -> u16 {
        u16::from_be_bytes([self.u8(), self.u8()])
    }

    /// Un `u32` big-endian.
    pub fn u32(&mut self) -> u32 {
        u32::from_be_bytes([self.u8(), self.u8(), self.u8(), self.u8()])
    }

    /// Hasta `len` bytes.
    pub fn take(&mut self, len: usize) -> &'a [u8] {
        let len = len.min(self.data.len());
        let (head, rest) = self.data.split_at(len);
        self.data = rest;
        head
    }

    /// Un trozo con su longitud delante (`u16` big-endian).
    pub fn chunk(&mut self) -> &'a [u8] {
        let len = usize::from(self.u16());
        self.take(len)
    }

    /// Lo que queda.
    pub fn rest(&mut self) -> &'a [u8] {
        let rest = self.data;
        self.data = &[];
        rest
    }
}

/// Codifica trozos con su longitud delante, el formato de [`Bytes::chunk`].
pub fn chunks<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        let len = u16::try_from(part.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&part[..usize::from(len)]);
    }
    out
}
