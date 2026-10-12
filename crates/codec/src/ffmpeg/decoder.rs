//! Decodificador de FFmpeg ([`FfmpegDecoder`]): software (dav1d, `h264`, `hevc`) o `hwaccel`.
//!
//! Dónde deja las imágenes lo decide [`DecodeOutput`]:
//!
//! - [`DecodeOutput::Cpu`]: la imagen de la GPU se baja con
//!   `av_hwframe_transfer_data` y se copia a un [`RawFrame`] I420 o NV12 (dos copias a CPU; una con
//!   los decodificadores por software).
//! - [`DecodeOutput::Gpu`] con el dispositivo Vulkan del presentador: con el backend Vulkan, FFmpeg
//!   decodifica en ese mismo `VkDevice` y la imagen sale tal cual ([`GpuImage::Vulkan`], cero
//!   copias); con VA-API o D3D11VA se intenta mapear a Vulkan con `av_hwframe_map` (también sin
//!   copia) y, si FFmpeg no sabe, se baja a CPU. NVDEC (CUDA) no se mapea a Vulkan en FFmpeg 8.1:
//!   se baja a CPU.
//! - [`DecodeOutput::Gpu`] sin dispositivo (sin ventana): la imagen de GPU se queda donde la dejó
//!   el decodificador ([`GpuImage::Opaque`]), sin copia.

use std::sync::Arc;

use rsmpeg::avcodec::{AVCodec, AVCodecContext, AVCodecRef, AVPacket};
use rsmpeg::avutil::{AVFrame, AVHWDeviceContext, AVHWFramesContext};
use rsmpeg::error::RsmpegError;
use rsmpeg::ffi;

use super::vulkan::{SemaphoreWaiter, VkFrameAccess, buffer_key, device_from_shared, handles};
use super::{backend_error, create_device, decoder_device, decoder_name, hw_pixel_format};
use crate::gpu::{GpuImage, OpaqueImage, SharedVulkanDevice, VulkanImage};
use crate::{
    Codec, CodecError, ColorSpace, DecodeOutput, Decoder, DecoderBackend, EncodedSlice,
    PixelFormat, RawFrame,
};

/// Fotogramas de más en el pool del decodificador de GPU cuando la imagen sale sin copia: los que
/// esperan en el canal hacia la presentación, el que se presenta y los que la GPU aún muestrea.
const EXTRA_HW_FRAMES: i32 = 6;

