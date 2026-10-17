//! Avatares en la aplicación, sin interfaz: preparar la imagen que elige el usuario para subirla,
//! leer con límites los avatares que llegan del servidor y la caché en disco.
//!
//! - [`prepare`]: PNG, JPEG o WebP (con su orientación EXIF) → recorte al cuadrado central →
//!   reducción a [`AVATAR_SIDE_MAX`] como mucho → PNG. Lo que no se puede leer, demasiado grande o
//!   demasiado pequeño se explica con [`PrepareError`].
//! - [`decode`]: el PNG que sirve el servidor (él ya lo validó y volvió a codificar; aquí se lee
//!   otra vez con límites, por si acaso) → píxeles RGBA cuadrados.
//! - [`DiskCache`]: `avatars/<versión>.png` en el directorio de datos. La versión es la del
//!   servidor ([`avatar_version`], parte del SHA-256 del PNG): cada fichero se comprueba al leerlo
//!   y uno que no cuadra se borra, así que la caché nunca devuelve otra imagen que la pedida.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::{ImageDecoder, ImageEncoder, ImageFormat, ImageReader, Limits, RgbaImage};

use coroom_transport::crypto::sha256;
use coroom_transport::proto::api::{
    AVATAR_MAX_BYTES, AVATAR_SIDE_MAX, AVATAR_SIDE_MIN, avatar_version,
};

/// Bytes como mucho de la imagen que se elige (una foto grande de un móvil cabe de sobra).
pub const SOURCE_MAX_BYTES: u64 = 40 * 1024 * 1024;

/// Lado como mucho de la imagen que se elige, en píxeles.
pub const SOURCE_MAX_SIDE: u32 = 12_000;

/// Memoria como mucho del decodificador de la imagen que se elige.
const SOURCE_MAX_ALLOC: u64 = 768 * 1024 * 1024;

/// Ficheros como mucho en la caché de disco (cada uno ocupa como mucho 320 KiB).
pub const DISK_CACHE_MAX_FILES: usize = 400;

/// Por qué no se puede usar una imagen como avatar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepareError {
    /// El fichero no se puede leer.
    Unreadable,
    /// Pasa de [`SOURCE_MAX_BYTES`] o de [`SOURCE_MAX_SIDE`].
    TooLarge,
    /// No es PNG, JPEG ni WebP.
    Unsupported,
    /// Está dañada.
    Damaged,
    /// El lado corto no llega a [`AVATAR_SIDE_MIN`] píxeles.
    TooSmall,
}

impl PrepareError {
    /// Texto para el usuario.
    pub fn message(&self) -> &'static str {
        match self {
            PrepareError::Unreadable => t!("avatar-no-se-puede-leer"),
            PrepareError::TooLarge => t!("avatar-demasiado-grande"),
            PrepareError::Unsupported => t!("avatar-formato-no-admitido"),
            PrepareError::Damaged => t!("avatar-danada"),
            PrepareError::TooSmall => t!("avatar-demasiado-pequena"),
        }
    }
}

/// Lee `path` y lo prepara con [`prepare`].
pub fn prepare_file(path: &Path) -> Result<Vec<u8>, PrepareError> {
    let size = std::fs::metadata(path)
        .map_err(|_| PrepareError::Unreadable)?
        .len();
    if size > SOURCE_MAX_BYTES {
        return Err(PrepareError::TooLarge);
    }
    let bytes = std::fs::read(path).map_err(|_| PrepareError::Unreadable)?;
    prepare(&bytes)
}

