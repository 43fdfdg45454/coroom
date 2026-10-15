//! Presentación de vídeo de coroom.
//!
//! Define el trait [`Presenter`] (lo que hace el hilo `present` del pipeline con cada fotograma) y,
//! el vídeo como capa de la interfaz: el [`wgpu::GpuContext`] de la ventana (un
//! solo `wgpu::Device` con backend Vulkan, D3D12 en Windows, para la interfaz `iced` y el vídeo) y
//! [`layer::VideoLayer`], que el *widget* de vídeo dibuja en la pasada de la interfaz. Importa la
//! imagen decodificada sin copias, escala en GPU, presenta en modo inmediato o *mailbox* (VRR si
//! existe) y dibuja el cursor en local con los metadatos del anfitrión.
//!
//! La imagen llega en memoria de CPU y se sube a la GPU sin convertir en CPU: en un solo búfer
//! con sus planos por desplazamiento ([`PresentImage::Planar`], lo que entrega el decodificador,
//! sin partirlo: I420 e I444 en tres texturas, NV12 y P010 en dos; el búfer vuelve a su pool al
//! soltarse, [`Recycle`]) o en planos sueltos ([`PresentImage::I420`], [`PresentImage::Nv12`]…), o
//! como imagen Vulkan del decodificador ([`PresentImage::Vulkan`]) creada en el mismo
//! `VkDevice` del presentador ([`wgpu::WgpuPresenter::vulkan_share`]), que se importa en `wgpu`
//! sin copia (`texture_from_raw`) y se sincroniza con el semáforo de FFmpeg. El cursor se dibuja
//! con su forma real ([`CursorImage`], textura RGBA con punto activo) o, mientras no llega, con un
//! cuadrado blanco. [`Presenter::cpu_copies`] dice si el último fotograma pasó por la CPU. Módulos:
//!
//! - [`wgpu`]: dispositivo de la ventana ([`wgpu::GpuContext`]), superficie, selección de
//!   adaptador e importación Vulkan ([`wgpu::VideoImporter`]).
//! - [`layer`]: [`layer::VideoLayer`], el vídeo dentro de la pasada de la interfaz.
//! - [`offscreen`]: el mismo dibujo en una textura que se puede leer, para tests.
//! - [`overlay`]: [`overlay::OverlayBlit`], la textura de la interfaz encima del vídeo (el
//!   compositor de la ventana dibuja el vídeo en cuanto llega y la interfaz, ya dibujada, encima).
//! - [`NullPresenter`]: no dibuja; lo usa el modo sin ventana.
//!
//! Las estadísticas (el antiguo overlay de `egui`) son ahora un panel de la interfaz.
//!
//! El shader `yuv.wgsl` convierte con la matriz del flujo ([`ColorInfo`]): BT.709 en
//! rango limitado, la de `coroom_codec::convert`, o BT.2020 con PQ en HDR, que se presenta en una
//! superficie HDR si la hay ([`hdr`]) o con tone mapping a SDR ([`color`]). Acepta 4:2:0 (I420,
//! NV12), 4:4:4 (I444) y 10 bits (P010).

use std::any::Any;
use std::ffi::CString;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

pub mod color;
pub mod hdr;
pub mod layer;
pub mod offscreen;
pub mod overlay;
mod video;
pub mod wgpu;

pub use color::{ColorInfo, ColorMatrix, OutputMode};
pub use video::video_rect;

/// Modo de presentación de la superficie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PresentMode {
    /// Sin esperar a la sincronía vertical (latencia mínima, puede haber *tearing*).
    Immediate,
    /// Triple búfer sin bloqueo: el último fotograma gana.
    Mailbox,
    /// Sincronía vertical clásica; respaldo cuando no hay otra cosa.
    Fifo,
}

/// Posición y visibilidad del cursor que se dibuja en local.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorOverlay {
    /// Posición horizontal en píxeles del fotograma remoto.
    pub x: i32,
    /// Posición vertical en píxeles del fotograma remoto.
    pub y: i32,
    /// Si se dibuja.
    pub visible: bool,
}

