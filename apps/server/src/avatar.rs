//! Validación de los avatares que suben los usuarios (`PUT /api/v1/me/avatar`).
//!
//! El cliente manda un PNG cuadrado de como mucho [`AVATAR_SIDE_MAX`] píxeles de lado. El servidor
//! nunca guarda ni sirve los bytes tal como llegan: los decodifica con límites (tamaño del cuerpo,
//! dimensiones leídas de la cabecera antes de reservar memoria, y la memoria del decodificador),
//! se queda solo con los píxeles del primer fotograma y vuelve a codificarlos en un PNG propio, sin
//! fragmentos auxiliares (texto, perfiles, metadatos) ni animación. La versión es la de
//! [`avatar_version`] sobre ese PNG.

use std::io::Cursor;

use sha2::{Digest, Sha256};

use coroom_transport::proto::api::{
    AVATAR_MAX_BYTES, AVATAR_SIDE_MAX, AVATAR_SIDE_MIN, avatar_version,
};

/// Firma de los ficheros PNG.
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// Memoria como mucho del decodificador: el fotograma más grande en RGBA de 16 bits y holgura para
/// los fragmentos que descomprime (que además se ignoran).
const DECODE_LIMIT: usize = (AVATAR_SIDE_MAX as usize) * (AVATAR_SIDE_MAX as usize) * 8 + 64 * 1024;

/// Por qué no se acepta un avatar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AvatarError {
    /// Más de [`AVATAR_MAX_BYTES`].
    TooLarge,
    /// No es un PNG.
    NotPng,
    /// No es cuadrado o se sale de los lados permitidos.
    Dimensions {
        /// Ancho.
        width: u32,
        /// Alto.
        height: u32,
    },
    /// El PNG está dañado o no se puede leer.
    Invalid(String),
}

impl std::fmt::Display for AvatarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AvatarError::TooLarge => {
                write!(f, "el avatar ocupa más de {} KiB", AVATAR_MAX_BYTES / 1024)
            }
            AvatarError::NotPng => f.write_str("el avatar debe ser una imagen PNG"),
            AvatarError::Dimensions { width, height } => write!(
                f,
                "el avatar debe ser cuadrado y medir entre {AVATAR_SIDE_MIN} y {AVATAR_SIDE_MAX} \
                 píxeles de lado (llegó de {width}×{height})"
            ),
            AvatarError::Invalid(why) => write!(f, "el PNG del avatar no es válido: {why}"),
        }
    }
}

impl std::error::Error for AvatarError {}

/// Avatar listo para guardar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Avatar {
    /// El PNG que codificó el servidor.
    pub png: Vec<u8>,
    /// Su versión ([`avatar_version`]).
    pub version: u64,
    /// Lado en píxeles.
    pub side: u32,
}

/// Valida `bytes` y devuelve el avatar vuelto a codificar.
pub fn validate(bytes: &[u8]) -> Result<Avatar, AvatarError> {
    if bytes.len() > AVATAR_MAX_BYTES {
        return Err(AvatarError::TooLarge);
    }
    if !bytes.starts_with(PNG_SIGNATURE) {
        return Err(AvatarError::NotPng);
    }
    // Las dimensiones, de la cabecera y antes de que el decodificador lea nada más: una que pide
    // gigas se rechaza sin reservar ni descomprimir.
    let (width, height) = header_size(bytes)
        .ok_or_else(|| AvatarError::Invalid("falta la cabecera IHDR".to_owned()))?;
    if width != height || !(AVATAR_SIDE_MIN..=AVATAR_SIDE_MAX).contains(&width) {
        return Err(AvatarError::Dimensions { width, height });
    }
    let invalid = |err: png::DecodingError| AvatarError::Invalid(err.to_string());
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(bytes),
        png::Limits {
            bytes: DECODE_LIMIT,
        },
    );
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    let mut reader = decoder.read_info().map_err(invalid)?;
    if reader.info().size() != (width, height) {
        return Err(AvatarError::Invalid("cabecera incoherente".to_owned()));
    }
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| AvatarError::Invalid("tamaño imposible".to_owned()))?;
    let mut pixels = vec![0; size];
    let frame = reader.next_frame(&mut pixels).map_err(invalid)?;
    // Hasta el final (`IEND`): un fichero cortado no vale aunque los píxeles estén enteros.
    reader.finish().map_err(invalid)?;
    pixels.truncate(frame.buffer_size());
    let rgba = to_rgba(&pixels, frame.color_type, width, height)?;
    let png = encode(&rgba, width)?;
    let version = avatar_version(&Sha256::digest(&png).into());
    Ok(Avatar {
        png,
        version,
        side: width,
    })
}

