//! Prueba de latencia local (`coroom probe`): el pipeline de `coroom_session::loopback` con la
//! fuente elegida y un presentador de `coroom_render`.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use coroom_render::{
    ColorInfo, ColorMatrix, CursorImage, CursorOverlay, ExternalImageState, ExternalImageUpdate,
    ExternalVulkanImage, NullPresenter, PlanarImage, PlanarLayout, PlaneBuffer, PresentFrame,
    PresentImage, Presenter, Recycle, VulkanDeviceShare, VulkanFrame,
};
use coroom_session::capture::synthetic::SyntheticCapturer;
use coroom_session::capture::{self, Capturer, CursorShape};
use coroom_session::codec::color::{Matrix, Range, Transfer};
use coroom_session::codec::gpu::{
    SharedFeatures, SharedQueueFamily, SharedVulkanDevice, VulkanFrameAccess, VulkanFrameUpdate,
};
use coroom_session::codec::{
    Backend, BufferPool, Codec, ColorSpace, DEFAULT_SLICES, DecodeOutput, DecoderBackend,
    DecoderSet, EncoderConfig, EncoderSet, GpuImage, PixelFormat, RawFrame,
};
use coroom_session::latency::{LatencyLedger, LatencySummary, Stage};
use coroom_session::loopback::{
    DecodedFrame, FrameSink, LoopbackConfig, LoopbackPipeline, SinkError,
};

/// Fuente de vídeo de la prueba.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Patrón sintético a cadencia fija.
    Synthetic,
    /// Pantalla real: portal `ScreenCast` en Linux, monitor principal en Windows.
    Screen,
}

impl Source {
    /// Nombre legible.
    pub const fn label(self) -> &'static str {
        match self {
            Source::Synthetic => "sintética",
            Source::Screen => "pantalla",
        }
    }
}

/// Parámetros de `coroom probe`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeOptions {
    /// Fuente de vídeo.
    pub source: Source,
    /// Configuración del pipeline.
    pub config: LoopbackConfig,
    /// Códec.
    pub codec: Codec,
    /// Backend del codificador (`None`: el mejor que funcione).
    pub encoder: Option<Backend>,
    /// Backend del decodificador (`None`: el mejor que funcione).
    pub decoder: Option<DecoderBackend>,
}

impl ProbeOptions {
    /// Prueba con OpenH264 en los dos extremos (la de referencia).
    pub fn h264(source: Source, config: LoopbackConfig) -> Self {
        Self {
            source,
            config,
            codec: Codec::H264,
            encoder: None,
            decoder: None,
        }
    }
}

impl fmt::Display for ProbeOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = &self.config;
        let backend = |id: Option<&'static str>| id.unwrap_or("auto");
        write!(
            f,
            "fuente {}, {}×{} a {} fps, {} kbit/s, {} {} (codificador {}, decodificador {}), ",
            self.source.label(),
            c.width,
            c.height,
            c.fps,
            c.bitrate_kbps,
            self.codec,
            c.wanted_format(),
            backend(self.encoder.map(Backend::id)),
            backend(self.decoder.map(DecoderBackend::id)),
        )?;
        match c.frames {
            Some(n) => write!(f, "{n} fotogramas"),
            None => f.write_str("sin límite de fotogramas"),
        }
    }
}

/// Adapta un [`Presenter`] de `coroom-render` a la última etapa del pipeline de `coroom-session`.
pub struct PresenterSink<P: Presenter> {
    presenter: P,
    /// Última forma del cursor convertida (se reutiliza mientras no cambie).
    cursor: Option<(Arc<CursorShape>, Arc<CursorImage>)>,
}

impl<P: Presenter> PresenterSink<P> {
    /// Envuelve un presentador.
    pub fn new(presenter: P) -> Self {
        Self {
            presenter,
            cursor: None,
        }
    }

    /// Presentador envuelto.
    pub fn presenter(&self) -> &P {
        &self.presenter
    }
}