/// La imagen elegida, lista para subir: el cuadrado central, de [`AVATAR_SIDE_MAX`] píxeles de
/// lado como mucho (las pequeñas no se agrandan), en PNG.
pub fn prepare(bytes: &[u8]) -> Result<Vec<u8>, PrepareError> {
    if bytes.len() as u64 > SOURCE_MAX_BYTES {
        return Err(PrepareError::TooLarge);
    }
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| PrepareError::Unreadable)?;
    if !matches!(
        reader.format(),
        Some(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP)
    ) {
        return Err(PrepareError::Unsupported);
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(SOURCE_MAX_SIDE);
    limits.max_image_height = Some(SOURCE_MAX_SIDE);
    limits.max_alloc = Some(SOURCE_MAX_ALLOC);
    let mut reader = reader;
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(decode_error)?;
    let orientation = decoder.orientation().ok();
    // La única copia grande es la imagen decodificada: el recorte y la reducción la leen tal cual
    // (sin pasarla antes a RGBA ni copiar el cuadrado) y la orientación se aplica a lo reducido.
    let image = image::DynamicImage::from_decoder(decoder).map_err(decode_error)?;
    let side = image.width().min(image.height());
    if side < AVATAR_SIDE_MIN {
        return Err(PrepareError::TooSmall);
    }
    let mut square = centred_square(&image, side.min(AVATAR_SIDE_MAX));
    drop(image);
    if let Some(orientation) = orientation {
        // Girar o voltear deja el cuadrado central en su sitio: da igual hacerlo antes o después.
        square.apply_orientation(orientation);
    }
    let png = encode_png(&square.into_rgba8());
    debug_assert!(png.len() <= AVATAR_MAX_BYTES, "{} B", png.len());
    Ok(png)
}

/// El cuadrado central de `image`, de `target` píxeles de lado (como mucho el lado corto), en el
/// mismo formato de píxel; lo único que se reserva es el resultado.
fn centred_square(image: &image::DynamicImage, target: u32) -> image::DynamicImage {
    use image::DynamicImage as D;
    use image::imageops::{crop_imm, thumbnail};
    let side = image.width().min(image.height());
    let x = (image.width() - side) / 2;
    let y = (image.height() - side) / 2;
    macro_rules! square {
        ($buffer:expr) => {{
            let view = crop_imm($buffer, x, y, side, side);
            if target < side {
                // Media por áreas: rápida y sin el aliasing de un remuestreo que salta píxeles.
                D::from(thumbnail(&*view, target, target))
            } else {
                D::from(view.to_image())
            }
        }};
    }
    match image {
        D::ImageLuma8(buffer) => square!(buffer),
        D::ImageLumaA8(buffer) => square!(buffer),
        D::ImageRgb8(buffer) => square!(buffer),
        D::ImageRgba8(buffer) => square!(buffer),
        D::ImageLuma16(buffer) => square!(buffer),
        D::ImageLumaA16(buffer) => square!(buffer),
        D::ImageRgb16(buffer) => square!(buffer),
        D::ImageRgba16(buffer) => square!(buffer),
        D::ImageRgb32F(buffer) => square!(buffer),
        D::ImageRgba32F(buffer) => square!(buffer),
        // Un formato de píxel que aún no existe: píxel a píxel, en RGBA de 8 bits.
        other => square!(other),
    }
}

fn decode_error(err: image::ImageError) -> PrepareError {
    match err {
        image::ImageError::Limits(_) => PrepareError::TooLarge,
        image::ImageError::Unsupported(_) => PrepareError::Unsupported,
        _ => PrepareError::Damaged,
    }
}

/// PNG RGBA con compresión alta.
fn encode_png(image: &RgbaImage) -> Vec<u8> {
    let mut out = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new_with_quality(
        &mut out,
        image::codecs::png::CompressionType::Best,
        image::codecs::png::FilterType::Adaptive,
    );
    // Escribir en un `Vec` no falla: los tamaños y el tipo de color son los de la imagen.
    let written = encoder.write_image(
        image.as_raw(),
        image.width(),
        image.height(),
        image::ExtendedColorType::Rgba8,
    );
    debug_assert!(written.is_ok(), "{written:?}");
    out
}

/// Si `png` es la versión `version` (el servidor la calcula igual).
pub fn matches_version(png: &[u8], version: u64) -> bool {
    avatar_version(&sha256(png)) == version
}

/// El PNG de un avatar del servidor, en píxeles RGBA: solo PNG, cuadrado y de los lados
/// permitidos, con límites de memoria. `None` si no cumple (se queda la inicial).
pub fn decode(png: &[u8]) -> Option<RgbaImage> {
    if png.len() > AVATAR_MAX_BYTES {
        return None;
    }
    let mut reader = ImageReader::with_format(Cursor::new(png), ImageFormat::Png);
    let mut limits = Limits::default();
    limits.max_image_width = Some(AVATAR_SIDE_MAX);
    limits.max_image_height = Some(AVATAR_SIDE_MAX);
    limits.max_alloc = Some(u64::from(AVATAR_SIDE_MAX * AVATAR_SIDE_MAX) * 16);
    reader.limits(limits);
    let image = reader.decode().ok()?.into_rgba8();
    let side = image.width();
    (side == image.height() && (AVATAR_SIDE_MIN..=AVATAR_SIDE_MAX).contains(&side)).then_some(image)
}

