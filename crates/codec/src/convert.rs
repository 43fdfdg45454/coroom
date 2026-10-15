//! Conversión de color en CPU.
//!
//! **SDR** con la crate `yuv` (SIMD: AVX2/SSE4.1 en x86-64, NEON en aarch64): BGRA → I420
//! (codificadores software), BGRA → NV12 (entrada de los de hardware) y BGRA →
//! I444 / NV24 (4:4:4). **Matriz: BT.709 en rango limitado** (Y ∈ [16, 235], U y V ∈ [16, 240]), la
//! que señala el VUI de los codificadores ([`MATRIX`], [`RANGE`]). El shader
//! WGSL de `coroom-render` la invierte con los coeficientes de [`crate::color::Matrix`].
//!
//! **HDR** (propia, sin SIMD): BGRA (sRGB, el SDR se coloca en el blanco de referencia de
//! 203 nits de BT.2408) o RGBA de medio flotante (scRGB lineal, 1,0 = 80 nits) → BT.2020 con PQ a
//! 10 bits en rango limitado, en P010 (hardware) o en tres planos de 10 bits (SVT-AV1); y, al
//! revés, RGBA de medio flotante → BGRA SDR con el tone mapping de [`crate::color::tone_map`]
//! cuando la captura es HDR y el flujo no.

use std::sync::OnceLock;

use yuv::{
    BufferStoreMut, YuvBiPlanarImageMut, YuvConversionMode, YuvPlanarImageMut, YuvRange,
    YuvStandardMatrix, bgra_to_yuv_nv12, bgra_to_yuv_nv24, bgra_to_yuv420, bgra_to_yuv444,
};

use crate::color::{
    BT709_TO_BT2020, Matrix, SCRGB_NITS, SDR_WHITE_NITS, f16_to_f32, linear_to_srgb, mul3,
    pq_from_nits, srgb_to_linear, tone_map,
};
use crate::{CodecError, ColorSpace, PixelFormat, RawFrame};

/// Matriz de color de la conversión SDR (ver la documentación del módulo).
pub const MATRIX: YuvStandardMatrix = YuvStandardMatrix::Bt709;

/// Rango de la conversión (ver la documentación del módulo).
pub const RANGE: YuvRange = YuvRange::Limited;

/// Convierte un fotograma [`PixelFormat::Bgra`] en uno [`PixelFormat::I420`] compacto.
pub fn bgra_to_i420(frame: &RawFrame) -> Result<RawFrame, CodecError> {
    let mut data = Vec::new();
    bgra_to_i420_into(frame, &mut data)?;
    Ok(RawFrame {
        frame_id: frame.frame_id,
        width: frame.width,
        height: frame.height,
        format: PixelFormat::I420,
        stride: frame.width,
        data,
        gpu: None,
        color: ColorSpace::SDR,
        pool: None,
    })
}

/// Como [`bgra_to_i420`], pero escribe los planos Y, U y V compactados en `out`, que se reutiliza
/// entre fotogramas para no reservar memoria en cada uno.
pub fn bgra_to_i420_into(frame: &RawFrame, out: &mut Vec<u8>) -> Result<(), CodecError> {
    check_bgra(frame)?;
    let (cw, ch) = RawFrame::chroma_size(frame.width, frame.height);
    let y_len = frame.width as usize * frame.height as usize;
    let c_len = cw as usize * ch as usize;
    out.resize(y_len + 2 * c_len, 0);
    let (y_plane, chroma) = out.split_at_mut(y_len);
    let (u_plane, v_plane) = chroma.split_at_mut(c_len);
    bgra_to_i420_planes(
        frame,
        [(y_plane, frame.width), (u_plane, cw), (v_plane, cw)],
    )
}

/// Convierte un fotograma BGRA directamente en tres planos I420 ajenos (por ejemplo, los de un
/// `AVFrame`), cada uno con su fila en bytes.
pub fn bgra_to_i420_planes(
    frame: &RawFrame,
    [
        (y_plane, y_stride),
        (u_plane, u_stride),
        (v_plane, v_stride),
    ]: [(&mut [u8], u32); 3],
) -> Result<(), CodecError> {
    check_bgra(frame)?;
    let mut image = YuvPlanarImageMut {
        y_plane: BufferStoreMut::Borrowed(y_plane),
        y_stride,
        u_plane: BufferStoreMut::Borrowed(u_plane),
        u_stride,
        v_plane: BufferStoreMut::Borrowed(v_plane),
        v_stride,
        width: frame.width,
        height: frame.height,
    };
    bgra_to_yuv420(
        &mut image,
        &frame.data,
        frame.stride,
        RANGE,
        MATRIX,
        YuvConversionMode::Balanced,
    )
    .map_err(|err| CodecError::Backend(format!("conversión BGRA → I420: {err}")))
}

