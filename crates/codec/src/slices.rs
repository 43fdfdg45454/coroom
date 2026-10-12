//! Reparto de un paquete del codificador en slices reales.
//!
//! FFmpeg entrega una unidad de acceso completa por `AVPacket` en todos los backends (no expone la
//! salida por slice de NVENC ni de los demás), así que los slices se separan después:
//!
//! - **H.264 y HEVC** (Annex B): un slice por NAL de VCL (H.264: tipos 1 a 5 y 20; HEVC: 0 a 31).
//!   Las NAL que no son de VCL (SPS, PPS, VPS, SEI, AUD…) van con el slice que las sigue; las del
//!   final, con el último.
//! - **AV1** (OBU con tamaño, como los escribe FFmpeg): un slice por OBU de grupo de *tiles*
//!   (`OBU_TILE_GROUP`, `OBU_FRAME` o `OBU_TILE_LIST`); la cabecera de secuencia, el delimitador
//!   temporal, la cabecera de fotograma y los metadatos van con el siguiente. SVT-AV1 y casi todos
//!   los codificadores de hardware escriben un solo `OBU_FRAME` por fotograma: los *tiles* de AV1
//!   (`tile-rows`/`tile-columns`) no son slices independientes en el flujo, así que AV1 sale con un
//!   slice salvo que el codificador emita varios grupos de *tiles*.
//!
//! Concatenar los slices en orden devuelve el paquete original, que es lo que recibe el
//! decodificador.

use std::ops::Range;

use crate::Codec;

/// Rangos de `data` con cada slice, en orden y sin huecos. Si el paquete no se entiende, un solo
/// rango con todo.
pub fn split_slices(codec: Codec, data: &[u8]) -> Vec<Range<usize>> {
    let units = match codec {
        Codec::H264 | Codec::Hevc => annex_b_units(data, codec),
        Codec::Av1 => obu_units(data),
    };
    match units {
        Some(units) if !units.is_empty() => group(&units, data.len()),
        _ => whole(data),
    }
}

fn whole(data: &[u8]) -> Vec<Range<usize>> {
    if data.is_empty() {
        Vec::new()
    } else {
        std::iter::once(0..data.len()).collect()
    }
}

/// Unidad del flujo: dónde empieza y si es de VCL (empieza un slice).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Unit {
    start: usize,
    vcl: bool,
}

/// Agrupa las unidades en slices: cada unidad de VCL abre uno nuevo, que empieza en la primera
/// unidad sin VCL que la precede inmediatamente.
fn group(units: &[Unit], len: usize) -> Vec<Range<usize>> {
    let mut starts = Vec::new();
    let mut pending: Option<usize> = None;
    for unit in units {
        if unit.vcl {
            starts.push(pending.take().unwrap_or(unit.start));
        } else if pending.is_none() {
            pending = Some(unit.start);
        }
    }
    if starts.is_empty() {
        return std::iter::once(0..len).collect();
    }
    // Lo que va delante del primer slice (sin VCL) se le une.
    starts[0] = 0;
    let mut ranges: Vec<Range<usize>> = starts.windows(2).map(|pair| pair[0]..pair[1]).collect();
    if let Some(&last) = starts.last() {
        ranges.push(last..len);
    }
    ranges
}

/// NAL de un flujo Annex B (con su código de inicio delante).
fn annex_b_units(data: &[u8], codec: Codec) -> Option<Vec<Unit>> {
    let mut units = Vec::new();
    let mut at = 0;
    while let Some((start, header)) = next_start_code(data, at) {
        let byte = *data.get(header)?;
        let vcl = match codec {
            Codec::H264 => matches!(byte & 0x1F, 1..=5 | 20),
            Codec::Hevc => (byte >> 1) & 0x3F <= 31,
            Codec::Av1 => false,
        };
        units.push(Unit { start, vcl });
        at = header;
    }
    // El paquete debe empezar por un código de inicio (los ceros previos cuentan como relleno).
    match units.first() {
        Some(first) if data[..first.start].iter().all(|&b| b == 0) => Some(units),
        _ => None,
    }
}

/// Siguiente código de inicio (`00 00 01`, con los ceros previos de `00 00 00 01`) desde `from`:
/// devuelve dónde empieza el código y dónde empieza la cabecera de la NAL.
fn next_start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
    (from..data.len().saturating_sub(2))
        .find(|&i| data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1)
        .map(|i| {
            let start = if i > from && data[i - 1] == 0 {
                i - 1
            } else {
                i
            };
            (start, i + 3)
        })
}

/// Tipos de OBU de AV1 (§6.2.2).
#[cfg(test)]
const OBU_FRAME_HEADER: u8 = 3;
const OBU_TILE_GROUP: u8 = 4;
const OBU_FRAME: u8 = 6;
const OBU_TILE_LIST: u8 = 8;

/// OBU de un flujo AV1 de bajo coste (con `obu_has_size_field`).
fn obu_units(data: &[u8]) -> Option<Vec<Unit>> {
    let mut units = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let header = data[at];
        if header & 0x80 != 0 {
            return None; // obu_forbidden_bit
        }
        let kind = (header >> 3) & 0x0F;
        let extension = header & 0x04 != 0;
        let has_size = header & 0x02 != 0;
        let mut cursor = at + 1 + usize::from(extension);
        let end = if has_size {
            let (size, used) = leb128(data.get(cursor..)?)?;
            cursor += used;
            cursor.checked_add(usize::try_from(size).ok()?)?
        } else {
            data.len()
        };
        if end > data.len() {
            return None;
        }
        // La cabecera de fotograma (como el resto de OBU sin tiles) va con el grupo que la sigue.
        let vcl = matches!(kind, OBU_TILE_GROUP | OBU_FRAME | OBU_TILE_LIST);
        units.push(Unit { start: at, vcl });
        at = end;
    }
    Some(units)
}

