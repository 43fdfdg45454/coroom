//! Decodificador de FFmpeg ([`FfmpegDecoder`]): software (dav1d, `h264`, `hevc`) o `hwaccel`.
//!
//! Dónde deja las imágenes lo decide [`DecodeOutput`]:
//!
//! - [`DecodeOutput::Cpu`]: la imagen se deja en un búfer de un [`BufferPool`] (reutilizado entre
//!   fotogramas: ni reserva ni relleno de ceros por fotograma). De la GPU se baja con
//!   `av_hwframe_transfer_data` directamente a ese búfer (una copia: un `AVFrame` cuyos planos
//!   apuntan a él, [`frame_over`]); de un decodificador por software se copia a él (una copia).
//!   Quien presenta el fotograma devuelve el búfer con [`RawFrame::recycle`].
//! - [`DecodeOutput::Gpu`] con el dispositivo Vulkan del presentador: con el backend Vulkan, FFmpeg
//!   decodifica en ese mismo `VkDevice` y la imagen sale tal cual ([`GpuImage::Vulkan`], cero
//!   copias); con VA-API o D3D11VA se intenta mapear a Vulkan con `av_hwframe_map` (también sin
//!   copia) y, si FFmpeg no sabe, se baja a CPU. NVDEC (CUDA) no se mapea a Vulkan en FFmpeg 8.1:
//!   se baja a CPU.
//! - [`DecodeOutput::Gpu`] sin dispositivo (sin ventana): la imagen de GPU se queda donde la dejó
//!   el decodificador ([`GpuImage::Opaque`]), sin copia.
//!
//! Con la salida en la GPU, [`Decoder::decode`] devuelve la imagen en cuanto FFmpeg la entrega: la
//! GPU puede seguir escribiéndola, y es el presentador quien espera en la GPU al semáforo de la
//! imagen antes de muestrearla (`coroom-render`). La marca `decodificado` de la sesión mide, por
//! tanto, la entrega del decodificador, no el fin del trabajo de la GPU. El hilo de decodificación
//! no se bloquea nunca esperando a la GPU; solo comprueba, sin esperar, que la imagen del fotograma
//! anterior terminó en [`GPU_WAIT_NS`] (si no, el dispositivo se perdió o se atascó y el fotograma
//! cuenta como error de decodificación).

use std::sync::Arc;
use std::time::Instant;

use rsmpeg::avcodec::{AVCodec, AVCodecContext, AVCodecRef, AVPacket};
use rsmpeg::avutil::{AVFrame, AVHWDeviceContext, AVHWFramesContext};
use rsmpeg::error::RsmpegError;
use rsmpeg::ffi;

use super::vulkan::{SemaphoreWaiter, VkFrameAccess, buffer_key, device_from_shared, handles};
use super::{backend_error, create_device, decoder_device, decoder_name, hw_pixel_format};
use crate::gpu::{GpuImage, OpaqueImage, SharedVulkanDevice, VulkanImage};
use crate::pool::BufferPool;
use crate::{
    Codec, CodecError, ColorSpace, DecodeOutput, Decoder, DecoderBackend, EncodedSlice,
    PixelFormat, RawFrame,
};

/// Fotogramas de más en el pool del decodificador de GPU cuando la imagen sale sin copia: los que
/// esperan en el canal hacia la presentación, el que se presenta y los que la GPU aún muestrea.
const EXTRA_HW_FRAMES: i32 = 6;

/// Tiempo máximo que puede tardar la GPU en terminar una imagen que sale sin copia: se comprueba
/// sin esperar, al fotograma siguiente.
const GPU_WAIT_NS: u64 = 100_000_000;

/// Imagen de GPU del último fotograma entregado, para comprobar al siguiente que la GPU la terminó.
struct PendingGpu {
    device: usize,
    semaphore: u64,
    value: u64,
    since: Instant,
}

/// Opciones de baja latencia de los decodificadores: sin hilos por fotograma (cada uno añade un
/// fotograma de retardo), solo por slice, y `low_delay`; dav1d con un único fotograma en vuelo.
fn low_latency_options(codec: Codec, backend: DecoderBackend) -> Vec<(&'static str, &'static str)> {
    let mut options = vec![("flags", "low_delay")];
    if codec == Codec::Av1 && backend == DecoderBackend::Software {
        options.push(("max_frame_delay", "1"));
    } else {
        options.push(("thread_type", "slice"));
    }
    options
}

