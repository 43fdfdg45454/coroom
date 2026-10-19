//! Códecs de vídeo de coroom.
//!
//! Define los traits [`Encoder`] y [`Decoder`] (`docs/ARQUITECTURA.md` §3). La ruta principal es **Vulkan Video**
//! vía `hwcontext_vulkan` de FFmpeg 8 (`h264_vulkan`, `hevc_vulkan`, `av1_vulkan`), con respaldo por
//! fabricante (NVENC/NVDEC, AMF, QSV, VAAPI) y por software (SVT-AV1, dav1d, openh264). El
//! codificador trabaja en baja latencia: sin B-frames, una referencia, CBR, GOP infinito (intra solo
//! al pedirlo, sin *intra-refresh*) y varios slices por fotograma para enviar cada uno en cuanto sale.
//!
//! Módulos:
//!
//! - [`openh264`]: H.264 por software, siempre compilado.
//! - [`color`]: espacio de color y formato del flujo ([`VideoFormat`]: BT.709 SDR o
//!   BT.2020 PQ, 4:2:0 o 4:4:4) y funciones de transferencia (sRGB, PQ, tone mapping).
//! - [`convert`]: conversión de color BGRA → I420 / NV12 / I444 / NV24 (BT.709 en rango limitado) y,
//!   con HDR, BGRA o RGBA de coma flotante → P010 (BT.2020 PQ).
//! - `ffmpeg` (feature `ffmpeg`, por defecto): codificadores y decodificadores de FFmpeg 8 por
//!   software (SVT-AV1, dav1d, `h264` y `hevc` de FFmpeg) y por hardware (Vulkan Video, NVENC/NVDEC,
//!   AMF, QSV, VAAPI, D3D11VA/D3D12VA), con copia a CPU o sin copias ([`gpu`]).
//! - [`caps`]: sondeo de lo que funciona de verdad en esta máquina ([`Capabilities::probe`]).
//! - [`select`]: [`EncoderSet`] y [`DecoderSet`], que abren el mejor backend disponible de un códec.
//! - [`gpu`]: imágenes de GPU que recorren el pipeline sin copias a CPU (DMA-BUF y
//!   texturas D3D11 a la entrada del codificador, imágenes Vulkan a la salida del decodificador).
//! - [`slices`]: reparto de cada paquete en sus slices reales (NAL u OBU).
//! - [`pool`]: búferes de imagen reutilizados entre fotogramas ([`BufferPool`]), para que la salida
//!   en CPU del decodificador no reserve memoria con cada fotograma.
//! - `mediacodec` (feature `mediacodec`, para el invitado de Android): decodificación por hardware
//!   con MediaCodec directa a la `Surface` de la vista del vídeo, sin copias a CPU.

use std::fmt;
use std::sync::Arc;

pub mod calibrate;
pub mod caps;
pub mod color;
pub mod convert;
#[cfg(feature = "ffmpeg")]
pub mod ffmpeg;
pub mod gpu;
#[cfg(feature = "mediacodec")]
pub mod mediacodec;
pub mod openh264;
pub mod pool;
pub mod select;
pub mod slices;

pub use calibrate::{CalibrationCache, CalibrationKey, CalibrationResult, calibrate};
pub use caps::{Capabilities, decoder_supports, encoder_supports};
pub use color::{Chroma, ColorSpace, VideoFormat};
pub use gpu::{DecodeOutput, GpuImage, GpuInput};
pub use pool::BufferPool;
pub use select::{DecodeLimits, DecoderSet, EncoderSet, StreamHint};

/// Slices por fotograma que pide la sesión a los codificadores.
pub const DEFAULT_SLICES: u8 = 4;

/// Códec de vídeo negociado entre anfitrión e invitado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Codec {
    /// H.264 / AVC.
    H264,
    /// H.265 / HEVC.
    Hevc,
    /// AV1.
    Av1,
}

impl Codec {
    /// Todos los códecs, del preferido (AV1) al de respaldo (H.264), como en `docs/ARQUITECTURA.md` §3.
    pub const PREFERENCE: [Codec; 3] = [Codec::Av1, Codec::Hevc, Codec::H264];