/// Puente entre la imagen Vulkan de `coroom-codec` (FFmpeg) y la de `coroom-render`: las dos
/// crates no se conocen (`docs/ARQUITECTURA.md` §5).
struct VulkanBridge(Arc<dyn VulkanFrameAccess>);

impl ExternalVulkanImage for VulkanBridge {
    fn lock(&self) -> ExternalImageState {
        let state = self.0.lock();
        ExternalImageState {
            image: state.image,
            layout: state.layout,
            access: state.access,
            semaphore: state.semaphore,
            semaphore_value: state.semaphore_value,
            queue_family: state.queue_family,
        }
    }

    fn unlock(&self, update: Option<ExternalImageUpdate>) {
        self.0.unlock(update.map(|update| VulkanFrameUpdate {
            layout: update.layout,
            access: update.access,
            semaphore_value: update.semaphore_value,
            queue_family: update.queue_family,
        }));
    }
}

/// Puente entre el pool de búferes del decodificador (`coroom-codec`) y el búfer de la imagen de
/// `coroom-render`, que lo devuelve al soltarse.
struct PoolBridge(Arc<BufferPool>);

impl Recycle for PoolBridge {
    fn recycle(&self, buffer: Vec<u8>) {
        self.0.give(buffer);
    }
}

/// Dónde debe dejar el decodificador las imágenes para este presentador: en su dispositivo Vulkan
/// si lo comparte (cero copias), si no en CPU.
pub fn decode_output_for(share: Option<Arc<VulkanDeviceShare>>) -> DecodeOutput {
    match share {
        Some(share) => {
            tracing::info!(
                "vídeo recibido: con Vulkan Video se decodifica en el dispositivo de la ventana, sin \
                 copia; extensiones de vídeo que el driver ofrece y ese dispositivo no activa: {}",
                coroom_render::wgpu::unused_label(&share.unused_video_extensions)
            );
            DecodeOutput::Gpu {
                vulkan: Some(Arc::new(to_shared_device(&share))),
            }
        }
        None => {
            tracing::info!(
                "vídeo recibido: se copia a memoria para presentarlo (sin dispositivo Vulkan \
                 compartido, o COROOM_ZERO_COPY=0)"
            );
            DecodeOutput::Cpu
        }
    }
}

/// El dispositivo de `coroom-render` con los tipos de `coroom-codec`.
pub fn to_shared_device(share: &Arc<VulkanDeviceShare>) -> SharedVulkanDevice {
    SharedVulkanDevice {
        get_instance_proc_addr: share.get_instance_proc_addr,
        instance: share.instance,
        physical_device: share.physical_device,
        device: share.device,
        instance_extensions: share.instance_extensions.clone(),
        device_extensions: share.device_extensions.clone(),
        queue_families: share
            .queue_families
            .iter()
            .map(|family| SharedQueueFamily {
                index: family.index,
                count: family.count,
                flags: family.flags,
                video_caps: family.video_caps,
            })
            .collect(),
        features: SharedFeatures {
            timeline_semaphore: share.timeline_semaphore,
            synchronization2: share.synchronization2,
            sampler_ycbcr_conversion: share.sampler_ycbcr_conversion,
        },
        // El dispositivo de `wgpu` vive lo que viva el último que lo use.
        keepalive: Arc::clone(share) as Arc<dyn std::any::Any + Send + Sync>,
    }
}

/// Espacio de color de `coroom-codec` en `coroom-render` (las dos crates no se conocen).
pub fn to_color_info(color: ColorSpace) -> ColorInfo {
    ColorInfo {
        matrix: match color.matrix {
            Matrix::Bt709 => ColorMatrix::Bt709,
            Matrix::Bt2020 => ColorMatrix::Bt2020,
        },
        full_range: color.range == Range::Full,
        pq: color.transfer == Transfer::Pq,
    }
}