/// Decodificador de FFmpeg.
pub struct FfmpegDecoder {
    codec: Codec,
    backend: DecoderBackend,
    av_codec: AVCodecRef<'static>,
    device: Option<AVHWDeviceContext>,
    hw_format: ffi::AVPixelFormat,
    ctx: AVCodecContext,
    hw_frames: u64,
    sw_frames: u64,
    output: DecodeOutput,
    /// El dispositivo de decodificación es el Vulkan del presentador.
    device_is_shared: bool,
    /// Dispositivo Vulkan del presentador para mapear (VA-API, D3D11VA → Vulkan), creado al
    /// primer fotograma.
    map_device: Option<AVHWDeviceContext>,
    /// Contexto de fotogramas Vulkan derivado del de la decodificación, por identidad de este.
    map_frames: Option<(usize, AVHWFramesContext)>,
    /// El mapeo a Vulkan falló: se baja a CPU.
    map_failed: bool,
    /// Copias a CPU del último fotograma.
    copies: u32,
    /// Fotogramas que salieron sin copia (en la GPU).
    gpu_frames: u64,
    /// Sondeo del semáforo de las imágenes Vulkan que salen sin copia (por dispositivo).
    waiters: Vec<(usize, SemaphoreWaiter)>,
    /// Última imagen de GPU entregada cuya GPU no se ha comprobado aún.
    pending_gpu: Option<PendingGpu>,
    /// Búferes de la salida en CPU.
    pool: Arc<BufferPool>,
}

/// `AVFrame` que solo se mantiene vivo (imagen de GPU opaca).
struct FrameKeep(#[allow(dead_code)] AVFrame);

// SAFETY: el `AVFrame` no se lee ni se escribe; solo se suelta, y los contadores de referencias de
// libavutil son atómicos.
unsafe impl Send for FrameKeep {}
// SAFETY: ver `Send`: no hay acceso compartido al fotograma.
unsafe impl Sync for FrameKeep {}

impl std::fmt::Debug for FfmpegDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfmpegDecoder")
            .field("codec", &self.codec)
            .field("backend", &self.backend)
            .field("hw_frames", &self.hw_frames)
            .field("sw_frames", &self.sw_frames)
            .finish_non_exhaustive()
    }
}

impl FfmpegDecoder {
    /// Abre el decodificador de `codec` con `backend` (crea el dispositivo de GPU si hace falta)
    /// y la salida en CPU.
    pub fn new(codec: Codec, backend: DecoderBackend) -> Result<Self, CodecError> {
        Self::with_output(codec, backend, DecodeOutput::Cpu)
    }

    /// Como [`FfmpegDecoder::new`], con la salida que se pida (ver el módulo). Con el backend
    /// Vulkan y el dispositivo del presentador, FFmpeg decodifica en ese dispositivo.
    pub fn with_output(
        codec: Codec,
        backend: DecoderBackend,
        output: DecodeOutput,
    ) -> Result<Self, CodecError> {
        if backend == DecoderBackend::MediaCodec {
            return Err(CodecError::DecoderUnavailable(backend));
        }
        super::init_logging();
        let av_codec = AVCodec::find_decoder_by_name(decoder_name(codec, backend))
            .ok_or(CodecError::DecoderUnavailable(backend))?;
        let device_type = decoder_device(backend);
        let device = match device_type {
            Some(device) => {
                // El decodificador tiene que saber usar ese dispositivo.
                let supported = (0..)
                    .map_while(|index| av_codec.hw_config(index))
                    .any(|config| {
                        config.device_type == device
                            && config.methods & super::HW_CONFIG_DEVICE_CTX != 0
                    });
                if !supported {
                    return Err(CodecError::DecoderUnavailable(backend));
                }
                Some(device)
            }
            None => None,
        };
        let shared = match &output {
            DecodeOutput::Gpu {
                vulkan: Some(shared),
            } if backend == DecoderBackend::Vulkan => device_from_shared(shared).ok(),
            _ => None,
        };
        let device_is_shared = shared.is_some();
        let device = match (shared, device) {
            (Some(shared), _) => Some(shared),
            (None, Some(device)) => {
                Some(create_device(device).map_err(|_| CodecError::DecoderUnavailable(backend))?)
            }
            (None, None) => None,
        };
        let ctx = Self::open(&av_codec, codec, backend, device.as_ref(), output.is_gpu())?;
        Ok(Self {
            codec,
            backend,
            av_codec,
            device,
            hw_format: device_type.map_or(ffi::AV_PIX_FMT_NONE, hw_pixel_format),
            ctx,
            hw_frames: 0,
            sw_frames: 0,
            output,
            device_is_shared,
            map_device: None,
            map_frames: None,
            map_failed: false,
            copies: 0,
            gpu_frames: 0,
            waiters: Vec::new(),
            pending_gpu: None,
            pool: BufferPool::shared(),
        })
    }