    /// Nombre corto para la línea de órdenes (`h264`, `hevc`, `av1`).
    pub const fn id(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::Hevc => "hevc",
            Codec::Av1 => "av1",
        }
    }

    /// Lee el nombre corto de [`Codec::id`].
    pub fn from_id(id: &str) -> Option<Codec> {
        Codec::PREFERENCE
            .into_iter()
            .find(|codec| codec.id().eq_ignore_ascii_case(id))
    }
}

impl fmt::Display for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Codec::H264 => "H.264",
            Codec::Hevc => "HEVC",
            Codec::Av1 => "AV1",
        })
    }
}

/// Backend de un codificador, del preferido al de respaldo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Backend {
    /// Vulkan Video (`h264_vulkan`, `hevc_vulkan`, `av1_vulkan`), la ruta principal.
    VulkanVideo,
    /// NVIDIA NVENC.
    Nvenc,
    /// AMD AMF.
    Amf,
    /// Intel Quick Sync Video con oneVPL.
    Qsv,
    /// VA-API (Linux).
    Vaapi,
    /// Por software: SVT-AV1 para AV1 y OpenH264 para H.264 (HEVC solo por hardware).
    Software,
}

impl Backend {
    /// Posición en el orden de preferencia de los codificadores de esta plataforma. En Windows,
    /// NVENC, AMF y QSV van antes que Vulkan Video: son los que cuidan los fabricantes, y la
    /// codificación por Vulkan de sus drivers aún da fallos (AV1 con referencias corruptas).
    pub fn encoder_rank(self) -> usize {
        let order: &[Backend] = if cfg!(target_os = "windows") {
            &[
                Backend::Nvenc,
                Backend::Amf,
                Backend::Qsv,
                Backend::VulkanVideo,
                Backend::Vaapi,
                Backend::Software,
            ]
        } else {
            &Backend::ALL
        };
        order.iter().position(|b| *b == self).unwrap_or(order.len())
    }

    /// Todos, en orden de preferencia.
    pub const ALL: [Backend; 6] = [
        Backend::VulkanVideo,
        Backend::Nvenc,
        Backend::Amf,
        Backend::Qsv,
        Backend::Vaapi,
        Backend::Software,
    ];

    /// Nombre corto para la línea de órdenes.
    pub const fn id(self) -> &'static str {
        match self {
            Backend::VulkanVideo => "vulkan",
            Backend::Nvenc => "nvenc",
            Backend::Amf => "amf",
            Backend::Qsv => "qsv",
            Backend::Vaapi => "vaapi",
            Backend::Software => "software",
        }
    }

    /// Lee el nombre corto de [`Backend::id`].
    pub fn from_id(id: &str) -> Option<Backend> {
        Backend::ALL
            .into_iter()
            .find(|backend| backend.id().eq_ignore_ascii_case(id))
    }

    /// Si usa la GPU.
    pub const fn is_hardware(self) -> bool {
        !matches!(self, Backend::Software)
    }
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Backend::VulkanVideo => "Vulkan Video",
            Backend::Nvenc => "NVENC",
            Backend::Amf => "AMF",
            Backend::Qsv => "QSV (oneVPL)",
            Backend::Vaapi => "VA-API",
            Backend::Software => "software",
        })
    }
}

/// Backend de un decodificador, del preferido al de respaldo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DecoderBackend {
    /// MediaCodec de Android (por hardware, directo a la `Surface` de la vista). Va el primero:
    /// en Android es el único de hardware y debe ganar a OpenH264. Existe en todas las
    /// plataformas porque el anfitrión lo recibe en las capacidades del invitado; solo se abre
    /// con la característica `mediacodec`.
    MediaCodec,
    /// Vulkan Video.
    Vulkan,
    /// NVIDIA NVDEC (por CUDA).
    Nvdec,
    /// Direct3D 12 Video (Windows).
    D3D12va,
    /// Direct3D 11 Video (Windows).
    D3D11va,
    /// VA-API (Linux).
    Vaapi,
    /// Por software: dav1d para AV1, OpenH264 para H.264 y `hevc` de FFmpeg para HEVC.
    Software,
}

impl DecoderBackend {
    /// Todos, en orden de preferencia.
    #[cfg(not(feature = "mediacodec"))]
    pub const ALL: [DecoderBackend; 6] = [
        DecoderBackend::Vulkan,
        DecoderBackend::Nvdec,
        DecoderBackend::D3D12va,
        DecoderBackend::D3D11va,
        DecoderBackend::Vaapi,
        DecoderBackend::Software,
    ];

