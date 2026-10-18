//! Imágenes de GPU que recorren el pipeline sin pasar por memoria de CPU.
//!
//! - **Entrada del codificador**: un DMA-BUF de PipeWire ([`DmaBufImage`], Linux) o una textura
//!   D3D11 de Windows.Graphics.Capture ([`D3D11Image`], Windows). El codificador de FFmpeg los
//!   envuelve en un `AVFrame` de `AV_PIX_FMT_DRM_PRIME` o `AV_PIX_FMT_D3D11` y los lleva a sus
//!   *hw frames* sin copia a CPU (ver `ffmpeg::import`).
//! - **Salida del decodificador**: una imagen Vulkan de FFmpeg ([`VulkanImage`]) creada en el
//!   mismo `VkDevice` que el presentador (el que describe [`SharedVulkanDevice`]), que
//!   `coroom-render` importa en `wgpu` sin copia, o una imagen de GPU que no se puede presentar sin
//!   copia ([`OpaqueImage`]: sin ventana no hace falta). En Android (feature `mediacodec`), un
//!   búfer de salida de MediaCodec que ya está en la `Surface` de la vista y solo falta soltar a la
//!   pantalla (`crate::mediacodec::SurfaceFrame`).
//!
//! [`GpuInput`] dice qué entrada de GPU acepta un codificador (y, por tanto, qué debe pedir la
//! captura) y [`DecodeOutput`] dónde deja el decodificador sus imágenes.
//!
//! Este módulo no tiene `unsafe`: las imágenes llevan descriptores (`OwnedFd`, punteros COM con
//! recuento de referencias de la crate `windows`) y manijas Vulkan como enteros; quien las usa de
//! verdad es `ffmpeg` (con su `// SAFETY:`).

use std::any::Any;
use std::ffi::CString;
use std::fmt;
use std::sync::Arc;

/// Modificador DRM lineal (`DRM_FORMAT_MOD_LINEAR`).
pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;

/// Modificador DRM «implícito»: la disposición la decide el driver (`DRM_FORMAT_MOD_INVALID`).
pub const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// Código de cuatro letras de un formato DRM (`drm_fourcc.h`).
pub const fn fourcc(code: &[u8; 4]) -> u32 {
    (code[0] as u32) | ((code[1] as u32) << 8) | ((code[2] as u32) << 16) | ((code[3] as u32) << 24)
}

/// `DRM_FORMAT_XRGB8888`: un plano de 32 bits B, G, R, X en memoria (little-endian).
pub const DRM_FORMAT_XRGB8888: u32 = fourcc(b"XR24");
/// `DRM_FORMAT_ARGB8888`: B, G, R, A en memoria.
pub const DRM_FORMAT_ARGB8888: u32 = fourcc(b"AR24");
/// `DRM_FORMAT_XBGR8888`: R, G, B, X en memoria.
pub const DRM_FORMAT_XBGR8888: u32 = fourcc(b"XB24");
/// `DRM_FORMAT_ABGR8888`: R, G, B, A en memoria.
pub const DRM_FORMAT_ABGR8888: u32 = fourcc(b"AB24");

/// Algo que hay que mantener vivo mientras se usa la imagen (el búfer de PipeWire, el fotograma
/// de WGC, el `AVFrame` de FFmpeg…). Se suelta con el último `Arc`.
pub type KeepAlive = Arc<dyn Any + Send + Sync>;

/// Plano de un DMA-BUF.
#[cfg(unix)]
#[derive(Debug)]
pub struct DmaBufPlane {
    /// Descriptor del objeto DMA-BUF que contiene el plano (propio: se cierra al soltarlo).
    pub fd: std::os::fd::OwnedFd,
    /// Desplazamiento del plano dentro del objeto, en bytes.
    pub offset: u32,
    /// Bytes por fila.
    pub stride: u32,
}

/// Imagen DMA-BUF con modificador explícito (o [`DRM_FORMAT_MOD_INVALID`]).
#[cfg(unix)]
pub struct DmaBufImage {
    /// Planos (uno para los formatos RGB de 32 bits).
    pub planes: Vec<DmaBufPlane>,
    /// Modificador DRM de la disposición en memoria.
    pub modifier: u64,
    /// Formato DRM (`DRM_FORMAT_*`, ver [`fourcc`]).
    pub fourcc: u32,
    /// Ancho en píxeles.
    pub width: u32,
    /// Alto en píxeles.
    pub height: u32,
    /// Lo que debe vivir mientras se lee la imagen (el búfer de PipeWire vuelve al productor al
    /// soltarlo).
    pub keepalive: Option<KeepAlive>,
}