    /// Anota la imagen Vulkan de `access` (del dispositivo `device`) que se entrega ahora y
    /// comprueba, **sin esperar**, que la GPU terminó la del fotograma anterior: si lleva más de
    /// [`GPU_WAIT_NS`] sin acabar, el dispositivo se perdió o se atascó y el fotograma cuenta como
    /// error de decodificación (el presentador espera en la GPU al semáforo de cada imagen; aquí
    /// no hace falta bloquear el hilo).
    fn track_gpu(
        &mut self,
        device: &AVHWDeviceContext,
        access: &VkFrameAccess,
    ) -> Result<(), CodecError> {
        let key = buffer_key(device.as_ptr());
        if !self.waiters.iter().any(|(known, _)| *known == key)
            && let Some(waiter) = SemaphoreWaiter::new(device)
        {
            self.waiters.push((key, waiter));
        }
        let Some((_, waiter)) = self.waiters.iter().find(|(known, _)| *known == key) else {
            // Sin `vkWaitSemaphores` no se puede comprobar: se presenta como hasta ahora.
            return Ok(());
        };
        if let Some(pending) = self.pending_gpu.take()
            && pending.device == key
            && !waiter.wait(pending.semaphore, pending.value, 0)
        {
            if pending.since.elapsed().as_nanos() > u128::from(GPU_WAIT_NS) {
                return Err(self.gpu_timeout());
            }
            // Aún trabaja (va justo): se vuelve a mirar en el siguiente.
            self.pending_gpu = Some(pending);
            let state = crate::gpu::VulkanFrameAccess::lock(access);
            crate::gpu::VulkanFrameAccess::unlock(access, None);
            if state.semaphore == 0 {
                return Ok(());
            }
            // La imagen más nueva manda: cuando acabe, acabó la anterior (misma cola). El plazo
            // se cuenta desde que se espera a **esta** imagen: una ráfaga no acumula la espera de
            // las anteriores sobre un fotograma sano.
            if let Some(pending) = self.pending_gpu.as_mut()
                && state.semaphore == pending.semaphore
                && state.semaphore_value != pending.value
            {
                pending.value = state.semaphore_value;
                pending.since = Instant::now();
            }
            return Ok(());
        }
        let state = crate::gpu::VulkanFrameAccess::lock(access);
        crate::gpu::VulkanFrameAccess::unlock(access, None);
        if state.semaphore != 0 {
            self.pending_gpu = Some(PendingGpu {
                device: key,
                semaphore: state.semaphore,
                value: state.semaphore_value,
                since: Instant::now(),
            });
        }
        Ok(())
    }

    /// Error de un fotograma cuya GPU no terminó en [`GPU_WAIT_NS`].
    fn gpu_timeout(&self) -> CodecError {
        CodecError::Backend(format!(
            "la GPU no terminó de decodificar con {} en {} ms",
            self.backend,
            GPU_WAIT_NS / 1_000_000
        ))
    }

    fn open(
        av_codec: &AVCodec,
        codec: Codec,
        backend: DecoderBackend,
        device: Option<&AVHWDeviceContext>,
        gpu_output: bool,
    ) -> Result<AVCodecContext, CodecError> {
        let mut ctx = AVCodecContext::new(av_codec);
        if let Some(device) = device {
            // `avcodec_default_get_format` elige el formato de la GPU si hay dispositivo.
            ctx.set_hw_device_ctx(device.clone());
            if gpu_output {
                // SAFETY: `ctx` es un contexto sin abrir que poseemos; `extra_hw_frames` es un
                // entero que el `hwaccel` lee al crear su pool.
                unsafe { (*ctx.as_mut_ptr()).extra_hw_frames = EXTRA_HW_FRAMES };
            }
        }
        let options: Vec<(&str, String)> = low_latency_options(codec, backend)
            .into_iter()
            .map(|(key, value)| (key, value.to_owned()))
            .collect();
        super::open_codec(&mut ctx, &options).map_err(|err| {
            CodecError::Unsupported(format!(
                "no se pudo abrir {}: {}",
                av_codec.name().to_string_lossy(),
                super::describe(&err)
            ))
        })?;
        Ok(ctx)
    }

