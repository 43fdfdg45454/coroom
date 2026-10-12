//! Estructuras de `hwcontext_drm.h`, `hwcontext_vulkan.h` y `hwcontext_d3d11va.h` de FFmpeg 8.1,
//! que `rusty_ffmpeg` no genera (sus cabeceras dependen de las de Vulkan, libdrm y D3D11).
//!
//! Se declaran a mano con `#[repr(C)]`, copiando el orden y los tipos de las cabeceras de
//! `third_party/ffmpeg/include/libavutil/` (libavutil 60, con los campos obsoletos que aún
//! compila: `FF_API_VULKAN_FIXED_QUEUES` y `FF_API_VULKAN_SYNC_QUEUES`). Los tests comparan
//! tamaños y desplazamientos con los que da el compilador de C para esas cabeceras en x86-64
//! (Linux y Windows coinciden: punteros, `size_t` y `ptrdiff_t` de 8 bytes, enumeraciones de 4).
//!
//! Los tipos de Vulkan son los de `ash`, que también son `#[repr(C)]` y siguen `vulkan_core.h`.
//! Las interfaces COM de D3D11 van como punteros opacos.

use std::ffi::{c_int, c_uint, c_void};

use ash::vk;

/// `AV_NUM_DATA_POINTERS`.
pub(crate) const AV_NUM_DATA_POINTERS: usize = 8;

/// `AV_DRM_MAX_PLANES`.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) const AV_DRM_MAX_PLANES: usize = 4;

/// `AVDRMObjectDescriptor`: un objeto DMA-BUF.
// Solo en Linux (DMA-BUF); se declara en todos los sistemas para comprobar su disposición.
#[cfg_attr(not(unix), allow(dead_code))]
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AVDRMObjectDescriptor {
    /// Descriptor PRIME del objeto.
    pub fd: c_int,
    /// Tamaño total del objeto.
    pub size: usize,
    /// Modificador DRM (`DRM_FORMAT_MOD_*`).
    pub format_modifier: u64,
}

/// `AVDRMPlaneDescriptor`: un plano de una capa.
// Solo en Linux (DMA-BUF); se declara en todos los sistemas para comprobar su disposición.
#[cfg_attr(not(unix), allow(dead_code))]
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AVDRMPlaneDescriptor {
    /// Índice del objeto que contiene el plano.
    pub object_index: c_int,
    /// Desplazamiento del plano en el objeto.
    pub offset: isize,
    /// Bytes por fila.
    pub pitch: isize,
}

/// `AVDRMLayerDescriptor`: una capa (una imagen con su formato DRM).
// Solo en Linux (DMA-BUF); se declara en todos los sistemas para comprobar su disposición.
#[cfg_attr(not(unix), allow(dead_code))]
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AVDRMLayerDescriptor {
    /// Formato DRM de la capa.
    pub format: u32,
    /// Planos usados.
    pub nb_planes: c_int,
    /// Planos.
    pub planes: [AVDRMPlaneDescriptor; AV_DRM_MAX_PLANES],
}

/// `AVDRMFrameDescriptor`: lo que apunta `AVFrame.data[0]` en `AV_PIX_FMT_DRM_PRIME`.
// Solo en Linux (DMA-BUF); se declara en todos los sistemas para comprobar su disposición.
#[cfg_attr(not(unix), allow(dead_code))]
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AVDRMFrameDescriptor {
    /// Objetos usados.
    pub nb_objects: c_int,
    /// Objetos.
    pub objects: [AVDRMObjectDescriptor; AV_DRM_MAX_PLANES],
    /// Capas usadas.
    pub nb_layers: c_int,
    /// Capas.
    pub layers: [AVDRMLayerDescriptor; AV_DRM_MAX_PLANES],
}

/// `AVVulkanDeviceQueueFamily`.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AVVulkanDeviceQueueFamily {
    /// Índice de la familia.
    pub idx: c_int,
    /// Colas en uso.
    pub num: c_int,
    /// `VkQueueFlagBits`.
    pub flags: vk::QueueFlags,
    /// `VkVideoCodecOperationFlagBitsKHR`.
    pub video_caps: vk::VideoCodecOperationFlagsKHR,
}

