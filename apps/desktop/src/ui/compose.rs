//! El compositor de la ventana: el vídeo se presenta en cuanto llega, sin esperar a la interfaz.
//!
//! Antes, el hilo `present` del pipeline dejaba el fotograma y esperaba a que la ventana
//! reconstruyera y dibujara **toda** la interfaz (5-25 ms con jitter, y el vídeo dependía del
//! trabajo de la interfaz). Ahora la interfaz se dibuja en una textura propia solo cuando cambia
//! (eventos, animaciones) y el [`Compositor`] compone la imagen final desde el hilo que tenga algo
//! nuevo: el de `present` con un fotograma ([`Compositor::present_video`]) o el de la ventana con
//! la interfaz ([`Compositor::draw_ui`]). Componer es: fondo, las capas de vídeo
//! ([`VideoLayer`], una por [`VideoFeed`]) en el rectángulo que la interfaz les dio
//! ([`VideoFeed::set_area`]) y la textura de la interfaz encima con alfa premultiplicado
//! ([`OverlayBlit`]); en HDR, todo a la textura intermedia de [`HdrOutput`] y su pasada final a la
//! superficie.
//!
//! Dos candados ordenan los dos hilos. El del estado (`inner`: superficie, capas, texturas) se
//! toma poco: para componer y para intercambiar las texturas de la interfaz. La interfaz se dibuja
//! en una textura **trasera** sin ese candado (doble búfer: el hilo de `present` sigue componiendo
//! la delantera mientras `iced` dibuja) y al acabar se intercambian y se compone. El otro
//! (`submit`) ordena las entregas a la cola: la importación sin copias de una imagen Vulkan deja
//! su semáforo para la **siguiente** entrega, así que entre importar y entregar (en la
//! composición) no puede colarse la entrega del dibujo de la interfaz; es lo único por lo que el
//! hilo de `present` llega a esperar al de la ventana, y nunca a que la interfaz se reconstruya.
//! Dentro del dibujo de la interfaz nadie vuelve a llamar al compositor ([`VideoFeed::set_area`]
//! solo anota el área).
//!
//! Con el vídeo llegando, la interfaz no agrega cuadros: [`Present::WithVideo`] deja su textura
//! lista sin componer y el próximo fotograma la entrega junto con él; si la ventana quiere otro
//! cuadro de animación ([`Compositor::want_ui_frame`]), ese fotograma la despierta
//! ([`Compositor::present_video`]). [`Compositor::video_interval`] dice si el vídeo sigue
//! llegando, medido con su propio ritmo.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use coroom_render::RenderError;
use coroom_render::hdr::{HDR_FORMAT, HdrOutput};
use coroom_render::layer::VideoLayer;
use coroom_render::overlay::OverlayBlit;
use coroom_render::wgpu::{
    Acquired, GpuContext, HdrSurface, SurfaceSetup, acquire, configure_surface,
};
use iced_core::Color;
use iced_wgpu::wgpu;

use super::video::VideoFeed;

/// Textura donde `iced` dibuja la interfaz, del tamaño de la superficie.
struct UiTexture {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    size: (u32, u32),
    /// Si la interfaz ya se dibujó en ella alguna vez (si no, no hay nada que poner encima).
    drawn: bool,
}

struct Inner {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    render_format: wgpu::TextureFormat,
    hdr: Option<(HdrOutput, HdrSurface)>,
    blit: OverlayBlit,
    /// Textura de la interfaz que se compone (la última que terminó de dibujarse).
    ui: Option<UiTexture>,
    /// Textura en la que la ventana dibuja la interfaz ahora; al acabar se intercambia con `ui`.
    ui_back: Option<UiTexture>,
    /// Fondo de la composición (el de la ventana o el del vídeo), como lo limpia `iced`.
    background: wgpu::Color,
    feeds: Vec<Weak<VideoFeed>>,
    layers: HashMap<u64, VideoLayer>,
    /// Hay que reconfigurar la superficie antes de la próxima composición.
    outdated: bool,
    /// La GPU ya no puede presentar en la superficie (el escritorio cerró la conexión con la
    /// ventana): no se vuelve a configurar ni a componer, y la ventana se cierra.
    rejected: bool,
    presented: u64,
    /// Modo de presentación de menor latencia que eligió la configuración de la superficie
    /// (`Immediate` o `Mailbox` si existen), al que se vuelve al quitar la sincronización vertical.
    low_latency_mode: wgpu::PresentMode,
    /// Sincronización vertical pedida (`Fifo`, que toda superficie admite).
    vsync: bool,
    /// La interfaz se dibujó con [`Present::WithVideo`] y todavía no salió en ningún fotograma.
    ui_pending: bool,
    /// Cuándo se compuso el último fotograma de vídeo y el intervalo medio entre fotogramas
    /// (media exponencial), para saber si el vídeo sigue llegando.
    last_video: Option<Instant>,
    video_interval: Option<Duration>,
}