    /// Fotogramas que salieron de la GPU (con `hwaccel` de verdad) y por software.
    pub fn output_counts(&self) -> (u64, u64) {
        (self.hw_frames, self.sw_frames)
    }

    /// Fotogramas que salieron sin copia a CPU (imagen de GPU).
    pub fn gpu_frames(&self) -> u64 {
        self.gpu_frames
    }

    /// Si decodifica en el dispositivo Vulkan del presentador.
    pub fn shares_presenter_device(&self) -> bool {
        self.device_is_shared
    }

    /// Imagen de salida de `frame`: en la GPU si se pidió y se puede, si no en CPU.
    fn output_frame(&mut self, frame: AVFrame, frame_id: u64) -> Result<RawFrame, CodecError> {
        let on_gpu = self.hw_format != ffi::AV_PIX_FMT_NONE && frame.format == self.hw_format;
        if on_gpu && let DecodeOutput::Gpu { vulkan } = self.output.clone() {
            let size = (
                u32::try_from(frame.width).unwrap_or(0),
                u32::try_from(frame.height).unwrap_or(0),
            );
            match vulkan {
                Some(shared) => {
                    if let Some(image) = self.vulkan_image(frame.clone(), &shared, size)? {
                        self.hw_frames += 1;
                        self.gpu_frames += 1;
                        self.copies = 0;
                        return Ok(RawFrame::gpu(frame_id, size, image));
                    }
                }
                None => {
                    self.hw_frames += 1;
                    self.gpu_frames += 1;
                    self.copies = 0;
                    if frame.format == ffi::AV_PIX_FMT_VULKAN
                        && let Some(device) = self.device.clone()
                        && let Some((access, _)) = VkFrameAccess::new(frame.clone())
                    {
                        self.track_gpu(&device, &access)?;
                    }
                    let kind = rsmpeg::avutil::get_pix_fmt_name(frame.format).map_or_else(
                        || "gpu".to_owned(),
                        |name| name.to_string_lossy().into_owned(),
                    );
                    let image = GpuImage::Opaque(Arc::new(OpaqueImage {
                        kind,
                        keepalive: Arc::new(FrameKeep(frame)),
                    }));
                    return Ok(RawFrame::gpu(frame_id, size, image));
                }
            }
        }
        self.download(&frame, frame_id)
    }

    /// Imagen Vulkan presentable en el dispositivo del presentador, decodificada en él o mapeada a
    /// él. `Ok(None)` si no se puede (se baja a CPU); error si la GPU no terminó a tiempo.
    fn vulkan_image(
        &mut self,
        frame: AVFrame,
        shared: &Arc<SharedVulkanDevice>,
        size: (u32, u32),
    ) -> Result<Option<GpuImage>, CodecError> {
        let frame = if frame.format == ffi::AV_PIX_FMT_VULKAN && self.device_is_shared {
            frame
        } else {
            let Some(mapped) = self.map_to_vulkan(&frame, shared) else {
                return Ok(None);
            };
            mapped
        };
        let device = if self.device_is_shared {
            self.device.clone()
        } else {
            self.map_device.clone()
        };
        let Some(device) = device else {
            return Ok(None);
        };
        let Some((access, info)) = VkFrameAccess::new(frame) else {
            return Ok(None);
        };
        self.track_gpu(&device, &access)?;
        let (instance, device) = handles(&device);
        Ok(Some(GpuImage::Vulkan(Arc::new(VulkanImage {
            width: size.0,
            height: size.1,
            image_width: info.extent.0,
            image_height: info.extent.1,
            format: info.format,
            instance,
            device,
            access,
        }))))
    }

