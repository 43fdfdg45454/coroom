//! El vídeo dentro de la interfaz.
//!
//! - [`VideoFeed`]: el último fotograma de una sesión (o de la vista previa del anfitrión), con su
//!   cursor. El hilo `present` del pipeline lo deja aquí y espera ([`FeedSink`]) a que la ventana
//!   lo muestre, como antes esperaba a su presentador: la marca de presentación del registro de
//!   latencia sigue siendo la de verdad.
//! - [`video_view`]: *widget* de shader de `iced` que dibuja el fotograma con
//!   [`coroom_render::layer::VideoLayer`] **en la pasada de la interfaz y con su mismo
//!   `wgpu::Device`**. Los planos I420/NV12 se suben como siempre (una copia de CPU a GPU); las
//!   imágenes Vulkan del decodificador se importan sin copia con el
//!   [`coroom_render::wgpu::VideoImporter`] del dispositivo compartido.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use coroom_render::layer::VideoLayer;
use coroom_render::wgpu::VideoImporter;
use coroom_render::{CursorImage, CursorOverlay, PresentFrame, PresentImage};
use coroom_session::capture::CursorShape;
use coroom_session::loopback::{DecodedFrame, FrameSink, SinkError};
use iced_core::{Length, Rectangle, mouse};
use iced_wgpu::graphics::Viewport;
use iced_wgpu::primitive::{Pipeline, Primitive};
use iced_widget::shader;

use crate::probe::{to_cursor_image, to_present_frame};
use coroom_ui::Element;
use iced_wgpu::wgpu;

/// Lo que espera el hilo `present` a que la ventana muestre un fotograma (si la ventana no dibuja,
/// por ejemplo minimizada, el pipeline no se para: sigue y gana el último).
pub const SHOW_TIMEOUT: Duration = Duration::from_millis(100);

static NEXT_FEED: AtomicU64 = AtomicU64::new(1);

/// Margen HDR de la pantalla (bits de un `f32`; 1,0 por defecto), que la ventana pone al
/// configurar una superficie HDR.
static HDR_HEADROOM: AtomicU32 = AtomicU32::new(0x3f80_0000);

/// Margen de la pantalla en múltiplos del blanco SDR para el vídeo HDR
/// ([`coroom_render::hdr::headroom`]).
pub fn set_hdr_headroom(headroom: f32) {
    HDR_HEADROOM.store(headroom.max(1.0).to_bits(), Ordering::Relaxed);
}

fn hdr_headroom() -> f32 {
    f32::from_bits(HDR_HEADROOM.load(Ordering::Relaxed))
}

/// Avisa a la ventana de que hay algo nuevo que dibujar.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct FeedState {
    frame: Option<PresentFrame>,
    cursor: Option<CursorOverlay>,
    cursor_image: Option<Arc<CursorImage>>,
    /// Último fotograma subido a la GPU (pendiente de que la interfaz lo entregue).
    uploaded: Option<u64>,
    /// Último fotograma entregado a la cola con la interfaz.
    shown: Option<u64>,
    closed: bool,
    /// Copias de CPU del último fotograma subido.
    copies: u32,
}

/// Último fotograma de una fuente de vídeo, entre el pipeline y la ventana.
pub struct VideoFeed {
    id: u64,
    state: Mutex<FeedState>,
    shown: Condvar,
    wake: Wake,
    importer: Option<VideoImporter>,
}

impl std::fmt::Debug for VideoFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoFeed")
            .field("id", &self.id)
            .field("importer", &self.importer.is_some())
            .finish_non_exhaustive()
    }
}

impl VideoFeed {
    /// Fuente nueva que llama a `wake` con cada fotograma; `importer` para las imágenes Vulkan.
    pub fn new(importer: Option<VideoImporter>, wake: Wake) -> Arc<Self> {
        Arc::new(Self {
            id: NEXT_FEED.fetch_add(1, Ordering::Relaxed),
            state: Mutex::new(FeedState::default()),
            shown: Condvar::new(),
            wake,
            importer,
        })
    }

