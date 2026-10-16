//! Vista previa de lo que comparte el anfitrión.
//!
//! La interfaz muestra en pequeño lo que se está capturando. El hilo `encode`, una vez codificado
//! cada fotograma, se lo ofrece a [`PreviewTap`], que como mucho cada [`PreviewTap::period`] se lo
//! queda ([`PreviewTap::wants`]); entonces el fotograma (sin copiarlo) pasa al hilo de
//! [`PreviewWorker`], que reduce la imagen BGRA a [`PreviewTap::max_width`] píxeles de ancho
//! (promedio por bloques), la entrega en I420 (la misma conversión del codificador) a la
//! retrollamada y devuelve el fotograma al hilo `encode`, que lo guarda para repetirlo si hace
//! falta. Reducir una imagen de 1080p cuesta de 2 a 4 ms: fuera de los hilos `capture` y `encode`
//! no retrasa ningún fotograma. Con las imágenes de GPU (DMA-BUF, D3D11) no hay copia en CPU que
//! reducir: la retrollamada recibe `None` una vez para que la interfaz explique por qué no hay
//! vista previa.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use coroom_codec::convert::bgra_to_i420;
use coroom_codec::{PixelFormat, RawFrame};

/// Periodo por defecto entre dos imágenes de la vista previa.
pub const PREVIEW_PERIOD: Duration = Duration::from_millis(500);

/// Ancho por defecto de la vista previa.
pub const PREVIEW_WIDTH: u32 = 480;

/// Retrollamada de la vista previa: una imagen I420 o `None` si la captura va por la GPU.
pub type PreviewCallback = Box<dyn Fn(Option<RawFrame>) + Send + Sync>;

/// Toma de la vista previa: decide qué fotogramas se reducen y los entrega.
pub struct PreviewTap {
    period: Duration,
    max_width: u32,
    callback: PreviewCallback,
    state: Mutex<TapState>,
    /// Si la interfaz tiene la vista previa a la vista; si no, no se reduce nada ni se despierta.
    enabled: AtomicBool,
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
            enabled: AtomicBool::new(true),
        }
    }

    /// La interfaz dice si la vista previa está a la vista (página de Compartir, ventana sin
    /// ocultar): mientras no lo esté, [`PreviewTap::wants`] no pide ninguna imagen, así no se
    /// reduce ningún fotograma ni se despierta la interfaz para nada.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Si la vista previa está a la vista.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Periodo entre imágenes.
    pub fn period(&self) -> Duration {
        self.period
    }

    /// Ancho máximo.
    pub fn max_width(&self) -> u32 {
        self.max_width
    }

    /// Mira un fotograma ya convertido para el codificador y dice si toca sacar su vista previa:
    /// entonces hay que pasárselo a [`PreviewTap::render`] (en el [`PreviewWorker`]). Con una
    /// imagen de GPU avisa `None` una vez y devuelve `false`. Barato: va en el hilo `encode`.
    pub fn wants(&self, frame: &RawFrame) -> bool {
        if !self.is_enabled() {
            return false;
        }
        let now = Instant::now();
        let gpu = frame.gpu.is_some();
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state
                .last
                .is_some_and(|last| now.duration_since(last) < self.period)
            {
                return false;
            }
            state.last = Some(now);
            if gpu {
                if state.gpu_reported {
                    return false;
                }
                state.gpu_reported = true;
            } else {
                state.gpu_reported = false;
            }
        }
        if gpu {
            (self.callback)(None);
            return false;
        }
        frame.format == PixelFormat::Bgra
    }

    /// Reduce `frame` (BGRA de CPU) y lo entrega a la retrollamada. Cuesta unos milisegundos: se
    /// llama desde el hilo del [`PreviewWorker`].
    pub fn render(&self, frame: &RawFrame) {
        if frame.format != PixelFormat::Bgra || frame.gpu.is_some() {
            return;
        }
        if let Some(small) = downscale_bgra(
            &frame.data,
            frame.stride,
            (frame.width, frame.height),
            self.max_width,
            frame.frame_id,
        ) && let Ok(i420) = bgra_to_i420(&small)
        {
            (self.callback)(Some(i420));
        }
    }
}

/// Hilo que reduce los fotogramas de la vista previa fuera del camino del vídeo. Recibe el
/// fotograma entero (sin copiarlo) con su toma, lo entrega y lo devuelve por
/// [`PreviewWorker::returned`] para que el hilo `encode` lo guarde otra vez. El hilo se crea con
/// el primer fotograma y termina al soltar el `PreviewWorker`.
pub(crate) struct PreviewWorker {
    jobs: Option<SyncSender<(Arc<PreviewTap>, RawFrame)>>,
    done: Receiver<RawFrame>,
    done_tx: mpsc::Sender<RawFrame>,
    thread: Option<JoinHandle<()>>,
}

