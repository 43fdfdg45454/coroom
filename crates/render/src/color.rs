//! Espacio de color del vídeo en la presentación y las cuentas que hace el shader
//! `yuv.wgsl`, en Rust para probarlas.
//!
//! `coroom-render` no depende de `coroom-codec` (`docs/ARQUITECTURA.md` §5): [`ColorInfo`] es su propia
//! copia del espacio de color que se negocia en `CodecSelected`, y estas funciones repiten las de
//! `coroom_codec::color` (las dos crates tienen los mismos tests de valores conocidos).
//!
//! - **SDR** (BT.709 en rango limitado): Y′CbCr → R′G′B′ con la matriz del flujo; la salida queda
//!   en espacio gamma, como siempre.
//! - **HDR10** (BT.2020 con PQ): Y′CbCr → R′G′B′ PQ → luz lineal en nits ([`pq_to_nits`]) →
//!   primarios BT.709 ([`BT2020_TO_BT709`]) relativos al blanco de referencia de 203 nits. En una
//!   superficie HDR ([`OutputMode::Hdr`]) se recorta suave al margen de la pantalla y se codifica en
//!   sRGB extendido (valores por encima de 1 son más brillantes que el blanco SDR; [`crate::hdr`] lo
//!   pasa a la superficie lineal). En una SDR se aplica [`tone_map`] (identidad hasta la rodilla y
//!   hombro de Reinhard sobre el máximo de RGB) y se codifica en sRGB.

/// Matriz YCbCr del flujo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ColorMatrix {
    /// BT.709 (Kr = 0,2126, Kb = 0,0722).
    #[default]
    Bt709,
    /// BT.2020 sin luminancia constante (Kr = 0,2627, Kb = 0,0593).
    Bt2020,
}

impl ColorMatrix {
    /// Coeficientes `(Kr, Kb)`.
    pub const fn coefficients(self) -> (f32, f32) {
        match self {
            ColorMatrix::Bt709 => (0.2126, 0.0722),
            ColorMatrix::Bt2020 => (0.2627, 0.0593),
        }
    }
}

/// Espacio de color de un fotograma que se presenta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColorInfo {
    /// Matriz.
    pub matrix: ColorMatrix,
    /// Rango completo (si no, limitado).
    pub full_range: bool,
    /// Transferencia PQ (HDR10); si no, SDR.
    pub pq: bool,
}

impl ColorInfo {
    /// BT.709 en rango limitado, SDR.
    pub const SDR: ColorInfo = ColorInfo {
        matrix: ColorMatrix::Bt709,
        full_range: false,
        pq: false,
    };

    /// BT.2020 con PQ en rango limitado (HDR10).
    pub const HDR10: ColorInfo = ColorInfo {
        matrix: ColorMatrix::Bt2020,
        full_range: false,
        pq: true,
    };
}

impl Default for ColorInfo {
    fn default() -> Self {
        Self::SDR
    }
}

/// Cómo se escribe el vídeo en el destino.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputMode {
    /// Destino SDR: el HDR se convierte con [`tone_map`].
    Sdr,
    /// Destino HDR en sRGB extendido ([`crate::hdr::HdrOutput`]): 1,0 es el blanco SDR y
    /// las luces llegan hasta `headroom` (el margen de la pantalla; 1,0 si no se sabe).
    Hdr {
        /// Brillo máximo de la pantalla en múltiplos del blanco SDR.
        headroom: f32,
    },
}

/// Luminancia del blanco de referencia del SDR en un flujo HDR (BT.2408), en nits.
pub const SDR_WHITE_NITS: f32 = 203.0;

/// Rodilla de [`tone_map`].
pub const TONE_KNEE: f32 = 0.9;

/// Primarios BT.2020 → BT.709 en luz lineal, por filas.
pub const BT2020_TO_BT709: [[f32; 3]; 3] = [
    [1.660_491, -0.587_641, -0.072_850],
    [-0.124_550, 1.132_9, -0.008_349],
    [-0.018_151, -0.100_579, 1.118_73],
];

