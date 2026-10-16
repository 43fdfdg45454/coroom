//! Pipeline de vídeo completo en una sola máquina: captura → codificación → UDP local →
//! reensamblado → decodificación → presentación, con una marca de tiempo por etapa.
//!
//! Cada etapa vive en su hilo (`capture`, `encode`, `send`, `recv`, `decode`, `present`) y se
//! comunica con la siguiente por un canal acotado de capacidad [`CHANNEL_CAPACITY`] con política
//! **gana el último**: si el canal está lleno se saca el fotograma más antiguo, se anota como
//! descarte en el [`LatencyLedger`] y se mete el nuevo. La excepción es el canal de `encode` a
//! `send`: un fotograma codificado es referencia del siguiente y no se tira, `encode` espera
//! ([`send_or_stop`]) y lo que se desaloja es captura sin codificar. Entre `send` y `recv` van
//! datagramas reales por `127.0.0.1` ([`coroom_transport::loopback`]).
//!
//! En los dos canales de imagen sin comprimir (`capture → encode` y `decode → present`) el
//! consumidor también aplica la política al leer: si hay varios fotogramas esperando, toma el más
//! reciente y descarta los demás. Así un codificador al límite trabaja siempre sobre el último
//! fotograma capturado en lugar de arrastrar dos periodos de cola. En los canales de vídeo
//! comprimido no se hace, porque saltarse un fotograma rompe las referencias.
//!
//! Un descarte es un fotograma que no llega a una etapa, y se anota en esa etapa: el canal
//! `capture → encode` descarta en [`Stage::Encoded`], el reensamblador en [`Stage::Received`], etc.
//! Como perder un fotograma comprimido rompe la cadena de referencias, cualquier descarte o error a
//! partir de la codificación pide un intra al codificador (lo que en la sesión remota hace el NACK/PLI por
//! la red).
//!
//! La presentación es un [`FrameSink`]: `session` no depende de `coroom-render` (`docs/ARQUITECTURA.md`
//! §5), así que la aplicación adapta su `Presenter` a este trait.
//!
//! Cero copias: al arrancar, la captura recibe las imágenes de GPU que acepta el
//! codificador ([`capture_gpu_frames`]); `encode` le pasa la imagen tal cual (DMA-BUF o textura
//! D3D11, sin tocarla) y el decodificador puede devolver una imagen de GPU que va así hasta la
//! presentación. Cada etapa anota en el [`LatencyLedger`] cuántas copias a CPU hizo
//! ([`LatencyLedger::record_cpu_copies`]). El codificador se configura con
//! [`coroom_codec::DEFAULT_SLICES`] slices.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use coroom_capture::{
    CaptureError, CaptureOptions, CapturedFrame, Capturer, CursorShape, CursorState, FramePixels,
    GpuFrames,
};
use coroom_codec::{
    Codec, CodecError, DEFAULT_SLICES, Decoder, DecoderBackend, DecoderSet, EncodedFrame,
    EncodedSlice, Encoder, EncoderConfig, GpuInput, PixelFormat, RawFrame, VideoFormat,
    decoder_label, encoder_label,
};
use coroom_transport::loopback::{UdpLoopback, UdpReceiver, UdpSender};
use crossbeam_channel::{
    Receiver, RecvTimeoutError, SendTimeoutError, Sender, TrySendError, bounded,
};
use tracing::{debug, info, warn};

use crate::latency::{LatencyLedger, LatencySummary, Stage};
use crate::preview::{PreviewTap, PreviewWorker};
use crate::stats::{Counters, bump};

/// Capacidad de cada canal entre etapas.
pub const CHANNEL_CAPACITY: usize = 2;

/// Cada cuánto miran los hilos si hay que parar mientras esperan.
const POLL: Duration = Duration::from_millis(50);

/// Parámetros del pipeline local.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopbackConfig {
    /// Ancho máximo del flujo: la captura de pantalla trae su propio tamaño y el codificador lo
    /// reduce si no cabe ([`LoopbackConfig::stream_size`]).
    pub width: u32,
    /// Alto máximo del flujo.
    pub height: u32,
    /// Fotogramas por segundo objetivo.
    pub fps: u32,
    /// Bitrate objetivo en kbit/s.
    pub bitrate_kbps: u32,
    /// Si está definido, el pipeline termina solo tras presentar este número de fotogramas.
    pub frames: Option<u64>,
    /// Pedir croma 4:4:4 cuando el codificador y los decodificadores lo admitan.
    pub yuv444: bool,
    /// Pedir HDR10 (10 bits, BT.2020 PQ) cuando el codificador y los decodificadores lo admitan;
    /// la captura se pide en HDR. Si se piden los dos, gana HDR.
    pub hdr: bool,
}

impl Default for LoopbackConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_kbps: 10_000,
            frames: None,
            yuv444: false,
            hdr: false,
        }
    }
}

impl LoopbackConfig {
    /// Formatos a intentar, del preferido al de siempre: HDR10, 4:4:4 y 4:2:0 SDR.
    pub fn format_candidates(&self) -> Vec<VideoFormat> {
        let mut out = Vec::with_capacity(3);
        if self.hdr {
            out.push(VideoFormat::HDR10_420);
        }
        if self.yuv444 {
            out.push(VideoFormat::SDR_444);
        }
        out.push(VideoFormat::SDR_420);
        out
    }

    /// El formato preferido de [`LoopbackConfig::format_candidates`].
    pub fn wanted_format(&self) -> VideoFormat {
        self.format_candidates()
            .first()
            .copied()
            .unwrap_or(VideoFormat::SDR_420)
    }

    /// Tamaño del flujo cuando la captura entrega `captured`: lo mismo si cabe en el tope
    /// (`width × height`), y si no, reducido a lo que cabe con su proporción y lados pares
    /// ([`coroom_transport::proto::video::fit`], la regla que aplican el codificador y la captura
    /// sintética).
    pub fn stream_size(&self, captured: (u32, u32)) -> (u32, u32) {
        coroom_transport::proto::video::fit(captured, (self.width, self.height))
    }

    /// Lo que se le pide a la captura: los fps, el tamaño máximo (lo aplica la fuente que elige
    /// el suyo, la sintética; la pantalla llega con el de la fuente y el codificador lo reduce) y,
    /// con HDR, imágenes en coma flotante.
    pub fn capture_options(&self) -> CaptureOptions {
        CaptureOptions {
            max_fps: self.fps,
            hdr: self.hdr,
            max_size: Some((self.width, self.height)),
        }
    }

    /// Imágenes de GPU que se le piden a la captura para `input`: solo con el formato de siempre
    /// (4:4:4 y HDR se convierten en CPU).
    pub fn gpu_frames(&self, input: &GpuInput) -> GpuFrames {
        if self.yuv444 || self.hdr {
            GpuFrames::Off
        } else {
            capture_gpu_frames(input)
        }
    }
}

/// Formas de cursor que se recuerdan (las más antiguas se olvidan).
const CURSOR_SHAPES: usize = 16;

/// Fotograma decodificado que llega a la presentación.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    /// Imagen I420 o NV12 con planos compactados (NV12 es lo que dan los decodificadores por
    /// hardware y se pasa sin convertir).
    pub frame: RawFrame,
    /// Último cursor conocido del anfitrión (viaja aparte del vídeo, como en la sesión remota).
    pub cursor: Option<CursorState>,
    /// Forma de ese cursor, si ya llegó.
    pub cursor_shape: Option<Arc<CursorShape>>,
}

/// Texto del códec en uso para el resumen y el overlay: `AV1 · SVT-AV1 → dav1d`.
pub fn video_label(encoder: Option<&dyn Encoder>, decoder: Option<&dyn Decoder>) -> String {
    let codec = encoder
        .map(|e| e.codec())
        .or_else(|| decoder.map(|d| d.codec()));
    let Some(codec) = codec else {
        return String::new();
    };
    let encoder = encoder.map_or_else(|| "?".to_owned(), |e| encoder_label(codec, e.backend()));
    let decoder = decoder.map_or_else(|| "?".to_owned(), |d| decoder_label(codec, d.backend()));
    format!("{codec} · {encoder} → {decoder}")
}

/// Imágenes de GPU que hay que pedirle a la captura para un codificador que acepta `input`.
pub fn capture_gpu_frames(input: &GpuInput) -> GpuFrames {
    match input {
        GpuInput::None => GpuFrames::Off,
        GpuInput::DmaBuf { modifiers } => GpuFrames::DmaBuf {
            modifiers: modifiers.clone(),
        },
        GpuInput::D3D11 => GpuFrames::D3D11,
    }
}

/// Si la captura, que entrega `capture`, tiene que pasar a memoria de CPU para un codificador que
/// acepta `input`: `Some(Off)` cuando entrega imágenes de GPU y el codificador no lee ninguna
/// (NVENC o software con DMA-BUF, software con D3D11). Con imágenes de GPU en los dos no se decide
/// aquí: el codificador las importa o las descarga y, si no puede, lo dice al codificar
/// ([`CodecError::GpuImport`]) y el hilo `encode` pide la memoria de CPU entonces.
pub fn capture_frames_for(capture: &GpuFrames, input: &GpuInput) -> Option<GpuFrames> {
    match (capture, input) {
        (GpuFrames::Off, _) => None,
        (_, GpuInput::None) => Some(GpuFrames::Off),
        _ => None,
    }
}

/// Convierte un fotograma de la captura en la entrada del codificador moviendo la imagen (sin
/// copiarla). El tamaño se recorta a par: H.264 y HEVC 4:2:0 lo necesitan.
pub fn to_raw_frame(frame: CapturedFrame) -> RawFrame {
    let size = (frame.width & !1, frame.height & !1);
    match frame.pixels {
        FramePixels::Bgra { data, stride } => {
            RawFrame::cpu(frame.frame_id, size, PixelFormat::Bgra, stride, data)
        }
        FramePixels::Rgba16F { data, stride } => {
            RawFrame::cpu(frame.frame_id, size, PixelFormat::Rgba16F, stride, data)
        }
        #[cfg(unix)]
        FramePixels::DmaBuf(dmabuf) => {
            use coroom_codec::gpu::{DmaBufImage, DmaBufPlane};
            // Normalmente el `Arc` es único y los descriptores se mueven; si no, se duplican.
            let (planes, modifier, fourcc, release) = match Arc::try_unwrap(dmabuf) {
                Ok(owned) => (
                    owned
                        .planes
                        .into_iter()
                        .map(|plane| DmaBufPlane {
                            fd: plane.fd,
                            offset: plane.offset,
                            stride: plane.stride,
                        })
                        .collect::<Vec<_>>(),
                    owned.modifier,
                    owned.fourcc,
                    owned.release,
                ),
                Err(shared) => (
                    shared
                        .planes
                        .iter()
                        .filter_map(|plane| {
                            Some(DmaBufPlane {
                                fd: plane.fd.try_clone().ok()?,
                                offset: plane.offset,
                                stride: plane.stride,
                            })
                        })
                        .collect(),
                    shared.modifier,
                    shared.fourcc,
                    shared.release.clone(),
                ),
            };
            let image = DmaBufImage {
                planes,
                modifier,
                fourcc,
                width: size.0,
                height: size.1,
                keepalive: release,
            };
            RawFrame::gpu(
                frame.frame_id,
                size,
                coroom_codec::GpuImage::DmaBuf(Arc::new(image)),
            )
        }
        #[cfg(windows)]
        FramePixels::D3D11(texture) => {
            let image = coroom_codec::gpu::D3D11Image {
                texture: texture.texture.clone(),
                subresource: texture.subresource,
                device: texture.device.clone(),
                width: size.0,
                height: size.1,
                keepalive: texture.release.clone(),
            };
            RawFrame::gpu(
                frame.frame_id,
                size,
                coroom_codec::GpuImage::D3D11(Arc::new(image)),
            )
        }
    }
}

/// Error de la etapa de presentación.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkError(pub String);

impl fmt::Display for SinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SinkError {}

/// Última etapa del pipeline: presenta un fotograma decodificado.
///
/// El hilo `present` marca [`Stage::Presented`] justo después de que vuelva
/// [`FrameSink::present`].
pub trait FrameSink: Send {
    /// Presenta el fotograma.
    fn present(&mut self, frame: DecodedFrame) -> Result<(), SinkError>;

    /// Copias de la imagen en memoria de CPU que hizo el último `present` (subir los planos a la
    /// GPU cuenta una; importar una imagen de GPU, ninguna). Por defecto, ninguna.
    fn cpu_copies(&self) -> u32 {
        0
    }