#[cfg(unix)]
impl fmt::Debug for DmaBufImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DmaBufImage")
            .field("planes", &self.planes)
            .field("modifier", &format_args!("{:#018x}", self.modifier))
            .field("fourcc", &fourcc_name(self.fourcc))
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// Las cuatro letras de un formato DRM, para mensajes.
pub fn fourcc_name(code: u32) -> String {
    code.to_le_bytes()
        .iter()
        .map(|&byte| {
            if byte.is_ascii_graphic() {
                char::from(byte)
            } else {
                '?'
            }
        })
        .collect()
}

/// Textura D3D11 de la captura (formato `DXGI_FORMAT_B8G8R8A8_UNORM`).
///
/// Las interfaces COM de la crate `windows` cuentan referencias y son `Send + Sync` (D3D11 es
/// *free-threaded*; el contexto inmediato se protege con `ID3D11Multithread`).
#[cfg(windows)]
pub struct D3D11Image {
    /// Textura (o matriz de texturas) que contiene la imagen.
    pub texture: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    /// Subrecurso de la textura con la imagen.
    pub subresource: u32,
    /// Dispositivo que creó la textura (el de la captura).
    pub device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    /// Ancho en píxeles.
    pub width: u32,
    /// Alto en píxeles.
    pub height: u32,
    /// El fotograma de WGC, que no vuelve al pool mientras viva.
    pub keepalive: Option<KeepAlive>,
}

#[cfg(windows)]
impl fmt::Debug for D3D11Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("D3D11Image")
            .field("subresource", &self.subresource)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// Estado de sincronización de una imagen Vulkan de FFmpeg (`AVVkFrame`), leído con el fotograma
/// bloqueado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VulkanFrameState {
    /// `VkImage`.
    pub image: u64,
    /// `VkImageLayout` actual.
    pub layout: i32,
    /// `VkAccessFlags` del último acceso.
    pub access: u32,
    /// Semáforo de línea temporal (`VkSemaphore`) de la imagen.
    pub semaphore: u64,
    /// Valor del semáforo a partir del cual la imagen está lista.
    pub semaphore_value: u64,
    /// Familia de colas que la usó por última vez.
    pub queue_family: u32,
}

/// Lo que cambió quien usó la imagen: el valor al que señalará el semáforo y el layout en que la
/// deja.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VulkanFrameUpdate {
    /// Nuevo `VkImageLayout`.
    pub layout: i32,
    /// Nuevos `VkAccessFlags`.
    pub access: u32,
    /// Valor al que se señala el semáforo al terminar (el anterior más uno).
    pub semaphore_value: u64,
    /// Familia de colas que la usa.
    pub queue_family: u32,
}

/// Acceso sincronizado a una imagen Vulkan de FFmpeg con el protocolo de `hwcontext_vulkan.h`:
/// bloquear justo antes de enviar los comandos, esperar al semáforo en `semaphore_value`, señalarlo
/// a `semaphore_value + 1`, anotar el layout nuevo y desbloquear justo después de enviarlos.
pub trait VulkanFrameAccess: Send + Sync {
    /// Bloquea el fotograma (`AVVulkanFramesContext.lock_frame`) y devuelve su estado.
    fn lock(&self) -> VulkanFrameState;
    /// Anota `update` (si se enviaron comandos) y desbloquea. Sin `lock` previo no hace nada.
    fn unlock(&self, update: Option<VulkanFrameUpdate>);
}

/// Imagen Vulkan decodificada por FFmpeg en el dispositivo compartido con el presentador.
#[derive(Clone)]
pub struct VulkanImage {
    /// Ancho visible en píxeles.
    pub width: u32,
    /// Alto visible en píxeles.
    pub height: u32,
    /// Ancho de la imagen reservada (el decodificador alinea, por ejemplo 1080 → 1088 filas).
    pub image_width: u32,
    /// Alto de la imagen reservada.
    pub image_height: u32,
    /// `VkFormat` de la imagen (normalmente `G8_B8R8_2PLANE_420_UNORM`, NV12).
    pub format: i32,
    /// `VkInstance` de FFmpeg (la del presentador).
    pub instance: u64,
    /// `VkDevice` de FFmpeg (el del presentador).
    pub device: u64,
    /// Sincronización y vida del `AVFrame`.
    pub access: Arc<dyn VulkanFrameAccess>,
}

impl fmt::Debug for VulkanImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VulkanImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("format", &self.format)
            .field("device", &format_args!("{:#x}", self.device))
            .finish_non_exhaustive()
    }
}