/// Matriz afín 3 × 4 (por filas) que el shader aplica a `(y, u, v, 1)` muestreados de las
/// texturas para obtener R′G′B′: incluye el rango (limitado o completo) y la profundidad. `scale`
/// pasa de la muestra normalizada al código entero (255 en 8 bits; 65535 / 64 en P010, que guarda
/// 10 bits en la parte alta de 16).
pub fn yuv_to_rgb(color: ColorInfo, bits: u8, scale: f32) -> [[f32; 4]; 3] {
    let unit = f32::from(1u16 << (bits - 8));
    let max = f32::from(((1u32 << bits) - 1) as u16);
    // Y′ = ay · s + cy y C = ac · s + cc, con s la muestra normalizada.
    let (ay, cy, ac, cc) = if color.full_range {
        (scale / max, 0.0, scale / max, -128.0 * unit / max)
    } else {
        (
            scale / (219.0 * unit),
            -16.0 / 219.0,
            scale / (224.0 * unit),
            -128.0 / 224.0,
        )
    };
    let (kr, kb) = color.matrix.coefficients();
    let kg = 1.0 - kr - kb;
    let r_cr = 2.0 * (1.0 - kr);
    let g_cb = -2.0 * kb * (1.0 - kb) / kg;
    let g_cr = -2.0 * kr * (1.0 - kr) / kg;
    let b_cb = 2.0 * (1.0 - kb);
    [
        [ay, 0.0, r_cr * ac, cy + r_cr * cc],
        [ay, g_cb * ac, g_cr * ac, cy + (g_cb + g_cr) * cc],
        [ay, b_cb * ac, 0.0, cy + b_cb * cc],
    ]
}

/// Aplica [`yuv_to_rgb`] a una muestra.
pub fn apply(m: &[[f32; 4]; 3], [y, u, v]: [f32; 3]) -> [f32; 3] {
    let row = |r: &[f32; 4]| r[0] * y + r[1] * u + r[2] * v + r[3];
    [row(&m[0]), row(&m[1]), row(&m[2])]
}

const PQ_M1: f32 = 2610.0 / 16384.0;
const PQ_M2: f32 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f32 = 3424.0 / 4096.0;
const PQ_C2: f32 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f32 = 2392.0 / 4096.0 * 32.0;

/// EOTF de PQ: señal en [0, 1] → nits.
pub fn pq_to_nits(signal: f32) -> f32 {
    let e = signal.clamp(0.0, 1.0).powf(1.0 / PQ_M2);
    ((e - PQ_C1).max(0.0) / (PQ_C2 - PQ_C3 * e)).powf(1.0 / PQ_M1) * 10_000.0
}

/// Lineal → sRGB, conservando el signo y sin recortar (sRGB extendido).
pub fn linear_to_srgb(value: f32) -> f32 {
    let v = value.abs();
    let encoded = if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    encoded.copysign(value)
}

/// sRGB → lineal, conservando el signo (lo que hace la pasada final de [`crate::hdr`]).
pub fn srgb_to_linear(value: f32) -> f32 {
    let v = value.abs();
    let linear = if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    };
    linear.copysign(value)
}

/// Tone mapping a SDR (el mismo que `coroom_codec::color::tone_map`): identidad hasta la rodilla
/// y hombro de Reinhard sobre el máximo de RGB, con los negativos recortados.
pub fn tone_map(rgb: [f32; 3]) -> [f32; 3] {
    tone_map_peak(rgb, 1.0)
}

/// [`tone_map`] hasta `peak` en vez de hasta 1 (el recorte suave de la salida HDR).
pub fn tone_map_peak(rgb: [f32; 3], peak: f32) -> [f32; 3] {
    let peak = peak.max(1.0);
    let [r, g, b] = rgb.map(|c| c.max(0.0) / peak);
    let top = r.max(g).max(b);
    if top <= TONE_KNEE {
        return [r * peak, g * peak, b * peak];
    }
    let t = (top - TONE_KNEE) / (1.0 - TONE_KNEE);
    let mapped = TONE_KNEE + (1.0 - TONE_KNEE) * t / (1.0 + t);
    let scale = mapped / top * peak;
    [r * scale, g * scale, b * scale]
}

