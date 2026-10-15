//! Reducción en CPU con libswscale ([`Scaler`]): cuando la captura trae un fotograma más grande que
//! el tope de resolución ([`crate::EncoderConfig`]), se reduce a lo que cabe
//! ([`coroom_proto::video::fit`]) **en el mismo paso** que la conversión de formato (BGRA → NV12,
//! I420, I444 o NV24; I420 o NV12 → NV12…), así que sustituye a la conversión propia de
//! [`crate::convert`] en vez de sumarse a ella. Solo en HDR desde RGB (BGRA o RGBA de coma flotante →
//! BT.2020 PQ de 10 bits) la conversión sigue siendo la propia, al tamaño de la captura, y la
//! reducción va después, P010 → P010 (o tres planos de 10 bits), en un segundo paso.
//!
//! Se usa la API por fotogramas de libswscale (`sws_scale_frame` sobre un contexto de
//! `sws_alloc_context`, FFmpeg ≥ 7.1): el contexto se reconfigura solo cuando cambian el tamaño o
//! el formato de la entrada o la salida, y la matriz y el rango salen de las etiquetas de color de
//! cada fotograma (RGB de rango completo → BT.709 en rango limitado en SDR, igual que
//! [`crate::convert`] y que lo que señala el VUI del codificador). Cada reducción se cronometra
//! ([`Scaler::last`], [`crate::Encoder::cpu_scale`]) para que el coste se vea en el registro.

use std::os::raw::{c_int, c_uint};
use std::ptr::NonNull;
use std::time::Instant;

use rsmpeg::avutil::AVFrame;
use rsmpeg::ffi;

use crate::{CodecError, ColorSpace, CpuScale, PixelFormat, RawFrame};

/// Filtro de la reducción: «área» (la media de los píxeles de origen que caen en cada uno de
/// destino). Al reducir es el que menos coeficientes gasta en libswscale (dos por eje a 2:1) y
/// no deja *aliasing* en el texto y las líneas finas de una pantalla; el bilineal usa el doble de
/// coeficientes al reducir y suaviza de forma menos regular. Con redondeo exacto.
const FLAGS: ffi::SwsFlags = ffi::SWS_AREA | ffi::SWS_ACCURATE_RND;

/// Hilos de libswscale como máximo (reparte las filas; más no ayuda con estos tamaños).
const MAX_THREADS: usize = 8;

/// Contexto de libswscale con una vista sin copia del fotograma de entrada.
pub(crate) struct Scaler {
    ctx: NonNull<ffi::SwsContext>,
    /// Vista de los planos del `RawFrame` de entrada: punteros prestados solo durante la llamada.
    src: AVFrame,
    /// La última reducción, con su coste.
    last: Option<CpuScale>,
}

// SAFETY: el contexto y la vista solo se tocan a través de `&mut self`, así que nunca los usan dos
// hilos a la vez; libswscale no ata un contexto a un hilo.
unsafe impl Send for Scaler {}

impl std::fmt::Debug for Scaler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scaler")
            .field("last", &self.last)
            .finish_non_exhaustive()
    }
}

impl Scaler {
    /// Crea el contexto (se configura solo con el primer fotograma).
    pub(crate) fn new() -> Result<Self, CodecError> {
        // SAFETY: `sws_alloc_context` devuelve un contexto nuevo (o nulo), que pasa a ser nuestro.
        let raw = unsafe { ffi::sws_alloc_context() };
        let ctx = NonNull::new(raw).ok_or_else(|| {
            CodecError::Backend("sin memoria para el contexto de reducción".into())
        })?;
        let threads = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(MAX_THREADS);
        // SAFETY: `ctx` es un contexto recién creado y nuestro; `flags` y `threads` son campos
        // públicos de `SwsContext` que se leen al configurarlo con el primer fotograma.
        unsafe {
            (*ctx.as_ptr()).flags = FLAGS as c_uint;
            (*ctx.as_ptr()).threads = c_int::try_from(threads).unwrap_or(1);
        }
        Ok(Self {
            ctx,
            src: AVFrame::new(),
            last: None,
        })
    }

    /// Reduce `frame` (en memoria de CPU) a `dst`, un `AVFrame` de CPU ya reservado y escribible,
    /// con el formato, el tamaño y las etiquetas de color de la salida ([`tag_color`]); la
    /// conversión de formato va en el mismo paso.
    pub(crate) fn scale(&mut self, frame: &RawFrame, dst: &mut AVFrame) -> Result<(), CodecError> {
        view(&mut self.src, frame)?;
        let result = self.run(dst, (frame.width, frame.height));
        // Los punteros prestados no viven más que esta llamada.
        clear_view(&mut self.src);
        result
    }

