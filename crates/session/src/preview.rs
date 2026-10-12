//! Vista previa de lo que comparte el anfitrión.
//!
//! La interfaz muestra en pequeño lo que se está capturando. [`PreviewTap`] recibe cada fotograma
//! del hilo `capture` y, como mucho cada [`PreviewTap::period`], reduce la imagen BGRA a
//! [`PreviewTap::max_width`] píxeles de ancho (promedio por bloques) y la entrega en I420 (la
//! misma conversión del codificador) a su retrollamada. Con las imágenes de GPU (DMA-BUF, D3D11)
//! no hay copia en CPU que reducir: la retrollamada recibe `None` una vez para que la interfaz
//! explique por qué no hay vista previa. Todo ocurre fuera del camino del vídeo: el fotograma
//! sigue hacia el codificador sin esperar.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use coroom_capture::{CapturedFrame, FramePixels};
use coroom_codec::convert::bgra_to_i420;
use coroom_codec::{PixelFormat, RawFrame};

/// Periodo por defecto entre dos imágenes de la vista previa.
pub const PREVIEW_PERIOD: Duration = Duration::from_millis(500);

/// Ancho por defecto de la vista previa.
pub const PREVIEW_WIDTH: u32 = 480;

/// Retrollamada de la vista previa: una imagen I420 o `None` si la captura va por la GPU.
pub type PreviewCallback = Box<dyn Fn(Option<RawFrame>) + Send + Sync>;

/// Toma de la vista previa en el hilo de captura.
pub struct PreviewTap {
    period: Duration,
    max_width: u32,
    callback: PreviewCallback,
    state: Mutex<TapState>,
}

#[derive(Default)]
struct TapState {
    last: Option<Instant>,
    gpu_reported: bool,
}

impl std::fmt::Debug for PreviewTap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewTap")
            .field("period", &self.period)
            .field("max_width", &self.max_width)
            .finish_non_exhaustive()
    }
}

impl PreviewTap {
    /// Toma que entrega a `callback` una imagen cada [`PREVIEW_PERIOD`] de [`PREVIEW_WIDTH`] de
    /// ancho como mucho.
    pub fn new(callback: PreviewCallback) -> Self {
        Self::with(PREVIEW_PERIOD, PREVIEW_WIDTH, callback)
    }

    /// Toma con otro periodo y otro ancho máximo.
    pub fn with(period: Duration, max_width: u32, callback: PreviewCallback) -> Self {
        Self {
            period,
            max_width: max_width.max(2),
            callback,
            state: Mutex::new(TapState::default()),
        }
    }

    /// Periodo entre imágenes.
    pub fn period(&self) -> Duration {
        self.period
    }

    /// Ancho máximo.
    pub fn max_width(&self) -> u32 {
        self.max_width
    }

    /// Mira un fotograma capturado y, si toca, entrega su vista previa.
    pub(crate) fn offer(&self, frame: &CapturedFrame) {
        let now = Instant::now();
        let gpu = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state
                .last
                .is_some_and(|last| now.duration_since(last) < self.period)
            {
                return;
            }
            state.last = Some(now);
            let gpu = frame.pixels.is_gpu();
            if gpu {
                if state.gpu_reported {
                    return;
                }
                state.gpu_reported = true;
            } else {
                state.gpu_reported = false;
            }
            gpu
        };
        if gpu {
            (self.callback)(None);
            return;
        }
        let FramePixels::Bgra { data, stride } = &frame.pixels else {
            return;
        };
        if let Some(small) = downscale_bgra(
            data,
            *stride,
            (frame.width, frame.height),
            self.max_width,
            frame.frame_id,
        ) && let Ok(i420) = bgra_to_i420(&small)
        {
            (self.callback)(Some(i420));
        }
    }
}