/// Imagen de GPU que no se puede presentar sin copiarla (por ejemplo, la de NVDEC sin dispositivo
/// Vulkan compartido): sin ventana no hace falta presentarla.
pub struct OpaqueImage {
    /// Qué es (para mensajes): `"cuda"`, `"vaapi"`…
    pub kind: String,
    /// El fotograma de FFmpeg.
    pub keepalive: KeepAlive,
}

impl fmt::Debug for OpaqueImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpaqueImage")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// Imagen de GPU de un [`crate::RawFrame`] con [`crate::PixelFormat::Gpu`].
#[derive(Clone)]
pub enum GpuImage {
    /// DMA-BUF de la captura de PipeWire.
    #[cfg(unix)]
    DmaBuf(Arc<DmaBufImage>),
    /// Textura D3D11 de la captura de WGC.
    #[cfg(windows)]
    D3D11(Arc<D3D11Image>),
    /// Imagen Vulkan decodificada en el dispositivo del presentador.
    Vulkan(Arc<VulkanImage>),
    /// Imagen de GPU que solo se puede descartar.
    Opaque(Arc<OpaqueImage>),
    /// Búfer de salida de MediaCodec decodificado en la `Surface` de la vista (Android): la
    /// presentación lo suelta a la pantalla con [`crate::mediacodec::SurfaceFrame::present`]; si
    /// se suelta sin presentar (el último `Arc`), vuelve al decodificador sin dibujarse.
    #[cfg(feature = "mediacodec")]
    MediaCodec(Arc<crate::mediacodec::SurfaceFrame>),
}

impl GpuImage {
    /// Nombre corto para mensajes.
    pub fn kind(&self) -> &str {
        match self {
            #[cfg(unix)]
            GpuImage::DmaBuf(_) => "DMA-BUF",
            #[cfg(windows)]
            GpuImage::D3D11(_) => "D3D11",
            GpuImage::Vulkan(_) => "Vulkan",
            GpuImage::Opaque(image) => &image.kind,
            #[cfg(feature = "mediacodec")]
            GpuImage::MediaCodec(_) => "MediaCodec",
        }
    }

    fn ptr(&self) -> *const () {
        match self {
            #[cfg(unix)]
            GpuImage::DmaBuf(image) => Arc::as_ptr(image).cast(),
            #[cfg(windows)]
            GpuImage::D3D11(image) => Arc::as_ptr(image).cast(),
            GpuImage::Vulkan(image) => Arc::as_ptr(image).cast(),
            GpuImage::Opaque(image) => Arc::as_ptr(image).cast(),
            #[cfg(feature = "mediacodec")]
            GpuImage::MediaCodec(image) => Arc::as_ptr(image).cast(),
        }
    }
}

impl fmt::Debug for GpuImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(unix)]
            GpuImage::DmaBuf(image) => image.fmt(f),
            #[cfg(windows)]
            GpuImage::D3D11(image) => image.fmt(f),
            GpuImage::Vulkan(image) => image.fmt(f),
            GpuImage::Opaque(image) => image.fmt(f),
            #[cfg(feature = "mediacodec")]
            GpuImage::MediaCodec(image) => image.fmt(f),
        }
    }
}

/// Dos imágenes son iguales si son la misma (no se comparan píxeles de GPU).
impl PartialEq for GpuImage {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.ptr(), other.ptr())
    }
}

impl Eq for GpuImage {}

/// Entrada de GPU que acepta un codificador sin copiar a CPU; la sesión se la pide a la captura.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum GpuInput {
    /// Solo memoria de CPU.
    #[default]
    None,
    /// DMA-BUF (Linux) con alguno de estos modificadores, en orden de preferencia.
    DmaBuf {
        /// Modificadores DRM que el codificador puede importar para `XRGB8888`.
        modifiers: Vec<u64>,
    },
    /// Texturas D3D11 (Windows).
    D3D11,
}

impl GpuInput {
    /// Si pide imágenes de GPU.
    pub fn is_gpu(&self) -> bool {
        !matches!(self, GpuInput::None)
    }
}

/// Familia de colas del dispositivo Vulkan compartido.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedQueueFamily {
    /// Índice de la familia.
    pub index: u32,
    /// Colas creadas en el dispositivo que puede usar FFmpeg.
    pub count: u32,
    /// `VkQueueFlags` de la familia.
    pub flags: u32,
    /// `VkVideoCodecOperationFlagsKHR` de la familia (0 si no es de vídeo).
    pub video_caps: u32,
}

/// Características de Vulkan activadas en el dispositivo compartido que le importan a FFmpeg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SharedFeatures {
    /// `timelineSemaphore` (obligatoria).
    pub timeline_semaphore: bool,
    /// `synchronization2` (obligatoria).
    pub synchronization2: bool,
    /// `samplerYcbcrConversion`.
    pub sampler_ycbcr_conversion: bool,
}