    /// Si el último `present` llegó a la pantalla. `false` cuando la ventana no podía mostrarlo
    /// (oculta, minimizada o sin superficie): el pipeline lo anota como descarte en la etapa de
    /// presentación en vez de darlo por presentado. Por defecto, sí.
    fn shown(&self) -> bool {
        true
    }

    /// Quien quiera enterarse del cursor del anfitrión **entre** fotogramas (viaja aparte del
    /// vídeo): el pipeline le pasa cada posición nueva desde el hilo que la recibe, con la forma
    /// si ya se conoce. Por defecto, nadie: el cursor va con el siguiente fotograma.
    fn cursor_sink(&self) -> Option<Arc<dyn CursorSink>> {
        None
    }
}

/// Destino del cursor del anfitrión entre fotogramas (ver [`FrameSink::cursor_sink`]). Se llama
/// desde el hilo de control de la sesión: no debe bloquearse (ni esperar a la pantalla).
pub trait CursorSink: Send + Sync {
    /// El cursor del anfitrión se movió (o cambió de forma o de visibilidad).
    fn cursor_moved(&self, cursor: CursorState, shape: Option<Arc<CursorShape>>);
}

/// Presentación que no dibuja nada; para tests y pruebas sin ventana.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl FrameSink for NullSink {
    fn present(&mut self, _frame: DecodedFrame) -> Result<(), SinkError> {
        Ok(())
    }
}

/// Error que detuvo el pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineError {
    /// Falló la captura.
    Capture(CaptureError),
    /// Falló el códec.
    Codec(CodecError),
    /// Falló el bucle UDP local.
    Transport(String),
    /// Falló la presentación.
    Sink(SinkError),
    /// Un hilo no se pudo crear o terminó con un pánico.
    Thread(String),
}

impl fmt::Display for PipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PipelineError::Capture(err) => write!(f, "captura: {err}"),
            PipelineError::Codec(err) => write!(f, "códec: {err}"),
            PipelineError::Transport(err) => write!(f, "transporte: {err}"),
            PipelineError::Sink(err) => write!(f, "presentación: {err}"),
            PipelineError::Thread(err) => write!(f, "hilo del pipeline: {err}"),
        }
    }
}

impl std::error::Error for PipelineError {}

/// Estado compartido entre los hilos (también los de la sesión remota).
#[derive(Debug, Default)]
pub(crate) struct Shared {
    pub(crate) stop: AtomicBool,
    pub(crate) keyframe_requested: AtomicBool,
    pub(crate) cursor: Mutex<Option<CursorState>>,
    /// Últimas formas de cursor, de la más antigua a la más nueva.
    cursor_shapes: Mutex<VecDeque<Arc<CursorShape>>>,
    finished: Mutex<bool>,
    finished_changed: Condvar,
    failure: Mutex<Option<PipelineError>>,
    /// Vista previa de la captura, si alguien la pidió.
    pub(crate) preview: Mutex<Option<Arc<crate::preview::PreviewTap>>>,
    /// Contadores de las etapas para el registro periódico ([`crate::stats`]).
    pub(crate) counters: Counters,
    /// Captura nueva, ya arrancada, que el hilo `capture` adopta (cambio de fuente en marcha).
    pub(crate) next_capturer: CapturerSlot,
    /// Opciones nuevas para la captura en marcha (fps o tamaño máximo cambiados con la sala
    /// abierta): el hilo `capture` se las pasa a la suya y a la que adopte después.
    capture_options: Mutex<Option<CaptureOptions>>,
    capture_options_pending: AtomicBool,
    /// Imágenes de GPU nuevas para la captura en marcha ([`GpuFrames::Off`] cuando el codificador
    /// que se negoció no lee las que entrega, [`capture_frames_for`]): el hilo `capture` se lo
    /// pasa a la suya y a la que adopte después. Una vez en CPU, la sala sigue en CPU.
    capture_gpu_frames: Mutex<Option<GpuFrames>>,
    capture_gpu_frames_pending: AtomicBool,
    /// Tamaño del último fotograma que entregó la captura (lo anota el hilo `encode`): con él y
    /// el tope, la sesión sabe el tamaño del flujo antes de anunciarlo
    /// ([`LoopbackConfig::stream_size`]).
    captured_size: Mutex<Option<(u32, u32)>>,
    /// A quién avisar del cursor entre fotogramas (lo pone el hilo `present` con su sumidero).
    cursor_sink: CursorSinkSlot,
    /// La fuente en uso no comparte nada ([`Capturer::shares_nothing`]): sala solo de voz o tras
    /// «Dejar de compartir». El codificador olvida el último fotograma y no lo repite.
    nothing_shared: AtomicBool,
}

/// Hueco para el [`CursorSink`] del sumidero de presentación.
#[derive(Default)]
struct CursorSinkSlot(Mutex<Option<Arc<dyn CursorSink>>>);

impl CursorSinkSlot {
    fn set(&self, sink: Option<Arc<dyn CursorSink>>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = sink;
    }

    fn get(&self) -> Option<Arc<dyn CursorSink>> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl fmt::Debug for CursorSinkSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let set = self.0.lock().map(|slot| slot.is_some()).unwrap_or(false);
        f.debug_tuple("CursorSinkSlot").field(&set).finish()
    }
}

/// Hueco para la captura que sustituye a la actual.
#[derive(Default)]
pub(crate) struct CapturerSlot(Mutex<Option<Box<dyn Capturer>>>);

impl CapturerSlot {
    /// Deja `capturer` para el hilo `capture`; si había otra esperando, se para.
    pub(crate) fn put(&self, capturer: Box<dyn Capturer>) {
        let old = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(capturer);
        if let Some(mut old) = old {
            old.stop();
        }
    }

    fn take(&self) -> Option<Box<dyn Capturer>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

/// Una captura que se quedó esperando (la sesión terminó antes de adoptarla) suelta su sesión.
impl Drop for CapturerSlot {
    fn drop(&mut self) {
        if let Some(mut capturer) = self.take() {
            capturer.stop();
        }
    }
}

impl fmt::Debug for CapturerSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let waiting = self.0.lock().map(|slot| slot.is_some()).unwrap_or(false);
        f.debug_tuple("CapturerSlot").field(&waiting).finish()
    }
}

impl Shared {
    pub(crate) fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Si la fuente en uso comparte algo (lo anuncia el anfitrión con `Sharing`).
    pub(crate) fn sharing(&self) -> bool {
        !self.nothing_shared.load(Ordering::Acquire)
    }

    /// Para los tests: como si la fuente en uso compartiese algo o no.
    #[cfg(test)]
    pub(crate) fn set_sharing(&self, sharing: bool) {
        self.nothing_shared.store(!sharing, Ordering::Release);
    }

    /// Anota si la fuente que entra comparte algo.
    pub(crate) fn note_source(&self, capturer: &dyn Capturer) {
        self.nothing_shared
            .store(capturer.shares_nothing(), Ordering::Release);
    }

    /// Deja opciones nuevas para la captura en marcha (gana la última).
    pub(crate) fn set_capture_options(&self, options: CaptureOptions) {
        *self
            .capture_options
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(options);
        self.capture_options_pending.store(true, Ordering::Release);
    }

    /// Las opciones nuevas aún sin aplicar, una vez.
    pub(crate) fn take_capture_options(&self) -> Option<CaptureOptions> {
        if !self.capture_options_pending.swap(false, Ordering::AcqRel) {
            return None;
        }
        self.latest_capture_options()
    }