    /// Reduce `src`, un `AVFrame` de CPU ya convertido, a `dst` (como [`Scaler::scale`]).
    pub(crate) fn scale_frame(
        &mut self,
        src: &AVFrame,
        dst: &mut AVFrame,
    ) -> Result<(), CodecError> {
        let from = (
            u32::try_from(src.width).unwrap_or(0),
            u32::try_from(src.height).unwrap_or(0),
        );
        let started = Instant::now();
        // SAFETY: ver `run`; `src` es un fotograma reservado por `alloc_buffer`, que vive lo que el
        // préstamo.
        let ret =
            unsafe { ffi::sws_scale_frame(self.ctx.as_ptr(), dst.as_mut_ptr(), src.as_ptr()) };
        self.finish(ret, started, from, dst)
    }

    fn run(&mut self, dst: &mut AVFrame, from: (u32, u32)) -> Result<(), CodecError> {
        let to = (dst.width, dst.height);
        debug_assert!(
            (from.0, from.1) != (to.0 as u32, to.1 as u32) || dst.format != self.src.format,
            "la reducción no se llama sin nada que cambiar (sws devolvería referencias a la entrada)"
        );
        let started = Instant::now();
        // SAFETY: `ctx` es nuestro contexto; `dst` es un fotograma de CPU reservado y escribible
        // con el formato y el tamaño de salida, y `src` describe planos válidos y del tamaño que
        // dice mientras dura la llamada: la vista se monta en `scale` justo antes, con el préstamo
        // del `RawFrame` vivo, y se deshace justo después. libswscale solo lee `src`.
        let ret =
            unsafe { ffi::sws_scale_frame(self.ctx.as_ptr(), dst.as_mut_ptr(), self.src.as_ptr()) };
        self.finish(ret, started, from, dst)
    }

    fn finish(
        &mut self,
        ret: c_int,
        started: Instant,
        from: (u32, u32),
        dst: &AVFrame,
    ) -> Result<(), CodecError> {
        if ret < 0 {
            return Err(CodecError::Backend(format!(
                "reducción en CPU (libswscale): {}",
                super::describe_code(ret)
            )));
        }
        self.last = Some(CpuScale {
            from,
            to: (
                u32::try_from(dst.width).unwrap_or(0),
                u32::try_from(dst.height).unwrap_or(0),
            ),
            took: started.elapsed(),
        });
        Ok(())
    }

    /// La última reducción, con su coste.
    pub(crate) fn last(&self) -> Option<CpuScale> {
        self.last
    }

    /// Olvida la última reducción (llegó un fotograma que no la necesitó).
    pub(crate) fn clear_last(&mut self) {
        self.last = None;
    }
}

/// Planos I420 de 8 bits reducidos, prestados del `AVFrame` de destino de un [`I420Reducer`].
pub(crate) struct I420Planes<'a> {
    pub(crate) y: &'a [u8],
    pub(crate) u: &'a [u8],
    pub(crate) v: &'a [u8],
    /// Bytes por fila del plano Y.
    pub(crate) y_stride: usize,
    /// Bytes por fila de los planos U y V (iguales en un `AVFrame` `yuv420p`).
    pub(crate) chroma_stride: usize,
    /// La reducción que los produjo, con su coste.
    pub(crate) scale: CpuScale,
}

/// Reducción a I420 de 8 bits (BT.709 en rango limitado) para los codificadores que reciben
/// planos de CPU sin FFmpeg (OpenH264): guarda el `AVFrame` de destino, que se reserva de nuevo
/// solo al cambiar el tamaño.
pub(crate) struct I420Reducer {
    scaler: Scaler,
    dst: Option<AVFrame>,
}

impl I420Reducer {
    pub(crate) fn new() -> Result<Self, CodecError> {
        Ok(Self {
            scaler: Scaler::new()?,
            dst: None,
        })
    }

