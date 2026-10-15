//! Geometría del vídeo: el tamaño del flujo a partir del capturado y del tope de resolución.
//!
//! El tope (`EncoderConfig.width × height` en `coroom-codec`, «resolución máxima» en los ajustes)
//! es un **máximo**, no un tamaño fijo: lo que la captura trae más grande se reduce para que quepa,
//! con su proporción; lo que ya cabe se queda como está. Lo usan la captura que elige su tamaño
//! (la sintética), el codificador (en la GPU o en CPU) y la sesión al anunciar el tamaño del flujo
//! (`HostFrameSize`, `CodecSelected`), así que vive aquí, donde todos llegan.

/// `captured` reducido a `max` si no cabe, con su proporción y lados pares (lo que piden los
/// códecs 4:2:0); nunca ampliado ni menor que 2 × 2.
pub fn fit((width, height): (u32, u32), (max_width, max_height): (u32, u32)) -> (u32, u32) {
    let (width, height) = (width.max(1), height.max(1));
    if width <= max_width && height <= max_height {
        return (width, height);
    }
    let (w, h, max_w, max_h) = (
        u64::from(width),
        u64::from(height),
        u64::from(max_width),
        u64::from(max_height),
    );
    // Manda el lado que más sobra; el otro se escala en entero (sin coma flotante).
    let (fit_w, fit_h) = if w * max_h <= h * max_w {
        (w * max_h / h, max_h)
    } else {
        (max_w, h * max_w / w)
    };
    let even = |side: u64| -> u32 { (u32::try_from(side).unwrap_or(u32::MAX) & !1).max(2) };
    (even(fit_w), even(fit_h))
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn fit_keeps_the_aspect_ratio_with_even_sides() {
        assert_eq!(fit((640, 360), (320, 180)), (320, 180));
        assert_eq!(fit((2560, 1440), (1920, 1080)), (1920, 1080));
        assert_eq!(fit((3840, 2160), (1280, 720)), (1280, 720));
        assert_eq!(fit((1000, 1000), (1280, 720)), (720, 720));
        assert_eq!(fit((641, 361), (320, 180)), (318, 180));
        assert_eq!(fit((1920, 1080), (1280, 720)), (1280, 720));
        assert_eq!(fit((1366, 768), (1280, 720)), (1280, 718));
        assert_eq!(fit((3440, 1440), (1920, 1080)), (1920, 802));
        // Un solo lado que sobra también reduce, por ese lado.
        assert_eq!(fit((1280, 1440), (1920, 1080)), (960, 1080));
        assert_eq!(fit((640, 360), (0, 0)), (2, 2));
    }

    #[test]
    fn fit_never_enlarges() {
        assert_eq!(fit((640, 360), (1920, 1080)), (640, 360));
        assert_eq!(fit((1000, 1000), (1920, 1080)), (1000, 1000));
        assert_eq!(fit((1920, 1080), (1920, 1080)), (1920, 1080));
        // Lo que ya cabe no se toca, ni aunque tenga lados impares.
        assert_eq!(fit((641, 361), (1920, 1080)), (641, 361));
        assert_eq!(fit((0, 0), (1920, 1080)), (1, 1));
    }
}