/// Ancho y alto del primer fragmento, que tiene que ser `IHDR` (13 bytes); `None` si no lo es.
fn header_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let chunk = bytes.get(PNG_SIGNATURE.len()..PNG_SIGNATURE.len() + 16)?;
    if chunk[..4] != 13u32.to_be_bytes() || &chunk[4..8] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(chunk[8..12].try_into().ok()?);
    let height = u32::from_be_bytes(chunk[12..16].try_into().ok()?);
    Some((width, height))
}

/// Píxeles de 8 bits de cualquier tipo de color → RGBA.
fn to_rgba(
    pixels: &[u8],
    color: png::ColorType,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, AvatarError> {
    let count = (width as usize) * (height as usize);
    let channels = match color {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        // `normalize_to_color8` expande la paleta: no puede llegar.
        png::ColorType::Indexed => {
            return Err(AvatarError::Invalid("paleta sin expandir".to_owned()));
        }
    };
    if pixels.len() < count * channels {
        return Err(AvatarError::Invalid("faltan píxeles".to_owned()));
    }
    let mut out = Vec::with_capacity(count * 4);
    for pixel in pixels.chunks_exact(channels).take(count) {
        let rgba = match *pixel {
            [g] => [g, g, g, 255],
            [g, a] => [g, g, g, a],
            [r, g, b] => [r, g, b, 255],
            [r, g, b, a] => [r, g, b, a],
            _ => unreachable!("chunks_exact da trozos de `channels` bytes"),
        };
        out.extend_from_slice(&rgba);
    }
    Ok(out)
}

/// PNG nuevo con solo la cabecera, los píxeles y el final: RGB si todo es opaco, si no RGBA.
fn encode(rgba: &[u8], side: u32) -> Result<Vec<u8>, AvatarError> {
    let (pixels, _) = rgba.as_chunks::<4>();
    let opaque = pixels.iter().all(|pixel| pixel[3] == 255);
    let (color, data) = if opaque {
        let rgb: Vec<u8> = pixels
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect();
        (png::ColorType::Rgb, rgb)
    } else {
        (png::ColorType::Rgba, rgba.to_vec())
    };
    let failed = |err: png::EncodingError| AvatarError::Invalid(err.to_string());
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, side, side);
    encoder.set_color(color);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::High);
    let mut writer = encoder.write_header().map_err(failed)?;
    writer.write_image_data(&data).map_err(failed)?;
    writer.finish().map_err(failed)?;
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// PNG de prueba de `width`×`height` con el color y la profundidad dados.
    pub(crate) fn png_of(
        width: u32,
        height: u32,
        color: png::ColorType,
        depth: png::BitDepth,
    ) -> Vec<u8> {
        let channels = match color {
            png::ColorType::Grayscale | png::ColorType::Indexed => 1,
            png::ColorType::GrayscaleAlpha => 2,
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
        };
        let bytes_per_sample = if depth == png::BitDepth::Sixteen {
            2
        } else {
            1
        };
        let len = (width * height) as usize * channels * bytes_per_sample;
        let data: Vec<u8> = (0..len).map(|i| (i * 7 % 251) as u8).collect();
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(color);
        encoder.set_depth(depth);
        if color == png::ColorType::Indexed {
            encoder.set_palette(
                (0..=255u8)
                    .flat_map(|i| [i, 255 - i, i / 2])
                    .collect::<Vec<_>>(),
            );
        }
        let mut writer = encoder.write_header().expect("cabecera");
        writer.write_image_data(&data).expect("píxeles");
        writer.finish().expect("fin");
        out
    }

    #[test]
    fn every_color_type_is_reencoded_as_eight_bit_png() {
        for (color, depth) in [
            (png::ColorType::Grayscale, png::BitDepth::Eight),
            (png::ColorType::GrayscaleAlpha, png::BitDepth::Eight),
            (png::ColorType::Rgb, png::BitDepth::Eight),
            (png::ColorType::Rgba, png::BitDepth::Eight),
            (png::ColorType::Rgba, png::BitDepth::Sixteen),
            (png::ColorType::Indexed, png::BitDepth::Eight),
        ] {
            let avatar = validate(&png_of(64, 64, color, depth)).expect("válido");
            assert_eq!(avatar.side, 64);
            let again = validate(&avatar.png).expect("el PNG propio también vale");
            assert_eq!(
                again.png, avatar.png,
                "{color:?}: volver a codificar es estable"
            );
            assert_eq!(again.version, avatar.version);
            let decoder = png::Decoder::new(Cursor::new(avatar.png.as_slice()));
            let reader = decoder.read_info().expect("legible");
            assert_eq!(reader.info().bit_depth, png::BitDepth::Eight);
            assert!(reader.info().uncompressed_latin1_text.is_empty());
        }
    }

    #[test]
    fn opaque_avatars_drop_the_alpha_channel() {
        let avatar =
            validate(&png_of(32, 32, png::ColorType::Rgb, png::BitDepth::Eight)).expect("válido");
        let decoder = png::Decoder::new(Cursor::new(avatar.png.as_slice()));
        assert_eq!(
            decoder.read_info().expect("legible").info().color_type,
            png::ColorType::Rgb
        );
    }

    #[test]
    fn wrong_shapes_and_sizes_are_rejected() {
        for (width, height) in [(64, 32), (8, 8), (AVATAR_SIDE_MAX + 1, AVATAR_SIDE_MAX + 1)] {
            assert_eq!(
                validate(&png_of(
                    width,
                    height,
                    png::ColorType::Rgb,
                    png::BitDepth::Eight
                )),
                Err(AvatarError::Dimensions { width, height })
            );
        }
        assert!(
            validate(&png_of(
                AVATAR_SIDE_MAX,
                AVATAR_SIDE_MAX,
                png::ColorType::Rgba,
                png::BitDepth::Eight
            ))
            .is_ok()
        );
    }

    #[test]
    fn garbage_is_rejected_without_panicking() {
        assert_eq!(validate(b"GIF89a"), Err(AvatarError::NotPng));
        assert_eq!(
            validate(&[0; AVATAR_MAX_BYTES + 1]),
            Err(AvatarError::TooLarge)
        );
        let good = png_of(64, 64, png::ColorType::Rgb, png::BitDepth::Eight);
        // Cortado en cualquier punto, o con bytes cambiados: error, nunca un pánico.
        for cut in (8..good.len()).step_by(7) {
            assert!(validate(&good[..cut]).is_err(), "cortado en {cut}");
        }
        for at in (8..good.len()).step_by(5) {
            let mut bad = good.clone();
            bad[at] ^= 0x5a;
            let _ = validate(&bad);
        }
    }

    #[test]
    fn a_header_claiming_a_huge_image_is_rejected_before_decoding() {
        // Cabecera de 60 000 × 60 000 (con su CRC bien) y nada más: se rechaza por las
        // dimensiones sin reservar los gigas que pediría.
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(b"IHDR");
        ihdr.extend_from_slice(&60_000u32.to_be_bytes());
        ihdr.extend_from_slice(&60_000u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        let crc = crc32(&ihdr);
        let mut bomb = PNG_SIGNATURE.to_vec();
        bomb.extend_from_slice(&13u32.to_be_bytes());
        bomb.extend_from_slice(&ihdr);
        bomb.extend_from_slice(&crc.to_be_bytes());
        assert_eq!(
            validate(&bomb),
            Err(AvatarError::Dimensions {
                width: 60_000,
                height: 60_000
            })
        );
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for byte in data {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
}