/// Lo que escribe el shader para un píxel ya convertido a R′G′B′ (`rgb`), según el espacio de
/// color y el destino.
pub fn output(rgb: [f32; 3], color: ColorInfo, mode: OutputMode) -> [f32; 3] {
    if !color.pq {
        return rgb.map(|c| c.clamp(0.0, 1.0));
    }
    let nits = rgb.map(pq_to_nits);
    let relative = nits.map(|n| n / SDR_WHITE_NITS);
    let [r, g, b] = relative;
    let m = &BT2020_TO_BT709;
    let linear = [
        m[0][0] * r + m[0][1] * g + m[0][2] * b,
        m[1][0] * r + m[1][1] * g + m[1][2] * b,
        m[2][0] * r + m[2][1] * g + m[2][2] * b,
    ];
    match mode {
        OutputMode::Sdr => tone_map(linear).map(|c| linear_to_srgb(c.min(1.0))),
        OutputMode::Hdr { headroom } => tone_map_peak(linear, headroom).map(linear_to_srgb),
    }
}

/// Lee un `f16` IEEE 754 como `f32` (para leer destinos `Rgba16Float`).
pub fn f16_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits >> 15) << 31;
    let exponent = u32::from((bits >> 10) & 0x1f);
    let mantissa = u32::from(bits & 0x3ff);
    let out = match (exponent, mantissa) {
        (0, 0) => sign,
        (0, m) => {
            let shift = m.leading_zeros() - 21;
            let m = (m << shift) & 0x3ff;
            sign | ((127 - 15 + 1 - shift) << 23) | (m << 13)
        }
        (0x1f, m) => sign | 0x7f80_0000 | (m << 13),
        (e, m) => sign | ((e + 127 - 15) << 23) | (m << 13),
    };
    f32::from_bits(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3], tolerance: f32) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tolerance)
    }

    /// Las barras de BT.709 en rango limitado vuelven a sus colores (los valores de
    /// `coroom_codec::convert`).
    #[test]
    fn bt709_limited_bars_invert() {
        let m = yuv_to_rgb(ColorInfo::SDR, 8, 255.0);
        let s = |c: [u8; 3]| c.map(|v| f32::from(v) / 255.0);
        assert!(close(apply(&m, s([235, 128, 128])), [1.0, 1.0, 1.0], 1e-3));
        assert!(close(apply(&m, s([16, 128, 128])), [0.0, 0.0, 0.0], 1e-3));
        assert!(close(apply(&m, s([63, 102, 240])), [1.0, 0.0, 0.0], 0.01));
        assert!(close(apply(&m, s([173, 42, 26])), [0.0, 1.0, 0.0], 0.01));
        assert!(close(apply(&m, s([32, 240, 118])), [0.0, 0.0, 1.0], 0.01));
    }

    /// BT.2020 a 10 bits en P010: el código 940 es el blanco de la señal, 64 el negro, y el rojo
    /// de BT.2020 vuelve a rojo.
    #[test]
    fn bt2020_ten_bit_matrix_inverts() {
        let scale = 65535.0 / 64.0;
        let m = yuv_to_rgb(ColorInfo::HDR10, 10, scale);
        let s = |c: [u16; 3]| c.map(|v| f32::from(v) * 64.0 / 65535.0);
        assert!(close(apply(&m, s([940, 512, 512])), [1.0, 1.0, 1.0], 2e-3));
        assert!(close(apply(&m, s([64, 512, 512])), [0.0, 0.0, 0.0], 2e-3));
        // Rojo puro BT.2020 (Kr = 0,2627): Y = 64 + 876·0,2627, Cb = 512 − 448·0,2627/0,9407…
        let (kr, kb) = ColorMatrix::Bt2020.coefficients();
        let y = 64.0 + 876.0 * kr;
        let cb = 512.0 + 896.0 * (-kr) / (2.0 * (1.0 - kb));
        let cr = 512.0 + 896.0 * (1.0 - kr) / (2.0 * (1.0 - kr));
        let red = apply(&m, [y, cb, cr].map(|v| v.round() * 64.0 / 65535.0));
        assert!(close(red, [1.0, 0.0, 0.0], 5e-3), "{red:?}");
    }

    #[test]
    fn full_range_has_no_offsets() {
        let full = ColorInfo {
            full_range: true,
            ..ColorInfo::SDR
        };
        let m = yuv_to_rgb(full, 8, 255.0);
        assert!(close(
            apply(&m, [1.0, 128.0 / 255.0, 128.0 / 255.0]),
            [1.0; 3],
            1e-3
        ));
        assert!(close(
            apply(&m, [0.0, 128.0 / 255.0, 128.0 / 255.0]),
            [0.0; 3],
            1e-3
        ));
    }

    /// El blanco de referencia de un flujo HDR (PQ de 203 nits) se ve blanco en las dos salidas;
    /// 1000 nits satura en SDR y sale por encima de 1 en HDR con margen.
    #[test]
    fn pq_output_tone_maps_or_extends() {
        let white = 0.580_689; // PQ de 203 nits.
        let sdr = output([white; 3], ColorInfo::HDR10, OutputMode::Sdr);
        assert!(sdr.iter().all(|c| (0.97..=1.0).contains(c)), "{sdr:?}");
        let hdr = output(
            [white; 3],
            ColorInfo::HDR10,
            OutputMode::Hdr { headroom: 4.0 },
        );
        assert!(close(hdr, [1.0; 3], 2e-3), "{hdr:?}");
        let bright = 0.751_827; // PQ de 1000 nits.
        let sdr = output([bright; 3], ColorInfo::HDR10, OutputMode::Sdr);
        assert!(sdr.iter().all(|c| *c <= 1.0 && *c > 0.98));
        let hdr = output(
            [bright; 3],
            ColorInfo::HDR10,
            OutputMode::Hdr { headroom: 8.0 },
        );
        // 1000 / 203 = 4,93 en lineal, por debajo de la rodilla (7,2): sRGB extendido de 4,93.
        assert!(
            (hdr[0] - linear_to_srgb(1000.0 / 203.0)).abs() < 0.01,
            "{hdr:?}"
        );
        // Sin margen conocido (1,0), se queda en el blanco SDR.
        let flat = output(
            [bright; 3],
            ColorInfo::HDR10,
            OutputMode::Hdr { headroom: 1.0 },
        );
        assert!(flat.iter().all(|c| *c <= 1.0));
        // SDR pasa tal cual.
        let sdr_in = [0.25, 0.5, 1.2];
        assert_eq!(
            output(sdr_in, ColorInfo::SDR, OutputMode::Hdr { headroom: 4.0 }),
            [0.25, 0.5, 1.0]
        );
    }

    #[test]
    fn half_floats_read_back() {
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x0001), 5.960_464_5e-8);
    }

    #[test]
    fn transfer_curves_match_the_references() {
        assert!((pq_to_nits(0.580_689) - 203.0).abs() < 0.5);
        assert!((pq_to_nits(1.0) - 10_000.0).abs() < 1.0);
        assert!(pq_to_nits(0.0) < 1e-3);
        for v in [-0.3, 0.001, 0.2, 0.7, 1.0, 2.5] {
            assert!((srgb_to_linear(linear_to_srgb(v)) - v).abs() < 1e-5);
        }
        let mut last = 0.0;
        for i in 0..200 {
            let [y, _, _] = tone_map([i as f32 * 0.25; 3]);
            assert!(y >= last && y < 1.0);
            last = y;
        }
        assert_eq!(tone_map_peak([2.0, 1.0, 0.0], 4.0), [2.0, 1.0, 0.0]);
    }
}
