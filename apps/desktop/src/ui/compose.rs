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
//! Un solo candado ordena los dos hilos: la importación sin copias de una imagen Vulkan deja su
//! semáforo para la **siguiente** entrega de la cola, así que entre importar y entregar no puede
//! colarse otra entrega. El hilo de la ventana dibuja `iced` con el candado tomado (unos ms, solo
//! cuando la interfaz cambia) y el de `present` nunca espera a que la interfaz se reconstruya.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use coroom_render::hdr::{HDR_FORMAT, HdrOutput};
use coroom_render::layer::VideoLayer;
use coroom_render::overlay::OverlayBlit;
use coroom_render::wgpu::{Acquired, GpuContext, HdrSurface, SurfaceSetup, acquire};
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
    ui: Option<UiTexture>,
    /// Fondo de la composición (el de la ventana o el del vídeo), como lo limpia `iced`.
    background: wgpu::Color,
    feeds: Vec<Weak<VideoFeed>>,
    layers: HashMap<u64, VideoLayer>,
    /// Hay que reconfigurar la superficie antes de la próxima composición.
    outdated: bool,
    presented: u64,
}

/// El compositor de la ventana (ver el módulo). Se comparte entre la ventana y las fuentes de
/// vídeo ([`VideoFeed`]).
pub struct Compositor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    inner: Mutex<Inner>,
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
                background: wgpu::Color::BLACK,
                feeds: Vec::new(),
                layers: HashMap::new(),
                outdated: false,
                presented: 0,
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
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
        inner.surface.configure(&self.device, &inner.config);
        inner.outdated = false;
        inner.ui = None;
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
        inner.surface.configure(&self.device, &setup.config);
        inner.config = setup.config.clone();
        if setup.render_format != inner.render_format {
            inner.render_format = setup.render_format;
            inner.blit = OverlayBlit::new(&self.device, setup.render_format);
            inner.layers.clear();
        }
        inner.ui = None;
        inner.hdr = super::runner::make_hdr(gpu, setup.hdr);
        inner.outdated = false;
        Ok(setup)
    }

    /// Una fuente de vídeo que se compone en la ventana (hasta que se suelte).
    pub fn register(&self, feed: &Arc<VideoFeed>) {
        let mut inner = self.lock();
        inner.feeds.retain(|weak| weak.strong_count() > 0);
        inner.feeds.push(Arc::downgrade(feed));
    }

    /// Dibuja la interfaz: `draw` recibe la vista de la textura de la interfaz (limpiarla a
    /// transparente y dibujar encima) y, después, se compone con el vídeo y se entrega.
    /// `background` es el fondo bajo la interfaz y el vídeo.
    pub fn draw_ui(&self, background: Color, draw: impl FnOnce(&wgpu::TextureView)) {
        let mut inner = self.lock();
        inner.background = clear_color(background);
        let size = (inner.config.width, inner.config.height);
        if inner.ui.as_ref().is_none_or(|ui| ui.size != size) {
            let ui = self.make_ui_texture(&inner, size);
            inner.ui = Some(ui);
        }
        if let Some(ui) = inner.ui.as_mut() {
            draw(&ui.view);
            ui.drawn = true;
        }
        self.composite(&mut inner);
    }

    /// Llegó un fotograma a alguna fuente: compone y entrega sin esperar a la interfaz.
    pub fn present_video(&self) {
        let mut inner = self.lock();
        self.composite(&mut inner);
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
    /// fotograma ya salió).
    fn composite(&self, inner: &mut Inner) {
        if inner.outdated {
            inner.surface.configure(&self.device, &inner.config);
            inner.outdated = false;
        }
        let frame = match acquire(&inner.surface, &self.device, &inner.config) {
            Ok(Acquired::Frame(frame)) => frame,
            Ok(Acquired::Skip) => return,
            Err(err) => {
                tracing::warn!(error = %err, "no se pudo dibujar la ventana");
                inner.outdated = true;
                return;
            }
        };
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
        self.queue.present(frame);
        inner.presented += 1;
        for feed in &feeds {
            feed.commit_shown();
        }
    }
}