/// Dispositivo Vulkan del presentador (`wgpu`) que FFmpeg usa para decodificar, de modo que las
/// imágenes decodificadas se puedan muestrear sin copia.
///
/// Lo describe `coroom-render` (que creó el dispositivo con las colas y extensiones de vídeo) y lo
/// consume `ffmpeg::vulkan`. FFmpeg solo recibe familias de colas que `wgpu` no usa,
/// salvo la de gráficos, que va la última para que las imágenes se
/// creen en modo concurrente con ella.
pub struct SharedVulkanDevice {
    /// `vkGetInstanceProcAddr` del cargador que usa `wgpu`, como dirección.
    pub get_instance_proc_addr: usize,
    /// `VkInstance`.
    pub instance: u64,
    /// `VkPhysicalDevice`.
    pub physical_device: u64,
    /// `VkDevice`.
    pub device: u64,
    /// Extensiones de instancia activadas.
    pub instance_extensions: Vec<CString>,
    /// Extensiones de dispositivo activadas.
    pub device_extensions: Vec<CString>,
    /// Familias de colas para FFmpeg, en orden de preferencia.
    pub queue_families: Vec<SharedQueueFamily>,
    /// Características activadas.
    pub features: SharedFeatures,
    /// El dispositivo de `wgpu`, que debe vivir más que FFmpeg.
    pub keepalive: KeepAlive,
}

impl fmt::Debug for SharedVulkanDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedVulkanDevice")
            .field("device", &format_args!("{:#x}", self.device))
            .field("device_extensions", &self.device_extensions)
            .field("queue_families", &self.queue_families)
            .field("features", &self.features)
            .finish_non_exhaustive()
    }
}

/// Dónde deja el decodificador las imágenes.
#[derive(Debug, Clone, Default)]
pub enum DecodeOutput {
    /// En memoria de CPU (I420 o NV12): lo de siempre, con copia si el decodificador es de GPU.
    #[default]
    Cpu,
    /// En la GPU. Con `vulkan`, en ese dispositivo (decodificando en él o mapeando a él); si no se
    /// puede, se baja a CPU. Sin `vulkan` (no hay ventana), la imagen se queda donde la deje el
    /// decodificador ([`GpuImage::Opaque`]).
    Gpu {
        /// Dispositivo del presentador.
        vulkan: Option<Arc<SharedVulkanDevice>>,
    },
}

impl DecodeOutput {
    /// Si pide imágenes de GPU.
    pub fn is_gpu(&self) -> bool {
        matches!(self, DecodeOutput::Gpu { .. })
    }
}

/// `COROOM_ZERO_COPY=1` (o `on`, `yes`, `true`): fuerza las rutas sin copias que están desactivadas
/// por defecto por fallos de drivers (D3D11 → Vulkan Video en Windows).
pub fn zero_copy_forced() -> bool {
    std::env::var("COROOM_ZERO_COPY").is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "on" | "yes" | "true"
        )
    })
}

/// Si el usuario desactivó la ruta sin copias (`COROOM_ZERO_COPY=0`, `off` o `no`): captura en
/// memoria compartida o en textura de *staging*, subida a la GPU y bajada a CPU (la ruta con copias).
pub fn zero_copy_disabled() -> bool {
    std::env::var("COROOM_ZERO_COPY").is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "off" | "no" | "false"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_codes_match_drm_fourcc_h() {
        // Valores de `drm_fourcc.h`.
        assert_eq!(DRM_FORMAT_XRGB8888, 0x3432_5258);
        assert_eq!(DRM_FORMAT_ARGB8888, 0x3432_5241);
        assert_eq!(DRM_FORMAT_XBGR8888, 0x3432_4258);
        assert_eq!(DRM_FORMAT_ABGR8888, 0x3432_4241);
        assert_eq!(fourcc_name(DRM_FORMAT_XRGB8888), "XR24");
        assert_eq!(fourcc_name(0), "????");
    }

    #[test]
    fn gpu_images_compare_by_identity() {
        let a = GpuImage::Opaque(Arc::new(OpaqueImage {
            kind: "cuda".into(),
            keepalive: Arc::new(()),
        }));
        let b = GpuImage::Opaque(Arc::new(OpaqueImage {
            kind: "cuda".into(),
            keepalive: Arc::new(()),
        }));
        assert_eq!(a, a.clone());
        assert_ne!(a, b);
        assert_eq!(a.kind(), "cuda");
        assert!(GpuInput::D3D11.is_gpu() && !GpuInput::None.is_gpu());
        assert!(!DecodeOutput::default().is_gpu());
    }
}