/// Forma del cursor: RGBA de 8 bits sin premultiplicar, con su punto activo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorImage {
    /// Identificador de la forma (si no cambia, no se vuelve a subir).
    pub id: u64,
    /// Ancho en píxeles del fotograma remoto.
    pub width: u32,
    /// Alto en píxeles del fotograma remoto.
    pub height: u32,
    /// Punto activo, horizontal (el píxel que cae en la posición del cursor).
    pub hotspot_x: u32,
    /// Punto activo, vertical.
    pub hotspot_y: u32,
    /// `width × height × 4` bytes R, G, B, A.
    pub rgba: Vec<u8>,
}

impl CursorImage {
    /// Comprueba que el tamaño y los datos cuadran.
    pub fn is_valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.width <= 512
            && self.height <= 512
            && self.rgba.len() == self.width as usize * self.height as usize * 4
    }
}

/// Estado de sincronización de una imagen Vulkan externa, leído con ella bloqueada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalImageState {
    /// `VkImage`.
    pub image: u64,
    /// `VkImageLayout` actual.
    pub layout: i32,
    /// `VkAccessFlags` del último acceso.
    pub access: u32,
    /// Semáforo de línea temporal de la imagen.
    pub semaphore: u64,
    /// Valor del semáforo a partir del cual la imagen está lista.
    pub semaphore_value: u64,
    /// Familia de colas que la usó por última vez.
    pub queue_family: u32,
}

/// Lo que deja el presentador tras usar la imagen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalImageUpdate {
    /// Nuevo `VkImageLayout`.
    pub layout: i32,
    /// Nuevos `VkAccessFlags`.
    pub access: u32,
    /// Valor al que señalará el semáforo al terminar.
    pub semaphore_value: u64,
    /// Familia de colas que la usó.
    pub queue_family: u32,
}

/// Imagen Vulkan de otro componente (FFmpeg) en el dispositivo del presentador, con su protocolo
/// de sincronización: bloquear antes de enviar comandos, esperar al semáforo, señalarlo al valor
/// siguiente y desbloquear anotando el layout nuevo.
pub trait ExternalVulkanImage: Send + Sync {
    /// Bloquea la imagen y devuelve su estado.
    fn lock(&self) -> ExternalImageState;
    /// Anota `update` (si se enviaron comandos) y desbloquea.
    fn unlock(&self, update: Option<ExternalImageUpdate>);
}

/// Fotograma NV12 en una imagen Vulkan multiplano (`G8_B8R8_2PLANE_420_UNORM`).
#[derive(Clone)]
pub struct VulkanFrame {
    /// Ancho visible en píxeles.
    pub width: u32,
    /// Alto visible en píxeles.
    pub height: u32,
    /// Ancho de la imagen (el decodificador alinea).
    pub image_width: u32,
    /// Alto de la imagen.
    pub image_height: u32,
    /// `VkFormat`.
    pub format: i32,
    /// `VkDevice` donde vive (debe ser el del presentador).
    pub device: u64,
    /// La imagen y su sincronización (también la mantiene viva).
    pub image: Arc<dyn ExternalVulkanImage>,
}

impl fmt::Debug for VulkanFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VulkanFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("image_width", &self.image_width)
            .field("image_height", &self.image_height)
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

/// Dos fotogramas Vulkan son iguales si son la misma imagen.
impl PartialEq for VulkanFrame {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.image, &other.image)
    }
}

impl Eq for VulkanFrame {}

/// Familia de colas que el presentador cede a otro componente.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedQueueFamily {
    /// Índice de la familia.
    pub index: u32,
    /// Colas que puede usar.
    pub count: u32,
    /// `VkQueueFlags`.
    pub flags: u32,
    /// `VkVideoCodecOperationFlagsKHR` (0 si no es de vídeo).
    pub video_caps: u32,
}