    /// Mapea un fotograma de VA-API o D3D11VA al dispositivo Vulkan del presentador
    /// (`av_hwframe_ctx_create_derived` + `av_hwframe_map`, sin copia). La primera vez que falla
    /// deja de intentarlo.
    fn map_to_vulkan(
        &mut self,
        frame: &AVFrame,
        shared: &Arc<SharedVulkanDevice>,
    ) -> Option<AVFrame> {
        if self.map_failed
            || !matches!(
                frame.format,
                ffi::AV_PIX_FMT_VAAPI | ffi::AV_PIX_FMT_D3D11 | ffi::AV_PIX_FMT_DRM_PRIME
            )
            || frame.hw_frames_ctx.is_null()
        {
            return None;
        }
        let mapped = self.try_map(frame, shared);
        if mapped.is_none() {
            self.map_failed = true;
            super::log_warning(&format!(
                "la salida de {} no se pudo mapear al dispositivo Vulkan del presentador: se baja a \
                 CPU",
                self.backend
            ));
        }
        mapped
    }

    fn try_map(&mut self, frame: &AVFrame, shared: &Arc<SharedVulkanDevice>) -> Option<AVFrame> {
        if self.map_device.is_none() {
            self.map_device = Some(device_from_shared(shared).ok()?);
        }
        let key = buffer_key(frame.hw_frames_ctx);
        if self
            .map_frames
            .as_ref()
            .is_none_or(|(known, _)| *known != key)
        {
            let device = self.map_device.as_ref()?;
            let mut derived: *mut ffi::AVBufferRef = std::ptr::null_mut();
            // SAFETY: `derived` recibe una referencia nueva (o nada si falla); el dispositivo y el
            // contexto de origen son referencias válidas que no se consumen.
            let ret = unsafe {
                ffi::av_hwframe_ctx_create_derived(
                    &mut derived,
                    ffi::AV_PIX_FMT_VULKAN,
                    device.as_ptr() as *mut ffi::AVBufferRef,
                    frame.hw_frames_ctx,
                    super::MAP_READ_DIRECT,
                )
            };
            if ret < 0 {
                return None;
            }
            // SAFETY: `derived` es la referencia nueva que devolvió FFmpeg, ahora nuestra.
            let derived = unsafe { AVHWFramesContext::from_raw(std::ptr::NonNull::new(derived)?) };
            self.map_frames = Some((key, derived));
        }
        let (_, derived) = self.map_frames.as_ref()?;
        let mut mapped = AVFrame::new();
        mapped.set_format(ffi::AV_PIX_FMT_VULKAN);
        // SAFETY: `mapped` es un fotograma vacío nuestro con una referencia propia del contexto
        // Vulkan derivado; `av_hwframe_map` lo rellena desde `frame` (que no cambia) y guarda una
        // referencia de este.
        let ret = unsafe {
            let raw = mapped.as_mut_ptr();
            (*raw).hw_frames_ctx = ffi::av_buffer_ref(derived.as_ptr());
            if (*raw).hw_frames_ctx.is_null() {
                return None;
            }
            ffi::av_hwframe_map(raw, frame.as_ptr(), super::MAP_READ_DIRECT)
        };
        (ret >= 0).then_some(mapped)
    }