    /// Fuente sin ventana (capturas de pantalla y tests).
    pub fn detached() -> Arc<Self> {
        Self::new(None, Arc::new(|| {}))
    }

    fn lock(&self) -> MutexGuard<'_, FeedState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Identificador (uno por fuente).
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Deja `frame` como el último y avisa a la ventana, sin esperar.
    pub fn push(
        &self,
        frame: PresentFrame,
        cursor: Option<CursorOverlay>,
        cursor_image: Option<Arc<CursorImage>>,
    ) {
        {
            let mut state = self.lock();
            state.frame = Some(frame);
            state.cursor = cursor;
            state.cursor_image = cursor_image;
        }
        (self.wake)();
    }

    /// Como [`VideoFeed::push`], y espera a que la ventana lo muestre (o a `timeout`, o a que se
    /// cierre). Devuelve si se mostró.
    pub fn push_and_wait(
        &self,
        frame: PresentFrame,
        cursor: Option<CursorOverlay>,
        cursor_image: Option<Arc<CursorImage>>,
        timeout: Duration,
    ) -> bool {
        let frame_id = frame.frame_id;
        self.push(frame, cursor, cursor_image);
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            if state.closed {
                return false;
            }
            if state.shown.is_some_and(|shown| shown >= frame_id) {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            state = self
                .shown
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// La interfaz entregó a la cola lo que subió: despierta a quien esperaba.
    pub fn commit_shown(&self) {
        let mut state = self.lock();
        if state.uploaded.is_some() && state.uploaded != state.shown {
            state.shown = state.uploaded;
            self.shown.notify_all();
        }
    }

    /// Ya no se va a mostrar más (suelta al hilo que espera).
    pub fn close(&self) {
        self.lock().closed = true;
        self.shown.notify_all();
    }

    /// Si hay algún fotograma.
    pub fn has_frame(&self) -> bool {
        self.lock().frame.is_some()
    }

    /// Tamaño del último fotograma.
    pub fn frame_size(&self) -> Option<(u32, u32)> {
        self.lock()
            .frame
            .as_ref()
            .map(|frame| (frame.width, frame.height))
    }

    /// Copias de CPU del último fotograma mostrado.
    pub fn cpu_copies(&self) -> u32 {
        self.lock().copies
    }

    /// Sube (o importa) el fotograma pendiente en `layer` y coloca el cursor.
    fn prepare(&self, layer: &mut VideoLayer, device: &wgpu::Device, queue: &wgpu::Queue) {
        let mut state = self.lock();
        layer.set_cursor(device, queue, state.cursor, state.cursor_image.clone());
        let Some(frame) = state.frame.as_ref() else {
            return;
        };
        if layer.last_frame() == Some(frame.frame_id) {
            return;
        }
        let frame_id = frame.frame_id;
        match layer.upload(device, queue, self.importer.as_ref(), frame) {
            Ok(()) => {
                state.uploaded = Some(frame_id);
                state.copies = layer.cpu_copies();
            }
            Err(err) => {
                tracing::warn!(error = %err, "no se pudo mostrar el fotograma");
                // No se vuelve a intentar con el mismo: el hilo que espera sigue.
                state.uploaded = Some(frame_id);
            }
        }
    }
}

/// Última etapa del pipeline para la ventana: deja cada fotograma en su [`VideoFeed`] y espera a
/// que se muestre.
pub struct FeedSink {
    feed: Arc<VideoFeed>,
    /// Última forma del cursor convertida (se reutiliza mientras no cambie).
    cursor: Option<(Arc<CursorShape>, Arc<CursorImage>)>,
    gpu: bool,
}

impl FeedSink {
    /// Presentación en `feed`.
    pub fn new(feed: Arc<VideoFeed>) -> Self {
        Self {
            feed,
            cursor: None,
            gpu: false,
        }
    }
}

impl FrameSink for FeedSink {
    fn present(&mut self, decoded: DecodedFrame) -> Result<(), SinkError> {
        let cursor = decoded.cursor.map(|cursor| CursorOverlay {
            x: cursor.x,
            y: cursor.y,
            visible: cursor.visible,
        });
        let image = match (&decoded.cursor_shape, &self.cursor) {
            (Some(shape), Some((last, image))) if Arc::ptr_eq(shape, last) => {
                Some(Arc::clone(image))
            }
            (Some(shape), _) => {
                let image = Arc::new(to_cursor_image(shape));
                self.cursor = Some((Arc::clone(shape), Arc::clone(&image)));
                Some(image)
            }
            (None, _) => None,
        };
        let frame = to_present_frame(decoded.frame)?;
        self.gpu = matches!(frame.image, PresentImage::Vulkan(_));
        self.feed.push_and_wait(frame, cursor, image, SHOW_TIMEOUT);
        Ok(())
    }

    fn cpu_copies(&self) -> u32 {
        u32::from(!self.gpu)
    }
}

/// El *widget* de vídeo: llena su espacio con el fotograma de `feed`, con bandas negras.
pub fn video_view<'a, M: 'a>(feed: &Arc<VideoFeed>) -> Element<'a, M> {
    shader::Shader::new(VideoProgram {
        feed: Arc::clone(feed),
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

struct VideoProgram {
    feed: Arc<VideoFeed>,
}

impl<M> shader::Program<M> for VideoProgram {
    type State = ();
    type Primitive = VideoPrimitive;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, _bounds: Rectangle) -> VideoPrimitive {
        VideoPrimitive {
            feed: Arc::clone(&self.feed),
        }
    }
}

/// Lo que se dibuja en un fotograma de la interfaz.
#[derive(Debug)]
pub struct VideoPrimitive {
    feed: Arc<VideoFeed>,
}

/// Capas de vídeo de la ventana, una por fuente.
pub struct VideoPipelines {
    format: wgpu::TextureFormat,
    layers: HashMap<u64, VideoLayer>,
}

impl Pipeline for VideoPipelines {
    fn new(_device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        Self {
            format,
            layers: HashMap::new(),
        }
    }
}

impl Primitive for VideoPrimitive {
    type Pipeline = VideoPipelines;

    fn prepare(
        &self,
        pipeline: &mut VideoPipelines,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        let format = pipeline.format;
        let layer = pipeline
            .layers
            .entry(self.feed.id)
            .or_insert_with(|| VideoLayer::new(device, queue, format));
        if format == coroom_render::hdr::HDR_FORMAT {
            layer.set_output(coroom_render::OutputMode::Hdr {
                headroom: hdr_headroom(),
            });
        }
        self.feed.prepare(layer, device, queue);
        let scale = viewport.scale_factor();
        layer.place(
            queue,
            [
                bounds.x * scale,
                bounds.y * scale,
                bounds.width * scale,
                bounds.height * scale,
            ],
        );
    }

    fn draw(&self, pipeline: &VideoPipelines, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        if let Some(layer) = pipeline.layers.get(&self.feed.id) {
            layer.draw(render_pass);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn frame(id: u64) -> PresentFrame {
        PresentFrame {
            frame_id: id,
            width: 2,
            height: 2,
            image: PresentImage::I420 {
                y: vec![16; 4],
                u: vec![128],
                v: vec![128],
                y_stride: 2,
                uv_stride: 1,
            },
            color: coroom_render::ColorInfo::SDR,
        }
    }

    #[test]
    fn the_present_thread_waits_until_the_window_shows_the_frame() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let feed = VideoFeed::new(
            None,
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        );
        // Sin ventana que lo muestre: vence el plazo.
        assert!(!feed.push_and_wait(frame(1), None, None, Duration::from_millis(20)));
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        assert!(feed.has_frame());
        assert_eq!(feed.frame_size(), Some((2, 2)));
        // La ventana lo sube y lo entrega: el hilo que espera sigue.
        let shower = Arc::clone(&feed);
        let window = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            shower.lock().uploaded = Some(2);
            shower.commit_shown();
        });
        assert!(feed.push_and_wait(frame(2), None, None, Duration::from_secs(5)));
        window.join().expect("hilo");
        // Al cerrar no se espera.
        feed.close();
        assert!(!feed.push_and_wait(frame(3), None, None, Duration::from_secs(5)));
    }
}
