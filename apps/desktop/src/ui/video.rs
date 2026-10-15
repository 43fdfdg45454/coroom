//! El vídeo dentro de la interfaz.
//!
//! - [`VideoFeed`]: el último fotograma de una sesión (o de la vista previa del anfitrión), con su
//!   cursor. El hilo `present` del pipeline lo deja aquí ([`FeedSink`]) y, si la fuente está en la
//!   ventana, el [`Compositor`] lo compone y lo entrega **en ese mismo hilo**, sin esperar a que la
//!   interfaz se reconstruya: la marca de presentación del registro de latencia es la de la
//!   entrega real. Sin compositor (capturas de pantalla, tests), espera a que quien dibuje la
//!   interfaz lo muestre, como antes.
//! - [`video_view`]: *widget* de `iced` que ocupa el sitio del vídeo. Con compositor solo anota
//!   dónde ([`VideoFeed::set_area`]: nunca llama al compositor desde dentro del dibujo de la
//!   interfaz, que ya compone al terminar); sin él dibuja el fotograma con
//!   [`coroom_render::layer::VideoLayer`] en la pasada de la interfaz y con su mismo
//!   `wgpu::Device`. Los planos I420/NV12 se suben como siempre (una copia de CPU a GPU); las
//!   imágenes Vulkan del decodificador se importan sin copia con el
//!   [`coroom_render::wgpu::VideoImporter`] del dispositivo compartido.
//! - [`CursorPump`]: el cursor del anfitrión viaja aparte del vídeo; con compositor, cada
//!   posición nueva se compone sobre el último fotograma desde un hilo propio (como mucho una vez
//!   por [`CURSOR_PERIOD`]), sin esperar al siguiente fotograma ni bloquear al hilo de control.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use coroom_render::layer::VideoLayer;
use coroom_render::wgpu::VideoImporter;
use coroom_render::{CursorImage, CursorOverlay, PresentFrame, PresentImage};
use coroom_session::capture::{CursorShape, CursorState};
use coroom_session::loopback::{CursorSink, DecodedFrame, FrameSink, SinkError};
use iced_core::{Length, Rectangle, mouse};
use iced_wgpu::graphics::Viewport;
use iced_wgpu::primitive::{Pipeline, Primitive};
use iced_widget::shader;

use super::compose::Compositor;
use crate::probe::{to_cursor_image, to_present_frame};
use coroom_ui::Element;
use iced_wgpu::wgpu;

/// Lo que espera el hilo `present` a que la ventana muestre un fotograma (si la ventana no dibuja,
/// por ejemplo minimizada, el pipeline no se para: sigue y gana el último).
pub const SHOW_TIMEOUT: Duration = Duration::from_millis(100);

/// Intervalo mínimo entre dos composiciones por el cursor solo (sin fotograma nuevo): ~120 Hz,
/// más que cualquier pantalla corriente; si llegan más posiciones, gana la última.
pub const CURSOR_PERIOD: Duration = Duration::from_millis(8);

static NEXT_FEED: AtomicU64 = AtomicU64::new(1);

/// Margen HDR de la pantalla (bits de un `f32`; 1,0 por defecto), que la ventana pone al
/// configurar una superficie HDR.
static HDR_HEADROOM: AtomicU32 = AtomicU32::new(0x3f80_0000);

/// Margen de la pantalla en múltiplos del blanco SDR para el vídeo HDR
/// ([`coroom_render::hdr::headroom`]).
pub fn set_hdr_headroom(headroom: f32) {
    HDR_HEADROOM.store(headroom.max(1.0).to_bits(), Ordering::Relaxed);
}

pub(super) fn hdr_headroom() -> f32 {
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
    /// Dónde va el vídeo en la ventana (píxeles físicos), según la interfaz.
    area: Option<[f32; 4]>,
}