/// Convierte un fotograma de `coroom-codec` (I420, NV12, I444 o P010 compacto, o una imagen de
/// GPU) en uno de `coroom-render`, sin convertir el color (el shader entiende todos), sin partir
/// ni copiar los planos (viajan en su búfer, con sus desplazamientos) y sin copiar las imágenes de
/// GPU. El búfer vuelve al pool del decodificador cuando la presentación lo suelta.
pub fn to_present_frame(frame: RawFrame) -> Result<PresentFrame, SinkError> {
    let color = to_color_info(frame.color);
    if frame.format == PixelFormat::Gpu {
        let image = match &frame.gpu {
            Some(GpuImage::Vulkan(image)) => PresentImage::Vulkan(VulkanFrame {
                width: image.width,
                height: image.height,
                image_width: image.image_width,
                image_height: image.image_height,
                format: image.format,
                device: image.device,
                image: Arc::new(VulkanBridge(Arc::clone(&image.access))),
            }),
            Some(GpuImage::Opaque(_)) => PresentImage::Gpu,
            other => {
                return Err(SinkError(format!(
                    "la presentación no recibe imágenes {}",
                    other.as_ref().map_or("vacías", |image| image.kind())
                )));
            }
        };
        return Ok(PresentFrame {
            frame_id: frame.frame_id,
            width: frame.width,
            height: frame.height,
            image,
            color,
        });
    }
    let layout = match frame.format {
        PixelFormat::I420 => PlanarLayout::I420,
        PixelFormat::Nv12 => PlanarLayout::Nv12,
        PixelFormat::I444 => PlanarLayout::I444,
        PixelFormat::P010 => PlanarLayout::P010,
        other => {
            return Err(SinkError(format!(
                "se esperaba I420, NV12, I444 o P010 compacto y llegó {other:?}"
            )));
        }
    };
    let extents = layout.plane_extents(frame.width, frame.height);
    let row = extents[0].0;
    if frame.stride < row {
        return Err(SinkError(format!(
            "fila de {} bytes demasiado corta para {:?}",
            frame.stride, frame.format
        )));
    }
    let chroma_len: usize = extents[1..]
        .iter()
        .map(|&(row, rows)| row as usize * rows as usize)
        .sum();
    let y_len = frame.stride as usize * frame.height as usize;
    if frame.data.len() < y_len + chroma_len {
        return Err(SinkError("planos de vídeo incompletos".into()));
    }
    let RawFrame {
        frame_id,
        width,
        height,
        stride,
        data,
        pool,
        ..
    } = frame;
    let buffer = match pool {
        Some(pool) => PlaneBuffer::recycled(data, Arc::new(PoolBridge(pool))),
        None => PlaneBuffer::new(data),
    };
    Ok(PresentFrame {
        frame_id,
        width,
        height,
        image: PresentImage::Planar(PlanarImage::compact(layout, width, height, stride, buffer)),
        color,
    })
}

/// Forma del cursor de la captura para `coroom-render`.
pub fn to_cursor_image(shape: &CursorShape) -> CursorImage {
    CursorImage {
        id: shape.shape_id,
        width: shape.width,
        height: shape.height,
        hotspot_x: shape.hotspot_x,
        hotspot_y: shape.hotspot_y,
        rgba: shape.rgba.clone(),
    }
}

impl<P: Presenter> FrameSink for PresenterSink<P> {
    fn present(&mut self, decoded: DecodedFrame) -> Result<(), SinkError> {
        if let Some(cursor) = decoded.cursor {
            self.presenter.set_cursor(CursorOverlay {
                x: cursor.x,
                y: cursor.y,
                visible: cursor.visible,
            });
        }
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
        self.presenter.set_cursor_image(image);
        let frame = to_present_frame(decoded.frame)?;
        self.presenter
            .present(&frame)
            .map_err(|err| SinkError(err.to_string()))
    }

    fn cpu_copies(&self) -> u32 {
        self.presenter.cpu_copies()
    }
}