/// `VkDevice` del presentador, creado con colas y extensiones de vídeo para que el decodificador
/// (FFmpeg) decodifique en él y el presentador importe las imágenes sin copia.
///
/// Las familias van en orden de preferencia; la de gráficos de `wgpu` va la última y solo para que
/// las imágenes se creen compartidas con ella (FFmpeg encuentra antes otra de cómputo y otra de
/// transferencia, y así nunca envía trabajo a la cola de `wgpu`).
pub struct VulkanDeviceShare {
    /// `vkGetInstanceProcAddr` del cargador de `wgpu`, como dirección.
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
    /// Familias de colas para el decodificador.
    pub queue_families: Vec<SharedQueueFamily>,
    /// `timelineSemaphore` activada.
    pub timeline_semaphore: bool,
    /// `synchronization2` activada.
    pub synchronization2: bool,
    /// `samplerYcbcrConversion` activada.
    pub sampler_ycbcr_conversion: bool,
    /// Extensiones de mantenimiento de vídeo que el driver ofrece y este dispositivo no activa
    /// (para el registro: FFmpeg, en su propio dispositivo, sí las usa).
    pub unused_video_extensions: Vec<String>,
    /// El dispositivo de `wgpu`, que debe vivir mientras se use.
    pub keepalive: Arc<dyn Any + Send + Sync>,
}

impl fmt::Debug for VulkanDeviceShare {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VulkanDeviceShare")
            .field("device", &format_args!("{:#x}", self.device))
            .field("device_extensions", &self.device_extensions)
            .field("queue_families", &self.queue_families)
            .finish_non_exhaustive()
    }
}

/// Disposición de los planos de una imagen de CPU ([`PresentImage::Planar`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlanarLayout {
    /// YUV 4:2:0 de 8 bits en tres planos (Y, U, V).
    I420,
    /// YUV 4:2:0 de 8 bits semiplanar (Y, UV entrelazado).
    Nv12,
    /// YUV 4:4:4 de 8 bits en tres planos del tamaño del fotograma.
    I444,
    /// YUV 4:2:0 de 10 bits semiplanar en palabras de 16 bits (HDR).
    P010,
}

impl PlanarLayout {
    /// Número de planos: tres planares, dos semiplanares.
    pub const fn planes(self) -> usize {
        if self.semiplanar() { 2 } else { 3 }
    }

    /// Si el croma va entrelazado en un plano de dos canales.
    pub const fn semiplanar(self) -> bool {
        matches!(self, PlanarLayout::Nv12 | PlanarLayout::P010)
    }

    /// Bytes útiles por fila y filas de cada plano para un fotograma de `width × height`; el
    /// tercero es `(0, 0)` en los semiplanares.
    pub const fn plane_extents(self, width: u32, height: u32) -> [(u32, u32); 3] {
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        match self {
            PlanarLayout::I420 => [(width, height), (cw, ch), (cw, ch)],
            PlanarLayout::Nv12 => [(width, height), (2 * cw, ch), (0, 0)],
            PlanarLayout::I444 => [(width, height), (width, height), (width, height)],
            PlanarLayout::P010 => [(2 * width, height), (4 * cw, ch), (0, 0)],
        }
    }
}

/// Dónde empieza un plano dentro del búfer de una [`PlanarImage`] y cuánto mide cada fila.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlaneRegion {
    /// Desplazamiento del plano en el búfer, en bytes.
    pub offset: usize,
    /// Bytes por fila.
    pub stride: u32,
}

/// A quien vuelve el búfer de una [`PlaneBuffer`] al soltarse (el pool del decodificador, que
/// vive en otra crate).
pub trait Recycle: Send + Sync {
    /// Recibe el búfer para reutilizarlo.
    fn recycle(&self, buffer: Vec<u8>);
}

/// Búfer de píxeles que, al soltarse, vuelve a quien lo prestó (si alguien lo prestó).
pub struct PlaneBuffer {
    data: Vec<u8>,
    recycle: Option<Arc<dyn Recycle>>,
}

