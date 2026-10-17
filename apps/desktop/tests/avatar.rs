//! Memoria de [`coroom_desktop::avatar::prepare`] con una foto grande: el pico es más o menos la
//! imagen decodificada, no varias copias de ella (antes se pasaba entera a RGBA y luego se copiaba
//! el cuadrado: una foto de 12 000 px de lado pasaba de 1,5 GB).
//!
//! Se mide con el pico de memoria residente del proceso (`VmHWM`, que `clear_refs` pone a cero),
//! así que va en su propio binario y solo en Linux; en Windows el código es el mismo.

#[cfg(target_os = "linux")]
#[test]
fn preparing_a_large_photo_keeps_about_one_decoded_image_in_memory() {
    use std::io::Cursor;

    use coroom_desktop::avatar::prepare;

    // Una foto de 24 Mpx en JPEG (RGB): 72 MB decodificada; en RGBA serían 96 MB más.
    let (width, height) = (6000u32, 4000u32);
    let decoded = u64::from(width) * u64::from(height) * 3;
    let jpeg = {
        let photo = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 251) as u8, (y % 241) as u8, ((x ^ y) % 239) as u8])
        });
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(photo)
            .write_to(&mut out, image::ImageFormat::Jpeg)
            .expect("JPEG");
        out.into_inner()
    };
    // La foto de origen ya está liberada: el pico de aquí en adelante es el de `prepare`.
    if let Err(err) = std::fs::write("/proc/self/clear_refs", "5") {
        // Sin `clear_refs` (algunos núcleos y contenedores) no hay cómo medir el pico.
        eprintln!("sin /proc/self/clear_refs ({err}): no se mide el pico de memoria");
        return;
    }
    let before = status_kib("VmRSS:");
    assert!(
        status_kib("VmHWM:") <= before + 1024,
        "el pico no se puso a cero"
    );
    let png = prepare(&jpeg).expect("preparada");
    let peak = (status_kib("VmHWM:") - before) * 1024;
    assert!(!png.is_empty());
    assert!(
        peak < decoded * 3 / 2,
        "pico de {} MB al preparar una foto de {} MB decodificada",
        peak >> 20,
        decoded >> 20
    );
}

/// Un campo en KiB de `/proc/self/status`.
#[cfg(target_os = "linux")]
fn status_kib(field: &str) -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("/proc/self/status");
    status
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse().ok())
        .unwrap_or_else(|| panic!("{field} en /proc/self/status"))
}