/// Crea la fuente de vídeo.
pub fn make_capturer(options: &ProbeOptions) -> Result<Box<dyn Capturer>, String> {
    match options.source {
        Source::Synthetic => Ok(Box::new(SyntheticCapturer::new(
            options.config.width,
            options.config.height,
            options.config.fps,
        ))),
        Source::Screen => capture::screen_capturer()
            .map_err(|err| format!("no se puede capturar la pantalla: {err}")),
    }
}

/// Arranca el pipeline con el códec y los backends pedidos (o los mejores que funcionen) y la
/// presentación `sink`, con la imagen decodificada en CPU.
pub fn start_pipeline(
    options: &ProbeOptions,
    sink: Box<dyn FrameSink>,
    ledger: Arc<LatencyLedger>,
) -> Result<LoopbackPipeline, String> {
    start_pipeline_with(options, sink, ledger, DecodeOutput::Cpu)
}

/// Mensaje cuando el backend pedido no está en este equipo.
fn unavailable(what: &str, codec: Codec, backend: Option<String>) -> String {
    match backend {
        Some(backend) => format!(
            "backend no disponible: este equipo no {what} {codec} con {backend} (mira `coroom \
             doctor`; sin GPU solo hay software)"
        ),
        None => format!("este equipo no {what} {codec} (mira `coroom doctor`)"),
    }
}

/// Como [`start_pipeline`], con la salida del decodificador `output` (en la GPU para el
/// presentador que comparte su dispositivo, o sin ventana).
pub fn start_pipeline_with(
    options: &ProbeOptions,
    sink: Box<dyn FrameSink>,
    ledger: Arc<LatencyLedger>,
    output: DecodeOutput,
) -> Result<LoopbackPipeline, String> {
    let codec = options.codec;
    let encoders = EncoderSet::probe()
        .with_calibration_cache(Some(
            crate::remote::default_data_dir().join("calibration.json"),
        ))
        .only_codec(Some(codec))
        .only_backend(options.encoder);
    if encoders.is_empty() {
        return Err(unavailable(
            "codifica",
            codec,
            options.encoder.map(|backend| backend.to_string()),
        ));
    }
    let decoders = DecoderSet::probe()
        .only_codec(Some(codec))
        .only_backend(options.decoder)
        .with_output(output);
    if decoders.is_empty() {
        return Err(unavailable(
            "decodifica",
            codec,
            options.decoder.map(|backend| backend.to_string()),
        ));
    }
    let c = &options.config;
    let (encoder, calibrated) = encoders
        .open_calibrated(&EncoderConfig {
            codec,
            width: c.width,
            height: c.height,
            fps: c.fps,
            bitrate_kbps: c.bitrate_kbps,
            slices: DEFAULT_SLICES,
            format: c.wanted_format(),
        })
        .map_err(|err| format!("no se pudo abrir el codificador: {err}"))?;
    if let Some(choice) = calibrated {
        tracing::info!(
            backend = %choice.backend,
            p50_ms = choice.p50.as_secs_f64() * 1000.0,
            p95_ms = choice.p95.as_secs_f64() * 1000.0,
            from_cache = choice.from_cache,
            kbit_s = choice.kbit_s,
            "codificador «auto» calibrado"
        );
    }
    let decoder = decoders
        .open_format(codec, c.wanted_format())
        .map_err(|err| format!("no se pudo abrir el decodificador: {err}"))?;
    let capturer = make_capturer(options)?;
    LoopbackPipeline::start(capturer, encoder, decoder, sink, ledger, options.config)
        .map_err(|err| format!("no se pudo arrancar la prueba: {err}"))
}

/// Fotogramas presentados a los que les falta alguna marca (debería ser cero).
pub fn incomplete_presented(ledger: &LatencyLedger) -> usize {
    ledger
        .timelines()
        .iter()
        .filter(|timeline| timeline.at(Stage::Presented).is_some() && !timeline.is_complete())
        .count()
}

