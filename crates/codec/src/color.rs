//! Espacio de color y formato del vídeo.
//!
//! El vídeo va en **BT.709 de rango limitado** (lo que esperan todos los
//! decodificadores y pantallas para contenido HD) y, con HDR, en
//! **BT.2020 con PQ** (SMPTE ST 2084, «HDR10») a 10 bits. El formato de un flujo
//! ([`VideoFormat`]) junta el submuestreo de croma ([`Chroma`]: 4:2:0 o 4:4:4) y el espacio de
//! color ([`ColorSpace`]); viaja en `CodecSelected` y lo usan la conversión ([`crate::convert`]),
//! el VUI de los codificadores y el shader de `coroom-render`.
//!
//! Aquí también están las funciones de transferencia en coma flotante que comparten la conversión
//! en CPU y los tests del shader: sRGB, PQ (EOTF e inversa), la matriz de primarios BT.709 →
//! BT.2020 y la lectura de medios flotantes (`f16`) de la captura HDR.

use std::fmt;

/// Matriz de coeficientes YCbCr (y primarios) del flujo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Matrix {
    /// BT.709 (Kr = 0,2126, Kb = 0,0722), primarios sRGB.
    Bt709,
    /// BT.2020 sin luminancia constante (Kr = 0,2627, Kb = 0,0593), primarios BT.2020.
    Bt2020,
}

impl Matrix {
    /// Coeficientes `(Kr, Kb)` de la luma.
    pub const fn coefficients(self) -> (f32, f32) {
        match self {
            Matrix::Bt709 => (0.2126, 0.0722),
            Matrix::Bt2020 => (0.2627, 0.0593),
        }
    }
}

/// Rango de los valores YCbCr.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Range {
    /// Rango limitado («de televisión»): Y en [16, 235] y croma en [16, 240] a 8 bits.
    Limited,
    /// Rango completo: [0, 255] a 8 bits.
    Full,
}

/// Función de transferencia.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Transfer {
    /// Rango dinámico estándar: la curva de la pantalla (sRGB / BT.1886); el vídeo lleva los
    /// valores tal como salen de la captura.
    Sdr,
    /// PQ (SMPTE ST 2084), luminancia absoluta hasta 10 000 nits.
    Pq,
}

/// Espacio de color de un flujo de vídeo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColorSpace {
    /// Matriz y primarios.
    pub matrix: Matrix,
    /// Rango.
    pub range: Range,
    /// Transferencia.
    pub transfer: Transfer,
}

impl ColorSpace {
    /// BT.709 en rango limitado, SDR: el formato por defecto.
    pub const SDR: ColorSpace = ColorSpace {
        matrix: Matrix::Bt709,
        range: Range::Limited,
        transfer: Transfer::Sdr,
    };

    /// BT.2020 con PQ en rango limitado (HDR10).
    pub const HDR10: ColorSpace = ColorSpace {
        matrix: Matrix::Bt2020,
        range: Range::Limited,
        transfer: Transfer::Pq,
    };

    /// Si es HDR.
    pub const fn is_hdr(self) -> bool {
        matches!(self.transfer, Transfer::Pq)
    }
}

impl Default for ColorSpace {
    fn default() -> Self {
        Self::SDR
    }
}

impl fmt::Display for ColorSpace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let matrix = match self.matrix {
            Matrix::Bt709 => "BT.709",
            Matrix::Bt2020 => "BT.2020",
        };
        let range = match self.range {
            Range::Limited => "rango limitado",
            Range::Full => "rango completo",
        };
        match self.transfer {
            Transfer::Sdr => write!(f, "{matrix} SDR, {range}"),
            Transfer::Pq => write!(f, "{matrix} PQ (HDR10), {range}"),
        }
    }
}

/// Submuestreo de croma.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Chroma {
    /// 4:2:0: un par de croma cada 2 × 2 píxeles (lo normal en vídeo).
    #[default]
    Yuv420,
    /// 4:4:4: croma en cada píxel (texto nítido; pocos codificadores lo admiten).
    Yuv444,
}