/// Retrollamada de bloqueo de colas (`lock_queue`/`unlock_queue`, obsoleta en libavutil 62).
pub(crate) type QueueLockFn =
    Option<unsafe extern "C" fn(ctx: *mut rsmpeg::ffi::AVHWDeviceContext, family: u32, index: u32)>;

/// `AVVulkanDeviceContext` (`AVHWDeviceContext.hwctx` de un dispositivo Vulkan).
#[repr(C)]
pub(crate) struct AVVulkanDeviceContext {
    /// `const VkAllocationCallbacks *`.
    pub alloc: *const vk::AllocationCallbacks<'static>,
    /// `PFN_vkGetInstanceProcAddr`.
    pub get_proc_addr: Option<vk::PFN_vkGetInstanceProcAddr>,
    /// Instancia (Vulkan ≥ 1.3).
    pub inst: vk::Instance,
    /// Dispositivo físico.
    pub phys_dev: vk::PhysicalDevice,
    /// Dispositivo lógico.
    pub act_dev: vk::Device,
    /// Características activadas (con su cadena `pNext`).
    pub device_features: vk::PhysicalDeviceFeatures2<'static>,
    /// Extensiones de instancia activadas.
    pub enabled_inst_extensions: *const *const std::ffi::c_char,
    /// Número de extensiones de instancia.
    pub nb_enabled_inst_extensions: c_int,
    /// Extensiones de dispositivo activadas.
    pub enabled_dev_extensions: *const *const std::ffi::c_char,
    /// Número de extensiones de dispositivo.
    pub nb_enabled_dev_extensions: c_int,
    /// Obsoletos (`FF_API_VULKAN_FIXED_QUEUES`): familia de gráficos y colas.
    pub queue_family_index: c_int,
    pub nb_graphics_queues: c_int,
    pub queue_family_tx_index: c_int,
    pub nb_tx_queues: c_int,
    pub queue_family_comp_index: c_int,
    pub nb_comp_queues: c_int,
    pub queue_family_encode_index: c_int,
    pub nb_encode_queues: c_int,
    pub queue_family_decode_index: c_int,
    pub nb_decode_queues: c_int,
    /// Obsoletos (`FF_API_VULKAN_SYNC_QUEUES`).
    pub lock_queue: QueueLockFn,
    pub unlock_queue: QueueLockFn,
    /// Familias de colas, en orden de preferencia.
    pub qf: [AVVulkanDeviceQueueFamily; 64],
    /// Familias usadas.
    pub nb_qf: c_int,
}

/// Retrollamada de bloqueo de un fotograma (`lock_frame`/`unlock_frame`).
pub(crate) type FrameLockFn =
    Option<unsafe extern "C" fn(fc: *mut rsmpeg::ffi::AVHWFramesContext, vkf: *mut AVVkFrame)>;

/// `AVVulkanFramesContext` (`AVHWFramesContext.hwctx` de un contexto de fotogramas Vulkan).
#[repr(C)]
pub(crate) struct AVVulkanFramesContext {
    /// `VkImageTiling`.
    pub tiling: vk::ImageTiling,
    /// `VkImageUsageFlagBits` extra.
    pub usage: vk::ImageUsageFlags,
    /// Cadena `pNext` de `VkImageCreateInfo`.
    pub create_pnext: *mut c_void,
    /// Cadenas `pNext` de `VkMemoryAllocateInfo`, por plano.
    pub alloc_pnext: [*mut c_void; AV_NUM_DATA_POINTERS],
    /// `AVVkFrameFlags`.
    pub flags: c_uint,
    /// `VkImageCreateFlags`.
    pub img_flags: vk::ImageCreateFlags,
    /// `VkFormat` de cada imagen.
    pub format: [vk::Format; AV_NUM_DATA_POINTERS],
    /// Capas de cada imagen.
    pub nb_layers: c_int,
    /// Bloquea un fotograma.
    pub lock_frame: FrameLockFn,
    /// Lo desbloquea.
    pub unlock_frame: FrameLockFn,
}