    /// Reduce `frame` (BGRA o I420) a `size` en I420 y presta sus planos.
    pub(crate) fn reduce(
        &mut self,
        frame: &RawFrame,
        (width, height): (u32, u32),
    ) -> Result<I420Planes<'_>, CodecError> {
        let size = (
            i32::try_from(width).unwrap_or(i32::MAX),
            i32::try_from(height).unwrap_or(i32::MAX),
        );
        if self
            .dst
            .as_ref()
            .is_none_or(|dst| (dst.width, dst.height) != size)
        {
            let mut dst = AVFrame::new();
            dst.set_format(ffi::AV_PIX_FMT_YUV420P);
            dst.set_width(size.0);
            dst.set_height(size.1);
            dst.alloc_buffer()
                .map_err(|err| super::backend_error("búfer de la reducción", &err))?;
            tag_color(&mut dst, false, false);
            self.dst = Some(dst);
        }
        let Some(dst) = self.dst.as_mut() else {
            return Err(CodecError::Backend("sin búfer de la reducción".into()));
        };
        dst.make_writable()
            .map_err(|err| super::backend_error("fotograma escribible", &err))?;
        self.scaler.scale(frame, dst)?;
        let scale = self
            .scaler
            .last()
            .ok_or_else(|| CodecError::Backend("la reducción no midió nada".into()))?;
        let (h, ch) = (height as usize, height.div_ceil(2) as usize);
        let planes = planes(dst, [h, ch, ch]);
        let [(y, y_stride), (u, chroma_stride), (v, _)] = planes.as_slice() else {
            return Err(CodecError::Backend(
                "el fotograma reducido no tiene tres planos".into(),
            ));
        };
        Ok(I420Planes {
            y,
            u,
            v,
            y_stride: *y_stride,
            chroma_stride: *chroma_stride,
            scale,
        })
    }
}

/// Planos de lectura de un `AVFrame` de CPU reservado: `(plano, bytes por fila)`, con `rows`
/// filas cada uno.
fn planes(frame: &AVFrame, rows: [usize; 3]) -> Vec<(&[u8], usize)> {
    let mut out = Vec::with_capacity(3);
    for (plane, rows) in rows.into_iter().enumerate() {
        let (ptr, stride) = (frame.data[plane], frame.linesize[plane]);
        if ptr.is_null() || stride <= 0 || rows == 0 {
            break;
        }
        let stride = stride as usize;
        // SAFETY: `av_frame_get_buffer` reservó para el plano `plane` al menos `linesize × filas`
        // bytes (las filas del plano para el formato y el alto del fotograma); el préstamo queda
        // atado al `&AVFrame`, que nadie escribe mientras viva.
        let slice = unsafe { std::slice::from_raw_parts(ptr, stride * rows) };
        out.push((slice, stride));
    }
    out
}

impl Drop for Scaler {
    fn drop(&mut self) {
        let mut ctx = self.ctx.as_ptr();
        // SAFETY: el contexto es nuestro y no se vuelve a usar; `sws_free_context` lo libera y deja
        // el puntero a nulo.
        unsafe { ffi::sws_free_context(&mut ctx) };
    }
}

/// Etiqueta `frame` con el espacio de color de sus píxeles, lo que libswscale usa para elegir la
/// matriz y el rango: RGB de rango completo (`rgb`), o Y′CbCr en rango limitado con BT.709 (SDR) o
/// BT.2020 sin luminancia constante y PQ (`hdr`). Las primarias y la curva van iguales en entrada y
/// salida (BT.709, o BT.2020 con PQ en HDR) para que libswscale no inserte una conversión de
/// color aparte: la de HDR la hace [`crate::convert`] antes de reducir.
pub(crate) fn tag_color(frame: &mut AVFrame, hdr: bool, rgb: bool) {
    // SAFETY: `frame` es un fotograma propio; solo se escriben enumeraciones de color.
    unsafe {
        let raw = frame.as_mut_ptr();
        (*raw).colorspace = if rgb {
            ffi::AVCOL_SPC_RGB
        } else if hdr {
            ffi::AVCOL_SPC_BT2020_NCL
        } else {
            ffi::AVCOL_SPC_BT709
        };
        (*raw).color_range = if rgb {
            ffi::AVCOL_RANGE_JPEG
        } else {
            ffi::AVCOL_RANGE_MPEG
        };
        (*raw).color_primaries = if hdr {
            ffi::AVCOL_PRI_BT2020
        } else {
            ffi::AVCOL_PRI_BT709
        };
        (*raw).color_trc = if hdr {
            ffi::AVCOL_TRC_SMPTE2084
        } else {
            ffi::AVCOL_TRC_BT709
        };
    }
}