/// Lados de las copias reducidas de un avatar para la interfaz: la mayor es la imagen; cada una,
/// la mitad de la anterior hasta 32 px. Así una fila de 24 px no dibuja una imagen de 256 px
/// reducida de golpe (sin mipmaps, el filtro lineal deja dientes).
pub fn levels(image: &RgbaImage) -> Vec<RgbaImage> {
    let mut out = vec![image.clone()];
    while let Some(last) = out.last()
        && last.width() / 2 >= 32
    {
        let half = last.width() / 2;
        let smaller = image::imageops::thumbnail(last, half, half);
        out.push(smaller);
    }
    out
}

/// Caché de avatares en disco: `dir/<versión en hexadecimal>.png`.
#[derive(Debug, Clone)]
pub struct DiskCache {
    dir: PathBuf,
}

impl DiskCache {
    /// La caché en `data_dir/avatars`.
    pub fn in_data_dir(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("avatars"),
        }
    }

    fn path(&self, version: u64) -> PathBuf {
        self.dir.join(format!("{version:014x}.png"))
    }

    /// El PNG de `version`, si está y es el que dice ser (si no, se borra).
    pub fn load(&self, version: u64) -> Option<Vec<u8>> {
        let path = self.path(version);
        let bytes = std::fs::read(&path).ok()?;
        if bytes.len() <= AVATAR_MAX_BYTES && matches_version(&bytes, version) {
            return Some(bytes);
        }
        tracing::debug!(version, "avatar de la caché dañado; se borra");
        let _ = std::fs::remove_file(&path);
        None
    }

    /// Guarda el PNG de `version` (que debe serlo) de una vez: primero a un temporal y luego
    /// renombrado, para que una caída a medias no deje un fichero cortado. Después recorta la
    /// caché a [`DISK_CACHE_MAX_FILES`].
    pub fn store(&self, version: u64, png: &[u8]) -> std::io::Result<()> {
        if !matches_version(png, version) {
            return Err(std::io::Error::other("el avatar no es de esa versión"));
        }
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path(version);
        let temporary = path.with_extension("png.part");
        std::fs::write(&temporary, png)?;
        std::fs::rename(&temporary, &path)?;
        self.prune(DISK_CACHE_MAX_FILES);
        Ok(())
    }

    /// Borra los más viejos (por fecha de modificación) hasta dejar `keep`.
    pub fn prune(&self, keep: usize) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "png"))
            .filter_map(|entry| {
                let modified = entry.metadata().ok()?.modified().ok()?;
                Some((modified, entry.path()))
            })
            .collect();
        if files.len() <= keep {
            return;
        }
        files.sort();
        let excess = files.len() - keep;
        for (_, path) in files.into_iter().take(excess) {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8, 255])
        })
    }

    fn encoded(image: &RgbaImage, format: ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image.clone())
            .write_to(&mut out, format)
            .expect("codificar");
        out.into_inner()
    }

    #[test]
    fn a_wide_photo_becomes_its_centred_square_reduced() {
        // Mitad izquierda roja, centro verde, mitad derecha azul: el recorte se queda el centro.
        let wide = RgbaImage::from_fn(900, 300, |x, _| match x {
            0..300 => image::Rgba([255, 0, 0, 255]),
            300..600 => image::Rgba([0, 255, 0, 255]),
            _ => image::Rgba([0, 0, 255, 255]),
        });
        let png = prepare(&encoded(&wide, ImageFormat::Png)).expect("preparada");
        let square = decode(&png).expect("legible");
        assert_eq!(square.dimensions(), (AVATAR_SIDE_MAX, AVATAR_SIDE_MAX));
        assert_eq!(square.get_pixel(128, 128).0, [0, 255, 0, 255]);
        assert!(png.len() <= AVATAR_MAX_BYTES);
    }

    #[test]
    fn jpeg_and_webp_are_accepted_and_small_images_are_not_enlarged() {
        let jpeg = prepare(&encoded(&photo(640, 480), ImageFormat::Jpeg)).expect("JPEG");
        assert_eq!(decode(&jpeg).expect("legible").width(), AVATAR_SIDE_MAX);
        let webp = prepare(&encoded(&photo(100, 120), ImageFormat::WebP)).expect("WebP");
        assert_eq!(decode(&webp).expect("legible").dimensions(), (100, 100));
    }

    /// PNG de `image` con la orientación EXIF `orientation` (1-8).
    fn oriented_png(image: &RgbaImage, orientation: u16) -> Vec<u8> {
        // TIFF big endian con un solo IFD y una sola entrada: 0x0112 (Orientation), SHORT, 1.
        let mut exif = b"MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01".to_vec();
        exif.extend_from_slice(&orientation.to_be_bytes());
        exif.extend_from_slice(&[0; 6]);
        let mut out = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut out);
        encoder.set_exif_metadata(exif).expect("EXIF en PNG");
        encoder
            .write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                image::ExtendedColorType::Rgba8,
            )
            .expect("codificar");
        out
    }

    #[test]
    fn the_exif_orientation_turns_the_reduced_square() {
        // 600 × 300 con el cuadrado central rojo arriba y azul abajo (verde fuera de él).
        let red = [255, 0, 0, 255];
        let blue = [0, 0, 255, 255];
        let photo = RgbaImage::from_fn(600, 300, |x, y| match (x, y) {
            (150..450, 0..150) => image::Rgba(red),
            (150..450, _) => image::Rgba(blue),
            _ => image::Rgba([0, 255, 0, 255]),
        });
        let middle = AVATAR_SIDE_MAX / 2;
        let (near, far) = (middle / 2, middle + middle / 2);
        // (orientación EXIF, píxel que queda rojo, píxel que queda azul)
        let cases = [
            (1, (middle, near), (middle, far)),
            (3, (middle, far), (middle, near)), // 180°
            (6, (far, middle), (near, middle)), // 90° a la derecha
            (8, (near, middle), (far, middle)), // 90° a la izquierda
            (2, (middle, near), (middle, far)), // espejo horizontal: arriba sigue arriba
            (4, (middle, far), (middle, near)), // espejo vertical
        ];
        for (orientation, at_red, at_blue) in cases {
            let png = prepare(&oriented_png(&photo, orientation)).expect("preparada");
            let square = decode(&png).expect("legible");
            assert_eq!(square.dimensions(), (AVATAR_SIDE_MAX, AVATAR_SIDE_MAX));
            assert_eq!(
                square.get_pixel(at_red.0, at_red.1).0,
                red,
                "orientación {orientation}"
            );
            assert_eq!(
                square.get_pixel(at_blue.0, at_blue.1).0,
                blue,
                "orientación {orientation}"
            );
        }
    }

    #[test]
    fn every_pixel_format_is_cropped_and_reduced() {
        // Gris de 8 bits, RGB de 16 bits y RGBA de 16 bits: el recorte no pasa antes por RGBA.
        let gray = image::GrayImage::from_fn(700, 400, |x, _| image::Luma([(x % 256) as u8]));
        let rgb16 = image::ImageBuffer::<image::Rgb<u16>, _>::from_pixel(
            400,
            700,
            image::Rgb([65535, 0, 32768]),
        );
        let rgba16 = image::ImageBuffer::<image::Rgba<u16>, _>::from_pixel(
            100,
            90,
            image::Rgba([0, 65535, 0, 65535]),
        );
        for (source, side, colour) in [
            (image::DynamicImage::from(gray), AVATAR_SIDE_MAX, None),
            (
                image::DynamicImage::from(rgb16),
                AVATAR_SIDE_MAX,
                Some([255, 0, 128, 255]),
            ),
            (
                image::DynamicImage::from(rgba16),
                90,
                Some([0, 255, 0, 255]),
            ),
        ] {
            let mut bytes = Cursor::new(Vec::new());
            source.write_to(&mut bytes, ImageFormat::Png).expect("PNG");
            let square = decode(&prepare(bytes.get_ref()).expect("preparada")).expect("legible");
            assert_eq!(square.dimensions(), (side, side));
            if let Some(colour) = colour {
                assert_eq!(square.get_pixel(side / 2, side / 2).0, colour);
            }
        }
    }

    #[test]
    fn bad_images_are_explained_not_panicked_on() {
        assert_eq!(prepare(b"no es una imagen"), Err(PrepareError::Unsupported));
        assert_eq!(
            prepare(&encoded(&photo(64, 8), ImageFormat::Png)),
            Err(PrepareError::TooSmall)
        );
        let gif = b"GIF89a\x01\x00\x01\x00\x00\x00\x00;";
        assert_eq!(prepare(gif), Err(PrepareError::Unsupported));
        let good = encoded(&photo(64, 64), ImageFormat::Png);
        for cut in (16..good.len()).step_by(97) {
            assert!(prepare(&good[..cut]).is_err(), "cortada en {cut}");
        }
        let jpeg = encoded(&photo(64, 64), ImageFormat::Jpeg);
        for at in (2..jpeg.len()).step_by(13) {
            let mut bad = jpeg.clone();
            bad[at] ^= 0xa5;
            let _ = prepare(&bad);
        }
    }

    #[test]
    fn a_header_claiming_a_huge_image_is_rejected_by_the_limits() {
        // PNG cuya cabecera dice 50 000 × 50 000: se rechaza sin reservar la memoria.
        let mut png = encoded(&photo(16, 16), ImageFormat::Png);
        png[16..20].copy_from_slice(&50_000u32.to_be_bytes());
        png[20..24].copy_from_slice(&50_000u32.to_be_bytes());
        let crc = crc32(&png[12..29]);
        png[29..33].copy_from_slice(&crc.to_be_bytes());
        assert_eq!(prepare(&png), Err(PrepareError::TooLarge));
        assert!(decode(&png).is_none());
    }

    #[test]
    fn server_avatars_must_be_square_png_of_allowed_size() {
        assert!(decode(&encoded(&photo(64, 64), ImageFormat::Png)).is_some());
        assert!(decode(&encoded(&photo(64, 32), ImageFormat::Png)).is_none());
        assert!(decode(&encoded(&photo(300, 300), ImageFormat::Png)).is_none());
        assert!(decode(&encoded(&photo(64, 64), ImageFormat::Jpeg)).is_none());
        assert!(decode(b"\x89PNG\r\n\x1a\n").is_none());
    }

    #[test]
    fn levels_halve_down_to_thirty_two_pixels() {
        let sides: Vec<u32> = levels(&photo(256, 256))
            .iter()
            .map(RgbaImage::width)
            .collect();
        assert_eq!(sides, vec![256, 128, 64, 32]);
        let sides: Vec<u32> = levels(&photo(40, 40))
            .iter()
            .map(RgbaImage::width)
            .collect();
        assert_eq!(sides, vec![40]);
    }

    #[test]
    fn the_disk_cache_only_returns_what_was_asked_for() {
        let dir = tempfile::tempdir().expect("directorio");
        let cache = DiskCache::in_data_dir(dir.path());
        let png = prepare(&encoded(&photo(64, 64), ImageFormat::Png)).expect("preparada");
        let version = avatar_version(&sha256(&png));
        assert_eq!(cache.load(version), None);
        assert!(
            cache.store(version + 1, &png).is_err(),
            "otra versión no se guarda"
        );
        cache.store(version, &png).expect("guardar");
        assert_eq!(cache.load(version), Some(png.clone()));
        // Un fichero cambiado en disco no se devuelve y se borra.
        let path = cache.path(version);
        let mut damaged = png.clone();
        damaged[40] ^= 1;
        std::fs::write(&path, damaged).expect("estropear");
        assert_eq!(cache.load(version), None);
        assert!(!path.exists());
    }

    #[test]
    fn the_disk_cache_keeps_the_newest_files() {
        let dir = tempfile::tempdir().expect("directorio");
        let cache = DiskCache::in_data_dir(dir.path());
        let mut versions = Vec::new();
        for seed in 0..5u8 {
            let mut image = photo(32, 32);
            image.get_pixel_mut(0, 0).0 = [seed, seed, seed, 255];
            let png = encode_png(&image);
            let version = avatar_version(&sha256(&png));
            cache.store(version, &png).expect("guardar");
            // Fechas de modificación distintas y en orden, sin esperar.
            let file = std::fs::File::options()
                .write(true)
                .open(cache.path(version))
                .expect("abrir");
            let when = std::time::SystemTime::UNIX_EPOCH
                + std::time::Duration::from_secs(1_000 + u64::from(seed));
            file.set_modified(when).expect("fecha");
            versions.push(version);
        }
        cache.prune(2);
        let kept: Vec<bool> = versions.iter().map(|v| cache.load(*v).is_some()).collect();
        assert_eq!(kept, vec![false, false, false, true, true]);
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