impl Inner {
    fn reject(&mut self, err: &RenderError) {
        tracing::error!(error = %err, "la GPU ya no puede mostrar la ventana");
        self.rejected = true;
    }
}

/// Cómo sale a la pantalla un dibujo de la interfaz ([`Compositor::draw_ui`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Present {
    /// Se compone y se entrega ya.
    Now,
    /// Queda lista y la entrega el próximo fotograma de vídeo: con el vídeo llegando, la interfaz
    /// no agrega cuadros propios.
    WithVideo,
}

/// El compositor de la ventana (ver el módulo). Se comparte entre la ventana y las fuentes de
/// vídeo ([`VideoFeed`]).
pub struct Compositor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    inner: Mutex<Inner>,
    /// Orden de las entregas a la cola (ver el módulo). Se toma después de `inner`, nunca antes.
    submit: Mutex<()>,
    /// La ventana quiere otro cuadro de animación con el próximo fotograma de vídeo.
    ui_frame_wanted: AtomicBool,
}

impl std::fmt::Debug for Compositor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compositor").finish_non_exhaustive()
    }
}

/// Color de limpieza como lo calcula `iced_wgpu` para el mismo formato de destino.
fn clear_color(color: Color) -> wgpu::Color {
    let [r, g, b, a] = iced_wgpu::graphics::color::pack(color).components();
    wgpu::Color {
        r: f64::from(r),
        g: f64::from(g),
        b: f64::from(b),
        a: f64::from(a),
    }
}