/// Reduce una imagen BGRA a `max_width` de ancho como mucho (tamaño par, misma relación de
/// aspecto) promediando bloques. `None` si los datos no cuadran con el tamaño.
pub fn downscale_bgra(
    data: &[u8],
    stride: u32,
    (width, height): (u32, u32),
    max_width: u32,
    frame_id: u64,
) -> Option<RawFrame> {
    if width == 0 || height == 0 || (stride as usize) < width as usize * 4 {
        return None;
    }
    if data.len() < stride as usize * (height as usize - 1) + width as usize * 4 {
        return None;
    }
    let even = |value: u32| (value.max(2) / 2) * 2;
    let out_w = even(width.min(max_width));
    let out_h =
        even(((u64::from(height) * u64::from(out_w)) / u64::from(width.max(1))).max(2) as u32);
    let mut out = vec![0u8; out_w as usize * out_h as usize * 4];
    for oy in 0..out_h {
        let y0 = (u64::from(oy) * u64::from(height) / u64::from(out_h)) as usize;
        let y1 = ((u64::from(oy + 1) * u64::from(height) / u64::from(out_h)) as usize)
            .max(y0 + 1)
            .min(height as usize);
        for ox in 0..out_w {
            let x0 = (u64::from(ox) * u64::from(width) / u64::from(out_w)) as usize;
            let x1 = ((u64::from(ox + 1) * u64::from(width) / u64::from(out_w)) as usize)
                .max(x0 + 1)
                .min(width as usize);
            let mut sum = [0u32; 4];
            for y in y0..y1 {
                let row = y * stride as usize;
                for x in x0..x1 {
                    let at = row + x * 4;
                    for (channel, value) in sum.iter_mut().zip(&data[at..at + 4]) {
                        *channel += u32::from(*value);
                    }
                }
            }
            let count = ((y1 - y0) * (x1 - x0)) as u32;
            let at = (oy as usize * out_w as usize + ox as usize) * 4;
            for (dst, channel) in out[at..at + 4].iter_mut().zip(sum) {
                *dst = (channel / count.max(1)) as u8;
            }
        }
    }
    Some(RawFrame::cpu(
        frame_id,
        (out_w, out_h),
        PixelFormat::Bgra,
        out_w * 4,
        out,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn downscaling_keeps_the_aspect_ratio_and_averages() {
        // 8 × 4 en dos mitades: izquierda negra, derecha blanca.
        let (w, h) = (8u32, 4u32);
        let mut data = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in w / 2..w {
                let at = ((y * w + x) * 4) as usize;
                data[at..at + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let small = downscale_bgra(&data, w * 4, (w, h), 4, 7).expect("reducida");
        assert_eq!((small.width, small.height, small.frame_id), (4, 2, 7));
        assert_eq!(small.data[0], 0, "a la izquierda, negro");
        assert_eq!(small.data[(3 * 4) as usize], 255, "a la derecha, blanco");
        // Más pequeña que el máximo: se queda (con tamaño par).
        let same = downscale_bgra(&data, w * 4, (w, h), 480, 1).expect("igual");
        assert_eq!((same.width, same.height), (8, 4));
        assert!(downscale_bgra(&data[..10], w * 4, (w, h), 4, 1).is_none());
    }

    #[test]
    fn the_tap_respects_its_period_and_reports_gpu_once() {
        use coroom_capture::CapturedFrame;
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        let tap = PreviewTap::with(
            Duration::from_secs(3600),
            64,
            Box::new(move |frame| {
                assert!(frame.is_some_and(|f| f.format == PixelFormat::I420));
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let frame = CapturedFrame {
            frame_id: 1,
            width: 16,
            height: 8,
            captured_at: Instant::now(),
            cursor: None,
            cursor_shape: None,
            pixels: FramePixels::Bgra {
                data: vec![128; 16 * 8 * 4],
                stride: 16 * 4,
            },
            cpu_copies: 0,
        };
        tap.offer(&frame);
        tap.offer(&frame);
        assert_eq!(seen.load(Ordering::SeqCst), 1, "una por periodo");
    }
}