    /// Las últimas opciones cambiadas en marcha, si las hubo (para la captura que se adopta).
    pub(crate) fn latest_capture_options(&self) -> Option<CaptureOptions> {
        *self
            .capture_options
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Pide otras imágenes para la captura en marcha (gana la última).
    pub(crate) fn set_capture_gpu_frames(&self, frames: GpuFrames) {
        *self
            .capture_gpu_frames
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(frames);
        self.capture_gpu_frames_pending
            .store(true, Ordering::Release);
    }

    /// Las imágenes nuevas aún sin aplicar, una vez.
    pub(crate) fn take_capture_gpu_frames(&self) -> Option<GpuFrames> {
        if !self
            .capture_gpu_frames_pending
            .swap(false, Ordering::AcqRel)
        {
            return None;
        }
        self.latest_capture_gpu_frames()
    }

    /// Las últimas imágenes pedidas en marcha, si las hubo (para la captura que se adopta).
    pub(crate) fn latest_capture_gpu_frames(&self) -> Option<GpuFrames> {
        self.capture_gpu_frames
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Anota el tamaño del fotograma que acaba de entregar la captura; `true` si cambió.
    pub(crate) fn note_captured_size(&self, size: (u32, u32)) -> bool {
        let mut known = self
            .captured_size
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *known == Some(size) {
            return false;
        }
        *known = Some(size);
        true
    }

    /// Tamaño del último fotograma de la captura, si ya llegó alguno.
    pub(crate) fn captured_size(&self) -> Option<(u32, u32)> {
        *self
            .captured_size
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Termina el pipeline; guarda el primer error, si lo hay.
    pub(crate) fn finish(&self, error: Option<PipelineError>) {
        if let Some(error) = error {
            let mut failure = self.failure.lock().unwrap_or_else(PoisonError::into_inner);
            failure.get_or_insert(error);
        }
        self.stop.store(true, Ordering::Release);
        *self.finished.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.finished_changed.notify_all();
    }

    /// Guarda una forma de cursor (sustituye la del mismo id). Devuelve si era nueva.
    pub(crate) fn remember_shape(&self, shape: Arc<CursorShape>) -> bool {
        let mut shapes = self
            .cursor_shapes
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(at) = shapes.iter().position(|s| s.shape_id == shape.shape_id) {
            if Arc::ptr_eq(&shapes[at], &shape) || *shapes[at] == *shape {
                return false;
            }
            shapes.remove(at);
        }
        shapes.push_back(shape);
        while shapes.len() > CURSOR_SHAPES {
            shapes.pop_front();
        }
        true
    }

    /// Cursor nuevo del anfitrión (posición, visibilidad o forma): se guarda para el siguiente
    /// fotograma y, si el sumidero de presentación quiere saberlo entre fotogramas, se le avisa
    /// ya, con la forma si se conoce.
    pub(crate) fn set_cursor(&self, cursor: CursorState) {
        *self.cursor.lock().unwrap_or_else(PoisonError::into_inner) = Some(cursor);
        if let Some(sink) = self.cursor_sink.get() {
            sink.cursor_moved(cursor, self.cursor_shape(cursor.shape_id));
        }
    }

    /// Forma del cursor `shape_id`, si se conoce.
    pub(crate) fn cursor_shape(&self, shape_id: u64) -> Option<Arc<CursorShape>> {
        self.cursor_shapes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .rev()
            .find(|shape| shape.shape_id == shape_id)
            .cloned()
    }

    pub(crate) fn request_keyframe(&self) {
        self.keyframe_requested.store(true, Ordering::Release);
    }

    /// Consume una petición de intra pendiente.
    pub(crate) fn take_keyframe_request(&self) -> bool {
        self.keyframe_requested.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn is_finished(&self) -> bool {
        *self.finished.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn wait(&self, timeout: Duration) -> bool {
        let finished = self.finished.lock().unwrap_or_else(PoisonError::into_inner);
        let (finished, _) = self
            .finished_changed
            .wait_timeout_while(finished, timeout, |finished| !*finished)
            .unwrap_or_else(PoisonError::into_inner);
        *finished
    }

    pub(crate) fn failure(&self) -> Option<PipelineError> {
        self.failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Lanza un hilo con nombre de una etapa; su error termina el pipeline.
pub(crate) fn spawn_stage(
    shared: &Arc<Shared>,
    name: &'static str,
    body: impl FnOnce() -> Result<(), PipelineError> + Send + 'static,
) -> Result<JoinHandle<()>, PipelineError> {
    let thread_shared = Arc::clone(shared);
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            if let Err(err) = body() {
                thread_shared.finish(Some(err));
            }
        })
        .map_err(|err| {
            let err = PipelineError::Thread(format!("no se pudo crear `{name}`: {err}"));
            shared.finish(Some(err.clone()));
            err
        })
}

/// Mete `item` en el canal; si está lleno, saca el más antiguo, lo anota como descarte en `stage`
/// y reintenta. Devuelve si hubo descarte, o `Err` si el consumidor ya no existe.
pub(crate) fn send_latest<T>(
    tx: &Sender<T>,
    evict: &Receiver<T>,
    mut item: T,
    ledger: &LatencyLedger,
    stage: Stage,
) -> Result<bool, ()> {
    let mut dropped = false;
    loop {
        match tx.try_send(item) {
            Ok(()) => return Ok(dropped),
            Err(TrySendError::Full(back)) => {
                item = back;
                if evict.try_recv().is_ok() {
                    ledger.record_drop(stage);
                    dropped = true;
                }
            }
            Err(TrySendError::Disconnected(_)) => return Err(()),
        }
    }
}

/// Espera el siguiente elemento; `None` si hay que parar o el productor terminó.
pub(crate) fn recv_or_stop<T>(rx: &Receiver<T>, shared: &Shared) -> Option<T> {
    loop {
        if shared.stopping() {
            return None;
        }
        match rx.recv_timeout(POLL) {
            Ok(item) => return Some(item),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
}

/// Como [`recv_or_stop`], pero si hay más elementos esperando devuelve el más reciente y anota los
/// demás como descartes en `stage`.
pub(crate) fn recv_latest<T>(
    rx: &Receiver<T>,
    shared: &Shared,
    ledger: &LatencyLedger,
    stage: Stage,
) -> Option<T> {
    let mut item = recv_or_stop(rx, shared)?;
    while let Ok(newer) = rx.try_recv() {
        ledger.record_drop(stage);
        item = newer;
    }
    Some(item)
}

/// Reconstruye un id de 64 bits a partir de los 32 bits de la cabecera, eligiendo el más cercano
/// al último recibido (los ids crecen de uno en uno, así que la vuelta de los 32 bits se detecta).
pub fn extend_frame_id(last: Option<u64>, low: u32) -> u64 {
    let Some(last) = last else {
        return u64::from(low);
    };
    let base = (last & !0xFFFF_FFFF) | u64::from(low);
    let span = 1u64 << 32;
    [base.checked_sub(span), Some(base), base.checked_add(span)]
        .into_iter()
        .flatten()
        .min_by_key(|candidate| candidate.abs_diff(last))
        .unwrap_or(base)
}

/// Si `image` trae otro tamaño que el último decodificado (y lo anota); el primero no cuenta.
fn resized(last: &mut Option<(u32, u32)>, image: &RawFrame) -> bool {
    let size = (image.width, image.height);
    let changed = last.is_some_and(|known| known != size);
    *last = Some(size);
    changed
}

/// Fotograma comprimido en tránsito.
#[derive(Debug)]
pub(crate) struct Compressed {
    pub(crate) frame_id: u64,
    pub(crate) keyframe: bool,
    pub(crate) data: Vec<u8>,
}

/// Control del codificador desde fuera del pipeline (sesión remota): bitrate objetivo, si hay a
/// quién enviar y el codificador nuevo cuando se negocia otro códec.
pub(crate) struct EncodeControl {
    /// Bitrate objetivo en kbit/s; el hilo `encode` lo aplica cuando cambia.
    pub(crate) bitrate_kbps: AtomicU32,
    /// Sin destinatarios no se codifica (ni se cuenta como descarte).
    pub(crate) active: AtomicBool,
    /// Codificador ya configurado que el hilo `encode` adopta antes del siguiente fotograma (su
    /// primer fotograma es intra).
    pub(crate) next: Mutex<Option<Box<dyn Encoder>>>,
    /// A quién avisar cuando cambia el tamaño del flujo que produce el codificador (la captura
    /// trae otro tamaño, o el tope lo reduce): el anfitrión reenvía `HostFrameSize`.
    size_listener: Mutex<Option<SizeListener>>,
}

/// Aviso del tamaño nuevo del flujo ([`EncodeControl::set_size_listener`]).
pub(crate) type SizeListener = Box<dyn Fn((u32, u32)) + Send + Sync>;

impl EncodeControl {
    pub(crate) fn new(bitrate_kbps: u32) -> Self {
        Self {
            bitrate_kbps: AtomicU32::new(bitrate_kbps),
            active: AtomicBool::new(false),
            next: Mutex::new(None),
            size_listener: Mutex::new(None),
        }
    }

    /// Entrega un codificador nuevo al hilo `encode`.
    pub(crate) fn replace(&self, encoder: Box<dyn Encoder>) {
        *self.next.lock().unwrap_or_else(PoisonError::into_inner) = Some(encoder);
    }

    /// A quién avisar cuando cambie el tamaño del flujo.
    pub(crate) fn set_size_listener(&self, listener: SizeListener) {
        *self
            .size_listener
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(listener);
    }

    fn output_size_changed(&self, size: (u32, u32)) {
        if let Some(listener) = self
            .size_listener
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            listener(size);
        }
    }

    fn take_next(&self) -> Option<Box<dyn Encoder>> {
        self.next
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

/// Abre un decodificador de `codec` en `format` con lo que quede de un [`DecoderSet`].
pub(crate) type DecoderOpener = Box<
    dyn Fn(&DecoderSet, Codec, VideoFormat) -> Result<Box<dyn Decoder>, CodecError> + Send + Sync,
>;

/// Control del decodificador desde fuera del pipeline (invitado de la sesión remota): el códec que
/// anunció el anfitrión; el hilo `decode` abre el decodificador de ese códec con lo mejor de
/// `set` antes del siguiente fotograma (y lo vuelve a abrir si cambia).
///
/// Respaldo: un decodificador que no saca imagen con los intra que recibe ([`DecodeHealth`]) se
/// aparta (`excluded`) y se abre el siguiente del orden; si no queda ninguno de ese códec, el hilo
/// `decode` pide renegociar y el invitado vuelve a anunciar sus capacidades sin él, con lo que el
/// anfitrión elige otro códec (normalmente H.264).
pub(crate) struct DecodeControl {
    pub(crate) set: DecoderSet,
    wanted: Mutex<Option<(Codec, VideoFormat)>>,
    /// Lo que el invitado acepta además de 4:2:0 SDR: `(4:4:4, HDR10)`.
    pub(crate) accept: (bool, bool),
    /// Decodificadores que fallaron en esta sesión.
    excluded: Mutex<Vec<(Codec, DecoderBackend)>>,
    /// El invitado tiene que volver a anunciar sus capacidades ([`DecodeControl::usable`]).
    renegotiate: AtomicBool,
    opener: DecoderOpener,
    /// Solo decodificadores por software (Ajustes del invitado, en marcha).
    software_only: AtomicBool,
    /// Hay que volver a abrir el decodificador (cambió [`DecodeControl::software_only`]).
    reopen: AtomicBool,
}

impl fmt::Debug for DecodeControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecodeControl")
            .field("set", &self.set)
            .field("wanted", &self.wanted())
            .field("accept", &self.accept)
            .field("excluded", &self.excluded())
            .finish_non_exhaustive()
    }
}

impl DecodeControl {
    pub(crate) fn new(set: DecoderSet) -> Self {
        Self {
            set,
            wanted: Mutex::new(None),
            accept: (false, false),
            excluded: Mutex::new(Vec::new()),
            renegotiate: AtomicBool::new(false),
            opener: Box::new(|set, codec, format| set.open_format(codec, format)),
            software_only: AtomicBool::new(false),
            reopen: AtomicBool::new(false),
        }
    }

    /// Decodificar solo por software (o volver a todos), en marcha: el hilo `decode` vuelve a
    /// abrir el decodificador y se renegocia el códec con lo que quede.
    pub(crate) fn set_software_only(&self, on: bool) {
        if self.software_only.swap(on, Ordering::AcqRel) != on {
            self.reopen.store(true, Ordering::Release);
            self.renegotiate.store(true, Ordering::Release);
        }
    }

    /// Olvida las peticiones de volver a abrir y de renegociar (al arrancar).
    pub(crate) fn take_pending(&self) {
        self.reopen.store(false, Ordering::Release);
        self.renegotiate.store(false, Ordering::Release);
    }

    /// Consume la petición de volver a abrir el decodificador.
    fn take_reopen(&self) -> bool {
        self.reopen.swap(false, Ordering::AcqRel)
    }

    /// Con lo que acepta el invitado: 4:4:4 y HDR10.
    pub(crate) fn accepting(mut self, yuv444: bool, hdr: bool) -> Self {
        self.accept = (yuv444, hdr);
        self
    }

    /// Con otra forma de abrir los decodificadores (los tests usan decodificadores falsos).
    #[cfg(test)]
    pub(crate) fn with_opener(mut self, opener: DecoderOpener) -> Self {
        self.opener = opener;
        self
    }

    /// Pide decodificar `codec` en `format` a partir de ahora.
    pub(crate) fn request(&self, codec: Codec, format: VideoFormat) {
        *self.wanted.lock().unwrap_or_else(PoisonError::into_inner) = Some((codec, format));
    }

    pub(crate) fn wanted(&self) -> Option<(Codec, VideoFormat)> {
        *self.wanted.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn excluded(&self) -> Vec<(Codec, DecoderBackend)> {
        self.excluded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Los decodificadores que quedan (sin los que fallaron): lo que se abre y lo que se anuncia.
    pub(crate) fn usable(&self) -> DecoderSet {
        let usable = self.set.without(&self.excluded());
        if !self.software_only.load(Ordering::Acquire) {
            return usable;
        }
        // Solo software; si ninguno lo es, mejor con todos que sin vídeo.
        let software = usable
            .clone()
            .only_backend(Some(coroom_codec::DecoderBackend::Software));
        if software.is_empty() {
            usable
        } else {
            software
        }
    }

    fn open(&self, codec: Codec, format: VideoFormat) -> Result<Box<dyn Decoder>, CodecError> {
        (self.opener)(&self.usable(), codec, format)
    }

    /// Aparta el decodificador `backend` de `codec`.
    fn exclude(&self, codec: Codec, backend: DecoderBackend) {
        let mut excluded = self.excluded.lock().unwrap_or_else(PoisonError::into_inner);
        if !excluded.contains(&(codec, backend)) {
            excluded.push((codec, backend));
        }
    }

    /// Aparta todos los decodificadores de `codec` y pide renegociar sin él. Si con eso no quedase
    /// ninguno, se vuelve a empezar con todos: mejor reintentar que quedarse sin vídeo.
    fn give_up_codec(&self, codec: Codec) {
        for &(c, backend) in self.set.available() {
            if c == codec {
                self.exclude(c, backend);
            }
        }
        if self.usable().is_empty() {
            warn!("ningún decodificador funcionó; se vuelve a probar con todos");
            self.excluded
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clear();
        }
        self.renegotiate.store(true, Ordering::Release);
    }

    /// Consume una petición de renegociación del hilo `decode`.
    pub(crate) fn take_renegotiation(&self) -> bool {
        self.renegotiate.swap(false, Ordering::AcqRel)
    }
}

impl fmt::Debug for EncodeControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EncodeControl")
            .field("bitrate_kbps", &self.bitrate_kbps)
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

/// Pipeline local en marcha.
#[derive(Debug)]
pub struct LoopbackPipeline {
    shared: Arc<Shared>,
    ledger: Arc<LatencyLedger>,
    threads: Vec<(&'static str, JoinHandle<()>)>,
}

impl LoopbackPipeline {
    /// Configura el codificador, arranca la captura del primer objetivo disponible y lanza los seis
    /// hilos. Los errores de configuración se devuelven aquí; los posteriores, en
    /// [`LoopbackPipeline::failure`].
    pub fn start(
        mut capturer: Box<dyn Capturer>,
        mut encoder: Box<dyn Encoder>,
        decoder: Box<dyn Decoder>,
        sink: Box<dyn FrameSink>,
        ledger: Arc<LatencyLedger>,
        config: LoopbackConfig,
    ) -> Result<Self, PipelineError> {
        encoder
            .configure(&EncoderConfig {
                codec: encoder.codec(),
                width: config.width,
                height: config.height,
                fps: config.fps,
                bitrate_kbps: config.bitrate_kbps,
                slices: DEFAULT_SLICES,
                format: config.wanted_format(),
            })
            .map_err(PipelineError::Codec)?;
        if decoder.codec() != encoder.codec() {
            return Err(PipelineError::Codec(CodecError::Unsupported(format!(
                "el codificador produce {} y el decodificador acepta {}",
                encoder.codec(),
                decoder.codec()
            ))));
        }
        ledger.set_video(video_label(Some(encoder.as_ref()), Some(decoder.as_ref())));
        let target = capturer
            .targets()
            .map_err(PipelineError::Capture)?
            .into_iter()
            .next()
            .ok_or_else(|| {
                PipelineError::Capture(CaptureError::Unsupported("no hay nada que capturar".into()))
            })?;
        capturer.set_gpu_frames(config.gpu_frames(&encoder.gpu_input()));
        capturer.set_options(config.capture_options());
        capturer.start(&target).map_err(PipelineError::Capture)?;
        let (sender, receiver) = UdpLoopback::new().map_err(|err| {
            capturer.stop();
            PipelineError::Transport(err.to_string())
        })?;

        let shared = Arc::new(Shared::default());
        let (raw_tx, raw_rx) = bounded::<CapturedFrame>(CHANNEL_CAPACITY);
        let (enc_tx, enc_rx) = bounded::<Compressed>(CHANNEL_CAPACITY);
        let (rcv_tx, rcv_rx) = bounded::<Compressed>(CHANNEL_CAPACITY);
        let (dec_tx, dec_rx) = bounded::<RawFrame>(CHANNEL_CAPACITY);

        let mut pipeline = Self {
            shared: Arc::clone(&shared),
            ledger: Arc::clone(&ledger),
            threads: Vec::with_capacity(6),
        };
        let spawn =
            |pipeline: &mut Self,
             name: &'static str,
             body: Box<dyn FnOnce() -> Result<(), PipelineError> + Send>| {
                let handle = spawn_stage(&pipeline.shared, name, body)?;
                pipeline.threads.push((name, handle));
                Ok::<(), PipelineError>(())
            };

        {
            let (shared, ledger, evict) =
                (Arc::clone(&shared), Arc::clone(&ledger), raw_rx.clone());
            spawn(
                &mut pipeline,
                "capture",
                Box::new(move || capture_loop(capturer, raw_tx, evict, &shared, &ledger)),
            )?;
        }
        {
            let (shared, ledger) = (Arc::clone(&shared), Arc::clone(&ledger));
            spawn(
                &mut pipeline,
                "encode",
                Box::new(move || {
                    encode_loop(Some(encoder), raw_rx, enc_tx, &shared, &ledger, None)
                }),
            )?;
        }
        {
            let (shared, ledger) = (Arc::clone(&shared), Arc::clone(&ledger));
            spawn(
                &mut pipeline,
                "send",
                Box::new(move || send_loop(sender, enc_rx, &shared, &ledger)),
            )?;
        }
        {
            let (shared, ledger, evict) =
                (Arc::clone(&shared), Arc::clone(&ledger), rcv_rx.clone());
            spawn(
                &mut pipeline,
                "recv",
                Box::new(move || recv_loop(receiver, rcv_tx, evict, &shared, &ledger)),
            )?;
        }
        {
            let (shared, ledger, evict) =
                (Arc::clone(&shared), Arc::clone(&ledger), dec_rx.clone());
            spawn(
                &mut pipeline,
                "decode",
                Box::new(move || {
                    decode_loop(Some(decoder), rcv_rx, dec_tx, evict, &shared, &ledger, None)
                }),
            )?;
        }
        {
            let (shared, ledger) = (Arc::clone(&shared), Arc::clone(&ledger));
            let frames = config.frames;
            spawn(
                &mut pipeline,
                "present",
                Box::new(move || present_loop(sink, dec_rx, frames, &shared, &ledger)),
            )?;
        }
        Ok(pipeline)
    }

    /// Registro de marcas de tiempo del pipeline.
    pub fn ledger(&self) -> &Arc<LatencyLedger> {
        &self.ledger
    }

    /// Indica si el pipeline terminó (llegó al número de fotogramas o falló).
    pub fn is_finished(&self) -> bool {
        self.shared.is_finished()
    }

    /// Espera hasta que el pipeline termine o pase `timeout`. Devuelve si terminó.
    pub fn wait(&self, timeout: Duration) -> bool {
        self.shared.wait(timeout)
    }

    /// Primer error que detuvo el pipeline, si lo hubo.
    pub fn failure(&self) -> Option<PipelineError> {
        self.shared.failure()
    }

    /// Detiene los hilos en orden (de la captura a la presentación) y devuelve el resumen final.
    pub fn stop(mut self) -> LatencySummary {
        self.join_all();
        self.ledger.summary()
    }

    fn join_all(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        for (name, handle) in self.threads.drain(..) {
            if handle.join().is_err() {
                self.shared.finish(Some(PipelineError::Thread(format!(
                    "`{name}` terminó con un pánico"
                ))));
            }
        }
        self.shared.finish(None);
    }
}

impl Drop for LoopbackPipeline {
    fn drop(&mut self) {
        if !self.threads.is_empty() {
            self.join_all();
        }
    }
}

pub(crate) fn capture_loop(
    mut capturer: Box<dyn Capturer>,
    tx: Sender<CapturedFrame>,
    evict: Receiver<CapturedFrame>,
    shared: &Shared,
    ledger: &LatencyLedger,
) -> Result<(), PipelineError> {
    shared.note_source(capturer.as_ref());
    let result = (|| {
        while !shared.stopping() {
            if let Some(mut next) = shared.next_capturer.take() {
                // Cambio de fuente: la de antes suelta su sesión (y en Linux su portal) y la
                // nueva empieza con un intra, que con otro tamaño reabre el codificador. Si los
                // fps o el tamaño cambiaron en marcha, la nueva los lleva también.
                capturer.stop();
                if let Some(options) = shared.latest_capture_options() {
                    next.set_options(options);
                }
                if let Some(frames) = shared.latest_capture_gpu_frames() {
                    next.set_gpu_frames(frames);
                }
                capturer = next;
                shared.note_source(capturer.as_ref());
                shared.request_keyframe();
                debug!(comparte = shared.sharing(), "captura sustituida");
            }
            if let Some(options) = shared.take_capture_options() {
                // fps o tamaño máximo cambiados con la sala abierta.
                let applied = capturer.set_options(options);
                debug!(?applied, "opciones de la captura en marcha");
            }
            if let Some(frames) = shared.take_capture_gpu_frames() {
                // El codificador negociado no lee las imágenes de GPU de la captura.
                let applied = capturer.set_gpu_frames(frames);
                debug!(?applied, "imágenes de la captura en marcha");
            }
            let Some(frame) = capturer
                .next_frame_timeout(POLL)
                .map_err(PipelineError::Capture)?
            else {
                continue;
            };
            ledger.mark(frame.frame_id, Stage::Captured, frame.captured_at);
            ledger.record_cpu_copies(Stage::Captured, frame.cpu_copies);
            bump(&shared.counters.captured);
            if let Some(cursor) = frame.cursor {
                *shared.cursor.lock().unwrap_or_else(PoisonError::into_inner) = Some(cursor);
            }
            if let Some(shape) = &frame.cursor_shape {
                shared.remember_shape(Arc::clone(shape));
            }
            if send_latest(&tx, &evict, frame, ledger, Stage::Encoded).is_err() {
                break;
            }
        }
        Ok(())
    })();
    capturer.stop();
    if let Some(mut pending) = shared.next_capturer.take() {
        pending.stop();
    }
    result
}

/// Cambio relativo de bitrate por debajo del cual no se reabre un codificador sin bitrate en
/// caliente ([`Encoder::live_bitrate`]): cada reapertura es un intra.
pub(crate) const REOPEN_STEP: f64 = 0.20;

/// Tiempo mínimo entre dos subidas de bitrate de un codificador sin bitrate en caliente. El control
/// de tasa sube un 5 % cada 100 ms y su salida cambia con cada 10 %: aplicado tal cual, SVT-AV1,
/// VA-API, AMF, Vulkan u OpenH264 se reabrían (y mandaban un intra) varias veces por segundo.
pub(crate) const REOPEN_INCREASE_PERIOD: Duration = Duration::from_secs(4);

/// Sin captura durante esto y con un intra pedido, se vuelve a codificar el último fotograma (la
/// pantalla está quieta: WGC y PipeWire solo entregan fotogramas cuando cambia algo).
pub(crate) const IDLE_KEYFRAME: Duration = Duration::from_millis(250);

/// Decide cuándo se aplica al codificador el bitrate objetivo. Con bitrate en caliente, siempre;
/// sin él, las bajadas de al menos [`REOPEN_STEP`] al momento (si no, se congestiona el enlace) y
/// las subidas de al menos [`REOPEN_STEP`] como mucho cada [`REOPEN_INCREASE_PERIOD`].
#[derive(Debug, Default)]
pub(crate) struct BitrateGate {
    applied: Option<u32>,
    last_increase: Option<Instant>,
}

impl BitrateGate {
    /// El bitrate que hay que aplicar ahora para `target`, si hay que cambiarlo.
    pub(crate) fn decide(&mut self, target: u32, live: bool, now: Instant) -> Option<u32> {
        let Some(applied) = self.applied else {
            self.applied = Some(target);
            self.last_increase = Some(now);
            return Some(target);
        };
        if target == applied {
            return None;
        }
        let change = (f64::from(target) - f64::from(applied)).abs() / f64::from(applied.max(1));
        let apply = live
            || (change >= REOPEN_STEP
                && (target < applied
                    || self.last_increase.is_none_or(|at| {
                        now.saturating_duration_since(at) >= REOPEN_INCREASE_PERIOD
                    })));
        if !apply {
            return None;
        }
        if target > applied {
            self.last_increase = Some(now);
        }
        self.applied = Some(target);
        Some(target)
    }
}

/// Lo siguiente que codifica el hilo `encode`.
enum EncodeInput {
    /// Un fotograma de la captura.
    Captured(CapturedFrame),
    /// El último fotograma otra vez, como intra ([`IDLE_KEYFRAME`]).
    Repeat,
}

/// Si `encode_loop` puede guardar `raw` para repetirlo: las imágenes de CPU y los DMA-BUF de
/// PipeWire (negocia de 4 a 16 búferes: retener uno no deja a la captura sin dónde escribir).
fn can_repeat(raw: &RawFrame) -> bool {
    match &raw.gpu {
        None => true,
        #[cfg(unix)]
        Some(coroom_codec::GpuImage::DmaBuf(_)) => true,
        Some(_) => false,
    }
}

/// Devuelve a la captura el búfer de un fotograma de CPU que ya no se necesita
/// ([`coroom_capture::pool`]): el siguiente se copia en él sin reservar memoria.
fn recycle(raw: RawFrame) {
    if raw.gpu.is_none() && matches!(raw.format, PixelFormat::Bgra | PixelFormat::Rgba16F) {
        coroom_capture::pool::give(raw.data);
    }
}

/// Guarda `raw` como último fotograma (si se puede repetir, [`can_repeat`]) tras ofrecérselo a la
/// vista previa, que se lo lleva a su hilo cuando le toca y lo devuelve después
/// ([`PreviewWorker::returned`]). Lo que no se guarda vuelve al pool de la captura.
fn stash(
    raw: Option<RawFrame>,
    tap: Option<&Arc<PreviewTap>>,
    worker: &mut PreviewWorker,
    last: &mut Option<RawFrame>,
    last_id: &mut Option<u64>,
) {
    if let Some(old) = last.take() {
        recycle(old);
    }
    let Some(raw) = raw else {
        return;
    };
    *last_id = Some(raw.frame_id);
    let raw = match tap {
        Some(tap) if tap.wants(&raw) => match worker.offer(Arc::clone(tap), raw) {
            Some(back) => back,
            None => return,
        },
        _ => raw,
    };
    if can_repeat(&raw) {
        *last = Some(raw);
    } else {
        recycle(raw);
    }
}

/// Una reducción en marcha: tamaño de la captura y tamaño del flujo.
type ScalePair = ((u32, u32), (u32, u32));

/// Tras codificar: si el flujo cambió de tamaño (otra captura, o el tope que la reduce), lo dice
/// una vez por el registro y avisa al anfitrión ([`EncodeControl::set_size_listener`]); si el
/// codificador redujo en CPU, anota el coste ([`Counters::record_scale`]) y lo dice una vez por
/// cada par de tamaños.
fn note_output_size(
    encoder: &dyn Encoder,
    control: Option<&EncodeControl>,
    output_size: &mut Option<(u32, u32)>,
    scaling: &mut Option<ScalePair>,
    shared: &Shared,
) {
    if let Some(size) = encoder.output_size()
        && *output_size != Some(size)
    {
        *output_size = Some(size);
        match shared.captured_size() {
            Some(captured) if captured != size => info!(
                width = size.0,
                height = size.1,
                captured_width = captured.0,
                captured_height = captured.1,
                "flujo reducido al tope de resolución"
            ),
            _ => debug!(width = size.0, height = size.1, "tamaño del flujo"),
        }
        if let Some(control) = control {
            control.output_size_changed(size);
        }
    }
    match encoder.cpu_scale() {
        Some(scale) => {
            shared.counters.record_scale(scale.took);
            if *scaling != Some((scale.from, scale.to)) {
                *scaling = Some((scale.from, scale.to));
                info!(
                    %scale,
                    "reducción en CPU al tope de resolución (libswscale); el coste va en la línea \
                     periódica del anfitrión"
                );
            }
        }
        None => *scaling = None,
    }
}

/// Deja los slices de un fotograma seguidos en `out` (la ruta de [`Encoder::repeat_last`]).
fn concat_slices(slices: Vec<EncodedSlice>, out: &mut Vec<u8>) -> Option<EncodedFrame> {
    out.clear();
    let first = slices.first()?;
    let encoded = EncodedFrame {
        frame_id: first.frame_id,
        keyframe: slices.iter().any(|slice| slice.keyframe),
    };
    for slice in &slices {
        out.extend_from_slice(&slice.data);
    }
    Some(encoded)
}

pub(crate) fn encode_loop(
    mut encoder: Option<Box<dyn Encoder>>,
    rx: Receiver<CapturedFrame>,
    tx: Sender<Compressed>,
    shared: &Shared,
    ledger: &LatencyLedger,
    control: Option<&EncodeControl>,
) -> Result<(), PipelineError> {
    let mut gate = BitrateGate::default();
    // Último fotograma, para volver a codificarlo como intra si se pide uno con la pantalla quieta
    // (o con una ventana que no se redibuja: oculta, minimizada o un juego en pausa). Se guardan
    // los de CPU y los DMA-BUF de PipeWire, que retienen uno de sus 4 a 16 búferes
    // ([`can_repeat`]). Los D3D11 no: WGC tiene tres y la captura se quedaría sin dónde escribir;
    // con ellos repite el propio codificador desde su copia ([`Encoder::repeat_last`]).
    let mut last: Option<RawFrame> = None;
    // Id del último fotograma guardado u ofrecido a la vista previa (para volver a guardarlo
    // cuando lo devuelva, si no llegó otro mientras).
    let mut last_id: Option<u64> = None;
    let mut last_at = Instant::now();
    // La vista previa se reduce en su propio hilo con el fotograma que acaba de codificarse.
    let mut preview = PreviewWorker::new();
    // Tamaño del último fotograma comprimido, para reservar el siguiente de una vez.
    let mut expected_len = 0usize;
    // Tamaño del flujo que produce el codificador (la captura reducida al tope si no cabe) y la
    // reducción en CPU en marcha, para anunciarlos una vez cuando cambian.
    let mut output_size: Option<(u32, u32)> = None;
    let mut scaling: Option<ScalePair> = None;
    // Ya se avisó de que el codificador no lee las imágenes de GPU de la captura (se avisa una vez;
    // los fotogramas de GPU que siguen llegando hasta que la captura pasa a CPU se descartan).
    let mut gpu_import_warned = false;
    loop {
        if shared.stopping() {
            break;
        }
        for back in preview.returned() {
            if last.is_none() && last_id == Some(back.frame_id) {
                last = Some(back);
            } else {
                recycle(back);
            }
        }
        if !shared.sharing() {
            // Se dejó de compartir: lo último que se vio no se guarda ni se repite (un intra
            // pedido por quien entra después no debe enseñar la pantalla de antes).
            if let Some(old) = last.take() {
                recycle(old);
            }
            last_id = None;
        }
        let input = match rx.recv_timeout(POLL) {
            Ok(mut frame) => {
                // Como `recv_latest`: el más reciente, y los demás cuentan como descartes.
                while let Ok(newer) = rx.try_recv() {
                    ledger.record_drop(Stage::Encoded);
                    coroom_capture::pool::recycle_frame(std::mem::replace(&mut frame, newer));
                }
                last_at = Instant::now();
                EncodeInput::Captured(frame)
            }
            Err(RecvTimeoutError::Timeout) => {
                if shared.sharing()
                    && shared.keyframe_requested.load(Ordering::Acquire)
                    && last_at.elapsed() >= IDLE_KEYFRAME
                {
                    // Un intento cada `IDLE_KEYFRAME` como mucho.
                    last_at = Instant::now();
                    EncodeInput::Repeat
                } else {
                    continue;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let raw = match input {
            EncodeInput::Captured(frame) => {
                // H.264 4:2:0 necesita un tamaño par: se recorta la última fila o columna si sobra.
                let raw = to_raw_frame(frame);
                if raw.width == 0 || raw.height == 0 {
                    ledger.record_drop(Stage::Encoded);
                    recycle(raw);
                    continue;
                }
                if shared.note_captured_size((raw.width, raw.height)) {
                    debug!(
                        width = raw.width,
                        height = raw.height,
                        "tamaño de la captura"
                    );
                }
                Some(raw)
            }
            // Sin imagen de CPU guardada, repite el codificador (si puede).
            EncodeInput::Repeat => last.take(),
        };
        let tap = shared
            .preview
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        // Sin invitados o sin códec no se codifica, pero se guarda la imagen: si la pantalla está
        // quieta cuando llegue uno, es la que se le envía.
        if let Some(control) = control {
            if let Some(next) = control.take_next() {
                // Códec nuevo (negociación): sale ya configurado y su primer fotograma es intra.
                encoder = Some(next);
                gate = BitrateGate::default();
            }
            if !control.active.load(Ordering::Acquire) {
                stash(raw, tap.as_ref(), &mut preview, &mut last, &mut last_id);
                continue;
            }
        }
        let Some(encoder) = encoder.as_mut() else {
            // Todavía no hay códec (el anfitrión espera las capacidades del invitado).
            stash(raw, tap.as_ref(), &mut preview, &mut last, &mut last_id);
            continue;
        };
        if let Some(control) = control {
            let target = control.bitrate_kbps.load(Ordering::Acquire);
            if let Some(kbps) = gate.decide(target, encoder.live_bitrate(), Instant::now()) {
                // Un bitrate que el codificador no acepta (con «Auto sin tope», uno por encima de lo
                // que admite su driver) no corta la sesión: sigue con el que tenía.
                match encoder.set_bitrate(kbps) {
                    Ok(()) => bump(&shared.counters.bitrate_changes),
                    Err(err) => warn!(kbps, error = %err, "el codificador no acepta ese bitrate"),
                }
            }
        }
        let forced = shared.take_keyframe_request();
        if forced {
            encoder.request_keyframe();
        }
        let started = Instant::now();
        // El paquete entero, de una vez, en el búfer que viaja al hilo `send`.
        let mut data = Vec::with_capacity(expected_len);
        let encoded = match &raw {
            Some(raw) => match encoder.encode_into(raw, &mut data) {
                Ok(encoded) => Ok(encoded),
                Err(CodecError::GpuImport(what)) => Err(what),
                Err(err) => return Err(PipelineError::Codec(err)),
            },
            None => match encoder.repeat_last() {
                Some(slices) => Ok(concat_slices(
                    slices.map_err(PipelineError::Codec)?,
                    &mut data,
                )),
                None => {
                    // Nada que repetir: el intra queda pedido para el siguiente fotograma.
                    if forced {
                        shared.request_keyframe();
                    }
                    continue;
                }
            },
        };
        let encoded = match encoded {
            Ok(encoded) => encoded,
            Err(what) => {
                // El codificador que se negoció (NVENC, software…) no importa ni descarga las
                // imágenes de GPU que entrega la captura: la captura pasa a memoria de CPU en
                // marcha y este fotograma se pierde. Antes la sala se cerraba.
                if !gpu_import_warned {
                    gpu_import_warned = true;
                    warn!(
                        encoder = %encoder.backend(),
                        error = %what,
                        "la captura pasa a memoria de CPU: el codificador no lee sus imágenes"
                    );
                }
                shared.set_capture_gpu_frames(GpuFrames::Off);
                ledger.record_drop(Stage::Encoded);
                if forced {
                    shared.request_keyframe();
                }
                if let Some(raw) = raw {
                    recycle(raw);
                }
                continue;
            }
        };
        if forced {
            bump(&shared.counters.forced_keyframes);
        }
        let encoded_at = Instant::now();
        shared.counters.record_encode(encoded_at - started);
        note_output_size(
            encoder.as_ref(),
            control,
            &mut output_size,
            &mut scaling,
            shared,
        );
        // El fotograma repetido conserva su id: sus marcas de captura son las del original.
        let Some(frame_id) = raw
            .as_ref()
            .map(|raw| raw.frame_id)
            .or(encoded.map(|encoded| encoded.frame_id))
        else {
            ledger.record_drop(Stage::Encoded);
            continue;
        };
        // Una imagen D3D11 (y el búfer de la captura que retiene) se suelta en cuanto se codifica;
        // la de CPU va a la vista previa si le toca y se guarda para repetirla.
        stash(raw, tap.as_ref(), &mut preview, &mut last, &mut last_id);
        ledger.record_cpu_copies(Stage::Encoded, encoder.cpu_copies());
        let Some(encoded) = encoded else {
            // El codificador se saltó el fotograma.
            ledger.record_drop(Stage::Encoded);
            continue;
        };
        let keyframe = encoded.keyframe;
        expected_len = data.len() + data.len() / 4;
        ledger.mark(frame_id, Stage::Encoded, encoded_at);
        if control.is_none() {
            // En el bucle local no se cifra: la etapa dura 0.
            ledger.mark(frame_id, Stage::Encrypted, encoded_at);
        }
        ledger.record_encoded_bytes_at(data.len() as u64, encoded_at);
        bump(&shared.counters.encoded);
        shared
            .counters
            .encoded_bytes
            .fetch_add(data.len() as u64, Ordering::Relaxed);
        let compressed = Compressed {
            frame_id,
            keyframe,
            data,
        };
        // Contrapresión, no desalojo: un fotograma ya codificado es referencia del siguiente, y
        // tirarlo obligaba a pedir un intra (el más grande, justo cuando el envío iba justo). Si el
        // hilo `send` no da abasto, se espera: lo que se acumula es captura sin codificar, que se
        // desaloja gratis en el canal anterior.
        if send_or_stop(&tx, compressed, shared).is_err() {
            break;
        }
    }
    Ok(())
}

/// Mete `item` en el canal esperando a que haya sitio; `Err` si hay que parar o el consumidor ya no
/// existe.
pub(crate) fn send_or_stop<T>(tx: &Sender<T>, mut item: T, shared: &Shared) -> Result<(), ()> {
    loop {
        match tx.send_timeout(item, POLL) {
            Ok(()) => return Ok(()),
            Err(SendTimeoutError::Timeout(back)) => {
                if shared.stopping() {
                    return Err(());
                }
                item = back;
            }
            Err(SendTimeoutError::Disconnected(_)) => return Err(()),
        }
    }
}

fn send_loop(
    sender: UdpSender,
    rx: Receiver<Compressed>,
    shared: &Shared,
    ledger: &LatencyLedger,
) -> Result<(), PipelineError> {
    while let Some(frame) = recv_or_stop(&rx, shared) {
        // Los ids de la cabecera son de 32 bits; el receptor reconstruye los altos.
        let wire_id = frame.frame_id as u32;
        sender
            .send_frame(wire_id, frame.keyframe, &frame.data)
            .map_err(|err| PipelineError::Transport(err.to_string()))?;
        ledger.mark(frame.frame_id, Stage::Sent, Instant::now());
        bump(&shared.counters.sent);
    }
    Ok(())
}

fn recv_loop(
    mut receiver: UdpReceiver,
    tx: Sender<Compressed>,
    evict: Receiver<Compressed>,
    shared: &Shared,
    ledger: &LatencyLedger,
) -> Result<(), PipelineError> {
    let mut last_id = None;
    let mut dropped = 0;
    while !shared.stopping() {
        let frame = receiver
            .recv_frame(POLL)
            .map_err(|err| PipelineError::Transport(err.to_string()))?;
        let now = Instant::now();
        let lost = receiver.dropped() - dropped;
        if lost > 0 {
            dropped = receiver.dropped();
            for _ in 0..lost {
                ledger.record_drop(Stage::Received);
            }
            shared.request_keyframe();
        }
        let Some(frame) = frame else {
            continue;
        };
        let frame_id = extend_frame_id(last_id, frame.frame_id);
        last_id = Some(frame_id);
        ledger.mark(frame_id, Stage::Received, now);
        // En el bucle local no se descifra: la etapa dura 0.
        ledger.mark(frame_id, Stage::Decrypted, now);
        let compressed = Compressed {
            frame_id,
            keyframe: frame.keyframe,
            data: frame.data,
        };
        match send_latest(&tx, &evict, compressed, ledger, Stage::Decoded) {
            Ok(true) => shared.request_keyframe(),
            Ok(false) => {}
            Err(()) => break,
        }
    }
    Ok(())
}

pub(crate) fn decode_loop(
    mut decoder: Option<Box<dyn Decoder>>,
    rx: Receiver<Compressed>,
    tx: Sender<RawFrame>,
    evict: Receiver<RawFrame>,
    shared: &Shared,
    ledger: &LatencyLedger,
    control: Option<&DecodeControl>,
) -> Result<(), PipelineError> {
    // Formato del decodificador que pidió `control` (el de la sesión remota; en local, el del
    // decodificador que se dio).
    let mut opened_format: Option<VideoFormat> = None;
    // Lo pedido que no se pudo abrir: no se reintenta en cada fotograma, sino cuando cambie.
    let mut unavailable: Option<(Codec, VideoFormat)> = None;
    let mut health = DecodeHealth::default();
    // En la sesión remota los ids son los grupos, consecutivos: un hueco es un fotograma perdido
    // (grupo vencido, descartado en la cola o sin descifrar). En local no: los ids son los de la
    // captura, que tiene sus propios descartes antes de codificar.
    let mut last_id: Option<u64> = None;
    // Tras una pérdida, un error o al abrir el decodificador se espera al siguiente intra:
    // decodificar contra referencias que faltan da basura sin error en AV1 (dav1d y los de
    // hardware no avisan) y artefactos que se arrastran en H.264 y HEVC.
    let mut need_keyframe = control.is_some();
    let mut last_none_request: Option<Instant> = None;
    // Tamaño de lo último decodificado. Cuando el anfitrión cambia la resolución en vivo (mismo
    // códec), el decodificador de hardware sigue con el contexto de imágenes del tamaño anterior
    // en algunos backends y presenta basura sin dar error: se vuelve a abrir.
    let mut decoded_size: Option<(u32, u32)> = None;
    while let Some(frame) = recv_or_stop(&rx, shared) {
        let now = Instant::now();
        if control.is_some_and(DecodeControl::take_reopen) {
            debug!("decodificador por software o no: se vuelve a abrir");
            decoder = None;
            opened_format = None;
            unavailable = None;
        }
        if control.is_some() {
            if last_id.is_some_and(|last| frame.frame_id != last.wrapping_add(1)) {
                need_keyframe = true;
            }
            last_id = Some(frame.frame_id);
        }
        if let Some(control) = control
            && let Some(wanted @ (codec, format)) = control.wanted()
            && (decoder
                .as_ref()
                .is_none_or(|current| current.codec() != codec)
                || opened_format != Some(format))
            && unavailable != Some(wanted)
        {
            decoder = None;
            opened_format = None;
            // El decodificador nuevo ya empieza con el tamaño que traiga.
            decoded_size = None;
            match control.open(codec, format) {
                Ok(opened) => {
                    set_decoder_label(ledger, codec, format, opened.backend());
                    decoder = Some(opened);
                    opened_format = Some(format);
                    unavailable = None;
                }
                Err(err) => {
                    // Ningún decodificador de este códec abre: se renegocia sin él.
                    warn!(%codec, %format, error = %err, "no abre ningún decodificador");
                    unavailable = Some(wanted);
                    control.give_up_codec(codec);
                }
            }
            health = DecodeHealth::default();
            // Lo anterior era de otro códec o formato: hace falta un intra del nuevo.
            need_keyframe = true;
            shared.request_keyframe();
        }
        let Some(current) = decoder.as_mut() else {
            // Aún no se sabe el códec (no ha llegado `CodecSelected`).
            ledger.record_drop(Stage::Decoded);
            shared.request_keyframe();
            continue;
        };
        if need_keyframe && !frame.keyframe {
            ledger.record_drop(Stage::Decoded);
            shared.request_keyframe();
            continue;
        }
        need_keyframe = false;
        let slice = EncodedSlice {
            frame_id: frame.frame_id,
            index: 0,
            count: 1,
            keyframe: frame.keyframe,
            data: frame.data,
        };
        let result = current.decode(std::slice::from_ref(&slice));
        shared.counters.record_decode(now.elapsed());
        match result {
            Ok(Some(image)) if control.is_some() && resized(&mut decoded_size, &image) => {
                // Este fotograma ya viene del decodificador viejo: se tira, se abre otro (en la
                // vuelta siguiente, con el mismo códec y formato) y se espera un intra.
                debug!(
                    width = image.width,
                    height = image.height,
                    "cambió el tamaño del vídeo: se vuelve a abrir el decodificador"
                );
                ledger.record_drop(Stage::Decoded);
                decoder = None;
                opened_format = None;
                need_keyframe = true;
                shared.request_keyframe();
                continue;
            }
            Ok(Some(mut image)) => {
                health = DecodeHealth::default();
                ledger.mark(frame.frame_id, Stage::Decoded, Instant::now());
                ledger.record_cpu_copies(Stage::Decoded, current.cpu_copies());
                bump(&shared.counters.decoded);
                image.frame_id = frame.frame_id;
                // El espacio de color negociado manda (`CodecSelected`).
                if let Some(format) = opened_format {
                    image.color = format.color;
                }
                if send_latest(&tx, &evict, image, ledger, Stage::Presented).is_err() {
                    break;
                }
            }
            Ok(None) => {
                ledger.record_drop(Stage::Decoded);
                health.failed(frame.keyframe, false, now);
                // Un fotograma normal sin imagen: el decodificador se quedó sin referencias
                // sin decir nada. Se pide un intra (sin insistir más que el invitado).
                if !frame.keyframe
                    && last_none_request
                        .is_none_or(|at| now.saturating_duration_since(at) >= DECODER_RETRY)
                {
                    shared.request_keyframe();
                    last_none_request = Some(now);
                }
            }
            Err(_) => {
                // Fotograma corrupto o sin referencias: se descarta y se pide un intra.
                ledger.record_drop(Stage::Decoded);
                ledger.record_decode_error();
                current.reset();
                shared.request_keyframe();
                health.failed(frame.keyframe, true, now);
                need_keyframe = control.is_some();
            }
        }
        if let Some(control) = control
            && health.broken(now)
            && let Some(failed) = decoder.take()
        {
            // El decodificador no saca imagen con los intra que recibe: el siguiente del orden.
            let (codec, backend) = (failed.codec(), failed.backend());
            drop(failed);
            bump(&shared.counters.decoder_fallbacks);
            control.exclude(codec, backend);
            health = DecodeHealth::default();
            need_keyframe = true;
            shared.request_keyframe();
            let format = opened_format.unwrap_or(VideoFormat::SDR_420);
            match control.open(codec, format) {
                Ok(next) => {
                    warn!(
                        from = %decoder_label(codec, backend),
                        to = %decoder_label(codec, next.backend()),
                        "el decodificador no saca imagen; se cambia"
                    );
                    set_decoder_label(ledger, codec, format, next.backend());
                    decoder = Some(next);
                }
                Err(_) => {
                    warn!(
                        %codec,
                        "ningún decodificador de este códec saca imagen; se renegocia sin él"
                    );
                    opened_format = None;
                    unavailable = control.wanted();
                    control.give_up_codec(codec);
                }
            }
        }
    }
    Ok(())
}

/// Fallos con un intra ya entregado (sin ninguna imagen desde entonces) tras los que se deja el
/// decodificador.
pub(crate) const FALLBACK_ERRORS: u32 = 4;

/// Tiempo sin imagen desde el primer intra entregado (con al menos dos) tras el que se deja el
/// decodificador.
pub(crate) const FALLBACK_STALL: Duration = Duration::from_secs(2);

/// Intervalo mínimo entre dos intra que pide el decodificador porque un fotograma no dio imagen.
const DECODER_RETRY: Duration = Duration::from_millis(250);

/// Si el decodificador funciona: cuenta los intra que se le dieron sin que saliera ninguna imagen
/// y los errores desde el primero de ellos. Los errores de antes de un intra no cuentan: son los
/// de una pérdida normal (faltan referencias hasta que llegue el intra pedido).
#[derive(Debug, Default)]
struct DecodeHealth {
    keyframes: u32,
    errors: u32,
    since: Option<Instant>,
}

impl DecodeHealth {
    fn failed(&mut self, keyframe: bool, error: bool, now: Instant) {
        if keyframe {
            self.keyframes += 1;
            self.since.get_or_insert(now);
        }
        if error && self.keyframes > 0 {
            self.errors += 1;
        }
    }

    fn broken(&self, now: Instant) -> bool {
        self.errors >= FALLBACK_ERRORS
            || (self.keyframes >= 2
                && self
                    .since
                    .is_some_and(|since| now.saturating_duration_since(since) >= FALLBACK_STALL))
    }
}

/// Texto del vídeo del invitado: `AV1 · decodifica dav1d`.
fn set_decoder_label(
    ledger: &LatencyLedger,
    codec: Codec,
    format: VideoFormat,
    backend: DecoderBackend,
) {
    let suffix = if format.is_default() {
        String::new()
    } else {
        format!(" · {format}")
    };
    ledger.set_video(format!(
        "{codec} · decodifica {}{suffix}",
        decoder_label(codec, backend)
    ));
}

pub(crate) fn present_loop(
    mut sink: Box<dyn FrameSink>,
    rx: Receiver<RawFrame>,
    frames: Option<u64>,
    shared: &Shared,
    ledger: &LatencyLedger,
) -> Result<(), PipelineError> {
    let mut presented = 0u64;
    // El cursor entre fotogramas va directo al sumidero mientras este hilo viva.
    shared.cursor_sink.set(sink.cursor_sink());
    let result = present_frames(sink.as_mut(), &rx, frames, shared, ledger, &mut presented);
    shared.cursor_sink.set(None);
    result
}

fn present_frames(
    sink: &mut dyn FrameSink,
    rx: &Receiver<RawFrame>,
    frames: Option<u64>,
    shared: &Shared,
    ledger: &LatencyLedger,
    presented: &mut u64,
) -> Result<(), PipelineError> {
    while let Some(frame) = recv_latest(rx, shared, ledger, Stage::Presented) {
        let frame_id = frame.frame_id;
        let cursor = *shared.cursor.lock().unwrap_or_else(PoisonError::into_inner);
        let cursor_shape = cursor.and_then(|cursor| shared.cursor_shape(cursor.shape_id));
        sink.present(DecodedFrame {
            frame,
            cursor,
            cursor_shape,
        })
        .map_err(PipelineError::Sink)?;
        if sink.shown() {
            ledger.mark(frame_id, Stage::Presented, Instant::now());
        } else {
            // La ventana no podía mostrarlo (oculta): no es una latencia, es un descarte.
            ledger.record_drop(Stage::Presented);
        }
        ledger.record_cpu_copies(Stage::Presented, sink.cpu_copies());
        bump(&shared.counters.presented);
        *presented += 1;
        if frames.is_some_and(|limit| *presented >= limit) {
            shared.finish(None);
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use coroom_capture::synthetic::SyntheticCapturer;
    use coroom_codec::openh264::{OpenH264Decoder, OpenH264Encoder};

    /// Las opciones nuevas de la captura se recogen una vez (gana la última) y la que se adopte
    /// después las encuentra.
    #[test]
    fn live_capture_options_are_taken_once_and_kept_for_the_next_capturer() {
        let shared = Shared::default();
        assert_eq!(shared.take_capture_options(), None);
        assert_eq!(shared.latest_capture_options(), None);
        let first = CaptureOptions {
            max_fps: 30,
            hdr: false,
            max_size: Some((640, 360)),
        };
        let second = CaptureOptions {
            max_fps: 15,
            ..first
        };
        shared.set_capture_options(first);
        shared.set_capture_options(second);
        assert_eq!(shared.take_capture_options(), Some(second));
        assert_eq!(shared.take_capture_options(), None, "ya aplicadas");
        assert_eq!(shared.latest_capture_options(), Some(second));
    }

    /// El tamaño del flujo es el de la captura reducido al tope, con la misma regla en todas
    /// partes.
    #[test]
    fn the_stream_size_is_the_capture_fitted_into_the_maximum() {
        let video = LoopbackConfig {
            width: 1920,
            height: 1080,
            ..LoopbackConfig::default()
        };
        assert_eq!(video.stream_size((3840, 2160)), (1920, 1080));
        assert_eq!(video.stream_size((2560, 1080)), (1920, 810));
        assert_eq!(
            video.stream_size((1366, 768)),
            (1366, 768),
            "lo que cabe no se toca"
        );
        assert_eq!(
            video.stream_size((3840, 2160)),
            coroom_capture::synthetic::fit((3840, 2160), (1920, 1080))
        );
        let shared = Shared::default();
        assert_eq!(shared.captured_size(), None);
        assert!(shared.note_captured_size((3840, 2160)));
        assert!(
            !shared.note_captured_size((3840, 2160)),
            "sin cambio no avisa"
        );
        assert!(shared.note_captured_size((1280, 720)));
        assert_eq!(shared.captured_size(), Some((1280, 720)));
    }

    /// Lo que se pide a la captura lleva el tamaño máximo del vídeo.
    #[test]
    fn capture_options_carry_the_maximum_size() {
        let video = LoopbackConfig {
            width: 1280,
            height: 720,
            fps: 90,
            ..LoopbackConfig::default()
        };
        assert_eq!(
            video.capture_options(),
            CaptureOptions {
                max_fps: 90,
                hdr: false,
                max_size: Some((1280, 720)),
            }
        );
    }

    #[test]
    fn frame_ids_survive_the_32_bit_wrap() {
        assert_eq!(extend_frame_id(None, 7), 7);
        assert_eq!(extend_frame_id(Some(7), 8), 8);
        assert_eq!(extend_frame_id(Some(u64::from(u32::MAX)), 0), 1 << 32);
        assert_eq!(
            extend_frame_id(Some((1 << 32) + 1), u32::MAX),
            u64::from(u32::MAX)
        );
        assert_eq!(extend_frame_id(Some((5 << 32) + 10), 12), (5 << 32) + 12);
    }

    #[test]
    fn latest_wins_drops_the_oldest() {
        let ledger = LatencyLedger::new();
        let (tx, rx) = bounded::<u32>(CHANNEL_CAPACITY);
        for i in 0..5 {
            send_latest(&tx, &rx, i, &ledger, Stage::Encoded).unwrap();
        }
        assert_eq!(rx.try_recv(), Ok(3));
        assert_eq!(rx.try_recv(), Ok(4));
        assert_eq!(ledger.summary().drops[Stage::Encoded.index()].count, 3);
    }

    #[test]
    fn the_consumer_takes_the_newest_frame() {
        let ledger = LatencyLedger::new();
        let shared = Shared::default();
        let (tx, rx) = bounded::<u32>(CHANNEL_CAPACITY);
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        assert_eq!(recv_latest(&rx, &shared, &ledger, Stage::Encoded), Some(2));
        assert_eq!(ledger.summary().drops[Stage::Encoded.index()].count, 1);
        drop(tx);
        assert_eq!(recv_latest(&rx, &shared, &ledger, Stage::Encoded), None);
    }

    #[test]
    fn a_small_pipeline_presents_every_stage() {
        let ledger = Arc::new(LatencyLedger::new());
        let config = LoopbackConfig {
            width: 320,
            height: 180,
            fps: 60,
            bitrate_kbps: 1_000,
            frames: Some(30),
            yuv444: false,
            hdr: false,
        };
        let pipeline = LoopbackPipeline::start(
            Box::new(SyntheticCapturer::new(320, 180, 60)),
            Box::new(OpenH264Encoder::new()),
            Box::new(OpenH264Decoder::new().unwrap()),
            Box::new(NullSink),
            Arc::clone(&ledger),
            config,
        )
        .unwrap();
        assert!(pipeline.wait(Duration::from_secs(20)), "terminó a tiempo");
        assert_eq!(pipeline.failure(), None);
        let summary = pipeline.stop();
        assert!(summary.frames_presented >= 30, "{summary}");
        let complete = ledger
            .timelines()
            .iter()
            .filter(|timeline| timeline.is_complete())
            .count();
        assert!(complete >= 30, "{summary}");
        for span in &summary.spans {
            assert!(span.samples >= 30, "{summary}");
        }
        // Software: captura sintética (0), conversión de OpenH264 (1), salida de OpenH264 (1) y
        // presentación nula (0).
        let per_stage: Vec<(Stage, f64)> = summary
            .cpu_copies
            .iter()
            .map(|copies| (copies.stage, copies.per_frame))
            .collect();
        assert_eq!(
            per_stage,
            vec![
                (Stage::Captured, 0.0),
                (Stage::Encoded, 1.0),
                (Stage::Decoded, 1.0),
                (Stage::Presented, 0.0)
            ],
            "{summary}"
        );
    }

    #[test]
    fn gpu_inputs_become_capture_requests() {
        assert_eq!(capture_gpu_frames(&GpuInput::None), GpuFrames::Off);
        assert_eq!(capture_gpu_frames(&GpuInput::D3D11), GpuFrames::D3D11);
        assert_eq!(
            capture_gpu_frames(&GpuInput::DmaBuf {
                modifiers: vec![0, 7]
            }),
            GpuFrames::DmaBuf {
                modifiers: vec![0, 7]
            }
        );
    }

    /// Un DMA-BUF de la captura llega al codificador sin copiar la imagen: se mueven sus
    /// descriptores y su `release` (el búfer de PipeWire) viaja con él.
    #[cfg(unix)]
    #[test]
    fn dmabuf_frames_move_into_the_encoder_input() {
        use std::os::fd::AsRawFd;

        let fd: std::os::fd::OwnedFd = std::fs::File::open("/dev/null").unwrap().into();
        let raw_fd = fd.as_raw_fd();
        let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        struct Release(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Release {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let frame = CapturedFrame {
            frame_id: 9,
            width: 101,
            height: 50,
            captured_at: Instant::now(),
            cursor: None,
            cursor_shape: None,
            pixels: FramePixels::DmaBuf(Arc::new(coroom_capture::DmaBufFrame {
                planes: vec![coroom_capture::DmaBufPlane {
                    fd,
                    offset: 0,
                    stride: 512,
                }],
                modifier: 3,
                fourcc: 0x3432_5258,
                width: 101,
                height: 50,
                release: Some(Arc::new(Release(Arc::clone(&released)))),
            })),
            cpu_copies: 0,
        };
        let raw = to_raw_frame(frame);
        assert_eq!(
            (raw.width, raw.height, raw.format),
            (100, 50, PixelFormat::Gpu)
        );
        assert!(raw.data.is_empty());
        let Some(coroom_codec::GpuImage::DmaBuf(image)) = &raw.gpu else {
            panic!("se esperaba un DMA-BUF");
        };
        assert_eq!(
            image.planes[0].fd.as_raw_fd(),
            raw_fd,
            "el descriptor se mueve"
        );
        assert_eq!((image.modifier, image.width), (3, 100));
        assert!(!released.load(Ordering::SeqCst));
        drop(raw);
        assert!(
            released.load(Ordering::SeqCst),
            "el búfer vuelve al soltar la imagen"
        );
    }

    /// Decodificador falso: da una imagen por fotograma si `works`, si no un error; anota los ids
    /// que le llegan.
    struct FakeDecoder {
        codec: Codec,
        backend: DecoderBackend,
        works: bool,
        seen: Arc<Mutex<Vec<u64>>>,
    }

    impl Decoder for FakeDecoder {
        fn codec(&self) -> Codec {
            self.codec
        }

        fn backend(&self) -> DecoderBackend {
            self.backend
        }

        fn decode(&mut self, slices: &[EncodedSlice]) -> Result<Option<RawFrame>, CodecError> {
            let id = slices.first().map_or(0, |slice| slice.frame_id);
            self.seen.lock().unwrap().push(id);
            if self.works {
                Ok(Some(RawFrame::cpu(
                    id,
                    (2, 2),
                    PixelFormat::I420,
                    2,
                    vec![0; RawFrame::i420_len(2, 2)],
                )))
            } else {
                Err(CodecError::Backend("falla siempre".into()))
            }
        }

        fn reset(&mut self) {}
    }

    /// Hace pasar `frames` (`(id, intra)`) por `decode_loop` con `control` y devuelve los ids de
    /// las imágenes que salieron.
    fn run_decode(control: &DecodeControl, shared: &Shared, frames: &[(u64, bool)]) -> Vec<u64> {
        let ledger = LatencyLedger::new();
        let (tx, rx) = bounded::<Compressed>(frames.len().max(1));
        let (out_tx, out_rx) = bounded::<RawFrame>(frames.len().max(1));
        for &(frame_id, keyframe) in frames {
            tx.send(Compressed {
                frame_id,
                keyframe,
                data: vec![1, 2, 3],
            })
            .unwrap();
        }
        drop(tx);
        decode_loop(
            None,
            rx,
            out_tx,
            out_rx.clone(),
            shared,
            &ledger,
            Some(control),
        )
        .unwrap();
        out_rx.try_iter().map(|frame| frame.frame_id).collect()
    }

    /// Abre `FakeDecoder` con el primer backend que quede de `codec`; los de `broken` fallan.
    fn fake_opener(
        broken: Vec<(Codec, DecoderBackend)>,
        seen: Arc<Mutex<Vec<u64>>>,
    ) -> DecoderOpener {
        Box::new(move |set, codec, _format| {
            let &(_, backend) = set
                .available()
                .iter()
                .find(|(c, _)| *c == codec)
                .ok_or_else(|| CodecError::Unsupported("no queda ninguno".into()))?;
            Ok(Box::new(FakeDecoder {
                codec,
                backend,
                works: !broken.contains(&(codec, backend)),
                seen: Arc::clone(&seen),
            }))
        })
    }

    fn av1_set() -> DecoderSet {
        DecoderSet::new([
            (Codec::Av1, DecoderBackend::Vaapi),
            (Codec::Av1, DecoderBackend::Software),
            (Codec::H264, DecoderBackend::Software),
        ])
    }

    /// «Por software» en marcha: solo quedan los de software, se pide volver a abrir y
    /// renegociar; al quitarlo vuelven todos.
    #[test]
    fn the_software_decoder_switches_live() {
        let control = DecodeControl::new(av1_set());
        assert!(
            control
                .usable()
                .available()
                .contains(&(Codec::Av1, DecoderBackend::Vaapi))
        );
        control.set_software_only(true);
        assert!(control.take_reopen());
        assert!(control.take_renegotiation());
        assert!(
            control
                .usable()
                .available()
                .iter()
                .all(|(_, backend)| *backend == DecoderBackend::Software)
        );
        control.set_software_only(true);
        assert!(!control.take_reopen(), "sin cambio no se reabre");
        control.set_software_only(false);
        assert!(control.take_reopen());
        assert_eq!(control.usable().available().len(), 3);
    }

    /// Un decodificador que nunca saca imagen se deja por el siguiente del orden.
    #[test]
    fn a_broken_decoder_falls_back_to_the_next_backend() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let control = DecodeControl::new(av1_set()).with_opener(fake_opener(
            vec![(Codec::Av1, DecoderBackend::Vaapi)],
            Arc::clone(&seen),
        ));
        control.request(Codec::Av1, VideoFormat::SDR_420);
        let shared = Shared::default();
        let frames: Vec<(u64, bool)> = (1..=12).map(|id| (id, true)).collect();
        let decoded = run_decode(&control, &shared, &frames);
        assert_eq!(shared.counters.decoder_fallbacks.load(Ordering::Relaxed), 1);
        // Los cuatro primeros fallan con VA-API; desde el quinto, dav1d.
        assert_eq!(decoded, (5..=12).collect::<Vec<_>>());
        assert_eq!(
            control.usable().available(),
            &[
                (Codec::Av1, DecoderBackend::Software),
                (Codec::H264, DecoderBackend::Software)
            ]
        );
        assert!(!control.take_renegotiation());
    }

    /// Si no queda ningún decodificador del códec que funcione, se pide renegociar sin él (el
    /// anfitrión baja a H.264) y el invitado anuncia solo lo que le queda.
    #[test]
    fn without_working_decoders_the_guest_renegotiates_without_the_codec() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let control = DecodeControl::new(av1_set()).with_opener(fake_opener(
            vec![
                (Codec::Av1, DecoderBackend::Vaapi),
                (Codec::Av1, DecoderBackend::Software),
            ],
            Arc::clone(&seen),
        ));
        control.request(Codec::Av1, VideoFormat::SDR_420);
        let shared = Shared::default();
        let frames: Vec<(u64, bool)> = (1..=20).map(|id| (id, true)).collect();
        assert!(run_decode(&control, &shared, &frames).is_empty());
        assert!(control.take_renegotiation());
        assert_eq!(
            control.usable().available(),
            &[(Codec::H264, DecoderBackend::Software)]
        );
        // Lo que no abre no se reintenta en cada fotograma: después de los dos respaldos, nada.
        assert_eq!(seen.lock().unwrap().len(), 8);
        // El anfitrión pasa a H.264: se abre y decodifica.
        control.request(Codec::H264, VideoFormat::SDR_420);
        let decoded = run_decode(&control, &shared, &[(21, false), (22, true), (23, false)]);
        assert_eq!(decoded, vec![22, 23], "espera al intra del códec nuevo");
    }

    /// Tras un grupo perdido (un hueco en los ids) no se decodifica contra referencias que faltan:
    /// se descarta hasta el siguiente intra y se pide uno (AV1 no da error, da basura).
    #[test]
    fn after_a_lost_group_frames_wait_for_the_next_keyframe() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let control =
            DecodeControl::new(av1_set()).with_opener(fake_opener(Vec::new(), Arc::clone(&seen)));
        control.request(Codec::Av1, VideoFormat::SDR_420);
        let shared = Shared::default();
        // El primer intra se perdió: llegan 1 y 2 (sin intra), luego el 3 (intra), el 4, se
        // pierde el 5, y llegan 6, 7 y el intra 8.
        let frames = [
            (1, false),
            (2, false),
            (3, true),
            (4, false),
            (6, false),
            (7, false),
            (8, true),
            (9, false),
        ];
        let decoded = run_decode(&control, &shared, &frames);
        assert_eq!(decoded, vec![3, 4, 8, 9]);
        assert_eq!(*seen.lock().unwrap(), vec![3, 4, 8, 9]);
        assert!(shared.take_keyframe_request(), "se pidió un intra");
    }

    /// Codificador falso: cuenta los cambios de bitrate y los fotogramas, y anota si cada uno fue
    /// intra.
    struct FakeEncoder {
        live: bool,
        bitrates: Arc<AtomicU32>,
        keyframes: Arc<Mutex<Vec<(u64, bool)>>>,
        keyframe: bool,
        /// Fotogramas que rechaza con [`CodecError::GpuImport`] (como NVENC con DMA-BUF) antes
        /// de codificar normalmente.
        gpu_import_failures: u32,
    }

    impl Encoder for FakeEncoder {
        fn codec(&self) -> Codec {
            Codec::H264
        }

        fn backend(&self) -> coroom_codec::Backend {
            coroom_codec::Backend::Software
        }

        fn configure(&mut self, _config: &EncoderConfig) -> Result<(), CodecError> {
            Ok(())
        }

        fn set_bitrate(&mut self, _bitrate_kbps: u32) -> Result<(), CodecError> {
            self.bitrates.fetch_add(1, Ordering::SeqCst);
            if !self.live {
                // Reabrir: el siguiente fotograma es intra.
                self.keyframe = true;
            }
            Ok(())
        }

        fn live_bitrate(&self) -> bool {
            self.live
        }

        fn request_keyframe(&mut self) {
            self.keyframe = true;
        }

        fn encode(&mut self, frame: &RawFrame) -> Result<Vec<EncodedSlice>, CodecError> {
            if self.gpu_import_failures > 0 {
                self.gpu_import_failures -= 1;
                return Err(CodecError::GpuImport("de prueba".into()));
            }
            let keyframe = std::mem::take(&mut self.keyframe);
            self.keyframes
                .lock()
                .unwrap()
                .push((frame.frame_id, keyframe));
            Ok(vec![EncodedSlice {
                frame_id: frame.frame_id,
                index: 0,
                count: 1,
                keyframe,
                data: vec![0; 16],
            }])
        }
    }

    fn cpu_frame(frame_id: u64) -> CapturedFrame {
        CapturedFrame {
            frame_id,
            width: 4,
            height: 4,
            captured_at: Instant::now(),
            cursor: None,
            cursor_shape: None,
            pixels: FramePixels::Bgra {
                data: vec![0; 64],
                stride: 16,
            },
            cpu_copies: 0,
        }
    }

    /// Lanza `encode_loop` con un `FakeEncoder` y un `EncodeControl` activo.
    struct EncodeHarness {
        shared: Arc<Shared>,
        control: Arc<EncodeControl>,
        frames: Sender<CapturedFrame>,
        bitrates: Arc<AtomicU32>,
        encoded: Arc<Mutex<Vec<(u64, bool)>>>,
        thread: JoinHandle<()>,
        _out: Receiver<Compressed>,
    }

    impl EncodeHarness {
        fn start(live: bool, kbps: u32) -> Self {
            Self::start_with(live, kbps, 0)
        }

        fn start_with(live: bool, kbps: u32, gpu_import_failures: u32) -> Self {
            let shared = Arc::new(Shared::default());
            let control = Arc::new(EncodeControl::new(kbps));
            control.active.store(true, Ordering::SeqCst);
            let bitrates = Arc::new(AtomicU32::new(0));
            let encoded = Arc::new(Mutex::new(Vec::new()));
            let encoder = FakeEncoder {
                live,
                bitrates: Arc::clone(&bitrates),
                keyframes: Arc::clone(&encoded),
                keyframe: true,
                gpu_import_failures,
            };
            let (frames, rx) = bounded::<CapturedFrame>(CHANNEL_CAPACITY);
            let (tx, out) = bounded::<Compressed>(4096);
            let thread = {
                let (shared, control) = (Arc::clone(&shared), Arc::clone(&control));
                thread::spawn(move || {
                    let ledger = LatencyLedger::new();
                    encode_loop(
                        Some(Box::new(encoder)),
                        rx,
                        tx,
                        &shared,
                        &ledger,
                        Some(&control),
                    )
                    .unwrap();
                })
            };
            Self {
                shared,
                control,
                frames,
                bitrates,
                encoded,
                thread,
                _out: out,
            }
        }

        fn stop(self) -> (u32, Vec<(u64, bool)>) {
            self.shared.stop.store(true, Ordering::SeqCst);
            drop(self.frames);
            self.thread.join().unwrap();
            let encoded = self.encoded.lock().unwrap().clone();
            (self.bitrates.load(Ordering::SeqCst), encoded)
        }
    }

    /// Con un objetivo que oscila (como el del control de tasa: pasos del 10 % y alguno grande),
    /// un codificador sin bitrate en caliente se reabre pocas veces; uno con él lo sigue siempre.
    #[test]
    fn a_fluctuating_target_reopens_a_non_live_encoder_a_bounded_number_of_times() {
        for live in [false, true] {
            let harness = EncodeHarness::start(live, 10_000);
            let targets = [
                10_000, 11_000, 12_100, 11_000, 7_000, 15_000, 10_000, 16_000,
            ];
            let mut changes = 0;
            for (i, frame) in (0..80u64).enumerate() {
                let target = targets[(i / 10) % targets.len()];
                let previous = harness.control.bitrate_kbps.swap(target, Ordering::SeqCst);
                if previous != target {
                    changes += 1;
                }
                harness.frames.send(cpu_frame(frame)).unwrap();
                thread::sleep(Duration::from_millis(10));
            }
            thread::sleep(Duration::from_millis(50));
            let (bitrates, encoded) = harness.stop();
            let keyframes = encoded.iter().filter(|(_, keyframe)| *keyframe).count();
            if live {
                // El primero y cada cambio.
                assert_eq!(bitrates as usize, changes + 1, "{encoded:?}");
            } else {
                // El primero y la bajada de ≥ 20 % (a 7 000); los pasos del 10 % no, y las subidas
                // tampoco: no han pasado 4 s desde la apertura. Un intra por cada uno.
                assert_eq!(bitrates, 2, "{bitrates} reaperturas de {changes} cambios");
                assert_eq!(keyframes, 2, "{encoded:?}");
            }
        }
    }

    /// Un codificador que no lee las imágenes de GPU de la captura (NVENC con DMA-BUF, software
    /// con D3D11) no cierra la sala: el hilo `encode` pide la captura en memoria de CPU, descarta
    /// esos fotogramas y sigue con los que llegan después.
    #[test]
    fn an_encoder_that_cannot_import_gpu_images_switches_the_capture_to_cpu() {
        let harness = EncodeHarness::start_with(true, 10_000, 2);
        assert_eq!(harness.shared.take_capture_gpu_frames(), None);
        for frame in 0..4 {
            harness.frames.send(cpu_frame(frame)).unwrap();
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(50));
        assert_eq!(
            harness.shared.take_capture_gpu_frames(),
            Some(GpuFrames::Off),
            "pide memoria de CPU para la captura en marcha"
        );
        assert_eq!(harness.shared.take_capture_gpu_frames(), None, "una vez");
        assert_eq!(
            harness.shared.latest_capture_gpu_frames(),
            Some(GpuFrames::Off),
            "y la que se adopte después arranca así"
        );
        let (_, encoded) = harness.stop();
        let ids: Vec<u64> = encoded.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids,
            vec![2, 3],
            "los dos primeros se pierden; el hilo sigue"
        );
    }

    #[test]
    fn the_capture_leaves_gpu_images_only_for_an_encoder_that_reads_none() {
        let dmabuf = GpuFrames::DmaBuf { modifiers: vec![0] };
        assert_eq!(
            capture_frames_for(&dmabuf, &GpuInput::None),
            Some(GpuFrames::Off),
            "NVENC o software con DMA-BUF"
        );
        assert_eq!(
            capture_frames_for(&GpuFrames::D3D11, &GpuInput::None),
            Some(GpuFrames::Off),
            "software con D3D11"
        );
        assert_eq!(
            capture_frames_for(&dmabuf, &GpuInput::DmaBuf { modifiers: vec![1] }),
            None,
            "con GPU en los dos decide el codificador al importar"
        );
        assert_eq!(capture_frames_for(&GpuFrames::Off, &GpuInput::None), None);
        assert_eq!(
            capture_frames_for(&GpuFrames::Off, &GpuInput::D3D11),
            None,
            "en CPU no hay nada que bajar"
        );
    }

    /// Con la pantalla quieta, un intra pedido sale repitiendo el último fotograma (antes había
    /// que esperar a que la pantalla cambiase).
    #[test]
    fn a_keyframe_request_on_a_still_screen_repeats_the_last_frame() {
        let harness = EncodeHarness::start(true, 10_000);
        for frame in 0..3 {
            harness.frames.send(cpu_frame(frame)).unwrap();
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(100));
        harness.shared.request_keyframe();
        thread::sleep(IDLE_KEYFRAME + Duration::from_millis(300));
        let (_, encoded) = harness.stop();
        assert_eq!(encoded.len(), 4, "{encoded:?}");
        assert_eq!(encoded[3], (2, true), "el último otra vez, intra");
    }

    /// Lo mismo con DMA-BUF de PipeWire: la ventana capturada que deja de redibujarse (oculta o en
    /// pausa) no dejaba al invitado sin imagen. El búfer se retiene hasta el siguiente fotograma.
    #[cfg(unix)]
    #[test]
    fn a_keyframe_request_on_a_still_dmabuf_screen_repeats_the_last_frame() {
        use coroom_capture::{DmaBufFrame, DmaBufPlane};
        let released = Arc::new(AtomicU32::new(0));
        struct Release(Arc<AtomicU32>);
        impl Drop for Release {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let dmabuf_frame = |frame_id| CapturedFrame {
            pixels: FramePixels::DmaBuf(Arc::new(DmaBufFrame {
                planes: vec![DmaBufPlane {
                    fd: std::fs::File::open("/dev/null").unwrap().into(),
                    offset: 0,
                    stride: 16,
                }],
                modifier: 0,
                fourcc: coroom_codec::gpu::DRM_FORMAT_XRGB8888,
                width: 4,
                height: 4,
                release: Some(Arc::new(Release(Arc::clone(&released)))),
            })),
            ..cpu_frame(frame_id)
        };
        let harness = EncodeHarness::start(true, 10_000);
        for frame in 0..3 {
            harness.frames.send(dmabuf_frame(frame)).unwrap();
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(100));
        assert_eq!(
            released.load(Ordering::SeqCst),
            2,
            "solo se retiene el último"
        );
        harness.shared.request_keyframe();
        thread::sleep(IDLE_KEYFRAME + Duration::from_millis(300));
        let (_, encoded) = harness.stop();
        assert_eq!(encoded.len(), 4, "{encoded:?}");
        assert_eq!(encoded[3], (2, true), "el último otra vez, intra");
        assert_eq!(released.load(Ordering::SeqCst), 3, "al parar se devuelve");
    }

    #[test]
    fn the_bitrate_gate_spaces_reopens_but_not_live_changes() {
        let start = Instant::now();
        let mut gate = BitrateGate::default();
        assert_eq!(gate.decide(10_000, false, start), Some(10_000));
        // −10 %: no; −20 %: al momento.
        assert_eq!(gate.decide(9_000, false, start), None);
        assert_eq!(gate.decide(8_000, false, start), Some(8_000));
        // +25 % antes de 4 s desde la última subida (la apertura): no; pasados, sí.
        let at = |ms| start + Duration::from_millis(ms);
        assert_eq!(gate.decide(10_000, false, at(1_000)), None);
        assert_eq!(gate.decide(10_000, false, at(4_000)), Some(10_000));
        assert_eq!(gate.decide(13_000, false, at(5_000)), None);
        // En caliente, cualquier cambio.
        let mut live = BitrateGate::default();
        assert_eq!(live.decide(10_000, true, start), Some(10_000));
        assert_eq!(live.decide(10_100, true, start), Some(10_100));
    }

    #[test]
    fn start_reports_configuration_errors() {
        let err = LoopbackPipeline::start(
            Box::new(SyntheticCapturer::new(321, 180, 60)),
            Box::new(OpenH264Encoder::new()),
            Box::new(OpenH264Decoder::new().unwrap()),
            Box::new(NullSink),
            Arc::new(LatencyLedger::new()),
            LoopbackConfig {
                width: 321,
                ..LoopbackConfig::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, PipelineError::Codec(_)), "{err}");
    }
}