/// Formato de un flujo: croma y espacio de color (la profundidad sale de la transferencia: PQ va a
/// 10 bits, SDR a 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct VideoFormat {
    /// Submuestreo de croma.
    pub chroma: Chroma,
    /// Espacio de color.
    pub color: ColorSpace,
}

impl VideoFormat {
    /// 4:2:0 SDR BT.709 (el de siempre).
    pub const SDR_420: VideoFormat = VideoFormat {
        chroma: Chroma::Yuv420,
        color: ColorSpace::SDR,
    };

    /// 4:4:4 SDR BT.709.
    pub const SDR_444: VideoFormat = VideoFormat {
        chroma: Chroma::Yuv444,
        color: ColorSpace::SDR,
    };

    /// 4:2:0 HDR10 (BT.2020 PQ, 10 bits).
    pub const HDR10_420: VideoFormat = VideoFormat {
        chroma: Chroma::Yuv420,
        color: ColorSpace::HDR10,
    };

    /// Bits por muestra.
    pub const fn bit_depth(self) -> u8 {
        if self.color.is_hdr() { 10 } else { 8 }
    }

    /// Si es HDR.
    pub const fn is_hdr(self) -> bool {
        self.color.is_hdr()
    }

    /// Si es el formato de siempre (4:2:0 SDR de 8 bits), el único que va sin copias por la GPU.
    pub fn is_default(self) -> bool {
        self == Self::SDR_420
    }
}

impl fmt::Display for VideoFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let chroma = match self.chroma {
            Chroma::Yuv420 => "4:2:0",
            Chroma::Yuv444 => "4:4:4",
        };
        if self.is_hdr() {
            f.write_str(chroma)?;
            f.write_str(" HDR10 10 bits")
        } else {
            write!(f, "{chroma} SDR")
        }
    }
}

/// Luminancia del blanco de referencia del SDR dentro de un flujo HDR, en nits (BT.2408).
pub const SDR_WHITE_NITS: f32 = 203.0;

/// Luminancia de 1,0 en scRGB (el formato de coma flotante de la captura HDR de Windows y de los
/// compositores de Linux), en nits.
pub const SCRGB_NITS: f32 = 80.0;

/// Luminancia máxima de PQ, en nits.
pub const PQ_MAX_NITS: f32 = 10_000.0;

/// sRGB (con la curva por tramos) → lineal, de [0, 1] a [0, 1]; conserva el signo (scRGB
/// extendido).
pub fn srgb_to_linear(value: f32) -> f32 {
    let v = value.abs();
    let linear = if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    };
    linear.copysign(value)
}

/// Lineal → sRGB (inversa de [`srgb_to_linear`]); conserva el signo.
pub fn linear_to_srgb(value: f32) -> f32 {
    let v = value.abs();
    let encoded = if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    encoded.copysign(value)
}

const PQ_M1: f32 = 2610.0 / 16384.0;
const PQ_M2: f32 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f32 = 3424.0 / 4096.0;
const PQ_C2: f32 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f32 = 2392.0 / 4096.0 * 32.0;

/// Inversa de la EOTF de PQ: luminancia en nits → señal en [0, 1].
pub fn pq_from_nits(nits: f32) -> f32 {
    let y = (nits / PQ_MAX_NITS).clamp(0.0, 1.0);
    let p = y.powf(PQ_M1);
    ((PQ_C1 + PQ_C2 * p) / (1.0 + PQ_C3 * p)).powf(PQ_M2)
}

/// EOTF de PQ: señal en [0, 1] → luminancia en nits.
pub fn pq_to_nits(signal: f32) -> f32 {
    let e = signal.clamp(0.0, 1.0).powf(1.0 / PQ_M2);
    let y = ((e - PQ_C1).max(0.0) / (PQ_C2 - PQ_C3 * e)).powf(1.0 / PQ_M1);
    y * PQ_MAX_NITS
}

/// Primarios BT.709 → BT.2020 en luz lineal (BT.2087, tabla 2), por filas.
pub const BT709_TO_BT2020: [[f32; 3]; 3] = [
    [0.627_404, 0.329_283, 0.043_313],
    [0.069_097, 0.919_540, 0.011_362],
    [0.016_391, 0.088_013, 0.895_595],
];