/// Monta en `view` una vista sin copia de los planos de `frame` (formato, tamaño, punteros, filas
/// y color). Los punteros son préstamos de `frame`: [`clear_view`] los quita después.
fn view(view: &mut AVFrame, frame: &RawFrame) -> Result<(), CodecError> {
    let short = || CodecError::Unsupported("búfer del fotograma más corto que su tamaño".into());
    let (w, h) = (frame.width as usize, frame.height as usize);
    let cw = RawFrame::chroma_size(frame.width, frame.height).0 as usize;
    let stride = frame.stride as usize;
    let null: (*const u8, usize) = (std::ptr::null(), 0);
    let (format, planes, rgb) = match frame.format {
        PixelFormat::Bgra => {
            if stride < 4 * w || frame.data.len() < stride * h.saturating_sub(1) + 4 * w {
                return Err(short());
            }
            (
                ffi::AV_PIX_FMT_BGRA,
                [(frame.data.as_ptr(), stride), null, null],
                true,
            )
        }
        PixelFormat::I420 => {
            let (y, u, v) = frame.i420_planes().ok_or_else(short)?;
            (
                ffi::AV_PIX_FMT_YUV420P,
                [(y.as_ptr(), stride), (u.as_ptr(), cw), (v.as_ptr(), cw)],
                false,
            )
        }
        PixelFormat::Nv12 => {
            let (y, uv) = frame.nv12_planes().ok_or_else(short)?;
            (
                ffi::AV_PIX_FMT_NV12,
                [(y.as_ptr(), stride), (uv.as_ptr(), 2 * cw), null],
                false,
            )
        }
        PixelFormat::I444 => {
            let (y, u, v) = frame.i444_planes().ok_or_else(short)?;
            (
                ffi::AV_PIX_FMT_YUV444P,
                [(y.as_ptr(), stride), (u.as_ptr(), w), (v.as_ptr(), w)],
                false,
            )
        }
        PixelFormat::Nv24 => {
            let (y, uv) = frame.nv24_planes().ok_or_else(short)?;
            (
                ffi::AV_PIX_FMT_NV24,
                [(y.as_ptr(), stride), (uv.as_ptr(), 2 * w), null],
                false,
            )
        }
        PixelFormat::P010 => {
            let (y, uv) = frame.p010_planes().ok_or_else(short)?;
            (
                ffi::AV_PIX_FMT_P010LE,
                [(y.as_ptr(), stride), (uv.as_ptr(), 4 * cw), null],
                false,
            )
        }
        PixelFormat::Rgba16F | PixelFormat::Gpu => {
            return Err(CodecError::Unsupported(format!(
                "la reducción en CPU no lee {:?} (la conversión HDR va antes)",
                frame.format
            )));
        }
    };
    view.set_format(format);
    view.set_width(i32::try_from(frame.width).unwrap_or(i32::MAX));
    view.set_height(i32::try_from(frame.height).unwrap_or(i32::MAX));
    let data = view.data_mut();
    for (slot, (ptr, _)) in data.iter_mut().zip(planes) {
        // Solo lectura: `sws_scale_frame` toma la entrada como `const AVFrame *`.
        *slot = ptr.cast_mut();
    }
    let linesize = view.linesize_mut();
    for (slot, (_, stride)) in linesize.iter_mut().zip(planes) {
        *slot = i32::try_from(stride).unwrap_or(i32::MAX);
    }
    tag_color(view, frame.color == ColorSpace::HDR10, rgb);
    Ok(())
}