    /// Todos, en orden de preferencia.
    #[cfg(feature = "mediacodec")]
    pub const ALL: [DecoderBackend; 7] = [
        DecoderBackend::MediaCodec,
        DecoderBackend::Vulkan,
        DecoderBackend::Nvdec,
        DecoderBackend::D3D12va,
        DecoderBackend::D3D11va,
        DecoderBackend::Vaapi,
        DecoderBackend::Software,
    ];

    /// Nombre corto para la línea de órdenes.
    pub const fn id(self) -> &'static str {
        match self {
            DecoderBackend::MediaCodec => "mediacodec",
            DecoderBackend::Vulkan => "vulkan",
            DecoderBackend::Nvdec => "nvdec",
            DecoderBackend::D3D12va => "d3d12va",
            DecoderBackend::D3D11va => "d3d11va",
            DecoderBackend::Vaapi => "vaapi",
            DecoderBackend::Software => "software",
        }
    }

    /// Lee el nombre corto de [`DecoderBackend::id`].
    pub fn from_id(id: &str) -> Option<DecoderBackend> {
        DecoderBackend::ALL
            .into_iter()
            .find(|backend| backend.id().eq_ignore_ascii_case(id))
    }

    /// Si usa la GPU.
    pub const fn is_hardware(self) -> bool {
        !matches!(self, DecoderBackend::Software)
    }
}

impl fmt::Display for DecoderBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DecoderBackend::MediaCodec => "MediaCodec",
            DecoderBackend::Vulkan => "Vulkan Video",
            DecoderBackend::Nvdec => "NVDEC",
            DecoderBackend::D3D12va => "D3D12 Video",
            DecoderBackend::D3D11va => "D3D11 Video",
            DecoderBackend::Vaapi => "VA-API",
            DecoderBackend::Software => "software",
        })
    }
}

/// Nombre de la implementación concreta de un codificador (para `doctor` y el overlay).
pub fn encoder_label(codec: Codec, backend: Backend) -> String {
    match (codec, backend) {
        (Codec::Av1, Backend::Software) => "SVT-AV1".to_owned(),
        (Codec::H264, Backend::Software) => "OpenH264".to_owned(),
        (Codec::Hevc, Backend::Software) => "sin codificador software".to_owned(),
        (_, backend) => backend.to_string(),
    }
}

/// Nombre de la implementación concreta de un decodificador (para `doctor` y el overlay).
pub fn decoder_label(codec: Codec, backend: DecoderBackend) -> String {
    match (codec, backend) {
        (Codec::Av1, DecoderBackend::Software) => "dav1d".to_owned(),
        (Codec::H264, DecoderBackend::Software) => "OpenH264".to_owned(),
        (Codec::Hevc, DecoderBackend::Software) => "hevc de FFmpeg".to_owned(),
        (_, backend) => backend.to_string(),
    }
}

/// Cómo gasta el codificador el bitrate objetivo.
///
/// Medido con `tests/rate_modes.rs` (1080p60, techo de 20 Mbit/s, escritorio quieto, movimiento y
/// una ráfaga de partículas): en tasa constante, NVENC gasta los 20 Mbit/s también con la
/// pantalla quieta; con techo, 2-3 Mbit/s quieta (PSNR de 45-50 dB, sin diferencia a la vista),
/// los mismos 20 Mbit/s en la ráfaga y fotogramas máximos iguales o menores (134-224 KB frente a
/// 208-520 KB). De media, 6-7 Mbit/s en vez de 13-20.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RateMode {
    /// Tasa constante (CBR): gasta el objetivo siempre, haga falta o no.
    Constant,
    /// Con techo: gasta lo que pide la imagen hasta el objetivo, que hace de techo con el mismo
    /// búfer de un fotograma, y con un suelo de QP (más calidad que esa no se ve). Una pantalla
    /// quieta gasta poco y un pico (partículas, un cambio de escena) sube hasta el techo. Lo
    /// tienen todos (cómo, en [`ffmpeg::encoder::supports_capped`]); si un controlador se pasa del
    /// techo, se le baja el objetivo hasta que sus fotogramas caben.
    #[default]
    Capped,
}