/// Ejecuta la prueba sin ventana con [`NullPresenter`].
///
/// Si `config.frames` es `None`, la prueba dura hasta que `until_stopped` devuelva (por ejemplo,
/// al pulsar Intro). Sin ventana no hay dónde mostrar la imagen: los decodificadores de GPU la
/// dejan en la GPU (cero copias; los de software, en CPU como siempre).
pub fn run_headless(
    options: &ProbeOptions,
    until_stopped: impl FnOnce(&LoopbackPipeline),
) -> Result<LatencySummary, String> {
    let ledger = Arc::new(LatencyLedger::new());
    let sink = Box::new(PresenterSink::new(NullPresenter::new()));
    let pipeline = start_pipeline_with(options, sink, ledger, DecodeOutput::Gpu { vulkan: None })?;
    if options.config.frames.is_some() {
        while !pipeline.wait(Duration::from_millis(250)) {}
    } else {
        until_stopped(&pipeline);
    }
    let failure = pipeline.failure();
    let summary = pipeline.stop();
    match failure {
        Some(err) => Err(format!("la prueba se detuvo: {err}")),
        None => Ok(summary),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coroom_session::loopback::DecodedFrame;

    fn planar(present: PresentFrame, layout: PlanarLayout) -> PlanarImage {
        let PresentImage::Planar(image) = present.image else {
            panic!("se esperaba una imagen planar");
        };
        assert_eq!(image.layout, layout);
        image
    }

    #[test]
    fn i420_frames_travel_in_one_buffer_with_plane_offsets() {
        let frame = RawFrame::cpu(4, (4, 2), PixelFormat::I420, 4, (0..12).collect());
        let image = planar(to_present_frame(frame).unwrap(), PlanarLayout::I420);
        assert_eq!(&image.plane(0)[..8], &(0..8).collect::<Vec<u8>>()[..]);
        assert_eq!(&image.plane(1)[..2], &[8, 9]);
        assert_eq!(&image.plane(2)[..2], &[10, 11]);
        assert_eq!(
            image.planes.map(|p| (p.offset, p.stride)),
            [(0, 4), (8, 2), (10, 2)]
        );
        assert_eq!(image.data.len(), 12, "sin copias ni recortes");
    }

    #[test]
    fn nv12_frames_pass_through() {
        let frame = RawFrame::cpu(4, (4, 2), PixelFormat::Nv12, 4, (0..12).collect());
        let image = planar(to_present_frame(frame).unwrap(), PlanarLayout::Nv12);
        assert_eq!(&image.plane(0)[..8], &(0..8).collect::<Vec<u8>>()[..]);
        assert_eq!(image.plane(1), &[8, 9, 10, 11]);
        assert_eq!(
            image.planes[..2]
                .iter()
                .map(|p| (p.offset, p.stride))
                .collect::<Vec<_>>(),
            [(0, 4), (8, 4)]
        );
    }

    #[test]
    fn ten_bit_and_full_chroma_frames_pass_through_with_their_colour() {
        let p010 = RawFrame::cpu(5, (2, 2), PixelFormat::P010, 4, (0..12).collect())
            .with_color(ColorSpace::HDR10);
        let present = to_present_frame(p010).unwrap();
        assert_eq!(present.color, ColorInfo::HDR10);
        let image = planar(present, PlanarLayout::P010);
        assert_eq!(image.plane(0).len(), 12);
        assert_eq!(image.plane(1), &[8, 9, 10, 11]);
        assert_eq!((image.planes[0].stride, image.planes[1].stride), (4, 4));
        let i444 = RawFrame::cpu(6, (2, 1), PixelFormat::I444, 2, (0..6).collect());
        let present = to_present_frame(i444).unwrap();
        assert_eq!(present.color, ColorInfo::SDR);
        let image = planar(present, PlanarLayout::I444);
        assert_eq!(
            (&image.plane(1)[..2], image.plane(2)),
            (&[2, 3][..], &[4, 5][..])
        );
    }

    /// El búfer de un fotograma que salió de un pool vuelve a él cuando la presentación lo suelta.
    #[test]
    fn pooled_buffers_return_to_the_decoder_pool() {
        let pool = BufferPool::shared();
        let frame = RawFrame::cpu(4, (4, 2), PixelFormat::Nv12, 4, pool.take(12))
            .with_pool(Arc::clone(&pool));
        assert_eq!(pool.free_count(), 0);
        let present = to_present_frame(frame).unwrap();
        drop(present);
        assert_eq!(pool.free_count(), 1, "devuelto al soltar la imagen");
        let short = RawFrame::cpu(4, (4, 2), PixelFormat::Nv12, 4, vec![0; 11]);
        assert!(to_present_frame(short).is_err());
    }

    #[test]
    fn cursor_shapes_reach_the_presenter() {
        let shape = Arc::new(coroom_session::capture::synthetic::cursor_shape());
        let mut sink = PresenterSink::new(NullPresenter::new());
        let frame = RawFrame::cpu(
            1,
            (2, 2),
            PixelFormat::I420,
            2,
            vec![16, 16, 16, 16, 128, 128],
        );
        let cursor = coroom_session::capture::CursorState {
            x: 1,
            y: 1,
            visible: true,
            shape_id: shape.shape_id,
        };
        for _ in 0..2 {
            sink.present(DecodedFrame {
                frame: frame.clone(),
                cursor: Some(cursor),
                cursor_shape: Some(Arc::clone(&shape)),
            })
            .unwrap();
        }
        let image = sink.presenter().cursor_image().expect("forma").clone();
        assert_eq!((image.id, image.width), (shape.shape_id, shape.width));
        assert_eq!(image.rgba, shape.rgba);
    }

    #[test]
    fn non_i420_frames_are_rejected() {
        let frame = RawFrame::cpu(4, (4, 2), PixelFormat::Bgra, 16, vec![0; 32]);
        assert!(to_present_frame(frame).is_err());
    }

    /// Las imágenes de GPU llegan a la presentación sin copiarse: las opacas (sin ventana) como
    /// [`PresentImage::Gpu`], que el presentador nulo acepta, y las que no son de salida (un
    /// DMA-BUF de la captura) se rechazan.
    #[test]
    fn gpu_frames_reach_the_presenter_without_copies() {
        use coroom_session::codec::gpu::OpaqueImage;

        let opaque = GpuImage::Opaque(Arc::new(OpaqueImage {
            kind: "vaapi".into(),
            keepalive: Arc::new(()),
        }));
        let frame = RawFrame::gpu(7, (64, 32), opaque);
        let present = to_present_frame(frame.clone()).unwrap();
        assert_eq!(present.image, PresentImage::Gpu);
        assert_eq!((present.width, present.height), (64, 32));
        let mut sink = PresenterSink::new(NullPresenter::new());
        sink.present(DecodedFrame {
            frame,
            cursor: None,
            cursor_shape: None,
        })
        .unwrap();
        assert_eq!(sink.cpu_copies(), 0);
        let empty = RawFrame {
            gpu: None,
            ..RawFrame::gpu(
                8,
                (2, 2),
                GpuImage::Opaque(Arc::new(OpaqueImage {
                    kind: "x".into(),
                    keepalive: Arc::new(()),
                })),
            )
        };
        assert!(to_present_frame(empty).is_err());
        assert!(matches!(decode_output_for(None), DecodeOutput::Cpu));
    }

    #[test]
    fn options_describe_themselves_in_spanish() {
        let mut options = ProbeOptions::h264(
            Source::Synthetic,
            LoopbackConfig {
                frames: Some(600),
                ..LoopbackConfig::default()
            },
        );
        assert_eq!(
            options.to_string(),
            "fuente sintética, 1920×1080 a 60 fps, 10000 kbit/s, H.264 4:2:0 SDR (codificador \
             auto, decodificador auto), 600 fotogramas"
        );
        options.codec = Codec::Av1;
        options.encoder = Some(Backend::Software);
        assert!(
            options
                .to_string()
                .contains("AV1 4:2:0 SDR (codificador software")
        );
        options.config.hdr = true;
        assert!(options.to_string().contains("AV1 4:2:0 HDR10 10 bits"));
    }
}