/// Último fotograma de una fuente de vídeo, entre el pipeline y la ventana.
pub struct VideoFeed {
    id: u64,
    state: Mutex<FeedState>,
    shown: Condvar,
    wake: Wake,
    importer: Option<VideoImporter>,
    /// El compositor de la ventana, si la fuente se compone en ella.
    compositor: Option<Weak<Compositor>>,
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
    pub fn new(
        importer: Option<VideoImporter>,
        wake: Wake,
        compositor: Option<&Arc<Compositor>>,
    ) -> Arc<Self> {
        let feed = Arc::new(Self {
            id: NEXT_FEED.fetch_add(1, Ordering::Relaxed),
            state: Mutex::default(),
            shown: Condvar::new(),
            wake,
            importer,
            compositor: compositor.map(Arc::downgrade),
        });
        if let Some(compositor) = compositor {
            compositor.register(&feed);
        }
        feed
    }

    /// Fuente sin ventana (capturas de pantalla y tests): quien dibuje la interfaz muestra el
    /// fotograma.
    pub fn detached() -> Arc<Self> {
        Self::new(None, Arc::new(|| {}), None)
    }

    /// Si la fuente se compone en la ventana (el *widget* solo marca su sitio).
    pub fn is_composited(&self) -> bool {
        self.compositor.is_some()
    }

    /// Dónde va el vídeo en la ventana (píxeles físicos), lo que dijo la interfaz la última vez.
    pub(super) fn area(&self) -> Option<[f32; 4]> {
        self.lock().area
    }

    /// La interfaz dice dónde va el vídeo (píxeles físicos). Solo lo anota: se llama desde el
    /// `prepare` del *widget*, dentro del dibujo de la interfaz, y el compositor compone justo
    /// después con el área nueva. Llamar al compositor desde aquí volvería a tomar su candado en
    /// el mismo hilo (bloqueo mutuo).
    pub(super) fn set_area(&self, area: [f32; 4]) {
        self.lock().area = Some(area);
    }