impl PlaneBuffer {
    /// Búfer propio: se libera al soltarse.
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            data,
            recycle: None,
        }
    }

    /// Búfer prestado por `recycle`, que lo recibe de vuelta al soltarse.
    pub fn recycled(data: Vec<u8>, recycle: Arc<dyn Recycle>) -> Self {
        Self {
            data,
            recycle: Some(recycle),
        }
    }
}

impl Deref for PlaneBuffer {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.data
    }
}

impl Drop for PlaneBuffer {
    fn drop(&mut self) {
        if let Some(recycle) = self.recycle.take() {
            recycle.recycle(std::mem::take(&mut self.data));
        }
    }
}

impl fmt::Debug for PlaneBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlaneBuffer")
            .field("len", &self.data.len())
            .field("recycled", &self.recycle.is_some())
            .finish()
    }
}

/// La copia es propia (no vuelve al pool del original).
impl Clone for PlaneBuffer {
    fn clone(&self) -> Self {
        Self::new(self.data.clone())
    }
}

impl PartialEq for PlaneBuffer {
    fn eq(&self, other: &Self) -> bool {
        self.data == other.data
    }
}

impl Eq for PlaneBuffer {}

/// Imagen de CPU en un solo búfer con sus planos por desplazamiento y paso: lo que entrega un
/// decodificador, sin partirlo en copias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanarImage {
    /// Disposición de los planos.
    pub layout: PlanarLayout,
    /// Los píxeles.
    pub data: PlaneBuffer,
    /// Dónde está cada plano (el tercero no se usa en los semiplanares).
    pub planes: [PlaneRegion; 3],
}

impl PlanarImage {
    /// Imagen compacta como la entregan los decodificadores: el luma con filas de `y_stride`
    /// bytes y, seguidos, los planos de croma con la fila justa (`ceil(width / 2)` bytes, el
    /// doble entrelazados o en 16 bits; `width` en 4:4:4).
    pub fn compact(
        layout: PlanarLayout,
        width: u32,
        height: u32,
        y_stride: u32,
        data: PlaneBuffer,
    ) -> Self {
        let extents = layout.plane_extents(width, height);
        let mut planes = [PlaneRegion::default(); 3];
        let mut offset = y_stride as usize * height as usize;
        planes[0] = PlaneRegion {
            offset: 0,
            stride: y_stride,
        };
        for (plane, (row, rows)) in planes.iter_mut().zip(extents).skip(1) {
            if rows == 0 {
                break;
            }
            *plane = PlaneRegion {
                offset,
                stride: row,
            };
            offset += row as usize * rows as usize;
        }
        Self {
            layout,
            data,
            planes,
        }
    }

    /// Bytes del plano `index` desde su desplazamiento hasta el final del búfer (vacío si el
    /// desplazamiento se sale).
    pub fn plane(&self, index: usize) -> &[u8] {
        self.planes
            .get(index)
            .and_then(|region| self.data.get(region.offset..))
            .unwrap_or(&[])
    }
}