impl Compositor {
    /// Compositor sobre `surface`, ya configurada con `setup`, y con la salida HDR si la hay.
    pub fn new(
        gpu: &GpuContext,
        surface: wgpu::Surface<'static>,
        setup: &SurfaceSetup,
        hdr: Option<(HdrOutput, HdrSurface)>,
    ) -> Arc<Self> {
        let blit = OverlayBlit::new(&gpu.device, setup.render_format);
        Arc::new(Self {
            device: gpu.device.clone(),
            queue: gpu.queue.clone(),
            inner: Mutex::new(Inner {
                surface,
                config: setup.config.clone(),
                render_format: setup.render_format,
                hdr,
                blit,
                ui: None,
                ui_back: None,
                // Hasta el primer `draw_ui`, que trae el fondo del tema.
                background: wgpu::Color::TRANSPARENT,
                feeds: Vec::new(),
                layers: HashMap::new(),
                outdated: false,
                rejected: false,
                presented: 0,
                low_latency_mode: setup.config.present_mode,
                vsync: false,
                ui_pending: false,
                last_video: None,
                video_interval: None,
            }),
            submit: Mutex::new(()),
            ui_frame_wanted: AtomicBool::new(false),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Configura la superficie con `inner.config`, salvo que la GPU ya no pueda presentar en ella.
    fn configure(&self, inner: &mut Inner) {
        if inner.rejected {
            return;
        }
        if let Err(err) = configure_surface(&inner.surface, &self.device, &inner.config) {
            inner.reject(&err);
        }
    }

    /// Si la GPU ya no puede presentar en la ventana (el motivo está en el registro).
    pub fn rejected(&self) -> bool {
        self.lock().rejected
    }

    fn submit_lock(&self) -> MutexGuard<'_, ()> {
        self.submit.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Formato de las vistas en las que dibuja la interfaz (sin sRGB; en HDR, `Rgba16Float`).
    pub fn render_format(&self) -> wgpu::TextureFormat {
        self.lock().render_format
    }

    /// Si la superficie es HDR.
    pub fn is_hdr(&self) -> bool {
        self.lock().hdr.is_some()
    }

    /// Fotogramas compuestos y entregados a la superficie desde que se abrió.
    pub fn presented(&self) -> u64 {
        self.lock().presented
    }

    /// La ventana cambió de tamaño: la superficie se reconfigura y la textura de la interfaz se
    /// rehace al siguiente dibujo.
    pub fn resize(&self, width: u32, height: u32) {
        let mut inner = self.lock();
        inner.config.width = width.max(1);
        inner.config.height = height.max(1);
        self.configure(&mut inner);
        inner.outdated = false;
        inner.ui = None;
        inner.ui_back = None;
    }

    /// Rehace la superficie para el ajuste de HDR `wanted` y devuelve el formato de dibujo (si
    /// cambia, la ventana rehace el motor de `iced`).
    pub fn configure_hdr(
        &self,
        gpu: &GpuContext,
        width: u32,
        height: u32,
        wanted: bool,
    ) -> Result<SurfaceSetup, coroom_render::RenderError> {
        let mut inner = self.lock();
        let setup = gpu.surface_setup_with(&inner.surface, width, height, wanted)?;
        inner.low_latency_mode = setup.config.present_mode;
        inner.config = setup.config.clone();
        if inner.vsync {
            inner.config.present_mode = wgpu::PresentMode::Fifo;
        }
        self.configure(&mut inner);
        if setup.render_format != inner.render_format {
            inner.render_format = setup.render_format;
            inner.blit = OverlayBlit::new(&self.device, setup.render_format);
            inner.layers.clear();
        }
        inner.ui = None;
        inner.ui_back = None;
        inner.hdr = super::runner::make_hdr(gpu, setup.hdr);
        inner.outdated = false;
        Ok(setup)
    }

    /// Sincronización vertical (`Fifo`: sin desgarros, hasta un refresco más de latencia) o la
    /// presentación de menor latencia; la superficie se reconfigura en la siguiente composición.
    pub fn set_vsync(&self, on: bool) {
        let mut inner = self.lock();
        if inner.vsync == on {
            return;
        }
        inner.vsync = on;
        inner.config.present_mode = if on {
            wgpu::PresentMode::Fifo
        } else {
            inner.low_latency_mode
        };
        inner.outdated = true;
    }

    /// Una fuente de vídeo que se compone en la ventana (hasta que se suelte).
    pub fn register(&self, feed: &Arc<VideoFeed>) {
        let mut inner = self.lock();
        inner.feeds.retain(|weak| weak.strong_count() > 0);
        inner.feeds.push(Arc::downgrade(feed));
    }

    /// Dibuja la interfaz: `draw` recibe la vista de la textura trasera de la interfaz (limpiarla
    /// a transparente y dibujar encima) **sin el candado del compositor** (el hilo de `present`
    /// sigue componiendo la delantera mientras tanto) y, después, las texturas se intercambian y
    /// se compone con el vídeo y se entrega (o, con [`Present::WithVideo`], la entrega el próximo
    /// fotograma). `background` es el fondo bajo la interfaz y el vídeo. `draw` no debe llamar al
    /// compositor; `before_present` se llama justo antes de entregar la imagen con la interfaz
    /// (la ventana avisa así al sistema de ventanas, que marca el ritmo del siguiente cuadro).
    pub fn draw_ui(
        &self,
        background: Color,
        present: Present,
        draw: impl FnOnce(&wgpu::TextureView),
        before_present: impl FnOnce(),
    ) {
        let (view, size) = {
            let mut inner = self.lock();
            let size = (inner.config.width, inner.config.height);
            if inner.ui_back.as_ref().is_none_or(|ui| ui.size != size) {
                let ui = self.make_ui_texture(&inner, size);
                inner.ui_back = Some(ui);
            }
            let view = inner
                .ui_back
                .as_ref()
                .map(|ui| ui.view.clone())
                .expect("ui_back texture was just created");
            (view, size)
        };
        {
            // `iced` prepara y entrega a la cola: ordenado frente a la composición (semáforo de
            // la importación), pero sin retener el estado del compositor.
            let _order = self.submit_lock();
            draw(&view);
        }
        let mut inner = self.lock();
        inner.background = clear_color(background);
        if inner.ui_back.as_ref().is_some_and(|ui| ui.size == size) {
            let inner = &mut *inner;
            std::mem::swap(&mut inner.ui, &mut inner.ui_back);
            if let Some(ui) = inner.ui.as_mut() {
                ui.drawn = true;
            }
        }
        match present {
            Present::Now => self.composite(&mut inner, Some(before_present)),
            Present::WithVideo => inner.ui_pending = true,
        }
    }

    /// Llegó un fotograma a alguna fuente (`frame`; si no, cambió solo el cursor): compone y
    /// entrega sin esperar a la interfaz. Devuelve si la ventana pidió un cuadro de animación con
    /// él ([`Compositor::want_ui_frame`]).
    pub fn present_video(&self, frame: bool) -> bool {
        let mut inner = self.lock();
        if frame {
            let now = Instant::now();
            if let Some(last) = inner.last_video {
                let gap = now.saturating_duration_since(last);
                inner.video_interval = Some(match inner.video_interval {
                    // El vídeo se había detenido: el hueco no es su ritmo.
                    Some(mean) if gap > mean * 2 => mean,
                    Some(mean) => (mean * 7 + gap) / 8,
                    None => gap,
                });
            }
            inner.last_video = Some(now);
        }
        self.composite(&mut inner, None::<fn()>);
        self.ui_frame_wanted.swap(false, Ordering::AcqRel)
    }

    /// El intervalo medio del vídeo si sigue llegando en `now`: algún fotograma en el doble de
    /// ese intervalo. `None` sin vídeo o si se detuvo.
    pub fn video_interval(&self, now: Instant) -> Option<Duration> {
        let inner = self.lock();
        let interval = inner.video_interval?;
        let last = inner.last_video?;
        (now.saturating_duration_since(last) <= interval * 2).then_some(interval)
    }

    /// Que el próximo fotograma de vídeo despierte a la ventana para el siguiente cuadro de
    /// animación (ver [`Compositor::present_video`]).
    pub fn want_ui_frame(&self) {
        self.ui_frame_wanted.store(true, Ordering::Release);
    }

    /// Si la interfaz quedó dibujada con [`Present::WithVideo`] y todavía no salió.
    pub fn ui_pending(&self) -> bool {
        self.lock().ui_pending
    }

    fn make_ui_texture(&self, inner: &Inner, size: (u32, u32)) -> UiTexture {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("coroom-interfaz"),
            size: wgpu::Extent3d {
                width: size.0.max(1),
                height: size.1.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: inner.render_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = inner.blit.bind(&self.device, &view);
        UiTexture {
            _texture: texture,
            view,
            bind_group,
            size,
            drawn: false,
        }
    }

    /// Fondo, capas de vídeo e interfaz a la superficie (y el aviso a las fuentes de que su
    /// fotograma ya salió). Si la superficie no da imagen (ventana oculta), no se avisa a las
    /// fuentes: su fotograma no se mostró y el pipeline lo cuenta como descarte.
    fn composite(&self, inner: &mut Inner, before_present: Option<impl FnOnce()>) {
        if inner.outdated {
            self.configure(inner);
            inner.outdated = false;
        }
        if inner.rejected {
            return;
        }
        let frame = match acquire(&inner.surface, &self.device, &inner.config) {
            Ok(Acquired::Frame(frame)) => frame,
            Ok(Acquired::Skip) => return,
            Err(err @ RenderError::SurfaceRejected(_)) => {
                inner.reject(&err);
                return;
            }
            Err(err) => {
                tracing::warn!(error = %err, "no se pudo dibujar la ventana");
                inner.outdated = true;
                return;
            }
        };
        // Desde la primera importación hasta la entrega, ninguna otra entrega a la cola.
        let _order = self.submit_lock();
        // Las fuentes vivas suben su fotograma pendiente y se colocan donde dijo la interfaz.
        inner.feeds.retain(|weak| weak.strong_count() > 0);
        let feeds: Vec<Arc<VideoFeed>> = inner.feeds.iter().filter_map(Weak::upgrade).collect();
        let render_format = inner.render_format;
        let hdr = inner.hdr.is_some();
        let mut shown: Vec<u64> = Vec::with_capacity(feeds.len());
        for feed in &feeds {
            let Some(area) = feed.area() else {
                continue;
            };
            let layer = inner
                .layers
                .entry(feed.id())
                .or_insert_with(|| VideoLayer::new(&self.device, &self.queue, render_format));
            if hdr && render_format == HDR_FORMAT {
                layer.set_output(coroom_render::OutputMode::Hdr {
                    headroom: super::video::hdr_headroom(),
                });
            }
            feed.prepare(layer, &self.device, &self.queue);
            if layer.frame_size().is_none() {
                continue;
            }
            layer.place(&self.queue, area);
            shown.push(feed.id());
        }
        let surface_view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(if hdr {
                inner.config.format
            } else {
                render_format
            }),
            ..Default::default()
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("coroom-compositor"),
            });
        {
            let size = (inner.config.width, inner.config.height);
            let target = match inner.hdr.as_mut() {
                Some((output, _)) => output.target(&self.device, size).clone(),
                None => surface_view.clone(),
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("coroom-compositor"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(inner.background),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            for id in &shown {
                if let Some(layer) = inner.layers.get(id) {
                    layer.draw(&mut pass);
                }
            }
            if let Some(ui) = inner.ui.as_ref().filter(|ui| ui.drawn && ui.size == size) {
                inner.blit.draw(&mut pass, &ui.bind_group);
            }
        }
        if let Some((output, _)) = inner.hdr.as_ref() {
            output.composite(&mut encoder, &surface_view);
        }
        self.queue.submit([encoder.finish()]);
        if let Some(before_present) = before_present {
            before_present();
        }
        self.queue.present(frame);
        inner.presented += 1;
        inner.ui_pending = false;
        for feed in &feeds {
            feed.commit_shown();
        }
    }
}