/// Espera máxima a que la GPU termine de decodificar una imagen que sale sin copia.
const GPU_WAIT_NS: u64 = 100_000_000;

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
    /// Espera a la GPU de las imágenes Vulkan que salen sin copia (por dispositivo).
    waiters: Vec<(usize, SemaphoreWaiter)>,
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
        })
    }

    /// Espera en CPU a que la GPU termine de escribir la imagen Vulkan de `access` (así la marca
    /// `decodificado` mide la decodificación de verdad). `device` es el de la imagen.
    fn wait_for_gpu(&mut self, device: &AVHWDeviceContext, access: &VkFrameAccess) {
        let key = buffer_key(device.as_ptr());
        if !self.waiters.iter().any(|(known, _)| *known == key) {
            let Some(waiter) = SemaphoreWaiter::new(device) else {
                return;
            };
            self.waiters.push((key, waiter));
        }
        let state = crate::gpu::VulkanFrameAccess::lock(access);
        crate::gpu::VulkanFrameAccess::unlock(access, None);
        if let Some((_, waiter)) = self.waiters.iter().find(|(known, _)| *known == key) {
            waiter.wait(state.semaphore, state.semaphore_value, GPU_WAIT_NS);
        }
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
                    if let Some(image) = self.vulkan_image(frame.clone(), &shared, size) {
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
                        self.wait_for_gpu(&device, &access);
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
    /// él. `None` si no se puede (se baja a CPU).
    fn vulkan_image(
        &mut self,
        frame: AVFrame,
        shared: &Arc<SharedVulkanDevice>,
        size: (u32, u32),
    ) -> Option<GpuImage> {
        let frame = if frame.format == ffi::AV_PIX_FMT_VULKAN && self.device_is_shared {
            frame
        } else {
            self.map_to_vulkan(&frame, shared)?
        };
        let device = if self.device_is_shared {
            self.device.clone()?
        } else {
            self.map_device.clone()?
        };
        let (access, info) = VkFrameAccess::new(frame)?;
        self.wait_for_gpu(&device, &access);
        let (instance, device) = handles(&device);
        Some(GpuImage::Vulkan(Arc::new(VulkanImage {
            width: size.0,
            height: size.1,
            image_width: info.extent.0,
            image_height: info.extent.1,
            format: info.format,
            instance,
            device,
            access,
        })))
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
        let downloaded;
        let frame = if self.hw_format != ffi::AV_PIX_FMT_NONE && frame.format == self.hw_format {
            let mut sw = AVFrame::new();
            sw.hwframe_transfer_data(frame)
                .map_err(|err| backend_error("bajada de la GPU", &err))?;
            self.hw_frames += 1;
            // Bajada de la GPU y copia al búfer de salida.
            self.copies = 2;
            downloaded = sw;
            &downloaded
        } else {
            self.sw_frames += 1;
            self.copies = 1;
            frame
        };
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
        if frame.format == ffi::AV_PIX_FMT_YUV420P10LE {
            // 10 bits por software (dav1d, `hevc`): a P010, el formato de 10 bits de `render`.
            let plane = |index: usize, rows: usize| -> Result<(&[u8], usize), CodecError> {
                let stride = usize::try_from(frame.linesize[index])
                    .map_err(|_| CodecError::Backend("fila negativa".into()))?;
                if frame.data[index].is_null() || stride == 0 || rows == 0 {
                    return Err(CodecError::Backend("plano de 10 bits vacío".into()));
                }
                // SAFETY: un `AVFrame` decodificado tiene en `data[i]` al menos `linesize[i] ×
                // filas` bytes legibles mientras vive `frame`, y aquí solo se leen.
                let bytes = unsafe { std::slice::from_raw_parts(frame.data[index], stride * rows) };
                Ok((bytes, stride))
            };
            let (_, ch) = RawFrame::chroma_size(width, height);
            let planes = [
                plane(0, height as usize)?,
                plane(1, ch as usize)?,
                plane(2, ch as usize)?,
            ];
            let data = crate::convert::planar10_to_p010((width, height), planes)?;
            self.copies += 1;
            return Ok(RawFrame::cpu(
                frame_id,
                (width, height),
                PixelFormat::P010,
                2 * width,
                data,
            )
            .with_color(color));
        }
        let (format, expected) = match frame.format {
            ffi::AV_PIX_FMT_YUV420P | ffi::AV_PIX_FMT_YUVJ420P => {
                (PixelFormat::I420, RawFrame::i420_len(width, height))
            }
            ffi::AV_PIX_FMT_NV12 => (PixelFormat::Nv12, RawFrame::nv12_len(width, height)),
            ffi::AV_PIX_FMT_YUV444P | ffi::AV_PIX_FMT_YUVJ444P => {
                (PixelFormat::I444, RawFrame::i444_len(width, height))
            }
            ffi::AV_PIX_FMT_P010LE => (PixelFormat::P010, RawFrame::p010_len(width, height)),
            other => {
                return Err(CodecError::Unsupported(format!(
                    "el decodificador entregó un formato de píxel no admitido ({other})"
                )));
            }
        };
        let len = frame
            .image_get_buffer_size(1)
            .map_err(|err| backend_error("tamaño de la imagen", &err))?;
        let mut data = vec![0u8; len];
        frame
            .image_copy_to_buffer(&mut data, 1)
            .map_err(|err| backend_error("copia de la imagen", &err))?;
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
        Ok(RawFrame::cpu(frame_id, (width, height), format, stride, data).with_color(color))
    }
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
        match self.ctx.send_packet(Some(&packet)) {
            Ok(()) => {}
            // Hay fotogramas sin recoger: se sacan abajo y el paquete se reenvía.
            Err(RsmpegError::DecoderFullError) => {}
            Err(RsmpegError::SendPacketError(code)) if code == ffi::AVERROR_INVALIDDATA => {
                return Err(CodecError::MissingReference);
            }
            Err(err) => return Err(backend_error("entregar el paquete", &err)),
        }
        let mut latest = None;
        loop {
            match self.ctx.receive_frame() {
                Ok(frame) => latest = Some(frame),
                Err(RsmpegError::DecoderDrainError | RsmpegError::DecoderFlushedError) => break,
                Err(err) => return Err(backend_error("recibir el fotograma", &err)),
            }
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