/// Convierte un fotograma BGRA en los planos Y y UV de un NV12 ajeno, cada uno con su fila en
/// bytes (la entrada de los codificadores por hardware).
pub fn bgra_to_nv12_planes(
    frame: &RawFrame,
    [(y_plane, y_stride), (uv_plane, uv_stride)]: [(&mut [u8], u32); 2],
) -> Result<(), CodecError> {
    check_bgra(frame)?;
    let mut image = YuvBiPlanarImageMut {
        y_plane: BufferStoreMut::Borrowed(y_plane),
        y_stride,
        uv_plane: BufferStoreMut::Borrowed(uv_plane),
        uv_stride,
        width: frame.width,
        height: frame.height,
    };
    bgra_to_yuv_nv12(
        &mut image,
        &frame.data,
        frame.stride,
        RANGE,
        MATRIX,
        YuvConversionMode::Balanced,
    )
    .map_err(|err| CodecError::Backend(format!("conversión BGRA → NV12: {err}")))
}

/// Convierte un fotograma [`PixelFormat::Bgra`] en uno [`PixelFormat::I444`] compacto.
pub fn bgra_to_i444(frame: &RawFrame) -> Result<RawFrame, CodecError> {
    check_bgra(frame)?;
    let len = frame.width as usize * frame.height as usize;
    let mut data = vec![0u8; 3 * len];
    let (y_plane, chroma) = data.split_at_mut(len);
    let (u_plane, v_plane) = chroma.split_at_mut(len);
    let w = frame.width;
    bgra_to_i444_planes(frame, [(y_plane, w), (u_plane, w), (v_plane, w)])?;
    Ok(RawFrame::cpu(
        frame.frame_id,
        (frame.width, frame.height),
        PixelFormat::I444,
        frame.width,
        data,
    ))
}

/// Convierte un fotograma BGRA en tres planos I444 ajenos (por ejemplo, los de un `AVFrame`
/// `yuv444p`), cada uno con su fila en bytes.
pub fn bgra_to_i444_planes(
    frame: &RawFrame,
    [
        (y_plane, y_stride),
        (u_plane, u_stride),
        (v_plane, v_stride),
    ]: [(&mut [u8], u32); 3],
) -> Result<(), CodecError> {
    check_bgra(frame)?;
    let mut image = YuvPlanarImageMut {
        y_plane: BufferStoreMut::Borrowed(y_plane),
        y_stride,
        u_plane: BufferStoreMut::Borrowed(u_plane),
        u_stride,
        v_plane: BufferStoreMut::Borrowed(v_plane),
        v_stride,
        width: frame.width,
        height: frame.height,
    };
    bgra_to_yuv444(
        &mut image,
        &frame.data,
        frame.stride,
        RANGE,
        MATRIX,
        YuvConversionMode::Balanced,
    )
    .map_err(|err| CodecError::Backend(format!("conversión BGRA → I444: {err}")))
}

/// Convierte un fotograma BGRA en los planos Y y UV de un NV24 ajeno (la entrada 4:4:4 de Vulkan
/// Video), cada uno con su fila en bytes.
pub fn bgra_to_nv24_planes(
    frame: &RawFrame,
    [(y_plane, y_stride), (uv_plane, uv_stride)]: [(&mut [u8], u32); 2],
) -> Result<(), CodecError> {
    check_bgra(frame)?;
    let mut image = YuvBiPlanarImageMut {
        y_plane: BufferStoreMut::Borrowed(y_plane),
        y_stride,
        uv_plane: BufferStoreMut::Borrowed(uv_plane),
        uv_stride,
        width: frame.width,
        height: frame.height,
    };
    bgra_to_yuv_nv24(
        &mut image,
        &frame.data,
        frame.stride,
        RANGE,
        MATRIX,
        YuvConversionMode::Balanced,
    )
    .map_err(|err| CodecError::Backend(format!("conversión BGRA → NV24: {err}")))
}