    fn download(&mut self, frame: &AVFrame, frame_id: u64) -> Result<RawFrame, CodecError> {
        let (Ok(width), Ok(height)) = (u32::try_from(frame.width), u32::try_from(frame.height))
        else {
            return Err(CodecError::Backend(
                "tamaño de imagen fuera de rango".into(),
            ));
        };
        let color = if frame.color_trc == ffi::AVCOL_TRC_SMPTE2084 {
            ColorSpace::HDR10
        } else {
            ColorSpace::SDR
        };
        let on_gpu = self.hw_format != ffi::AV_PIX_FMT_NONE && frame.format == self.hw_format;
        let sw_format = if on_gpu {
            hw_sw_format(frame)?
        } else {
            frame.format
        };
        let (format, expected) = match sw_format {
            ffi::AV_PIX_FMT_YUV420P | ffi::AV_PIX_FMT_YUVJ420P => {
                (PixelFormat::I420, RawFrame::i420_len(width, height))
            }
            ffi::AV_PIX_FMT_NV12 => (PixelFormat::Nv12, RawFrame::nv12_len(width, height)),
            ffi::AV_PIX_FMT_YUV444P | ffi::AV_PIX_FMT_YUVJ444P => {
                (PixelFormat::I444, RawFrame::i444_len(width, height))
            }
            ffi::AV_PIX_FMT_P010LE => (PixelFormat::P010, RawFrame::p010_len(width, height)),
            // 10 bits planares (dav1d, `hevc`): se bajan a CPU en su formato y se convierten a
            // P010, el de 10 bits de `render` (una copia más).
            ffi::AV_PIX_FMT_YUV420P10LE => (PixelFormat::P010, 0),
            other => {
                return Err(CodecError::Unsupported(format!(
                    "el decodificador entregó un formato de píxel no admitido ({other})"
                )));
            }
        };
        let planar10 = sw_format == ffi::AV_PIX_FMT_YUV420P10LE;
        // Imagen de CPU en `sw_format`: la propia del decodificador por software, o la bajada de
        // la GPU a un búfer del pool (una copia).
        let mut downloaded: Option<Vec<u8>> = None;
        let mut data = None;
        let frame = if on_gpu {
            self.hw_frames += 1;
            let mut sw = AVFrame::new();
            sw.set_format(sw_format);
            sw.set_width(frame.width);
            sw.set_height(frame.height);
            let len = sw
                .image_get_buffer_size(1)
                .map_err(|err| backend_error("tamaño de la imagen", &err))?;
            let mut buffer = self.pool.take(len);
            frame_over(&mut sw, &mut buffer)?;
            sw.hwframe_transfer_data(frame)
                .map_err(|err| backend_error("bajada de la GPU", &err))?;
            // El `AVFrame` deja de apuntar al búfer antes de que este cambie de dueño.
            drop(sw);
            self.copies = 1;
            if planar10 {
                // El búfer compacto se vuelve a leer como planos para convertirlo.
                downloaded = Some(buffer);
                None
            } else {
                data = Some(buffer);
                None
            }
        } else {
            self.sw_frames += 1;
            self.copies = 1;
            Some(frame)
        };
        if planar10 {
            let (_, ch) = RawFrame::chroma_size(width, height);
            let (cw, _) = RawFrame::chroma_size(width, height);
            let planes: [(&[u8], usize); 3] = match (&downloaded, frame) {
                (Some(buffer), _) => {
                    // Planos compactos de 16 bits: Y de `2 × width`, U y V de `2 × cw`.
                    let y_len = 2 * width as usize * height as usize;
                    let c_len = 2 * cw as usize * ch as usize;
                    if buffer.len() < y_len + 2 * c_len {
                        return Err(CodecError::Backend("plano de 10 bits vacío".into()));
                    }
                    [
                        (&buffer[..y_len], 2 * width as usize),
                        (&buffer[y_len..y_len + c_len], 2 * cw as usize),
                        (&buffer[y_len + c_len..y_len + 2 * c_len], 2 * cw as usize),
                    ]
                }
                (None, Some(frame)) => {
                    let plane = |index: usize, rows: usize| -> Result<(&[u8], usize), CodecError> {
                        let stride = usize::try_from(frame.linesize[index])
                            .map_err(|_| CodecError::Backend("fila negativa".into()))?;
                        if frame.data[index].is_null() || stride == 0 || rows == 0 {
                            return Err(CodecError::Backend("plano de 10 bits vacío".into()));
                        }
                        // SAFETY: un `AVFrame` decodificado tiene en `data[i]` al menos
                        // `linesize[i] × filas` bytes legibles mientras vive `frame`, y aquí solo
                        // se leen.
                        let bytes =
                            unsafe { std::slice::from_raw_parts(frame.data[index], stride * rows) };
                        Ok((bytes, stride))
                    };
                    [
                        plane(0, height as usize)?,
                        plane(1, ch as usize)?,
                        plane(2, ch as usize)?,
                    ]
                }
                (None, None) => return Err(CodecError::Backend("sin imagen".into())),
            };
            let data = crate::convert::planar10_to_p010((width, height), planes)?;
            self.copies += 1;
            if let Some(buffer) = downloaded {
                self.pool.give(buffer);
            }
            return Ok(RawFrame::cpu(
                frame_id,
                (width, height),
                PixelFormat::P010,
                2 * width,
                data,
            )
            .with_color(color));
        }
        let data = match (data, frame) {
            (Some(data), _) => data,
            (None, Some(frame)) => {
                // Decodificador por software: una copia de sus planos al búfer del pool.
                let len = frame
                    .image_get_buffer_size(1)
                    .map_err(|err| backend_error("tamaño de la imagen", &err))?;
                let mut data = self.pool.take(len);
                frame
                    .image_copy_to_buffer(&mut data, 1)
                    .map_err(|err| backend_error("copia de la imagen", &err))?;
                data
            }
            (None, None) => return Err(CodecError::Backend("sin imagen".into())),
        };
        if data.len() != expected {
            return Err(CodecError::Backend(
                "el decodificador entregó planos incompletos".into(),
            ));
        }
        let stride = if format == PixelFormat::P010 {
            2 * width
        } else {
            width
        };
        Ok(
            RawFrame::cpu(frame_id, (width, height), format, stride, data)
                .with_color(color)
                .with_pool(Arc::clone(&self.pool)),
        )
    }
}