/// Parámetros del codificador.
///
/// `width × height` es el **tope** de resolución (la «resolución máxima» de los ajustes), no un
/// tamaño fijo: el codificador se abre con él y, cuando la captura entrega un fotograma de otro
/// tamaño, se reabre al de ese fotograma reducido para que quepa en el tope
/// ([`coroom_proto::video::fit`]: misma proporción, lados pares, nunca ampliado). Lo que ya cabe
/// se codifica tal cual.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    /// Códec que se usa.
    pub codec: Codec,
    /// Ancho máximo en píxeles (par).
    pub width: u32,
    /// Alto máximo en píxeles (par).
    pub height: u32,
    /// Fotogramas por segundo objetivo.
    pub fps: u32,
    /// Bitrate objetivo inicial en kbit/s.
    pub bitrate_kbps: u32,
    /// Slices por fotograma.
    pub slices: u8,
    /// Croma, espacio de color y profundidad del flujo.
    pub format: VideoFormat,
}

/// Disposición de los píxeles de un [`RawFrame`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// Un plano de 4 bytes por píxel en orden B, G, R, A (el byte de alfa se ignora).
    Bgra,
    /// YUV 4:2:0 planar de 8 bits: plano Y de `stride × height` bytes seguido de los planos U y V
    /// compactados, cada uno de `ceil(width / 2) × ceil(height / 2)` bytes.
    I420,
    /// YUV 4:2:0 semiplanar de 8 bits (lo que dan casi todos los decodificadores por hardware):
    /// plano Y de `stride × height` bytes seguido del plano UV entrelazado y compactado, de
    /// `ceil(height / 2)` filas de `2 × ceil(width / 2)` bytes (U, V, U, V…).
    Nv12,
    /// YUV 4:4:4 planar de 8 bits: tres planos del tamaño del fotograma; el Y con filas
    /// de `stride` bytes y U y V compactados (`width` bytes por fila).
    I444,
    /// YUV 4:4:4 semiplanar de 8 bits (la entrada de Vulkan Video): plano Y de
    /// `stride × height` bytes seguido del UV entrelazado y compactado (`2 × width` por fila).
    Nv24,
    /// YUV 4:2:0 semiplanar de 10 bits en palabras de 16 bits *little-endian* con los 10 bits
    /// altos (HDR): plano Y de `height` filas de `stride` bytes (al menos `2 × width`) y
    /// plano UV compactado de `ceil(height / 2)` filas de `4 × ceil(width / 2)` bytes.
    P010,
    /// RGBA de medio flotante (`f16` *little-endian*, 8 bytes por píxel) en scRGB lineal: 1,0 son
    /// 80 nits y los valores mayores, HDR (la captura HDR de WGC y PipeWire).
    Rgba16F,
    /// Imagen de GPU en [`RawFrame::gpu`]: DMA-BUF o textura D3D11 a la entrada del
    /// codificador, imagen Vulkan a la salida del decodificador. `data` va vacío.
    Gpu,
}

/// Fotograma sin comprimir, en memoria de CPU o en la GPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrame {
    /// Identificador del fotograma capturado.
    pub frame_id: u64,
    /// Ancho en píxeles.
    pub width: u32,
    /// Alto en píxeles.
    pub height: u32,
    /// Disposición de [`RawFrame::data`].
    pub format: PixelFormat,
    /// Bytes por fila del primer plano. En [`PixelFormat::I420`] los planos de croma van
    /// compactados (su fila mide `ceil(width / 2)` bytes) uno tras otro; en [`PixelFormat::Nv12`],
    /// el plano UV también (su fila mide `2 × ceil(width / 2)` bytes).
    pub stride: u32,
    /// Píxeles en el formato indicado por [`RawFrame::format`] (vacío con [`PixelFormat::Gpu`]).
    pub data: Vec<u8>,
    /// Imagen de GPU, con [`PixelFormat::Gpu`].
    pub gpu: Option<GpuImage>,
    /// Espacio de color de los píxeles YUV (en BGRA y RGBA no se usa).
    pub color: ColorSpace,
    /// Pool del que salió [`RawFrame::data`], si salió de uno: quien consume el fotograma lo
    /// devuelve con [`RawFrame::recycle`] (o lo deja caer, y el pool reserva otro).
    pub pool: Option<Arc<BufferPool>>,
}