/// Planos de destino de la conversión HDR de 10 bits, cada uno con su fila en **bytes**.
#[derive(Debug)]
pub enum TenBitPlanes<'a> {
    /// P010 (codificadores por hardware): Y y UV entrelazado, palabras de 16 bits *little-endian*
    /// con los 10 bits en la parte alta.
    P010 {
        /// Plano Y.
        y: (&'a mut [u8], u32),
        /// Plano UV.
        uv: (&'a mut [u8], u32),
    },
    /// `yuv420p10le` (SVT-AV1): tres planos de palabras de 16 bits *little-endian* con los 10 bits
    /// en la parte baja.
    Planar {
        /// Plano Y.
        y: (&'a mut [u8], u32),
        /// Plano U.
        u: (&'a mut [u8], u32),
        /// Plano V.
        v: (&'a mut [u8], u32),
    },
}

/// Tabla de la inversa de PQ indexada por los bits del `f32` de `nits / 10 000`: 128 tramos por
/// octava en [`PQ_TABLE_OCTAVES`] octavas por debajo de 1, con interpolación lineal. Así no hay
/// raíces ni potencias por canal (solo operaciones enteras y una lectura), y el error queda muy por
/// debajo de un código de 10 bits.
struct PqTable(Vec<f32>);

/// Octavas cubiertas por la tabla: de 2^-32 · 10 000 nits (2 · 10⁻⁶ nits) a 10 000 nits.
const PQ_TABLE_OCTAVES: u32 = 32;
/// Bits de mantisa que eligen el tramo dentro de la octava (128 tramos).
const PQ_TABLE_STEP_SHIFT: u32 = 23 - 7;
/// Bits del `f32` del menor valor de la tabla (2^-32).
const PQ_TABLE_MIN_BITS: u32 = (127 - PQ_TABLE_OCTAVES) << 23;
const PQ_TABLE_LEN: usize = (PQ_TABLE_OCTAVES << 7) as usize + 1;

impl PqTable {
    fn get() -> &'static PqTable {
        static TABLE: OnceLock<PqTable> = OnceLock::new();
        TABLE.get_or_init(|| {
            PqTable(
                (0..PQ_TABLE_LEN as u32)
                    .map(|i| {
                        let y = f32::from_bits(PQ_TABLE_MIN_BITS + (i << PQ_TABLE_STEP_SHIFT));
                        pq_from_nits(y * crate::color::PQ_MAX_NITS)
                    })
                    .collect(),
            )
        })
    }

    /// Señal PQ de `nits` (los negativos y `NaN` cuentan como negro).
    #[inline]
    fn encode(&self, nits: f32) -> f32 {
        // `max` descarta el NaN (con `clamp` llegaría a la tabla como blanco).
        #[allow(clippy::manual_clamp)]
        let y = (nits * (1.0 / crate::color::PQ_MAX_NITS)).max(0.0).min(1.0);
        let bits = y.to_bits();
        if bits < PQ_TABLE_MIN_BITS {
            // Por debajo de la tabla, recta hasta el primer valor (PQ(0) es prácticamente 0).
            return self.0[0] * y * (1u64 << PQ_TABLE_OCTAVES) as f32;
        }
        let rel = bits - PQ_TABLE_MIN_BITS;
        let index = (rel >> PQ_TABLE_STEP_SHIFT) as usize;
        let (Some(&low), Some(&high)) = (self.0.get(index), self.0.get(index + 1)) else {
            return self.0[PQ_TABLE_LEN - 1];
        };
        let frac = (rel & ((1 << PQ_TABLE_STEP_SHIFT) - 1)) as f32
            * (1.0 / (1u32 << PQ_TABLE_STEP_SHIFT) as f32);
        low + (high - low) * frac
    }
}

/// Medio flotante → `f32`, por tabla (256 KiB; ahorra la decodificación por canal).
fn f16_table() -> &'static [f32; 65536] {
    static TABLE: OnceLock<Box<[f32; 65536]>> = OnceLock::new();
    TABLE.get_or_init(|| Box::new(std::array::from_fn(|bits| f16_to_f32(bits as u16))))
}

/// sRGB de 8 bits → lineal, por tabla.
fn srgb_table() -> &'static [f32; 256] {
    static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|i| srgb_to_linear(i as f32 / 255.0)))
}

/// R′G′B′ PQ BT.2020 de la fila `row` de un fotograma BGRA o RGBA de medio flotante (ya
/// comprobado), en `out` (`width` píxeles).
fn hdr_row(frame: &RawFrame, row: usize, out: &mut [[f32; 3]]) {
    let pq = PqTable::get();
    let w = frame.width as usize;
    let start = row * frame.stride as usize;
    let encode = |rgb709: [f32; 3], scale: f32| {
        let [r, g, b] = mul3(&BT709_TO_BT2020, rgb709);
        [
            pq.encode(r * scale),
            pq.encode(g * scale),
            pq.encode(b * scale),
        ]
    };
    if frame.format == PixelFormat::Bgra {
        let lut = srgb_table();
        let line = &frame.data[start..start + 4 * w];
        for (dst, px) in out.iter_mut().zip(line.as_chunks::<4>().0) {
            let rgb = [
                lut[usize::from(px[2])],
                lut[usize::from(px[1])],
                lut[usize::from(px[0])],
            ];
            *dst = encode(rgb, SDR_WHITE_NITS);
        }
    } else {
        let halves = f16_table();
        let line = &frame.data[start..start + 8 * w];
        for (dst, px) in out.iter_mut().zip(line.as_chunks::<8>().0) {
            let half = |i: usize| halves[usize::from(u16::from_le_bytes([px[i], px[i + 1]]))];
            *dst = encode([half(0), half(2), half(4)], SCRGB_NITS);
        }
    }
}