/// `AVVkFrame`: lo que apunta `AVFrame.data[0]` en `AV_PIX_FMT_VULKAN`.
#[repr(C)]
pub(crate) struct AVVkFrame {
    /// Imágenes (una si es multiplano).
    pub img: [vk::Image; AV_NUM_DATA_POINTERS],
    /// Disposición en memoria.
    pub tiling: vk::ImageTiling,
    /// Memoria de cada imagen.
    pub mem: [vk::DeviceMemory; AV_NUM_DATA_POINTERS],
    /// Tamaño de cada memoria.
    pub size: [usize; AV_NUM_DATA_POINTERS],
    /// Propiedades de la memoria.
    pub flags: vk::MemoryPropertyFlags,
    /// Últimos accesos.
    pub access: [vk::AccessFlags; AV_NUM_DATA_POINTERS],
    /// Layouts actuales.
    pub layout: [vk::ImageLayout; AV_NUM_DATA_POINTERS],
    /// Semáforos de línea temporal.
    pub sem: [vk::Semaphore; AV_NUM_DATA_POINTERS],
    /// Valores a partir de los cuales cada imagen está lista.
    pub sem_value: [u64; AV_NUM_DATA_POINTERS],
    /// Interno de FFmpeg.
    pub internal: *mut c_void,
    /// Desplazamiento de cada plano en su memoria.
    pub offset: [isize; AV_NUM_DATA_POINTERS],
    /// Familia de colas que usó cada imagen por última vez.
    pub queue_family: [u32; AV_NUM_DATA_POINTERS],
}

/// `AVD3D11VADeviceContext` (`AVHWDeviceContext.hwctx` de un dispositivo D3D11VA).
#[repr(C)]
// Solo se usa en Windows; se declara en todos los sistemas para comprobar su disposición.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) struct AVD3D11VADeviceContext {
    /// `ID3D11Device *`.
    pub device: *mut c_void,
    /// `ID3D11DeviceContext *`.
    pub device_context: *mut c_void,
    /// `ID3D11VideoDevice *`.
    pub video_device: *mut c_void,
    /// `ID3D11VideoContext *`.
    pub video_context: *mut c_void,
    /// Bloqueo del contexto inmediato.
    pub lock: Option<unsafe extern "C" fn(lock_ctx: *mut c_void)>,
    /// Desbloqueo.
    pub unlock: Option<unsafe extern "C" fn(lock_ctx: *mut c_void)>,
    /// Argumento de `lock` y `unlock`.
    pub lock_ctx: *mut c_void,
    /// `D3D11_BIND_FLAG` de las texturas del pool.
    pub bind_flags: c_uint,
    /// `D3D11_RESOURCE_MISC_FLAG` de las texturas del pool.
    pub misc_flags: c_uint,
}

/// `AVD3D11FrameDescriptor`.
#[repr(C)]
// Solo se usa en Windows; se declara en todos los sistemas para comprobar su disposición.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub(crate) struct AVD3D11FrameDescriptor {
    /// `ID3D11Texture2D *`.
    pub texture: *mut c_void,
    /// Índice en la matriz de texturas.
    pub index: isize,
}