    /// Cursor nuevo del anfitrión sin fotograma nuevo. Devuelve si cambió algo que dibujar.
    pub(super) fn set_cursor(
        &self,
        cursor: Option<CursorOverlay>,
        cursor_image: Option<Arc<CursorImage>>,
    ) -> bool {
        let mut state = self.lock();
        if state.frame.is_none() {
            return false;
        }
        let same_image = match (&state.cursor_image, &cursor_image) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a.id == b.id,
            (None, None) => true,
            _ => false,
        };
        if state.cursor == cursor && same_image {
            return false;
        }
        state.cursor = cursor;
        state.cursor_image = cursor_image;
        true
    }

    /// Compone y entrega ya lo que haya (el fotograma de siempre con el cursor nuevo), si la
    /// fuente está en la ventana.
    pub(super) fn present_now(&self) {
        if let Some(compositor) = self.compositor() {
            compositor.present_video();
        }
    }

    fn compositor(&self) -> Option<Arc<Compositor>> {
        self.compositor.as_ref().and_then(Weak::upgrade)
    }

    fn lock(&self) -> MutexGuard<'_, FeedState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Identificador (uno por fuente).
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Deja `frame` como el último. Con compositor, lo compone y lo entrega ya; si no, avisa a la
    /// ventana. La interfaz se despierta solo cuando cambia algo que le importa (el primer fotograma
    /// o el tamaño), no con cada fotograma.
    pub fn push(
        &self,
        frame: PresentFrame,
        cursor: Option<CursorOverlay>,
        cursor_image: Option<Arc<CursorImage>>,
    ) {
        let layout_changed = {
            let mut state = self.lock();
            let changed = state
                .frame
                .as_ref()
                .is_none_or(|last| (last.width, last.height) != (frame.width, frame.height));
            state.frame = Some(frame);
            state.cursor = cursor;
            state.cursor_image = cursor_image;
            changed
        };
        match self.compositor() {
            Some(compositor) => {
                compositor.present_video();
                if layout_changed {
                    (self.wake)();
                }
            }
            None => (self.wake)(),
        }
    }

    /// Como [`VideoFeed::push`] y, sin compositor, espera a que la ventana lo muestre (o a
    /// `timeout`, o a que se cierre). Devuelve si se mostró.
    pub fn push_and_wait(
        &self,
        frame: PresentFrame,
        cursor: Option<CursorOverlay>,
        cursor_image: Option<Arc<CursorImage>>,
        timeout: Duration,
    ) -> bool {
        let frame_id = frame.frame_id;
        self.push(frame, cursor, cursor_image);
        if self.compositor.is_some() {
            // Ya se compuso en este hilo: o salió, o la ventana no podía (oculta) y no se espera.
            let state = self.lock();
            return !state.closed && state.shown.is_some_and(|shown| shown >= frame_id);
        }
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
    pub(super) fn prepare(
        &self,
        layer: &mut VideoLayer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) {
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

/// Última forma del cursor convertida a imagen (se reutiliza mientras no cambie). La comparten
/// el hilo `present` y el del cursor.
#[derive(Default)]
struct CursorCache(Mutex<Option<(Arc<CursorShape>, Arc<CursorImage>)>>);

impl CursorCache {
    fn image(&self, shape: Option<&Arc<CursorShape>>) -> Option<Arc<CursorImage>> {
        let shape = shape?;
        let mut cache = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((last, image)) = cache.as_ref()
            && (Arc::ptr_eq(shape, last) || last.shape_id == shape.shape_id)
        {
            return Some(Arc::clone(image));
        }
        let image = Arc::new(to_cursor_image(shape));
        *cache = Some((Arc::clone(shape), Arc::clone(&image)));
        Some(image)
    }
}

fn overlay(cursor: CursorState) -> CursorOverlay {
    CursorOverlay {
        x: cursor.x,
        y: cursor.y,
        visible: cursor.visible,
    }
}

/// El cursor del anfitrión entre fotogramas (ver el módulo). El hilo de control deja la última
/// posición ([`CursorSink::cursor_moved`], sin bloquearse) y el hilo «cursor» la compone, como
/// mucho una vez por [`CURSOR_PERIOD`]; entregar a la superficie puede esperar al refresco con la
/// sincronización vertical, y eso no debe frenar al control de la sesión.
pub struct CursorPump {
    feed: Weak<VideoFeed>,
    cache: Arc<CursorCache>,
    pending: Mutex<Option<(CursorState, Option<Arc<CursorShape>>)>>,
    changed: Condvar,
    stop: AtomicBool,
}

impl CursorPump {
    fn spawn(feed: &Arc<VideoFeed>, cache: Arc<CursorCache>) -> Option<Arc<Self>> {
        let pump = Arc::new(Self {
            feed: Arc::downgrade(feed),
            cache,
            pending: Mutex::new(None),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let worker = Arc::clone(&pump);
        std::thread::Builder::new()
            .name("cursor".into())
            .spawn(move || worker.run())
            .map_err(|err| tracing::warn!(error = %err, "sin hilo para el cursor"))
            .ok()?;
        Some(pump)
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.changed.notify_all();
    }

    /// Espera a la próxima posición (`None`: hay que parar).
    fn next(&self) -> Option<(CursorState, Option<Arc<CursorShape>>)> {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if self.stop.load(Ordering::Acquire) {
                return None;
            }
            if let Some(item) = pending.take() {
                return Some(item);
            }
            pending = self
                .changed
                .wait(pending)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn run(&self) {
        let mut last = Instant::now() - CURSOR_PERIOD;
        while let Some((mut cursor, mut shape)) = self.next() {
            let since = last.elapsed();
            if since < CURSOR_PERIOD {
                std::thread::sleep(CURSOR_PERIOD - since);
                // Mientras se esperaba pudo llegar otra: gana la última.
                if let Some(newer) = self
                    .pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                {
                    (cursor, shape) = newer;
                }
            }
            let Some(feed) = self.feed.upgrade() else {
                return;
            };
            let image = self.cache.image(shape.as_ref());
            if feed.set_cursor(Some(overlay(cursor)), image) {
                feed.present_now();
            }
            last = Instant::now();
        }
    }
}

impl CursorSink for CursorPump {
    fn cursor_moved(&self, cursor: CursorState, shape: Option<Arc<CursorShape>>) {
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some((cursor, shape));
        self.changed.notify_one();
    }
}

/// Última etapa del pipeline para la ventana: deja cada fotograma en su [`VideoFeed`] y espera a
/// que se muestre.
pub struct FeedSink {
    feed: Arc<VideoFeed>,
    cache: Arc<CursorCache>,
    /// El cursor entre fotogramas, con compositor.
    pump: Option<Arc<CursorPump>>,
    gpu: bool,
    /// Si el último fotograma llegó a la pantalla.
    shown: bool,
}

impl FeedSink {
    /// Presentación en `feed`.
    pub fn new(feed: Arc<VideoFeed>) -> Self {
        let cache = Arc::new(CursorCache::default());
        let pump = feed
            .is_composited()
            .then(|| CursorPump::spawn(&feed, Arc::clone(&cache)))
            .flatten();
        Self {
            feed,
            cache,
            pump,
            gpu: false,
            shown: true,
        }
    }
}

impl Drop for FeedSink {
    fn drop(&mut self) {
        if let Some(pump) = &self.pump {
            pump.stop();
        }
    }
}

impl FrameSink for FeedSink {
    fn present(&mut self, decoded: DecodedFrame) -> Result<(), SinkError> {
        let cursor = decoded.cursor.map(overlay);
        let image = self.cache.image(decoded.cursor_shape.as_ref());
        let frame = to_present_frame(decoded.frame)?;
        self.gpu = matches!(frame.image, PresentImage::Vulkan(_));
        self.shown = self.feed.push_and_wait(frame, cursor, image, SHOW_TIMEOUT);
        Ok(())
    }

    fn cpu_copies(&self) -> u32 {
        u32::from(!self.gpu)
    }

    fn shown(&self) -> bool {
        self.shown
    }

    fn cursor_sink(&self) -> Option<Arc<dyn CursorSink>> {
        self.pump
            .as_ref()
            .map(|pump| Arc::clone(pump) as Arc<dyn CursorSink>)
    }
}

/// El *widget* de vídeo: llena su espacio con el fotograma de `feed`, con bandas negras (con
/// compositor, el espacio queda transparente y el compositor pone el vídeo debajo).
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
        let scale = viewport.scale_factor();
        let area = [
            bounds.x * scale,
            bounds.y * scale,
            bounds.width * scale,
            bounds.height * scale,
        ];
        if self.feed.is_composited() {
            self.feed.set_area(area);
            return;
        }
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
        layer.place(queue, area);
    }

    fn draw(&self, pipeline: &VideoPipelines, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        if self.feed.is_composited() {
            // El compositor lo dibuja debajo de la interfaz: aquí queda transparente.
            return true;
        }
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
    fn without_a_compositor_the_present_thread_waits_until_the_window_shows_the_frame() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let feed = VideoFeed::new(
            None,
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
            None,
        );
        assert!(!feed.is_composited());
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

    #[test]
    fn the_sink_reports_whether_the_frame_reached_the_screen() {
        let feed = VideoFeed::detached();
        let mut sink = FeedSink::new(Arc::clone(&feed));
        assert!(sink.pump.is_none(), "sin compositor no hay hilo del cursor");
        assert!(sink.cursor_sink().is_none());
        assert!(
            sink.shown(),
            "antes del primer fotograma no hay nada que contar"
        );
        // La interfaz anota el área sin llamar a nadie.
        feed.set_area([0.0, 0.0, 2.0, 2.0]);
        assert_eq!(feed.area(), Some([0.0, 0.0, 2.0, 2.0]));
        // Sin ventana que lo muestre, el fotograma no llega a la pantalla: es un descarte.
        let raw = coroom_session::codec::RawFrame::cpu(
            1,
            (2, 2),
            coroom_session::codec::PixelFormat::I420,
            2,
            vec![16, 16, 16, 16, 128, 128],
        );
        assert!(
            sink.present(DecodedFrame {
                frame: raw,
                cursor: None,
                cursor_shape: None,
            })
            .is_ok()
        );
        assert!(!sink.shown());
        // El cursor solo cuenta como cambio con un fotograma y si de verdad cambia.
        assert!(feed.set_cursor(
            Some(CursorOverlay {
                x: 1,
                y: 1,
                visible: true
            }),
            None
        ));
        assert!(!feed.set_cursor(
            Some(CursorOverlay {
                x: 1,
                y: 1,
                visible: true
            }),
            None
        ));
    }
}
