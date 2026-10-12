//! FEC RaptorQ (RFC 6330, crate `raptorq`) de un grupo de medios.
//!
//! Cada grupo es un único bloque de origen de `K` símbolos de `T` bytes (la carga útil de un
//! datagrama). El emisor rellena con ceros hasta `K × T` y genera `R` símbolos de reparación; el
//! receptor los combina con los objetos de origen que lleguen. Los ESI de reparación empiezan en
//! `K` (`raptorq` añade internamente el relleno hasta `K'`), así que el índice `r` de la cabecera
//! corresponde al ESI `K + r`.

use raptorq::{
    EncodingPacket, ObjectTransmissionInformation, PayloadId, SourceBlockDecoder,
    SourceBlockEncoder,
};

/// Máximo de símbolos de origen de un bloque (RFC 6330, `K'_max`).
pub const MAX_SOURCE_SYMBOLS: usize = 56_403;

fn oti(symbols: usize, symbol_size: usize) -> Option<ObjectTransmissionInformation> {
    if symbols == 0 || symbols > MAX_SOURCE_SYMBOLS || symbol_size == 0 {
        return None;
    }
    let symbol_size = u16::try_from(symbol_size).ok()?;
    Some(ObjectTransmissionInformation::new(
        (symbols * usize::from(symbol_size)) as u64,
        symbol_size,
        1,
        1,
        1,
    ))
}

/// Genera `repairs` símbolos de reparación de `data` (`len == symbols × symbol_size`).
///
/// Devuelve `None` si el bloque no es válido para RaptorQ.
pub fn encode_repairs(data: &[u8], symbol_size: usize, repairs: u32) -> Option<Vec<Vec<u8>>> {
    if repairs == 0 || symbol_size == 0 || !data.len().is_multiple_of(symbol_size) {
        return None;
    }
    let config = oti(data.len() / symbol_size, symbol_size)?;
    let encoder = SourceBlockEncoder::new(0, &config, data);
    Some(
        encoder
            .repair_packets(0, repairs)
            .into_iter()
            .map(|packet| packet.split().1)
            .collect(),
    )
}

/// Decodificador incremental de un grupo.
pub struct GroupDecoder {
    decoder: SourceBlockDecoder,
    symbols: usize,
    symbol_size: usize,
    first_repair: u32,
}

impl std::fmt::Debug for GroupDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupDecoder")
            .field("symbols", &self.symbols)
            .field("symbol_size", &self.symbol_size)
            .finish_non_exhaustive()
    }
}

impl GroupDecoder {
    /// Decodificador de `symbols` símbolos de `symbol_size` bytes; `None` si no es válido.
    pub fn new(symbols: usize, symbol_size: usize) -> Option<Self> {
        let config = oti(symbols, symbol_size)?;
        let first_repair = u32::try_from(symbols).ok()?;
        Some(Self {
            decoder: SourceBlockDecoder::new(0, &config, (symbols * symbol_size) as u64),
            symbols,
            symbol_size,
            first_repair,
        })
    }

    /// Tamaño de símbolo de este bloque.
    pub fn symbol_size(&self) -> usize {
        self.symbol_size
    }

    /// Añade símbolos y devuelve el bloque (`symbols × symbol_size` bytes) si ya se puede
    /// reconstruir. Los símbolos de tamaño distinto o índice fuera de rango se ignoran.
    pub fn push(
        &mut self,
        sources: impl IntoIterator<Item = (u16, Vec<u8>)>,
        repairs: impl IntoIterator<Item = (u16, Vec<u8>)>,
    ) -> Option<Vec<u8>> {
        let size = self.symbol_size;
        let symbols = self.symbols;
        let first_repair = self.first_repair;
        let packets = sources
            .into_iter()
            .filter(|(index, data)| usize::from(*index) < symbols && data.len() == size)
            .map(|(index, data)| EncodingPacket::new(PayloadId::new(0, u32::from(index)), data))
            .chain(
                repairs
                    .into_iter()
                    .filter(|(_, data)| data.len() == size)
                    .map(|(index, data)| {
                        EncodingPacket::new(
                            PayloadId::new(0, first_repair + u32::from(index)),
                            data,
                        )
                    }),
            );
        self.decoder.decode(packets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(symbols: usize, size: usize) -> Vec<u8> {
        (0..symbols * size)
            .map(|i| (i as u8).wrapping_mul(17).wrapping_add(3))
            .collect()
    }

    #[test]
    fn repairs_replace_lost_sources() {
        let size = 1184;
        let data = block(17, size);
        let repairs = encode_repairs(&data, size, 4).expect("reparación");
        assert_eq!(repairs.len(), 4);
        let mut decoder = GroupDecoder::new(17, size).expect("decodificador");
        // Faltan 3 de origen: 14 de origen + 4 de reparación.
        let sources: Vec<(u16, Vec<u8>)> = (0..17u16)
            .filter(|i| ![2, 9, 16].contains(i))
            .map(|i| {
                (
                    i,
                    data[usize::from(i) * size..(usize::from(i) + 1) * size].to_vec(),
                )
            })
            .collect();
        let repairs = repairs
            .into_iter()
            .enumerate()
            .map(|(i, symbol)| (i as u16, symbol));
        assert_eq!(decoder.push(sources, repairs), Some(data));
    }

    #[test]
    fn too_few_symbols_do_not_decode() {
        let size = 64;
        let data = block(5, size);
        let repairs = encode_repairs(&data, size, 1).expect("reparación");
        let mut decoder = GroupDecoder::new(5, size).expect("decodificador");
        let sources: Vec<(u16, Vec<u8>)> = (0..3u16)
            .map(|i| {
                (
                    i,
                    data[usize::from(i) * size..(usize::from(i) + 1) * size].to_vec(),
                )
            })
            .collect();
        assert_eq!(decoder.push(sources, [(0, repairs[0].clone())]), None);
    }

    #[test]
    fn mismatched_symbols_are_ignored_without_panicking() {
        let mut decoder = GroupDecoder::new(4, 32).expect("decodificador");
        assert_eq!(
            decoder.push([(0, vec![0; 31]), (9, vec![0; 32])], [(0, vec![1; 7])]),
            None
        );
        assert!(GroupDecoder::new(0, 32).is_none());
        assert!(GroupDecoder::new(MAX_SOURCE_SYMBOLS + 1, 32).is_none());
        assert!(encode_repairs(&[0; 33], 32, 1).is_none());
    }
}