/// Imagen de un fotograma decodificado (el espacio de color va en [`PresentFrame::color`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresentImage {
    /// Imagen de CPU en un solo búfer con sus planos por desplazamiento (I420, NV12, I444 o
    /// P010): la ruta del pipeline, sin copias en CPU.
    Planar(PlanarImage),
    /// YUV 4:2:0 de 8 bits en tres planos de memoria de CPU.
    I420 {
        /// Luma: `height` filas de `y_stride` bytes.
        y: Vec<u8>,
        /// Croma azul: `ceil(height / 2)` filas de `uv_stride` bytes.
        u: Vec<u8>,
        /// Croma rojo: `ceil(height / 2)` filas de `uv_stride` bytes.
        v: Vec<u8>,
        /// Bytes por fila de `y`.
        y_stride: u32,
        /// Bytes por fila de `u` y `v`.
        uv_stride: u32,
    },
    /// YUV 4:2:0 de 8 bits semiplanar: luma y croma entrelazado.
    Nv12 {
        /// Luma: `height` filas de `y_stride` bytes.
        y: Vec<u8>,
        /// Croma U, V, U, V…: `ceil(height / 2)` filas de `uv_stride` bytes.
        uv: Vec<u8>,
        /// Bytes por fila de `y`.
        y_stride: u32,
        /// Bytes por fila de `uv` (al menos `2 × ceil(width / 2)`).
        uv_stride: u32,
    },
    /// YUV 4:4:4 de 8 bits en tres planos del tamaño del fotograma.
    I444 {
        /// Luma: `height` filas de `y_stride` bytes.
        y: Vec<u8>,
        /// Croma azul: `height` filas de `uv_stride` bytes.
        u: Vec<u8>,
        /// Croma rojo: `height` filas de `uv_stride` bytes.
        v: Vec<u8>,
        /// Bytes por fila de `y`.
        y_stride: u32,
        /// Bytes por fila de `u` y `v`.
        uv_stride: u32,
    },
    /// YUV 4:2:0 de 10 bits semiplanar (HDR): palabras de 16 bits *little-endian* con
    /// los 10 bits en la parte alta.
    P010 {
        /// Luma: `height` filas de `y_stride` bytes (al menos `2 × width`).
        y: Vec<u8>,
        /// Croma U, V…: `ceil(height / 2)` filas de `uv_stride` bytes (al menos
        /// `4 × ceil(width / 2)`).
        uv: Vec<u8>,
        /// Bytes por fila de `y`.
        y_stride: u32,
        /// Bytes por fila de `uv`.
        uv_stride: u32,
    },
    /// Imagen Vulkan NV12 del decodificador en el dispositivo del presentador, que se
    /// importa sin copia.
    Vulkan(VulkanFrame),
    /// Imagen de GPU que este presentador no puede mostrar sin copiarla (solo la acepta
    /// [`NullPresenter`]: sin ventana no hace falta mostrarla).
    Gpu,
}

/// Fotograma decodificado listo para presentar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentFrame {
    /// Identificador del fotograma, para la marca de tiempo de presentación.
    pub frame_id: u64,
    /// Ancho en píxeles.
    pub width: u32,
    /// Alto en píxeles.
    pub height: u32,
    /// Imagen.
    pub image: PresentImage,
    /// Espacio de color de la imagen.
    pub color: ColorInfo,
}

/// Error del presentador.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError {
    /// La superficie se perdió (ventana minimizada o cambio de GPU) y hay que recrearla.
    SurfaceLost,
    /// El adaptador no admite lo que se pide.
    Unsupported(String),
    /// Error interno del backend gráfico.
    Backend(String),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderError::SurfaceLost => f.write_str("se perdió la superficie de la ventana"),
            RenderError::Unsupported(what) => write!(f, "función gráfica no admitida: {what}"),
            RenderError::Backend(what) => write!(f, "error del backend gráfico: {what}"),
        }
    }
}

impl std::error::Error for RenderError {}

/// Presentador de fotogramas en una ventana.
///
/// Es `Send` para poder crearlo en el hilo de la interfaz y moverlo al hilo de presentación.
pub trait Presenter: Send {
    /// Modo de presentación en uso.
    fn present_mode(&self) -> PresentMode;

    /// Ajusta la superficie al nuevo tamaño de la ventana.
    fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError>;

    /// Actualiza el cursor que se dibuja encima del vídeo.
    fn set_cursor(&mut self, cursor: CursorOverlay);

    /// Cambia la forma del cursor; `None` vuelve al cuadrado de respaldo.
    fn set_cursor_image(&mut self, image: Option<Arc<CursorImage>>);

    /// Presenta un fotograma.
    fn present(&mut self, frame: &PresentFrame) -> Result<(), RenderError>;

    /// Copias de la imagen en memoria de CPU que hizo el último `present` (subir los planos cuenta
    /// una; importar una imagen Vulkan, ninguna). Por defecto, ninguna.
    fn cpu_copies(&self) -> u32 {
        0
    }
}