/// Y′CbCr de 10 bits en rango limitado de un R′G′B′ con la matriz `matrix`.
fn ycbcr10(matrix: Matrix, [r, g, b]: [f32; 3]) -> (f32, f32, f32) {
    let (kr, kb) = matrix.coefficients();
    let y = kr * r + (1.0 - kr - kb) * g + kb * b;
    let cb = (b - y) / (2.0 * (1.0 - kb));
    let cr = (r - y) / (2.0 * (1.0 - kr));
    (64.0 + 876.0 * y, 512.0 + 896.0 * cb, 512.0 + 896.0 * cr)
}

/// Código de 10 bits redondeado (`+ 0.5` y truncado: `f32::round` es una llamada a la libm sin
/// SSE4.1, y esto va por píxel).
#[inline]
fn code10(value: f32) -> u16 {
    (value.clamp(0.0, 1023.0) + 0.5) as u16
}

fn put_word(plane: &mut [u8], stride: usize, x: usize, y: usize, value: u16) {
    let at = y * stride + 2 * x;
    plane[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

/// Trozo de los planos de destino con unas filas de croma (y el doble de luma).
enum Band<'a> {
    P010 {
        y: &'a mut [u8],
        uv: &'a mut [u8],
    },
    Planar {
        y: &'a mut [u8],
        u: &'a mut [u8],
        v: &'a mut [u8],
    },
}

/// Convierte las filas de croma `first..first + rows` (y sus filas de luma) en `band`.
fn hdr_band(frame: &RawFrame, band: Band<'_>, strides: [usize; 3], first: usize, rows: usize) {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let cw = w.div_ceil(2);
    let matrix = Matrix::Bt2020;
    let mut top = vec![[0.0f32; 3]; w];
    let mut bottom = vec![[0.0f32; 3]; w];
    let msb = matches!(band, Band::P010 { .. });
    let shift = |code: u16| if msb { code << 6 } else { code };
    let mut band = band;
    for local in 0..rows {
        let cy = first + local;
        let (y0, y1) = (2 * cy, (2 * cy + 1).min(h - 1));
        hdr_row(frame, y0, &mut top);
        if y1 != y0 {
            hdr_row(frame, y1, &mut bottom);
        } else {
            bottom.copy_from_slice(&top);
        }
        let luma_plane = match &mut band {
            Band::P010 { y, .. } | Band::Planar { y, .. } => &mut **y,
        };
        for (line, pixels) in [(0, &top), (1, &bottom)] {
            if 2 * cy + line >= h {
                break;
            }
            for (x, rgb) in pixels.iter().enumerate() {
                let (yv, _, _) = ycbcr10(matrix, *rgb);
                put_word(
                    luma_plane,
                    strides[0],
                    x,
                    2 * local + line,
                    shift(code10(yv)),
                );
            }
        }
        for cx in 0..cw {
            // Media de R′G′B′ del bloque 2 × 2 (con los bordes impares repetidos).
            let (x0, x1) = (2 * cx, (2 * cx + 1).min(w - 1));
            let mut sum = [0.0f32; 3];
            for px in [top[x0], top[x1], bottom[x0], bottom[x1]] {
                for c in 0..3 {
                    sum[c] += px[c] * 0.25;
                }
            }
            let (_, cb, cr) = ycbcr10(matrix, sum);
            let (cb, cr) = (shift(code10(cb)), shift(code10(cr)));
            match &mut band {
                Band::P010 { uv, .. } => {
                    put_word(uv, strides[1], 2 * cx, local, cb);
                    put_word(uv, strides[1], 2 * cx + 1, local, cr);
                }
                Band::Planar { u, v, .. } => {
                    put_word(u, strides[1], cx, local, cb);
                    put_word(v, strides[2], cx, local, cr);
                }
            }
        }
    }
}

/// Convierte un fotograma BGRA (SDR) o [`PixelFormat::Rgba16F`] (scRGB) en BT.2020 PQ de 10 bits,
/// 4:2:0 en rango limitado (HDR10), escrito en `out`. Reparte las filas entre los núcleos.
pub fn to_hdr10_planes(frame: &RawFrame, out: TenBitPlanes<'_>) -> Result<(), CodecError> {
    match frame.format {
        PixelFormat::Bgra => check_bgra(frame)?,
        PixelFormat::Rgba16F => check_rgba16f(frame)?,
        other => {
            return Err(CodecError::Unsupported(format!(
                "la conversión HDR espera BGRA o RGBA de coma flotante, no {other:?}"
            )));
        }
    }
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (cw, ch) = RawFrame::chroma_size(frame.width, frame.height);
    let (cw, ch) = (cw as usize, ch as usize);
    let too_small = || CodecError::Unsupported("planos de 10 bits demasiado pequeños".into());
    let fits = |plane: &[u8], stride: u32, row: usize, rows: usize| {
        stride as usize >= row && plane.len() >= stride as usize * (rows - 1) + row
    };
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .clamp(1, 8);
    let per = ch.div_ceil(threads).max(1);
    let mut bands: Vec<(Band<'_>, usize, usize)> = Vec::with_capacity(threads);
    let strides;
    match out {
        TenBitPlanes::P010 { y, uv } => {
            if !fits(y.0, y.1, 2 * w, h) || !fits(uv.0, uv.1, 4 * cw, ch) {
                return Err(too_small());
            }
            strides = [y.1 as usize, uv.1 as usize, 0];
            let luma = y.0.chunks_mut(2 * per * strides[0]);
            let chroma = uv.0.chunks_mut(per * strides[1]);
            for (index, (y, uv)) in luma.zip(chroma).enumerate() {
                let first = index * per;
                if first < ch {
                    bands.push((Band::P010 { y, uv }, first, per.min(ch - first)));
                }
            }
        }
        TenBitPlanes::Planar { y, u, v } => {
            if !fits(y.0, y.1, 2 * w, h)
                || !fits(u.0, u.1, 2 * cw, ch)
                || !fits(v.0, v.1, 2 * cw, ch)
            {
                return Err(too_small());
            }
            strides = [y.1 as usize, u.1 as usize, v.1 as usize];
            let luma = y.0.chunks_mut(2 * per * strides[0]);
            let us = u.0.chunks_mut(per * strides[1]);
            let vs = v.0.chunks_mut(per * strides[2]);
            for (index, ((y, u), v)) in luma.zip(us).zip(vs).enumerate() {
                let first = index * per;
                if first < ch {
                    bands.push((Band::Planar { y, u, v }, first, per.min(ch - first)));
                }
            }
        }
    }
    std::thread::scope(|scope| {
        for (band, first, rows) in bands {
            scope.spawn(move || hdr_band(frame, band, strides, first, rows));
        }
    });
    Ok(())
}

/// Convierte un fotograma BGRA o RGBA de medio flotante en uno [`PixelFormat::P010`] compacto
/// (BT.2020 PQ).
pub fn to_p010(frame: &RawFrame) -> Result<RawFrame, CodecError> {
    let (cw, _) = RawFrame::chroma_size(frame.width, frame.height);
    let y_len = 2 * frame.width as usize * frame.height as usize;
    let mut data = vec![0u8; RawFrame::p010_len(frame.width, frame.height)];
    let (y, uv) = data.split_at_mut(y_len);
    to_hdr10_planes(
        frame,
        TenBitPlanes::P010 {
            y: (y, 2 * frame.width),
            uv: (uv, 4 * cw),
        },
    )?;
    Ok(RawFrame::cpu(
        frame.frame_id,
        (frame.width, frame.height),
        PixelFormat::P010,
        2 * frame.width,
        data,
    )
    .with_color(ColorSpace::HDR10))
}

/// Convierte un fotograma [`PixelFormat::Rgba16F`] (scRGB lineal, HDR) en BGRA SDR: el blanco de
/// referencia (203 nits) pasa a blanco, las luces se comprimen con [`tone_map`] y se codifica con
/// la curva sRGB. Lo usa la sesión cuando la captura es HDR y el flujo no.
pub fn rgba16f_to_bgra(frame: &RawFrame) -> Result<RawFrame, CodecError> {
    check_rgba16f(frame)?;
    let (w, h) = (frame.width as usize, frame.height as usize);
    let mut data = Vec::with_capacity(w * h * 4);
    let scale = SCRGB_NITS / SDR_WHITE_NITS;
    let byte = |c: f32| (linear_to_srgb(c.clamp(0.0, 1.0)) * 255.0).round() as u8;
    for row in frame.data.chunks(frame.stride as usize).take(h) {
        for px in row[..w * 8].as_chunks::<8>().0 {
            let half = |i: usize| f16_to_f32(u16::from_le_bytes([px[i], px[i + 1]])) * scale;
            let [r, g, b] = tone_map([half(0), half(2), half(4)]);
            data.extend_from_slice(&[byte(b), byte(g), byte(r), 255]);
        }
    }
    Ok(RawFrame::cpu(
        frame.frame_id,
        (frame.width, frame.height),
        PixelFormat::Bgra,
        frame.width * 4,
        data,
    ))
}

/// Convierte `yuv420p10le` (tres planos de 10 bits en la parte baja, filas en bytes) en un P010
/// compacto. Es lo que entregan los decodificadores por software de 10 bits.
pub fn planar10_to_p010(
    (width, height): (u32, u32),
    [(y, ys), (u, us), (v, vs)]: [(&[u8], usize); 3],
) -> Result<Vec<u8>, CodecError> {
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = RawFrame::chroma_size(width, height);
    let (cw, ch) = (cw as usize, ch as usize);
    let short = || CodecError::Backend("planos de 10 bits incompletos".into());
    let word = |plane: &[u8], stride: usize, x: usize, row: usize| -> Result<u16, CodecError> {
        let at = row * stride + 2 * x;
        let bytes = plane.get(at..at + 2).ok_or_else(short)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]) << 6)
    };
    let mut out = Vec::with_capacity(RawFrame::p010_len(width, height));
    for row in 0..h {
        for x in 0..w {
            out.extend_from_slice(&word(y, ys, x, row)?.to_le_bytes());
        }
    }
    for row in 0..ch {
        for x in 0..cw {
            out.extend_from_slice(&word(u, us, x, row)?.to_le_bytes());
            out.extend_from_slice(&word(v, vs, x, row)?.to_le_bytes());
        }
    }
    Ok(out)
}