impl RawFrame {
    /// Fotograma en memoria de CPU.
    pub fn cpu(
        frame_id: u64,
        (width, height): (u32, u32),
        format: PixelFormat,
        stride: u32,
        data: Vec<u8>,
    ) -> Self {
        Self {
            frame_id,
            width,
            height,
            format,
            stride,
            data,
            gpu: None,
            color: ColorSpace::SDR,
            pool: None,
        }
    }

    /// Fotograma con una imagen de GPU.
    pub fn gpu(frame_id: u64, (width, height): (u32, u32), image: GpuImage) -> Self {
        Self {
            frame_id,
            width,
            height,
            format: PixelFormat::Gpu,
            stride: 0,
            data: Vec::new(),
            gpu: Some(image),
            color: ColorSpace::SDR,
            pool: None,
        }
    }

    /// El mismo fotograma con el espacio de color `color`.
    pub fn with_color(mut self, color: ColorSpace) -> Self {
        self.color = color;
        self
    }

    /// El mismo fotograma anotando que [`RawFrame::data`] salió de `pool`.
    pub fn with_pool(mut self, pool: Arc<BufferPool>) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Devuelve [`RawFrame::data`] a su pool, si salió de uno. Lo llama quien ya no necesita los
    /// píxeles (la presentación, tras subirlos a la GPU).
    pub fn recycle(self) {
        if let Some(pool) = self.pool {
            pool.give(self.data);
        }
    }

    /// Ancho y alto de cada plano de croma de un fotograma I420.
    pub const fn chroma_size(width: u32, height: u32) -> (u32, u32) {
        (width.div_ceil(2), height.div_ceil(2))
    }

    /// Bytes que ocupa un fotograma I420 compacto (`stride == width`).
    pub const fn i420_len(width: u32, height: u32) -> usize {
        let (cw, ch) = Self::chroma_size(width, height);
        width as usize * height as usize + 2 * (cw as usize * ch as usize)
    }

    /// Planos Y, U y V de un fotograma I420, o `None` si el formato o el tamaño no cuadran.
    pub fn i420_planes(&self) -> Option<(&[u8], &[u8], &[u8])> {
        if self.format != PixelFormat::I420 || self.stride < self.width {
            return None;
        }
        let (cw, ch) = Self::chroma_size(self.width, self.height);
        let y_len = self.stride as usize * self.height as usize;
        let c_len = cw as usize * ch as usize;
        let y = self.data.get(..y_len)?;
        let u = self.data.get(y_len..y_len + c_len)?;
        let v = self.data.get(y_len + c_len..y_len + 2 * c_len)?;
        Some((y, u, v))
    }

    /// Bytes que ocupa un fotograma NV12 compacto (`stride == width`); igual que en I420.
    pub const fn nv12_len(width: u32, height: u32) -> usize {
        Self::i420_len(width, height)
    }

    /// Planos Y y UV de un fotograma NV12, o `None` si el formato o el tamaño no cuadran.
    pub fn nv12_planes(&self) -> Option<(&[u8], &[u8])> {
        if self.format != PixelFormat::Nv12 || self.stride < self.width {
            return None;
        }
        let (cw, ch) = Self::chroma_size(self.width, self.height);
        let y_len = self.stride as usize * self.height as usize;
        let uv_len = 2 * cw as usize * ch as usize;
        let y = self.data.get(..y_len)?;
        let uv = self.data.get(y_len..y_len + uv_len)?;
        Some((y, uv))
    }

    /// Bytes que ocupa un fotograma I444 compacto (`stride == width`).
    pub const fn i444_len(width: u32, height: u32) -> usize {
        3 * width as usize * height as usize
    }

    /// Planos Y, U y V de un fotograma I444, o `None` si el formato o el tamaño no cuadran.
    pub fn i444_planes(&self) -> Option<(&[u8], &[u8], &[u8])> {
        if self.format != PixelFormat::I444 || self.stride < self.width {
            return None;
        }
        let y_len = self.stride as usize * self.height as usize;
        let c_len = self.width as usize * self.height as usize;
        let y = self.data.get(..y_len)?;
        let u = self.data.get(y_len..y_len + c_len)?;
        let v = self.data.get(y_len + c_len..y_len + 2 * c_len)?;
        Some((y, u, v))
    }