/// Quita de `view` los punteros prestados por [`view`].
fn clear_view(view: &mut AVFrame) {
    for slot in view.data_mut().iter_mut() {
        *slot = std::ptr::null_mut();
    }
    for slot in view.linesize_mut().iter_mut() {
        *slot = 0;
    }
    view.set_width(0);
    view.set_height(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::{bgra_to_i420, bgra_to_nv12_planes};

    /// Fotograma BGRA liso de `color` (B, G, R).
    fn solid(width: u32, height: u32, [b, g, r]: [u8; 3]) -> RawFrame {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            data.extend_from_slice(&[b, g, r, 255]);
        }
        RawFrame::cpu(1, (width, height), PixelFormat::Bgra, width * 4, data)
    }

    fn cpu_frame(format: ffi::AVPixelFormat, (width, height): (u32, u32), hdr: bool) -> AVFrame {
        let mut frame = AVFrame::new();
        frame.set_format(format);
        frame.set_width(width as i32);
        frame.set_height(height as i32);
        frame.alloc_buffer().unwrap();
        tag_color(&mut frame, hdr, false);
        frame
    }

    fn plane(frame: &AVFrame, index: usize, rows: usize) -> Vec<u8> {
        let stride = frame.linesize[index] as usize;
        // SAFETY: plano reservado por `alloc_buffer` con `linesize × filas` bytes.
        unsafe { std::slice::from_raw_parts(frame.data[index], stride * rows) }.to_vec()
    }

    /// Un color liso reducido a la mitad y convertido a NV12 da los mismos Y, U y V que la
    /// conversión propia (BT.709 en rango limitado): la matriz y el rango son los del flujo.
    #[test]
    fn bgra_is_reduced_and_converted_with_bt709_limited_range() {
        for color in [[200u8, 30, 60], [16, 240, 100], [255, 255, 255], [0, 0, 0]] {
            let source = solid(64, 32, color);
            let mut expected_y = vec![0u8; 32 * 16];
            let mut expected_uv = vec![0u8; 32 * 8];
            bgra_to_nv12_planes(
                &solid(32, 16, color),
                [(&mut expected_y, 32), (&mut expected_uv, 32)],
            )
            .unwrap();
            let mut scaler = Scaler::new().unwrap();
            let mut dst = cpu_frame(ffi::AV_PIX_FMT_NV12, (32, 16), false);
            scaler.scale(&source, &mut dst).unwrap();
            let y = plane(&dst, 0, 16);
            let uv = plane(&dst, 1, 8);
            let stride = dst.linesize[0] as usize;
            for row in 0..16 {
                for x in 0..32 {
                    let got = y[row * stride + x];
                    let want = expected_y[row * 32 + x];
                    assert!(
                        got.abs_diff(want) <= 2,
                        "{color:?}: Y {got} frente a {want}"
                    );
                }
            }
            let stride = dst.linesize[1] as usize;
            for row in 0..8 {
                for x in 0..32 {
                    let got = uv[row * stride + x];
                    let want = expected_uv[row * 32 + x];
                    assert!(
                        got.abs_diff(want) <= 2,
                        "{color:?}: UV {got} frente a {want}"
                    );
                }
            }
            let last = scaler.last().expect("coste medido");
            assert_eq!((last.from, last.to), ((64, 32), (32, 16)));
            assert!(scaler.src.data[0].is_null(), "la vista no retiene punteros");
        }
    }

    /// Un degradado horizontal se reduce con «área»: cada píxel de salida es la media de los dos
    /// de entrada; I420 → I420 (el formato de la captura del respaldo y de los decodificadores).
    #[test]
    fn i420_is_reduced_by_averaging() {
        let (w, h) = (64u32, 8u32);
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..h {
            for x in 0..w {
                let v = (x * 4) as u8;
                data.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let source =
            bgra_to_i420(&RawFrame::cpu(1, (w, h), PixelFormat::Bgra, w * 4, data)).unwrap();
        let mut scaler = Scaler::new().unwrap();
        let mut dst = cpu_frame(ffi::AV_PIX_FMT_YUV420P, (32, 4), false);
        scaler.scale(&source, &mut dst).unwrap();
        let (sy, _, _) = source.i420_planes().unwrap();
        let y = plane(&dst, 0, 4);
        let stride = dst.linesize[0] as usize;
        for x in 0..32usize {
            let want = (u16::from(sy[2 * x]) + u16::from(sy[2 * x + 1])).div_ceil(2) as u8;
            let got = y[x];
            assert!(got.abs_diff(want) <= 2, "x={x}: {got} frente a {want}");
            assert_eq!(y[2 * stride + x], got, "todas las filas iguales");
        }
        // El contexto se reconfigura solo al cambiar el tamaño de salida.
        let mut smaller = cpu_frame(ffi::AV_PIX_FMT_YUV420P, (16, 2), false);
        scaler.scale(&source, &mut smaller).unwrap();
        assert_eq!(scaler.last().map(|s| s.to), Some((16, 2)));
    }

    /// P010 → P010 (la reducción HDR, tras la conversión propia) conserva los 10 bits.
    #[test]
    fn p010_is_reduced_keeping_ten_bits() {
        let (w, h) = (16u32, 8u32);
        let mut data = Vec::new();
        let word = |v: u16| (v << 6).to_le_bytes();
        for _ in 0..h {
            for _ in 0..w {
                data.extend_from_slice(&word(900));
            }
        }
        for _ in 0..h / 2 {
            for _ in 0..w / 2 {
                data.extend_from_slice(&word(300));
                data.extend_from_slice(&word(700));
            }
        }
        let source =
            RawFrame::cpu(1, (w, h), PixelFormat::P010, w * 2, data).with_color(ColorSpace::HDR10);
        let mut scaler = Scaler::new().unwrap();
        let mut dst = cpu_frame(ffi::AV_PIX_FMT_P010LE, (8, 4), true);
        scaler.scale(&source, &mut dst).unwrap();
        let y = plane(&dst, 0, 4);
        let uv = plane(&dst, 1, 2);
        let read = |p: &[u8], at: usize| u16::from_le_bytes([p[at], p[at + 1]]) >> 6;
        assert!((read(&y, 0) as i32 - 900).abs() <= 2, "{}", read(&y, 0));
        assert!((read(&uv, 0) as i32 - 300).abs() <= 2, "{}", read(&uv, 0));
        assert!((read(&uv, 2) as i32 - 700).abs() <= 2, "{}", read(&uv, 2));
    }

    /// La reducción a I420 para OpenH264: planos del tamaño pedido, BT.709 en rango limitado.
    #[test]
    fn the_i420_reducer_hands_out_planes_of_the_requested_size() {
        let mut reducer = I420Reducer::new().unwrap();
        let planes = reducer
            .reduce(&solid(64, 32, [200, 30, 60]), (32, 16))
            .unwrap();
        assert!(planes.y_stride >= 32 && planes.chroma_stride >= 16);
        assert_eq!(planes.y.len(), planes.y_stride * 16);
        assert_eq!(planes.u.len(), planes.chroma_stride * 8);
        let reference = bgra_to_i420(&solid(32, 16, [200, 30, 60])).unwrap();
        let (y, u, v) = reference.i420_planes().unwrap();
        assert!(planes.y[0].abs_diff(y[0]) <= 2);
        assert!(planes.u[0].abs_diff(u[0]) <= 2);
        assert!(planes.v[0].abs_diff(v[0]) <= 2);
        assert_eq!((planes.scale.from, planes.scale.to), ((64, 32), (32, 16)));
    }

    /// Coste de la reducción habitual, una pantalla 4K a 1080p en NV12 (lo que recibe un codificador
    /// de hardware por la ruta de CPU), medido en este equipo y escrito en la salida del test.
    #[test]
    fn a_4k_frame_is_reduced_to_1080p_and_the_cost_is_measured() {
        let (w, h) = (3840u32, 2160u32);
        let mut data = vec![0u8; (w * h * 4) as usize];
        for (i, px) in data.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            *px = [
                (i % 251) as u8,
                (i / 7 % 241) as u8,
                (i / 13 % 233) as u8,
                255,
            ];
        }
        let source = RawFrame::cpu(1, (w, h), PixelFormat::Bgra, w * 4, data);
        let mut scaler = Scaler::new().unwrap();
        let mut dst = cpu_frame(ffi::AV_PIX_FMT_NV12, (1920, 1080), false);
        let mut costs = Vec::new();
        for _ in 0..5 {
            scaler.scale(&source, &mut dst).unwrap();
            costs.push(scaler.last().unwrap().took.as_secs_f64() * 1000.0);
        }
        println!("reducción 3840×2160 BGRA → 1920×1080 NV12 en CPU, en ms: {costs:.2?}");
        assert_eq!(scaler.last().map(|s| s.to), Some((1920, 1080)));
        assert!(costs.iter().all(|ms| *ms < 1000.0));
    }

    #[test]
    fn gpu_and_float_frames_are_refused() {
        let mut scaler = Scaler::new().unwrap();
        let mut dst = cpu_frame(ffi::AV_PIX_FMT_NV12, (8, 8), false);
        let float = RawFrame::cpu(
            1,
            (16, 16),
            PixelFormat::Rgba16F,
            16 * 8,
            vec![0; 16 * 16 * 8],
        );
        assert!(scaler.scale(&float, &mut dst).is_err());
        let short = RawFrame::cpu(1, (16, 16), PixelFormat::Bgra, 16 * 4, vec![0; 16]);
        assert!(scaler.scale(&short, &mut dst).is_err());
        assert_eq!(scaler.last(), None);
    }
}