/// `AVD3D11VAFramesContext` (`AVHWFramesContext.hwctx` de un contexto de fotogramas D3D11).
#[repr(C)]
// Solo se usa en Windows; se declara en todos los sistemas para comprobar su disposición.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) struct AVD3D11VAFramesContext {
    /// Matriz de texturas propia (o nulo para que la cree FFmpeg).
    pub texture: *mut c_void,
    /// `D3D11_BIND_FLAG` de las texturas.
    pub bind_flags: c_uint,
    /// `D3D11_RESOURCE_MISC_FLAG`.
    pub misc_flags: c_uint,
    /// Descriptores de cada textura del pool.
    pub texture_infos: *mut AVD3D11FrameDescriptor,
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use std::mem::{offset_of, size_of};

    use super::*;

    /// Tamaños y desplazamientos de `gcc` con las cabeceras de FFmpeg 8.1 en x86-64.
    #[test]
    fn layouts_match_the_c_headers() {
        assert_eq!(size_of::<AVDRMObjectDescriptor>(), 24);
        assert_eq!(offset_of!(AVDRMObjectDescriptor, format_modifier), 16);
        assert_eq!(size_of::<AVDRMPlaneDescriptor>(), 24);
        assert_eq!(size_of::<AVDRMLayerDescriptor>(), 104);
        assert_eq!(offset_of!(AVDRMLayerDescriptor, planes), 8);
        assert_eq!(size_of::<AVDRMFrameDescriptor>(), 528);
        assert_eq!(offset_of!(AVDRMFrameDescriptor, objects), 8);
        assert_eq!(offset_of!(AVDRMFrameDescriptor, nb_layers), 104);
        assert_eq!(offset_of!(AVDRMFrameDescriptor, layers), 112);

        assert_eq!(size_of::<AVVulkanDeviceQueueFamily>(), 16);
        assert_eq!(offset_of!(AVVulkanDeviceQueueFamily, video_caps), 12);
        assert_eq!(size_of::<AVVulkanDeviceContext>(), 1400);
        assert_eq!(offset_of!(AVVulkanDeviceContext, get_proc_addr), 8);
        assert_eq!(offset_of!(AVVulkanDeviceContext, inst), 16);
        assert_eq!(offset_of!(AVVulkanDeviceContext, device_features), 40);
        assert_eq!(
            offset_of!(AVVulkanDeviceContext, enabled_inst_extensions),
            280
        );
        assert_eq!(
            offset_of!(AVVulkanDeviceContext, nb_enabled_dev_extensions),
            304
        );
        assert_eq!(offset_of!(AVVulkanDeviceContext, queue_family_index), 308);
        assert_eq!(offset_of!(AVVulkanDeviceContext, nb_decode_queues), 344);
        assert_eq!(offset_of!(AVVulkanDeviceContext, lock_queue), 352);
        assert_eq!(offset_of!(AVVulkanDeviceContext, unlock_queue), 360);
        assert_eq!(offset_of!(AVVulkanDeviceContext, qf), 368);
        assert_eq!(offset_of!(AVVulkanDeviceContext, nb_qf), 1392);

        assert_eq!(size_of::<AVVulkanFramesContext>(), 144);
        assert_eq!(offset_of!(AVVulkanFramesContext, create_pnext), 8);
        assert_eq!(offset_of!(AVVulkanFramesContext, alloc_pnext), 16);
        assert_eq!(offset_of!(AVVulkanFramesContext, flags), 80);
        assert_eq!(offset_of!(AVVulkanFramesContext, img_flags), 84);
        assert_eq!(offset_of!(AVVulkanFramesContext, format), 88);
        assert_eq!(offset_of!(AVVulkanFramesContext, nb_layers), 120);
        assert_eq!(offset_of!(AVVulkanFramesContext, lock_frame), 128);
        assert_eq!(offset_of!(AVVulkanFramesContext, unlock_frame), 136);

        assert_eq!(size_of::<AVVkFrame>(), 504);
        assert_eq!(offset_of!(AVVkFrame, tiling), 64);
        assert_eq!(offset_of!(AVVkFrame, mem), 72);
        assert_eq!(offset_of!(AVVkFrame, size), 136);
        assert_eq!(offset_of!(AVVkFrame, flags), 200);
        assert_eq!(offset_of!(AVVkFrame, access), 204);
        assert_eq!(offset_of!(AVVkFrame, layout), 236);
        assert_eq!(offset_of!(AVVkFrame, sem), 272);
        assert_eq!(offset_of!(AVVkFrame, sem_value), 336);
        assert_eq!(offset_of!(AVVkFrame, internal), 400);
        assert_eq!(offset_of!(AVVkFrame, offset), 408);
        assert_eq!(offset_of!(AVVkFrame, queue_family), 472);

        // `hwcontext_d3d11va.h`: siete punteros y dos `UINT`; tres campos y un puntero.
        assert_eq!(size_of::<AVD3D11VADeviceContext>(), 64);
        assert_eq!(offset_of!(AVD3D11VADeviceContext, bind_flags), 56);
        assert_eq!(size_of::<AVD3D11FrameDescriptor>(), 16);
        assert_eq!(size_of::<AVD3D11VAFramesContext>(), 24);
        assert_eq!(offset_of!(AVD3D11VAFramesContext, texture_infos), 16);
    }
}