/// Lee un entero LEB128 de como mucho 8 bytes: valor y bytes usados.
fn leb128(data: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for (i, &byte) in data.iter().take(8).enumerate() {
        value |= u64::from(byte & 0x7F) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nal(header: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![0, 0, 0, 1, header];
        out.extend_from_slice(body);
        out
    }

    fn one(range: Range<usize>) -> Vec<Range<usize>> {
        std::iter::once(range).collect()
    }

    fn pieces<'a>(data: &'a [u8], ranges: &[Range<usize>]) -> Vec<&'a [u8]> {
        ranges.iter().map(|range| &data[range.clone()]).collect()
    }

    #[test]
    fn h264_slices_carry_their_parameter_sets() {
        // SPS, PPS, IDR, IDR, IDR, IDR (cuatro slices) y fin de secuencia.
        let sps = nal(0x67, &[1, 2, 3]);
        let pps = nal(0x68, &[4]);
        let slices: Vec<Vec<u8>> = (0..4).map(|i| nal(0x65, &[0x88, i, 7])).collect();
        let end = nal(0x0B, &[]);
        let mut data = Vec::new();
        for part in [&sps, &pps].into_iter().chain(slices.iter()).chain([&end]) {
            data.extend_from_slice(part);
        }
        let ranges = split_slices(Codec::H264, &data);
        assert_eq!(ranges.len(), 4);
        let parts = pieces(&data, &ranges);
        assert_eq!(parts[0], [sps.as_slice(), &pps, &slices[0]].concat());
        assert_eq!(parts[1], slices[1].as_slice());
        assert_eq!(parts[3], [slices[3].as_slice(), &end].concat());
        assert_eq!(parts.concat(), data, "sin huecos ni solapes");
    }

    #[test]
    fn three_byte_start_codes_and_p_slices() {
        let mut data = vec![0, 0, 1, 0x41, 0xE0, 1];
        data.extend_from_slice(&[0, 0, 1, 0x41, 0x50, 2]);
        data.extend_from_slice(&[0, 0, 1, 0x06, 9]); // SEI entre slices: va con el siguiente.
        data.extend_from_slice(&[0, 0, 0, 1, 0x41, 0x30, 3]);
        let ranges = split_slices(Codec::H264, &data);
        let parts = pieces(&data, &ranges);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], [0, 0, 1, 0x41, 0xE0, 1]);
        assert_eq!(parts[2], [0, 0, 1, 0x06, 9, 0, 0, 0, 1, 0x41, 0x30, 3]);
    }

    #[test]
    fn hevc_vcl_types_open_slices() {
        // VPS (32), SPS (33), PPS (34), IDR_W_RADL (19) ×2, TRAIL_R (1).
        let header = |kind: u8| kind << 1;
        let mut data = Vec::new();
        for kind in [32, 33, 34, 19, 19, 1] {
            data.extend_from_slice(&[0, 0, 0, 1, header(kind), 1, 0xAA]);
        }
        let ranges = split_slices(Codec::Hevc, &data);
        assert_eq!(ranges.len(), 3);
        assert_eq!(ranges[0], 0..28);
        assert_eq!(pieces(&data, &ranges).concat(), data);
    }

    fn obu(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![(kind << 3) | 0x02, payload.len() as u8];
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn av1_tile_groups_become_slices() {
        let td = obu(2, &[]);
        let seq = obu(1, &[1, 2, 3]);
        let frame = obu(OBU_FRAME, &[9; 5]);
        let tg2 = obu(OBU_TILE_GROUP, &[8; 4]);
        let data = [td.as_slice(), &seq, &frame, &tg2].concat();
        let ranges = split_slices(Codec::Av1, &data);
        let parts = pieces(&data, &ranges);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], [td.as_slice(), &seq, &frame].concat());
        assert_eq!(parts[1], tg2.as_slice());

        // Cabecera de fotograma + grupo de tiles juntos; luego otro grupo.
        let fh = obu(OBU_FRAME_HEADER, &[1]);
        let tg1 = obu(OBU_TILE_GROUP, &[2, 2]);
        let data = [td.as_slice(), &fh, &tg1, &tg2].concat();
        let parts = pieces(&data, &split_slices(Codec::Av1, &data));
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert_eq!(parts[0], [td.as_slice(), &fh, &tg1].concat());
        assert_eq!(parts[1], tg2.as_slice());

        // Un solo OBU_FRAME (lo normal en SVT-AV1): un slice.
        let data = [td.as_slice(), &frame].concat();
        assert_eq!(split_slices(Codec::Av1, &data), one(0..data.len()));
    }

    #[test]
    fn garbage_is_a_single_slice() {
        let data = [0xFFu8, 1, 2, 3];
        assert_eq!(split_slices(Codec::H264, &data), one(0..4));
        assert_eq!(split_slices(Codec::Av1, &[0x80, 0]), one(0..2));
        assert!(split_slices(Codec::Hevc, &[]).is_empty());
        // OBU con tamaño que se sale del paquete.
        assert_eq!(split_slices(Codec::Av1, &[0x32, 9, 1]), one(0..3));
        assert_eq!(leb128(&[0x80, 0x01]), Some((128, 2)));
    }
}