impl PreviewWorker {
    pub(crate) fn new() -> Self {
        let (done_tx, done) = mpsc::channel();
        Self {
            jobs: None,
            done,
            done_tx,
            thread: None,
        }
    }

    /// Le lleva `frame` para la vista previa de `tap`. Lo devuelve si el hilo aún está con el
    /// anterior (esta vez no hay vista previa) o no se pudo crear (entonces la hace aquí mismo).
    pub(crate) fn offer(&mut self, tap: Arc<PreviewTap>, frame: RawFrame) -> Option<RawFrame> {
        if self.jobs.is_none() {
            let (tx, rx) = mpsc::sync_channel::<(Arc<PreviewTap>, RawFrame)>(1);
            let done = self.done_tx.clone();
            match std::thread::Builder::new()
                .name("preview".into())
                .spawn(move || {
                    for (tap, frame) in rx {
                        tap.render(&frame);
                        if done.send(frame).is_err() {
                            break;
                        }
                    }
                }) {
                Ok(handle) => {
                    self.thread = Some(handle);
                    self.jobs = Some(tx);
                }
                Err(err) => {
                    tracing::warn!(error = %err, "sin hilo para la vista previa; se reduce aquí");
                    tap.render(&frame);
                    return Some(frame);
                }
            }
        }
        match self.jobs.as_ref()?.try_send((tap, frame)) {
            Ok(()) => None,
            Err(TrySendError::Full((_, frame)) | TrySendError::Disconnected((_, frame))) => {
                Some(frame)
            }
        }
    }

    /// Fotogramas que el hilo ya usó.
    pub(crate) fn returned(&self) -> Vec<RawFrame> {
        self.done.try_iter().collect()
    }
}

impl Drop for PreviewWorker {
    fn drop(&mut self) {
        // Sin emisor, el bucle del hilo termina.
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
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

    #[test]
    fn a_hidden_preview_asks_for_nothing() {
        let tap = PreviewTap::new(Box::new(|_| {}));
        let frame = RawFrame::cpu(1, (2, 2), PixelFormat::Bgra, 8, vec![0; 16]);
        assert!(tap.wants(&frame));
        tap.set_enabled(false);
        let later = RawFrame::cpu(2, (2, 2), PixelFormat::Bgra, 8, vec![0; 16]);
        assert!(!tap.wants(&later), "oculta: ni se mira el periodo");
        tap.set_enabled(true);
        std::thread::sleep(PREVIEW_PERIOD + Duration::from_millis(5));
        assert!(tap.wants(&later), "a la vista otra vez");
    }
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
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        let tap = Arc::new(PreviewTap::with(
            Duration::from_secs(3600),
            64,
            Box::new(move |frame| {
                assert!(frame.is_some_and(|f| f.format == PixelFormat::I420));
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        ));
        let frame = RawFrame::cpu(1, (16, 8), PixelFormat::Bgra, 16 * 4, vec![128; 16 * 8 * 4]);
        assert!(tap.wants(&frame), "el primero toca");
        assert!(!tap.wants(&frame), "una por periodo");
        let mut worker = PreviewWorker::new();
        assert!(
            worker.offer(Arc::clone(&tap), frame).is_none(),
            "se lo lleva"
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        let back = loop {
            let mut back = worker.returned();
            if let Some(back) = back.pop() {
                break back;
            }
            assert!(
                Instant::now() < deadline,
                "el hilo de la vista previa no devolvió el fotograma"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(back.frame_id, 1, "lo devuelve entero");
        assert_eq!(seen.load(Ordering::SeqCst), 1, "una por periodo");
    }

    #[test]
    fn gpu_frames_are_reported_once_and_never_rendered() {
        use coroom_codec::gpu::{GpuImage, OpaqueImage};
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        let tap = PreviewTap::with(
            Duration::ZERO,
            64,
            Box::new(move |frame| {
                assert!(frame.is_none());
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let image = GpuImage::Opaque(Arc::new(OpaqueImage {
            kind: "x".into(),
            keepalive: Arc::new(()),
        }));
        let frame = RawFrame::gpu(1, (16, 8), image);
        assert!(!tap.wants(&frame));
        assert!(!tap.wants(&frame));
        assert_eq!(seen.load(Ordering::SeqCst), 1, "avisa una vez");
    }
}