    /// Planos Y y UV de un fotograma NV24, o `None` si el formato o el tamaño no cuadran.
    pub fn nv24_planes(&self) -> Option<(&[u8], &[u8])> {
        if self.format != PixelFormat::Nv24 || self.stride < self.width {
            return None;
        }
        let y_len = self.stride as usize * self.height as usize;
        let uv_len = 2 * self.width as usize * self.height as usize;
        let y = self.data.get(..y_len)?;
        let uv = self.data.get(y_len..y_len + uv_len)?;
        Some((y, uv))
    }

    /// Bytes que ocupa un fotograma P010 compacto (`stride == 2 × width`).
    pub const fn p010_len(width: u32, height: u32) -> usize {
        2 * Self::i420_len(width, height)
    }

    /// Planos Y y UV (en bytes) de un fotograma P010, o `None` si el formato o el tamaño no
    /// cuadran.
    pub fn p010_planes(&self) -> Option<(&[u8], &[u8])> {
        if self.format != PixelFormat::P010 || self.stride < 2 * self.width {
            return None;
        }
        let (cw, ch) = Self::chroma_size(self.width, self.height);
        let y_len = self.stride as usize * self.height as usize;
        let uv_len = 4 * cw as usize * ch as usize;
        let y = self.data.get(..y_len)?;
        let uv = self.data.get(y_len..y_len + uv_len)?;
        Some((y, uv))
    }

    /// Plano de luma (Y) de un fotograma YUV de 8 bits.
    pub fn luma(&self) -> Option<&[u8]> {
        match self.format {
            PixelFormat::I420 => self.i420_planes().map(|(y, _, _)| y),
            PixelFormat::Nv12 => self.nv12_planes().map(|(y, _)| y),
            PixelFormat::I444 => self.i444_planes().map(|(y, _, _)| y),
            PixelFormat::Nv24 => self.nv24_planes().map(|(y, _)| y),
            PixelFormat::Bgra | PixelFormat::P010 | PixelFormat::Rgba16F | PixelFormat::Gpu => None,
        }
    }
}

/// Lo que [`Encoder::encode_into`] dice del fotograma comprimido que dejó en el búfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodedFrame {
    /// Identificador del fotograma del que salió el paquete (el primero, si salieron varios).
    pub frame_id: u64,
    /// Si el fotograma es intra (decodificable sin referencias).
    pub keyframe: bool,
}

/// Slice comprimido, unidad mínima de envío.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedSlice {
    /// Identificador del fotograma al que pertenece.
    pub frame_id: u64,
    /// Índice del slice dentro del fotograma.
    pub index: u8,
    /// Número total de slices del fotograma.
    pub count: u8,
    /// Si el fotograma es intra (decodificable sin referencias).
    pub keyframe: bool,
    /// Datos comprimidos.
    pub data: Vec<u8>,
}

/// Error de un codificador o decodificador.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    /// El backend no admite el códec o la configuración pedida.
    Unsupported(String),
    /// El codificador no puede leer las imágenes de GPU que entrega la captura (ni sin copias ni
    /// descargándolas): quien captura tiene que pasar a memoria de CPU y volver a intentarlo.
    GpuImport(String),
    /// Faltan referencias: hace falta un fotograma intra.
    MissingReference,
    /// Error interno del backend.
    Backend(String),
    /// El backend no está en esta máquina o en esta build (sin GPU, sin driver, sin FFmpeg).
    BackendUnavailable(Backend),
    /// El backend de decodificación no está en esta máquina o en esta build.
    DecoderUnavailable(DecoderBackend),
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodecError::BackendUnavailable(backend) => {
                write!(
                    f,
                    "el codificador {backend} no está disponible en este equipo"
                )
            }
            CodecError::DecoderUnavailable(backend) => {
                write!(
                    f,
                    "el decodificador {backend} no está disponible en este equipo"
                )
            }
            CodecError::Unsupported(what) => write!(f, "configuración no admitida: {what}"),
            CodecError::GpuImport(what) => write!(
                f,
                "el codificador no puede leer las imágenes de GPU de la captura: {what}"
            ),
            CodecError::MissingReference => f.write_str("falta un fotograma de referencia"),
            CodecError::Backend(what) => write!(f, "error del backend de vídeo: {what}"),
        }
    }
}

impl std::error::Error for CodecError {}

/// Codificador de vídeo de baja latencia.
///
/// Es `Send` porque cada codificador vive en el hilo de su pipeline.
pub trait Encoder: Send {
    /// Códec que produce este codificador.
    fn codec(&self) -> Codec;