/// Copia un fotograma I420 en los planos de un NV12 ajeno, entrelazando U y V.
pub fn i420_to_nv12_planes(
    frame: &RawFrame,
    [(y_plane, y_stride), (uv_plane, uv_stride)]: [(&mut [u8], u32); 2],
) -> Result<(), CodecError> {
    let Some((y, u, v)) = frame.i420_planes() else {
        return Err(CodecError::Unsupported(format!(
            "se esperaba I420 y llegó {:?}",
            frame.format
        )));
    };
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (cw, ch) = RawFrame::chroma_size(frame.width, frame.height);
    let (cw, ch) = (cw as usize, ch as usize);
    let (ys, uvs) = (y_stride as usize, uv_stride as usize);
    if ys < w
        || uvs < 2 * cw
        || y_plane.len() < ys * (h - 1) + w
        || uv_plane.len() < uvs * (ch - 1) + 2 * cw
    {
        return Err(CodecError::Unsupported(
            "planos NV12 demasiado pequeños".into(),
        ));
    }
    for row in 0..h {
        let src = &y[row * frame.stride as usize..][..w];
        y_plane[row * ys..][..w].copy_from_slice(src);
    }
    for row in 0..ch {
        let dst = &mut uv_plane[row * uvs..][..2 * cw];
        for (x, pair) in dst.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            pair[0] = u[row * cw + x];
            pair[1] = v[row * cw + x];
        }
    }
    Ok(())
}