/// Primarios BT.2020 → BT.709 en luz lineal (inversa de [`BT709_TO_BT2020`]), por filas.
pub const BT2020_TO_BT709: [[f32; 3]; 3] = [
    [1.660_491, -0.587_641, -0.072_850],
    [-0.124_550, 1.132_9, -0.008_349],
    [-0.018_151, -0.100_579, 1.118_73],
];

/// Multiplica una matriz 3 × 3 (por filas) por un vector.
pub fn mul3(m: &[[f32; 3]; 3], [r, g, b]: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * r + m[0][1] * g + m[0][2] * b,
        m[1][0] * r + m[1][1] * g + m[1][2] * b,
        m[2][0] * r + m[2][1] * g + m[2][2] * b,
    ]
}

/// Rodilla del tone mapping a SDR ([`tone_map`]), en luz lineal relativa al blanco SDR.
pub const TONE_KNEE: f32 = 0.9;

/// Tone mapping de HDR a una pantalla SDR: luz lineal relativa al blanco SDR (1,0 =
/// [`SDR_WHITE_NITS`]) → [0, 1]. Identidad hasta la rodilla ([`TONE_KNEE`]) y hombro de Reinhard
/// por encima, aplicado al máximo de R, G y B para no cambiar el tono (la idea de la EETF de
/// BT.2390, más simple): el contenido SDR de un flujo HDR se ve casi igual y solo se comprimen
/// las luces. Los negativos (fuera de la gama BT.709) se recortan a 0. El shader de
/// `coroom-render` hace lo mismo.
pub fn tone_map([r, g, b]: [f32; 3]) -> [f32; 3] {
    let clamp = |c: f32| c.max(0.0);
    let (r, g, b) = (clamp(r), clamp(g), clamp(b));
    let peak = r.max(g).max(b);
    if peak <= TONE_KNEE {
        return [r, g, b];
    }
    let t = (peak - TONE_KNEE) / (1.0 - TONE_KNEE);
    let mapped = TONE_KNEE + (1.0 - TONE_KNEE) * t / (1.0 + t);
    let scale = mapped / peak;
    [r * scale, g * scale, b * scale]
}

/// Lee un `f16` IEEE 754 (binary16) como `f32` (la captura HDR entrega `R16G16B16A16Float`).
pub fn f16_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits >> 15) << 31;
    let exponent = u32::from((bits >> 10) & 0x1f);
    let mantissa = u32::from(bits & 0x3ff);
    let out = match (exponent, mantissa) {
        (0, 0) => sign,
        (0, m) => {
            // Subnormal: se normaliza.
            let shift = m.leading_zeros() - 21;
            let m = (m << shift) & 0x3ff;
            let e = 127 - 15 + 1 - shift;
            sign | (e << 23) | (m << 13)
        }
        (0x1f, m) => sign | 0x7f80_0000 | (m << 13),
        (e, m) => sign | ((e + 127 - 15) << 23) | (m << 13),
    };
    f32::from_bits(out)
}