/// Presentador que no dibuja: acepta cada fotograma y vuelve enseguida.
///
/// Lo usa `coroom probe --headless`; quien lo llama marca la presentación justo después.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct NullPresenter {
    presented: u64,
    last_frame_id: Option<u64>,
    cursor: Option<CursorOverlay>,
    cursor_image: Option<Arc<CursorImage>>,
    size: Option<(u32, u32)>,
}

impl NullPresenter {
    /// Crea un presentador nulo.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fotogramas aceptados.
    pub fn presented(&self) -> u64 {
        self.presented
    }

    /// Último fotograma aceptado.
    pub fn last_frame_id(&self) -> Option<u64> {
        self.last_frame_id
    }

    /// Último cursor recibido.
    pub fn cursor(&self) -> Option<CursorOverlay> {
        self.cursor
    }

    /// Última forma del cursor recibida.
    pub fn cursor_image(&self) -> Option<&Arc<CursorImage>> {
        self.cursor_image.as_ref()
    }
}

impl Presenter for NullPresenter {
    fn present_mode(&self) -> PresentMode {
        PresentMode::Immediate
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        self.size = Some((width, height));
        Ok(())
    }

    fn set_cursor(&mut self, cursor: CursorOverlay) {
        self.cursor = Some(cursor);
    }

    fn set_cursor_image(&mut self, image: Option<Arc<CursorImage>>) {
        self.cursor_image = image;
    }

    fn present(&mut self, frame: &PresentFrame) -> Result<(), RenderError> {
        self.presented += 1;
        self.last_frame_id = Some(frame.frame_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send + ?Sized>() {}

    #[test]
    fn presenter_is_send_and_object_safe() {
        assert_send::<dyn Presenter>();
        assert_send::<offscreen::OffscreenPresenter>();
        assert_send::<layer::VideoLayer>();
        assert_send::<wgpu::GpuContext>();
        let mut presenter: Box<dyn Presenter> = Box::new(NullPresenter::new());
        assert_eq!(presenter.present_mode(), PresentMode::Immediate);
        let frame = PresentFrame {
            frame_id: 1,
            width: 2,
            height: 2,
            image: PresentImage::I420 {
                y: vec![16; 4],
                u: vec![128],
                v: vec![128],
                y_stride: 2,
                uv_stride: 1,
            },
            color: ColorInfo::SDR,
        };
        assert_eq!(presenter.present(&frame), Ok(()));
        assert_eq!(presenter.cpu_copies(), 0);
        let gpu = PresentFrame {
            image: PresentImage::Gpu,
            ..frame.clone()
        };
        assert_eq!(
            presenter.present(&gpu),
            Ok(()),
            "sin ventana se acepta todo"
        );
        presenter.set_cursor(CursorOverlay {
            x: 1,
            y: 1,
            visible: true,
        });
    }

    #[test]
    fn cursor_images_are_validated() {
        let mut image = CursorImage {
            id: 1,
            width: 2,
            height: 2,
            hotspot_x: 0,
            hotspot_y: 0,
            rgba: vec![255; 16],
        };
        assert!(image.is_valid());
        image.rgba.pop();
        assert!(!image.is_valid());
        let mut presenter = NullPresenter::new();
        presenter.set_cursor_image(Some(Arc::new(image)));
        assert!(presenter.cursor_image().is_some());
    }

    #[test]
    fn null_presenter_counts_frames() {
        let mut presenter = NullPresenter::new();
        let frame = PresentFrame {
            frame_id: 9,
            width: 2,
            height: 2,
            image: PresentImage::I420 {
                y: vec![16; 4],
                u: vec![128],
                v: vec![128],
                y_stride: 2,
                uv_stride: 1,
            },
            color: ColorInfo::SDR,
        };
        presenter.present(&frame).unwrap();
        presenter.present(&frame).unwrap();
        assert_eq!(presenter.presented(), 2);
        assert_eq!(presenter.last_frame_id(), Some(9));
    }
}