    /// Backend que usa.
    fn backend(&self) -> Backend;

    /// Configura o reconfigura el codificador.
    fn configure(&mut self, config: &EncoderConfig) -> Result<(), CodecError>;

    /// Cambia el bitrate objetivo (el control de tasa lo llama cada 100 ms). Si el backend no lo
    /// admite en caliente ([`Encoder::live_bitrate`]), reabre y el siguiente fotograma sale intra.
    fn set_bitrate(&mut self, bitrate_kbps: u32) -> Result<(), CodecError>;

    /// Si [`Encoder::set_bitrate`] cambia el bitrate sin reabrir (sin intra). Por defecto, no: la
    /// sesión espacia entonces los cambios para no forzar un intra con cada uno.
    fn live_bitrate(&self) -> bool {
        false
    }

    /// Fuerza que el siguiente fotograma sea intra.
    fn request_keyframe(&mut self);

    /// Vuelve a codificar como intra el último fotograma de GPU que guarda el propio codificador
    /// (con la pantalla quieta la captura no entrega ninguno y la sesión no puede guardar la
    /// imagen de GPU de la captura sin retener su pool). `None` si no guarda ninguno: por
    /// defecto, y en las rutas que retendrían un búfer de la captura (DMA-BUF).
    fn repeat_last(&mut self) -> Option<Result<Vec<EncodedSlice>, CodecError>> {
        None
    }

    /// Codifica un fotograma y devuelve sus slices.
    fn encode(&mut self, frame: &RawFrame) -> Result<Vec<EncodedSlice>, CodecError>;

    /// Codifica un fotograma y deja la unidad de acceso completa (sus slices seguidos, tal como
    /// la recibe el decodificador) en `out`, que vacía antes y cuya memoria reutiliza.
    /// `Ok(None)` si el codificador se saltó el fotograma. Es la ruta del pipeline: por defecto
    /// concatena los slices de [`Encoder::encode`]; los backends la redefinen para copiar el
    /// paquete una sola vez, sin repartirlo en slices.
    fn encode_into(
        &mut self,
        frame: &RawFrame,
        out: &mut Vec<u8>,
    ) -> Result<Option<EncodedFrame>, CodecError> {
        out.clear();
        let slices = self.encode(frame)?;
        let Some(first) = slices.first() else {
            return Ok(None);
        };
        let encoded = EncodedFrame {
            frame_id: first.frame_id,
            keyframe: slices.iter().any(|slice| slice.keyframe),
        };
        for slice in &slices {
            out.extend_from_slice(&slice.data);
        }
        Ok(Some(encoded))
    }

    /// Imágenes de GPU que acepta sin copiarlas a CPU; la sesión se las pide a la
    /// captura. Por defecto, ninguna.
    fn gpu_input(&self) -> GpuInput {
        GpuInput::None
    }

    /// Copias de la imagen en memoria de CPU que hizo para el último fotograma (conversión de
    /// color, subida a la GPU…); en la ruta sin copias, 0. Por defecto, una.
    fn cpu_copies(&self) -> u32 {
        1
    }

    /// Control de tasa con el que quedó abierto, leído del propio codificador tras abrirlo (no
    /// lo que se pidió): `CBR 10000 kbit/s, VBV 167 kbit`. `None` si el backend no lo sabe
    /// decir.
    fn rate_control(&self) -> Option<String> {
        None
    }

    /// Tamaño del flujo que produce ahora: el del último fotograma entregado, reducido al tope de
    /// [`EncoderConfig`] si no cabía ([`coroom_proto::video::fit`]). `None` antes del primer
    /// fotograma, o si el backend no lo sabe decir.
    fn output_size(&self) -> Option<(u32, u32)> {
        None
    }

    /// Reducción en CPU que hizo el último fotograma para caber en el tope, con su coste medido.
    /// `None` si no hubo que reducir, o si la reducción fue en la GPU (en la ruta sin copias va en
    /// el mismo paso que la conversión de formato y no se mide aparte).
    fn cpu_scale(&self) -> Option<CpuScale> {
        None
    }
}

/// Reducción en CPU de un fotograma al tope de resolución ([`Encoder::cpu_scale`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuScale {
    /// Tamaño que trajo la captura.
    pub from: (u32, u32),
    /// Tamaño al que se redujo (el del flujo).
    pub to: (u32, u32),
    /// Lo que tardó la reducción (con la conversión de formato, si fue en el mismo paso).
    pub took: std::time::Duration,
}