/// Formato de CPU (`sw_format`) del contexto de *hw frames* de una imagen de GPU: el que
/// `av_hwframe_transfer_data` entrega al bajarla.
fn hw_sw_format(frame: &AVFrame) -> Result<ffi::AVPixelFormat, CodecError> {
    if frame.hw_frames_ctx.is_null() {
        return Err(CodecError::Backend(
            "imagen de GPU sin contexto de fotogramas".into(),
        ));
    }
    // SAFETY: `hw_frames_ctx` de un fotograma de GPU es una referencia válida a un
    // `AVHWFramesContext` iniciado; solo se lee `sw_format`.
    Ok(unsafe { (*(*frame.hw_frames_ctx).data.cast::<ffi::AVHWFramesContext>()).sw_format })
}

/// Destructor del búfer «prestado» de [`frame_over`]: la memoria es de un `Vec` y no se libera
/// aquí.
unsafe extern "C" fn borrowed_free(_opaque: *mut std::ffi::c_void, _data: *mut u8) {}

/// Apunta los planos de `frame` (con formato y tamaño ya puestos) a `buffer`, compacto
/// (`align` 1), como hace `av_image_fill_arrays`, con un `AVBufferRef` que no libera nada: así
/// `av_hwframe_transfer_data` escribe la imagen directamente en el búfer. `frame` debe soltarse
/// antes de que `buffer` cambie de sitio o de dueño.
fn frame_over(frame: &mut AVFrame, buffer: &mut [u8]) -> Result<(), CodecError> {
    let size = i32::try_from(buffer.len())
        .map_err(|_| CodecError::Backend("imagen demasiado grande".into()))?;
    // SAFETY: `frame` es un fotograma nuestro sin búferes; `av_buffer_create` envuelve `buffer`
    // (que vive más que `frame`, ver el contrato de la función) con un destructor que no hace
    // nada, y `av_image_fill_arrays` rellena `data` y `linesize` para el formato y el tamaño del
    // fotograma dentro de `size` bytes a partir de `buffer` (si no caben, devuelve error y no
    // escribe punteros).
    unsafe {
        let raw = frame.as_mut_ptr();
        let buf = ffi::av_buffer_create(
            buffer.as_mut_ptr(),
            buffer.len(),
            Some(borrowed_free),
            std::ptr::null_mut(),
            0,
        );
        if buf.is_null() {
            return Err(CodecError::Backend("sin memoria para la imagen".into()));
        }
        (*raw).buf[0] = buf;
        let ret = ffi::av_image_fill_arrays(
            (*raw).data.as_mut_ptr(),
            (*raw).linesize.as_mut_ptr(),
            buffer.as_ptr(),
            (*raw).format,
            (*raw).width,
            (*raw).height,
            1,
        );
        if ret < 0 || ret > size {
            return Err(CodecError::Backend(
                "la imagen no cabe en el búfer de salida".into(),
            ));
        }
        (*raw).extended_data = (*raw).data.as_mut_ptr();
    }
    Ok(())
}

/// Paquete con una copia de `data` (los slices de un fotograma, seguidos).
fn packet_from(slices: &[EncodedSlice], pts: i64) -> Result<AVPacket, CodecError> {
    let len: usize = slices.iter().map(|slice| slice.data.len()).sum();
    let size =
        i32::try_from(len).map_err(|_| CodecError::Unsupported("fotograma enorme".into()))?;
    let mut packet = AVPacket::new();
    // SAFETY: `av_new_packet` reserva `size` bytes (más el relleno que exige FFmpeg) para el
    // paquete recién creado, que es nuestro; se escriben exactamente `size` bytes en ese búfer.
    unsafe {
        let raw = packet.as_mut_ptr();
        if ffi::av_new_packet(raw, size) < 0 {
            return Err(CodecError::Backend("sin memoria para el paquete".into()));
        }
        let mut at = (*raw).data;
        for slice in slices {
            std::ptr::copy_nonoverlapping(slice.data.as_ptr(), at, slice.data.len());
            at = at.add(slice.data.len());
        }
        (*raw).pts = pts;
        (*raw).dts = pts;
    }
    Ok(packet)
}