fn check_rgba16f(frame: &RawFrame) -> Result<(), CodecError> {
    if frame.format != PixelFormat::Rgba16F {
        return Err(CodecError::Unsupported(format!(
            "se esperaba RGBA de coma flotante y llegó {:?}",
            frame.format
        )));
    }
    if frame.width == 0 || frame.height == 0 {
        return Err(CodecError::Unsupported("fotograma vacío".into()));
    }
    let row = frame.width as usize * 8;
    let needed = frame.stride as usize * (frame.height as usize - 1) + row;
    if (frame.stride as usize) < row || frame.data.len() < needed {
        return Err(CodecError::Unsupported(format!(
            "búfer RGBA de {} bytes con fila de {} bytes no alcanza para {}×{}",
            frame.data.len(),
            frame.stride,
            frame.width,
            frame.height
        )));
    }
    Ok(())
}

fn check_bgra(frame: &RawFrame) -> Result<(), CodecError> {
    if frame.format != PixelFormat::Bgra {
        return Err(CodecError::Unsupported(format!(
            "se esperaba BGRA y llegó {:?}",
            frame.format
        )));
    }
    if frame.width == 0 || frame.height == 0 {
        return Err(CodecError::Unsupported("fotograma vacío".into()));
    }
    let row = frame.width as usize * 4;
    let needed = frame.stride as usize * (frame.height as usize - 1) + row;
    if (frame.stride as usize) < row || frame.data.len() < needed {
        return Err(CodecError::Unsupported(format!(
            "búfer BGRA de {} bytes con fila de {} bytes no alcanza para {}×{}",
            frame.data.len(),
            frame.stride,
            frame.width,
            frame.height
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_bgra(width: u32, height: u32, stride: u32, [r, g, b]: [u8; 3]) -> RawFrame {
        let mut data = vec![0u8; stride as usize * height as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let at = y * stride as usize + x * 4;
                data[at..at + 4].copy_from_slice(&[b, g, r, 255]);
            }
        }
        RawFrame {
            frame_id: 3,
            width,
            height,
            format: PixelFormat::Bgra,
            stride,
            data,
            gpu: None,
            color: ColorSpace::SDR,
            pool: None,
        }
    }

    /// Referencia BT.709 en rango limitado, en coma flotante.
    fn reference([r, g, b]: [u8; 3]) -> [f32; 3] {
        let (r, g, b) = (f32::from(r), f32::from(g), f32::from(b));
        let (kr, kb) = (0.2126, 0.0722);
        let luma = kr * r + (1.0 - kr - kb) * g + kb * b;
        let y = 16.0 + 219.0 * luma / 255.0;
        let u = 128.0 + 224.0 * (b - luma) / (2.0 * (1.0 - kb)) / 255.0;
        let v = 128.0 + 224.0 * (r - luma) / (2.0 * (1.0 - kr)) / 255.0;
        [y, u, v]
    }

    /// Valores conocidos de BT.709 en rango limitado (los de las barras de color de SMPTE RP
    /// 219 a 8 bits): rojo (63, 102, 240), verde (173, 42, 26), azul (32, 240, 118).
    #[test]
    fn flat_colours_convert_to_bt709_limited_range() {
        let cases = [
            ([255, 255, 255], [235, 128, 128]),
            ([0, 0, 0], [16, 128, 128]),
            ([255, 0, 0], [63, 102, 240]),
            ([0, 255, 0], [173, 42, 26]),
            ([0, 0, 255], [32, 240, 118]),
            ([200, 100, 50], [0, 0, 0]),
        ];
        for (rgb, expected) in cases {
            // Ancho impar y filas con relleno: también se prueban el redondeo de croma y el stride.
            let frame = flat_bgra(37, 21, 37 * 4 + 12, rgb);
            let i420 = bgra_to_i420(&frame).unwrap();
            assert_eq!(i420.format, PixelFormat::I420);
            assert_eq!(i420.stride, 37);
            assert_eq!(i420.data.len(), RawFrame::i420_len(37, 21));
            let (y, u, v) = i420.i420_planes().unwrap();
            let [ry, ru, rv] = reference(rgb);
            for (plane, reference, name) in [(y, ry, "Y"), (u, ru, "U"), (v, rv, "V")] {
                for &value in plane {
                    assert!(
                        (f32::from(value) - reference).abs() <= 1.0,
                        "{name} de {rgb:?}: {value}, se esperaba {reference:.1}"
                    );
                }
            }
            if expected != [0, 0, 0] {
                // `yuv` redondea en coma fija: ±1 respecto a la tabla.
                for (got, want) in [y[0], u[0], v[0]].into_iter().zip(expected) {
                    assert!(got.abs_diff(want) <= 1, "{rgb:?}: {got} frente a {want}");
                }
            }
        }
    }

    #[test]
    fn nv12_matches_i420() {
        let frame = flat_bgra(38, 22, 38 * 4, [200, 100, 50]);
        let i420 = bgra_to_i420(&frame).unwrap();
        let (cw, ch) = RawFrame::chroma_size(38, 22);
        let mut y = vec![0u8; 38 * 22];
        let mut uv = vec![0u8; (2 * cw * ch) as usize];
        bgra_to_nv12_planes(&frame, [(&mut y, 38), (&mut uv, 2 * cw)]).unwrap();
        let (yi, ui, vi) = i420.i420_planes().unwrap();
        assert_eq!(y.as_slice(), yi);
        for (x, pair) in uv.as_chunks::<2>().0.iter().enumerate() {
            assert!((i16::from(pair[0]) - i16::from(ui[x])).abs() <= 1);
            assert!((i16::from(pair[1]) - i16::from(vi[x])).abs() <= 1);
        }
        let mut y2 = vec![0u8; 38 * 22];
        let mut uv2 = vec![0u8; (2 * cw * ch) as usize];
        i420_to_nv12_planes(&i420, [(&mut y2, 38), (&mut uv2, 2 * cw)]).unwrap();
        assert_eq!(y2, y);
        assert_eq!(uv2[0], ui[0]);
        assert_eq!(uv2[1], vi[0]);
    }

    #[test]
    fn i444_and_nv24_keep_full_resolution_chroma() {
        // Media pantalla roja y media azul: con 4:4:4 la frontera no mezcla el croma.
        let (w, h) = (6u32, 4u32);
        let mut frame = flat_bgra(w, h, w * 4, [255, 0, 0]);
        for y in 0..h as usize {
            for x in 3..w as usize {
                let at = y * (w as usize * 4) + x * 4;
                frame.data[at..at + 4].copy_from_slice(&[255, 0, 0, 255]);
            }
        }
        let i444 = bgra_to_i444(&frame).unwrap();
        assert_eq!(i444.data.len(), RawFrame::i444_len(w, h));
        let (y, u, v) = i444.i444_planes().unwrap();
        let near =
            |got: [u8; 3], want: [u8; 3]| got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 1);
        assert!(near([y[2], u[2], v[2]], [63, 102, 240]), "rojo");
        assert!(
            near([y[3], u[3], v[3]], [32, 240, 118]),
            "azul, sin mezclar"
        );
        let mut ny = vec![0u8; (w * h) as usize];
        let mut nuv = vec![0u8; (2 * w * h) as usize];
        bgra_to_nv24_planes(&frame, [(&mut ny, w), (&mut nuv, 2 * w)]).unwrap();
        assert_eq!(ny.as_slice(), y);
        assert_eq!([nuv[6], nuv[7]], [u[3], v[3]]);
    }

    fn flat_rgba16f(width: u32, height: u32, [r, g, b]: [f32; 3]) -> RawFrame {
        use crate::color::f32_to_f16;
        let mut data = Vec::new();
        for _ in 0..width * height {
            for c in [r, g, b, 1.0] {
                data.extend_from_slice(&f32_to_f16(c).to_le_bytes());
            }
        }
        RawFrame::cpu(1, (width, height), PixelFormat::Rgba16F, width * 8, data)
    }

    fn p010_word(plane: &[u8], index: usize) -> u16 {
        u16::from_le_bytes([plane[2 * index], plane[2 * index + 1]]) >> 6
    }

    /// Valores conocidos de HDR10: el blanco de referencia de BT.2408 (203 nits, PQ 0,5807) es el
    /// código 573 con croma neutro, 1000 nits (PQ 0,7518) es 723 y el negro, 64.
    #[test]
    fn pq_table_follows_the_curve() {
        let table = PqTable::get();
        let mut worst = 0.0f32;
        let mut nits = 1e-4f32;
        while nits <= 10_000.0 {
            let exact = pq_from_nits(nits);
            worst = worst.max((table.encode(nits) - exact).abs());
            nits *= 1.003;
        }
        // Una décima de código de 10 bits como mucho.
        assert!(worst * 876.0 < 0.1, "error máximo {worst}");
        assert_eq!(table.encode(-1.0), table.encode(0.0));
        assert_eq!(table.encode(f32::NAN), table.encode(0.0));
        assert!((table.encode(20_000.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn hdr10_conversion_hits_known_codes() {
        let white = to_p010(&flat_bgra(4, 2, 16, [255, 255, 255])).unwrap();
        assert_eq!(white.format, PixelFormat::P010);
        assert_eq!(white.color, ColorSpace::HDR10);
        let (y, uv) = white.p010_planes().unwrap();
        assert!(
            (i32::from(p010_word(y, 0)) - 573).abs() <= 1,
            "{}",
            p010_word(y, 0)
        );
        assert_eq!((p010_word(uv, 0), p010_word(uv, 1)), (512, 512));
        let black = to_p010(&flat_bgra(4, 2, 16, [0, 0, 0])).unwrap();
        assert_eq!(p010_word(black.p010_planes().unwrap().0, 0), 64);
        // scRGB: 12,5 son 1000 nits.
        let bright = to_p010(&flat_rgba16f(4, 2, [12.5, 12.5, 12.5])).unwrap();
        let code = p010_word(bright.p010_planes().unwrap().0, 3);
        assert!((i32::from(code) - 723).abs() <= 1, "{code}");
        // Rojo BT.709 puro dentro de BT.2020: Cr por encima del centro, Cb por debajo.
        let red = to_p010(&flat_bgra(2, 2, 8, [255, 0, 0])).unwrap();
        let (_, uv) = red.p010_planes().unwrap();
        assert!(p010_word(uv, 0) < 512 && p010_word(uv, 1) > 600);
    }

    #[test]
    fn hdr10_planar_matches_p010_and_converts_back() {
        let frame = flat_bgra(6, 4, 24, [200, 100, 50]);
        let p010 = to_p010(&frame).unwrap();
        let (cw, ch) = RawFrame::chroma_size(6, 4);
        let (mut y, mut u, mut v) = (
            vec![0u8; 2 * 6 * 4],
            vec![0u8; (2 * cw * ch) as usize],
            vec![0u8; (2 * cw * ch) as usize],
        );
        to_hdr10_planes(
            &frame,
            TenBitPlanes::Planar {
                y: (&mut y, 12),
                u: (&mut u, 2 * cw),
                v: (&mut v, 2 * cw),
            },
        )
        .unwrap();
        let back = planar10_to_p010(
            (6, 4),
            [(&y, 12), (&u, 2 * cw as usize), (&v, 2 * cw as usize)],
        )
        .unwrap();
        assert_eq!(back, p010.data);
    }

    #[test]
    fn hdr_capture_tone_maps_to_sdr() {
        // El blanco de referencia (203 nits = 2,5375 en scRGB) queda casi blanco; 1000 nits no
        // satura más que eso, y un gris medio SDR conserva su valor.
        let white = rgba16f_to_bgra(&flat_rgba16f(2, 2, [2.5375, 2.5375, 2.5375])).unwrap();
        assert!(white.data[0] >= 245, "{}", white.data[0]);
        let bright = rgba16f_to_bgra(&flat_rgba16f(2, 2, [12.5, 12.5, 12.5])).unwrap();
        assert!(bright.data[0] >= white.data[0]);
        let grey = 0.214 * 2.5375; // sRGB 0,5 en el blanco de referencia.
        let mid = rgba16f_to_bgra(&flat_rgba16f(2, 2, [grey, grey, grey])).unwrap();
        assert!((i32::from(mid.data[1]) - 128).abs() <= 2, "{}", mid.data[1]);
        assert_eq!(mid.format, PixelFormat::Bgra);
    }

    #[test]
    fn wrong_inputs_are_rejected() {
        let mut frame = flat_bgra(4, 4, 16, [1, 2, 3]);
        frame.data.truncate(10);
        assert!(bgra_to_i420(&frame).is_err());
        let mut frame = flat_bgra(4, 4, 16, [1, 2, 3]);
        frame.format = PixelFormat::I420;
        assert!(bgra_to_i420(&frame).is_err());
    }
}