impl std::fmt::Display for CpuScale {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}×{} → {}×{} en {:.2} ms",
            self.from.0,
            self.from.1,
            self.to.0,
            self.to.1,
            self.took.as_secs_f64() * 1000.0
        )
    }
}

/// Decodificador de vídeo.
///
/// Es `Send` porque cada decodificador vive en el hilo de su pipeline.
pub trait Decoder: Send {
    /// Códec que acepta este decodificador.
    fn codec(&self) -> Codec;

    /// Backend que usa.
    fn backend(&self) -> DecoderBackend;

    /// Entrega los slices de un fotograma completo y devuelve la imagen (I420 o NV12 compacta), si
    /// ya hay una.
    fn decode(&mut self, slices: &[EncodedSlice]) -> Result<Option<RawFrame>, CodecError>;

    /// Descarta el estado de referencias tras una pérdida irrecuperable.
    fn reset(&mut self);

    /// Copias de la imagen en memoria de CPU que hizo para el último fotograma (bajada de la GPU,
    /// copia al búfer de salida…); con la salida en la GPU, 0. Por defecto, una.
    fn cpu_copies(&self) -> u32 {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NullCodec;

    impl Encoder for NullCodec {
        fn codec(&self) -> Codec {
            Codec::Av1
        }

        fn backend(&self) -> Backend {
            Backend::Software
        }

        fn configure(&mut self, _config: &EncoderConfig) -> Result<(), CodecError> {
            Ok(())
        }

        fn set_bitrate(&mut self, _bitrate_kbps: u32) -> Result<(), CodecError> {
            Ok(())
        }

        fn request_keyframe(&mut self) {}

        fn encode(&mut self, _frame: &RawFrame) -> Result<Vec<EncodedSlice>, CodecError> {
            Ok(Vec::new())
        }
    }

    impl Decoder for NullCodec {
        fn codec(&self) -> Codec {
            Codec::Av1
        }

        fn backend(&self) -> DecoderBackend {
            DecoderBackend::Software
        }

        fn decode(&mut self, _slices: &[EncodedSlice]) -> Result<Option<RawFrame>, CodecError> {
            Err(CodecError::MissingReference)
        }

        fn reset(&mut self) {}
    }

    fn assert_send<T: Send + ?Sized>() {}

    #[test]
    fn encoder_and_decoder_are_send_and_object_safe() {
        assert_send::<dyn Encoder>();
        assert_send::<dyn Decoder>();
        let encoder: Box<dyn Encoder> = Box::new(NullCodec);
        let mut decoder: Box<dyn Decoder> = Box::new(NullCodec);
        assert_eq!(encoder.codec(), Codec::Av1);
        assert_eq!(decoder.decode(&[]), Err(CodecError::MissingReference));
    }

    #[test]
    fn names_round_trip_and_read_in_spanish() {
        for codec in Codec::PREFERENCE {
            assert_eq!(Codec::from_id(codec.id()), Some(codec));
        }
        for backend in Backend::ALL {
            assert_eq!(Backend::from_id(backend.id()), Some(backend));
        }
        for backend in DecoderBackend::ALL {
            assert_eq!(DecoderBackend::from_id(backend.id()), Some(backend));
        }
        assert_eq!(Codec::from_id("AV1"), Some(Codec::Av1));
        assert_eq!(Codec::from_id("vp9"), None);
        assert_eq!(encoder_label(Codec::Av1, Backend::Software), "SVT-AV1");
        assert_eq!(decoder_label(Codec::Av1, DecoderBackend::Software), "dav1d");
        assert_eq!(
            CodecError::BackendUnavailable(Backend::Nvenc).to_string(),
            "el codificador NVENC no está disponible en este equipo"
        );
    }

    #[test]
    fn nv12_planes_are_split() {
        let frame = RawFrame::cpu(1, (4, 2), PixelFormat::Nv12, 4, (0..12).collect());
        let (y, uv) = frame.nv12_planes().unwrap();
        assert_eq!(y, &[0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(uv, &[8, 9, 10, 11]);
        assert_eq!(frame.luma(), Some(y));
        assert_eq!(RawFrame::nv12_len(4, 2), 12);
        assert!(frame.i420_planes().is_none());
    }
}