impl Decoder for FfmpegDecoder {
    fn codec(&self) -> Codec {
        self.codec
    }

    fn backend(&self) -> DecoderBackend {
        self.backend
    }

    fn decode(&mut self, slices: &[EncodedSlice]) -> Result<Option<RawFrame>, CodecError> {
        let Some(first) = slices.first() else {
            return Ok(None);
        };
        let frame_id = first.frame_id;
        let pts = i64::try_from(frame_id).unwrap_or(i64::MAX);
        let packet = packet_from(slices, pts)?;
        let mut latest = None;
        // Como mucho dos intentos: si el decodificador está lleno (`EAGAIN`), se recogen los
        // fotogramas que tiene y el paquete se entrega otra vez; perderlo rompería las referencias.
        let mut pending = true;
        for _ in 0..2 {
            match self.ctx.send_packet(Some(&packet)) {
                Ok(()) => pending = false,
                Err(RsmpegError::DecoderFullError) => {}
                Err(RsmpegError::SendPacketError(code)) if code == ffi::AVERROR_INVALIDDATA => {
                    return Err(CodecError::MissingReference);
                }
                Err(err) => return Err(backend_error("entregar el paquete", &err)),
            }
            loop {
                match self.ctx.receive_frame() {
                    Ok(frame) => latest = Some(frame),
                    Err(RsmpegError::DecoderDrainError | RsmpegError::DecoderFlushedError) => {
                        break;
                    }
                    Err(err) => return Err(backend_error("recibir el fotograma", &err)),
                }
            }
            if !pending {
                break;
            }
        }
        if pending {
            // Lleno aun después de vaciarlo: el paquete no entró y lo que siga no tiene su
            // referencia. Se avisa como pérdida (la sesión pide un intra).
            return Err(CodecError::MissingReference);
        }
        match latest {
            Some(frame) => {
                let id = u64::try_from(frame.pts).unwrap_or(frame_id);
                self.output_frame(frame, id).map(Some)
            }
            None => Ok(None),
        }
    }

    fn reset(&mut self) {
        self.ctx.flush_buffers();
        // Si el decodificador quedó en mal estado, se reabre; si eso falla se conserva el anterior.
        if let Ok(ctx) = Self::open(
            &self.av_codec,
            self.codec,
            self.backend,
            self.device.as_ref(),
            self.output.is_gpu(),
        ) {
            self.ctx = ctx;
        }
    }

    fn cpu_copies(&self) -> u32 {
        self.copies
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn software_decoders_open() {
        for codec in Codec::PREFERENCE {
            let decoder = FfmpegDecoder::new(codec, DecoderBackend::Software)
                .unwrap_or_else(|err| panic!("{codec}: {err}"));
            assert_eq!(decoder.codec(), codec);
            assert_eq!(decoder.output_counts(), (0, 0));
        }
    }

    /// Sin GPU no hay salida en la GPU: un decodificador por software pedido con salida de GPU
    /// deja la imagen en CPU (y lo cuenta como una copia).
    #[test]
    fn software_decoders_ignore_gpu_output() {
        let decoder = FfmpegDecoder::with_output(
            Codec::Av1,
            DecoderBackend::Software,
            DecodeOutput::Gpu { vulkan: None },
        )
        .unwrap();
        assert!(!decoder.shares_presenter_device());
        assert_eq!(decoder.gpu_frames(), 0);
    }

    #[test]
    fn garbage_does_not_panic() {
        let mut decoder = FfmpegDecoder::new(Codec::H264, DecoderBackend::Software).unwrap();
        let slice = EncodedSlice {
            frame_id: 3,
            index: 0,
            count: 1,
            keyframe: false,
            data: vec![0, 0, 0, 1, 0x65, 0xff, 0x00, 0x13],
        };
        let _ = decoder.decode(&[slice]);
        decoder.reset();
        assert_eq!(decoder.decode(&[]), Ok(None));
    }
}