/// Escribe un `f32` como `f16` (redondeo al más cercano; para tests y patrones sintéticos).
pub fn f32_to_f16(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32;
    let mantissa = bits & 0x7f_ffff;
    if exponent == 0xff {
        return sign | 0x7c00 | if mantissa != 0 { 0x200 } else { 0 };
    }
    let e = exponent - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = (mantissa | 0x80_0000) >> (1 - e);
        let rounded = (m + 0x1000) >> 13;
        return sign | rounded as u16;
    }
    let rounded = (mantissa + 0x1000) >> 13;
    // Un acarreo del redondeo sube el exponente, como debe.
    sign | (((e as u32) << 10) + rounded) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pq_round_trips_and_hits_reference_points() {
        // Puntos de referencia de ST 2084: 100 nits ≈ 0,508; 1000 nits ≈ 0,752; 10 000 nits = 1.
        assert!((pq_from_nits(100.0) - 0.508).abs() < 0.002);
        assert!((pq_from_nits(1000.0) - 0.752).abs() < 0.002);
        assert!((pq_from_nits(10_000.0) - 1.0).abs() < 1e-6);
        assert_eq!(pq_from_nits(0.0).max(0.0), pq_from_nits(0.0));
        assert!(pq_from_nits(0.0) < 1e-6);
        for nits in [0.1, 1.0, 80.0, 203.0, 600.0, 4000.0] {
            let back = pq_to_nits(pq_from_nits(nits));
            assert!((back - nits).abs() / nits < 1e-3, "{nits} → {back}");
        }
    }

    #[test]
    fn primaries_matrices_are_inverse_and_keep_white() {
        let white = mul3(&BT709_TO_BT2020, [1.0, 1.0, 1.0]);
        for c in white {
            assert!((c - 1.0).abs() < 1e-4, "{white:?}");
        }
        for rgb in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.2, 0.5, 0.9]] {
            let back = mul3(&BT2020_TO_BT709, mul3(&BT709_TO_BT2020, rgb));
            for (a, b) in back.iter().zip(rgb) {
                assert!((a - b).abs() < 1e-4, "{rgb:?} → {back:?}");
            }
        }
    }

    #[test]
    fn srgb_curve_round_trips_with_sign() {
        for v in [-0.5, 0.0, 0.002, 0.04, 0.2, 0.5, 1.0, 1.5] {
            let back = linear_to_srgb(srgb_to_linear(v));
            assert!((back - v).abs() < 1e-5, "{v} → {back}");
        }
        assert!((srgb_to_linear(0.5) - 0.214).abs() < 0.001);
    }

    #[test]
    fn tone_mapping_keeps_sdr_and_compresses_highlights() {
        assert_eq!(tone_map([0.5, 0.2, 0.1]), [0.5, 0.2, 0.1]);
        assert_eq!(tone_map([-0.1, 0.2, 0.1]), [0.0, 0.2, 0.1]);
        let mut last = 0.0;
        for i in 0..400 {
            let x = i as f32 * 0.125;
            let [y, _, _] = tone_map([x, x, x]);
            assert!((0.0..1.0).contains(&y), "{x} → {y}");
            assert!(y >= last, "monótona");
            last = y;
        }
        // El blanco SDR queda casi igual y conserva el tono (proporciones entre canales).
        let [r, g, b] = tone_map([1.0, 1.0, 1.0]);
        assert!((r - 0.95).abs() < 1e-6 && r == g && g == b);
        let [r, g, _] = tone_map([4.0, 2.0, 0.0]);
        assert!((r / g - 2.0).abs() < 1e-5);
    }

    #[test]
    fn half_floats_convert_both_ways() {
        for (bits, value) in [
            (0x0000, 0.0),
            (0x3c00, 1.0),
            (0xc000, -2.0),
            (0x3800, 0.5),
            (0x7bff, 65504.0),
            (0x0001, 5.960_464_5e-8),
        ] {
            assert_eq!(f16_to_f32(bits), value, "{bits:#06x}");
            assert_eq!(f32_to_f16(value), bits, "{value}");
        }
        assert!(f16_to_f32(0x7c00).is_infinite());
        for v in [0.1f32, 1.25, 12.5, 0.0003] {
            let back = f16_to_f32(f32_to_f16(v));
            assert!((back - v).abs() / v < 1e-3, "{v} → {back}");
        }
    }

    #[test]
    fn formats_read_in_spanish() {
        assert_eq!(VideoFormat::SDR_420.to_string(), "4:2:0 SDR");
        assert_eq!(VideoFormat::SDR_444.to_string(), "4:4:4 SDR");
        assert_eq!(VideoFormat::HDR10_420.to_string(), "4:2:0 HDR10 10 bits");
        assert_eq!(VideoFormat::HDR10_420.bit_depth(), 10);
        assert_eq!(VideoFormat::default(), VideoFormat::SDR_420);
        assert_eq!(
            ColorSpace::HDR10.to_string(),
            "BT.2020 PQ (HDR10), rango limitado"
        );
    }
}
