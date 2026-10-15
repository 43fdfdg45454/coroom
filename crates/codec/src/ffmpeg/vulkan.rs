//! Vulkan de FFmpeg en la ruta sin copias.
//!
//! - [`device_from_shared`]: crea el `AVHWDeviceContext` Vulkan de FFmpeg sobre el `VkDevice` que
//!   creó `coroom-render` (`wgpu`) con las colas y extensiones de vídeo, para que las imágenes
//!   decodificadas se puedan muestrear en el presentador sin copia. FFmpeg no destruye un
//!   dispositivo que no creó él (no se pone `free` de dispositivo; solo el nuestro, que suelta las
//!   cadenas y estructuras que apunta el contexto).
//! - [`drm_modifiers`]: modificadores DRM que el dispositivo Vulkan del codificador puede importar
//!   y muestrear para `XRGB8888` (`VK_FORMAT_B8G8R8A8_UNORM`), los que la captura ofrece a
//!   PipeWire.
//! - [`VkFrameAccess`]: el `AVFrame` decodificado con el protocolo de sincronización de
//!   `hwcontext_vulkan.h` (`lock_frame`, semáforo de línea temporal, layout), que el presentador
//!   sigue al importar la imagen.

use std::ffi::{CStr, c_char, c_int};
use std::sync::{Arc, Mutex, PoisonError};

use ash::vk::{self, Handle};
use rsmpeg::avutil::{AVFrame, AVHWDeviceContext};
use rsmpeg::ffi;

use super::hw::{
    AVVkFrame, AVVulkanDeviceContext, AVVulkanDeviceQueueFamily, AVVulkanFramesContext,
};
use crate::gpu::{SharedVulkanDevice, VulkanFrameAccess, VulkanFrameState, VulkanFrameUpdate};

/// Lo que el contexto de FFmpeg apunta y debe vivir tanto como él.
struct SharedKeep {
    _shared: Arc<SharedVulkanDevice>,
    _instance_extensions: Vec<*const c_char>,
    _device_extensions: Vec<*const c_char>,
    _v11: Box<vk::PhysicalDeviceVulkan11Features<'static>>,
    _v12: Box<vk::PhysicalDeviceVulkan12Features<'static>>,
    _v13: Box<vk::PhysicalDeviceVulkan13Features<'static>>,
}

/// `AVHWDeviceContext.free`: suelta el [`SharedKeep`] de `user_opaque`.
unsafe extern "C" fn free_shared(ctx: *mut ffi::AVHWDeviceContext) {
    // SAFETY: FFmpeg llama a `free` una sola vez, al soltar la última referencia del contexto;
    // `user_opaque` es el `Box<SharedKeep>` que puso `device_from_shared` (o nulo).
    unsafe {
        let keep = (*ctx).user_opaque.cast::<SharedKeep>();
        if !keep.is_null() {
            drop(Box::from_raw(keep));
            (*ctx).user_opaque = std::ptr::null_mut();
        }
    }
}

/// Contexto de dispositivo Vulkan de FFmpeg sobre el `VkDevice` compartido del presentador.
pub(crate) fn device_from_shared(
    shared: &Arc<SharedVulkanDevice>,
) -> Result<AVHWDeviceContext, String> {
    if shared.get_instance_proc_addr == 0 || shared.device == 0 {
        return Err("el dispositivo Vulkan compartido no está completo".to_owned());
    }
    if !shared.features.timeline_semaphore || !shared.features.synchronization2 {
        return Err(
            "el dispositivo Vulkan compartido no tiene semáforos de línea temporal y \
             synchronization2, que FFmpeg necesita"
                .to_owned(),
        );
    }
    if shared.queue_families.is_empty() || shared.queue_families.len() > 64 {
        return Err("el dispositivo Vulkan compartido no tiene colas para FFmpeg".to_owned());
    }
    super::init_logging();
    let mut v13 = Box::new(vk::PhysicalDeviceVulkan13Features::default().synchronization2(true));
    let mut v12 = Box::new(vk::PhysicalDeviceVulkan12Features::default().timeline_semaphore(true));
    let mut v11 = Box::new(
        vk::PhysicalDeviceVulkan11Features::default()
            .sampler_ycbcr_conversion(shared.features.sampler_ycbcr_conversion),
    );
    v12.p_next = (&mut *v13 as *mut vk::PhysicalDeviceVulkan13Features<'static>).cast();
    v11.p_next = (&mut *v12 as *mut vk::PhysicalDeviceVulkan12Features<'static>).cast();
    let instance_extensions: Vec<*const c_char> = shared
        .instance_extensions
        .iter()
        .map(|name| name.as_ptr())
        .collect();
    let device_extensions: Vec<*const c_char> = shared
        .device_extensions
        .iter()
        .map(|name| name.as_ptr())
        .collect();

    let mut device = AVHWDeviceContext::alloc(ffi::AV_HWDEVICE_TYPE_VULKAN);
    // SAFETY: `get_instance_proc_addr` es la dirección de `vkGetInstanceProcAddr` del cargador de
    // `wgpu` (no nula, comprobado arriba), con la firma de `PFN_vkGetInstanceProcAddr`.
    let get_proc_addr = unsafe {
        std::mem::transmute::<usize, vk::PFN_vkGetInstanceProcAddr>(shared.get_instance_proc_addr)
    };
    // SAFETY: `device` es un `AVHWDeviceContext` Vulkan recién reservado y sin iniciar: su `hwctx`
    // es un `AVVulkanDeviceContext` puesto a cero (ver `hw.rs` para la disposición) que se rellena
    // antes de `av_hwdevice_ctx_init`, como pide `hwcontext_vulkan.h`. Las cadenas y estructuras
    // que se apuntan viven en el `SharedKeep`, que el propio contexto suelta en `free`; las
    // direcciones de las `Box` y de los búferes de los `Vec` no cambian al moverlos.
    unsafe {
        let ctx = (*device.as_ptr()).data.cast::<ffi::AVHWDeviceContext>();
        let hw = (*ctx).hwctx.cast::<AVVulkanDeviceContext>();
        (*hw).alloc = std::ptr::null();
        (*hw).get_proc_addr = Some(get_proc_addr);
        (*hw).inst = vk::Instance::from_raw(shared.instance);
        (*hw).phys_dev = vk::PhysicalDevice::from_raw(shared.physical_device);
        (*hw).act_dev = vk::Device::from_raw(shared.device);
        (*hw).device_features = vk::PhysicalDeviceFeatures2::default();
        (*hw).device_features.p_next =
            (&mut *v11 as *mut vk::PhysicalDeviceVulkan11Features<'static>).cast();
        (*hw).enabled_inst_extensions = instance_extensions.as_ptr();
        (*hw).nb_enabled_inst_extensions = c_int::try_from(instance_extensions.len()).unwrap_or(0);
        (*hw).enabled_dev_extensions = device_extensions.as_ptr();
        (*hw).nb_enabled_dev_extensions = c_int::try_from(device_extensions.len()).unwrap_or(0);
        // Campos obsoletos: sin familias fijas; manda `qf`.
        (*hw).queue_family_index = -1;
        (*hw).queue_family_tx_index = -1;
        (*hw).queue_family_comp_index = -1;
        (*hw).queue_family_encode_index = -1;
        (*hw).queue_family_decode_index = -1;
        (*hw).nb_graphics_queues = 0;
        (*hw).nb_tx_queues = 0;
        (*hw).nb_comp_queues = 0;
        (*hw).nb_encode_queues = 0;
        (*hw).nb_decode_queues = 0;
        for (slot, family) in (*hw).qf.iter_mut().zip(&shared.queue_families) {
            *slot = AVVulkanDeviceQueueFamily {
                idx: c_int::try_from(family.index).unwrap_or(0),
                num: c_int::try_from(family.count).unwrap_or(1),
                flags: vk::QueueFlags::from_raw(family.flags),
                video_caps: vk::VideoCodecOperationFlagsKHR::from_raw(family.video_caps),
            };
        }
        (*hw).nb_qf = c_int::try_from(shared.queue_families.len()).unwrap_or(0);
        let keep = Box::new(SharedKeep {
            _shared: Arc::clone(shared),
            _instance_extensions: instance_extensions,
            _device_extensions: device_extensions,
            _v11: v11,
            _v12: v12,
            _v13: v13,
        });
        (*ctx).user_opaque = Box::into_raw(keep).cast();
        (*ctx).free = Some(free_shared);
    }
    device.init().map_err(|err| {
        format!(
            "FFmpeg no aceptó el dispositivo Vulkan del presentador: {}",
            super::describe(&err)
        )
    })?;
    Ok(device)
}

/// `VkInstance` y `VkDevice` de un dispositivo Vulkan de FFmpeg.
pub(crate) fn handles(device: &AVHWDeviceContext) -> (u64, u64) {
    // SAFETY: `device` es un `AVHWDeviceContext` Vulkan iniciado, cuyo `hwctx` es un
    // `AVVulkanDeviceContext`; solo se leen dos manijas.
    unsafe {
        let ctx = (*device.as_ptr()).data.cast::<ffi::AVHWDeviceContext>();
        if (*ctx).type_ != ffi::AV_HWDEVICE_TYPE_VULKAN {
            return (0, 0);
        }
        let hw = (*ctx).hwctx.cast::<AVVulkanDeviceContext>();
        ((*hw).inst.as_raw(), (*hw).act_dev.as_raw())
    }
}

/// Si el dispositivo tiene activada la extensión `name`.
///
/// # Safety
///
/// `hw` debe ser el `AVVulkanDeviceContext` de un dispositivo iniciado.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
unsafe fn has_extension(hw: *const AVVulkanDeviceContext, name: &CStr) -> bool {
    // SAFETY: según el contrato, `enabled_dev_extensions` apunta a
    // `nb_enabled_dev_extensions` cadenas válidas (o es nulo con 0).
    unsafe {
        let count = usize::try_from((*hw).nb_enabled_dev_extensions).unwrap_or(0);
        let list = (*hw).enabled_dev_extensions;
        if list.is_null() {
            return false;
        }
        (0..count).any(|i| {
            let entry = *list.add(i);
            !entry.is_null() && CStr::from_ptr(entry) == name
        })
    }
}

/// Nombre del dispositivo Vulkan y versión de su driver (`NVIDIA GeForce RTX 5070 Ti /
/// 580.76.5`), la identidad de la GPU para la caché de calibración. `None` si no se puede crear
/// un dispositivo Vulkan de FFmpeg.
pub(crate) fn adapter_identity() -> Option<String> {
    let device = super::create_device(ffi::AV_HWDEVICE_TYPE_VULKAN).ok()?;
    // SAFETY: `device` es un `AVHWDeviceContext` Vulkan iniciado; su `get_proc_addr`, `inst` y
    // `phys_dev` son válidos mientras viva, y `vkGetPhysicalDeviceProperties` solo escribe en la
    // estructura local.
    unsafe {
        let ctx = (*device.as_ptr()).data.cast::<ffi::AVHWDeviceContext>();
        if (*ctx).type_ != ffi::AV_HWDEVICE_TYPE_VULKAN {
            return None;
        }
        let hw = (*ctx).hwctx.cast::<AVVulkanDeviceContext>();
        let get_proc_addr = (*hw).get_proc_addr?;
        let static_fn = ash::StaticFn {
            get_instance_proc_addr: get_proc_addr,
        };
        let instance = ash::Instance::load(&static_fn, (*hw).inst);
        let props = instance.get_physical_device_properties((*hw).phys_dev);
        let name = props
            .device_name_as_c_str()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|_| {
                format!(
                    "vendor {:#x} device {:#x}",
                    props.vendor_id, props.device_id
                )
            });
        Some(format!(
            "{name} / driver {} / vulkan {}.{}.{}",
            driver_version_text(props.vendor_id, props.driver_version),
            vk::api_version_major(props.api_version),
            vk::api_version_minor(props.api_version),
            vk::api_version_patch(props.api_version)
        ))
    }
}

/// `driverVersion` en texto, con la codificación de cada fabricante (NVIDIA: 10.8.8.6 bits; los
/// demás, la de Vulkan).
fn driver_version_text(vendor_id: u32, version: u32) -> String {
    if vendor_id == 0x10DE {
        format!(
            "{}.{}.{}.{}",
            version >> 22,
            (version >> 14) & 0xff,
            (version >> 6) & 0xff,
            version & 0x3f
        )
    } else {
        format!(
            "{}.{}.{}",
            vk::api_version_major(version),
            vk::api_version_minor(version),
            vk::api_version_patch(version)
        )
    }
}

/// Modificadores DRM que el dispositivo Vulkan `device` importa y muestrea para
/// `VK_FORMAT_B8G8R8A8_UNORM` (el `XRGB8888` de la captura), en el orden del driver y con el lineal
/// al final. Vacío si el dispositivo no tiene `VK_EXT_image_drm_format_modifier` y
/// `VK_EXT_external_memory_dma_buf` activadas.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn drm_modifiers(device: &AVHWDeviceContext) -> Vec<u64> {
    // SAFETY: `device` es un `AVHWDeviceContext` Vulkan iniciado (lo comprueba `type_`); su
    // `get_proc_addr`, `inst` y `phys_dev` son válidos mientras viva. Las funciones de instancia
    // se cargan con ese `vkGetInstanceProcAddr` y solo se hacen consultas de propiedades con
    // estructuras locales; la lista de propiedades se reserva con el tamaño que da la primera
    // llamada.
    unsafe {
        let ctx = (*device.as_ptr()).data.cast::<ffi::AVHWDeviceContext>();
        if (*ctx).type_ != ffi::AV_HWDEVICE_TYPE_VULKAN {
            return Vec::new();
        }
        let hw = (*ctx).hwctx.cast::<AVVulkanDeviceContext>();
        let Some(get_proc_addr) = (*hw).get_proc_addr else {
            return Vec::new();
        };
        if !has_extension(hw, ash::ext::image_drm_format_modifier::NAME)
            || !has_extension(hw, ash::ext::external_memory_dma_buf::NAME)
        {
            return Vec::new();
        }
        let static_fn = ash::StaticFn {
            get_instance_proc_addr: get_proc_addr,
        };
        let instance = ash::Instance::load(&static_fn, (*hw).inst);
        let format = vk::Format::B8G8R8A8_UNORM;
        let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
        {
            let mut props = vk::FormatProperties2::default().push_next(&mut list);
            instance.get_physical_device_format_properties2((*hw).phys_dev, format, &mut props);
        }
        let count = list.drm_format_modifier_count as usize;
        if count == 0 {
            return Vec::new();
        }
        let mut entries = vec![vk::DrmFormatModifierPropertiesEXT::default(); count];
        list.p_drm_format_modifier_properties = entries.as_mut_ptr();
        {
            let mut props = vk::FormatProperties2::default().push_next(&mut list);
            instance.get_physical_device_format_properties2((*hw).phys_dev, format, &mut props);
        }
        let filled = (list.drm_format_modifier_count as usize).min(count);
        let mut modifiers: Vec<u64> = entries[..filled]
            .iter()
            .filter(|entry| {
                entry.drm_format_modifier_plane_count == 1
                    && entry
                        .drm_format_modifier_tiling_features
                        .contains(vk::FormatFeatureFlags::SAMPLED_IMAGE)
            })
            .map(|entry| entry.drm_format_modifier)
            .collect();
        // El lineal, si está, al final: los demás son los nativos del compositor.
        if let Some(at) = modifiers
            .iter()
            .position(|m| *m == crate::gpu::DRM_FORMAT_MOD_LINEAR)
        {
            let linear = modifiers.remove(at);
            modifiers.push(linear);
        }
        modifiers
    }
}

/// `AVFrame` Vulkan decodificado, compartido con el presentador (ver [`VulkanFrameAccess`]).
pub(crate) struct VkFrameAccess {
    frame: AVFrame,
    locked: Mutex<bool>,
}

// SAFETY: el `AVFrame` solo se suelta (contadores de referencias atómicos de libavutil) y se
// lee o escribe su `AVVkFrame` con `lock_frame` tomado, que es el mecanismo de FFmpeg para
// compartirlo entre hilos; el `Mutex` serializa nuestras llamadas a `lock`/`unlock`.
unsafe impl Send for VkFrameAccess {}
// SAFETY: ver `Send`.
unsafe impl Sync for VkFrameAccess {}

impl std::fmt::Debug for VkFrameAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VkFrameAccess").finish_non_exhaustive()
    }
}

/// Descripción de una imagen Vulkan decodificada que se puede presentar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VkImageInfo {
    /// `VkFormat`.
    pub format: i32,
    /// Ancho y alto de la imagen reservada (pueden ser mayores que los visibles).
    pub extent: (u32, u32),
}

impl VkFrameAccess {
    /// Envuelve `frame` si es un fotograma Vulkan NV12 de una sola imagen multiplano (lo que
    /// importa el presentador). `None` si no.
    pub(crate) fn new(frame: AVFrame) -> Option<(Arc<Self>, VkImageInfo)> {
        if frame.format != ffi::AV_PIX_FMT_VULKAN || frame.hw_frames_ctx.is_null() {
            return None;
        }
        // SAFETY: un `AVFrame` de `AV_PIX_FMT_VULKAN` tiene `hw_frames_ctx` apuntando a un
        // `AVHWFramesContext` Vulkan (cuyo `hwctx` es un `AVVulkanFramesContext`) y `data[0]` a su
        // `AVVkFrame`; solo se leen campos.
        let info = unsafe {
            let fc = (*frame.hw_frames_ctx).data.cast::<ffi::AVHWFramesContext>();
            let vkf = frame.data[0].cast::<AVVkFrame>();
            if vkf.is_null() || (*fc).sw_format != ffi::AV_PIX_FMT_NV12 {
                return None;
            }
            if (*vkf).img[0].as_raw() == 0 || (*vkf).img[1].as_raw() != 0 {
                return None;
            }
            let vkfc = (*fc).hwctx.cast::<AVVulkanFramesContext>();
            let format = match (*vkfc).format[0] {
                vk::Format::UNDEFINED => vk::Format::G8_B8R8_2PLANE_420_UNORM,
                format => format,
            };
            if format != vk::Format::G8_B8R8_2PLANE_420_UNORM {
                return None;
            }
            VkImageInfo {
                format: format.as_raw(),
                extent: (
                    u32::try_from((*fc).width).ok()?,
                    u32::try_from((*fc).height).ok()?,
                ),
            }
        };
        Some((
            Arc::new(Self {
                frame,
                locked: Mutex::new(false),
            }),
            info,
        ))
    }

    /// `(AVHWFramesContext, AVVkFrame, AVVulkanFramesContext)` del fotograma.
    fn parts(
        &self,
    ) -> (
        *mut ffi::AVHWFramesContext,
        *mut AVVkFrame,
        *mut AVVulkanFramesContext,
    ) {
        // SAFETY: comprobado en `new`: fotograma Vulkan con contexto y `AVVkFrame`.
        unsafe {
            let fc = (*self.frame.hw_frames_ctx)
                .data
                .cast::<ffi::AVHWFramesContext>();
            (
                fc,
                self.frame.data[0].cast::<AVVkFrame>(),
                (*fc).hwctx.cast::<AVVulkanFramesContext>(),
            )
        }
    }
}

impl VulkanFrameAccess for VkFrameAccess {
    fn lock(&self) -> VulkanFrameState {
        let mut locked = self.locked.lock().unwrap_or_else(PoisonError::into_inner);
        let (fc, vkf, vkfc) = self.parts();
        // SAFETY: `lock_frame` es la función de FFmpeg (o la del usuario del contexto) para
        // bloquear el `AVVkFrame` de este contexto; se llama una vez por `unlock` (lo garantiza
        // `locked`). Con el bloqueo tomado se leen los campos de la primera imagen.
        unsafe {
            if !*locked {
                if let Some(lock_frame) = (*vkfc).lock_frame {
                    lock_frame(fc, vkf);
                }
                *locked = true;
            }
            VulkanFrameState {
                image: (*vkf).img[0].as_raw(),
                layout: (*vkf).layout[0].as_raw(),
                access: (*vkf).access[0].as_raw(),
                semaphore: (*vkf).sem[0].as_raw(),
                semaphore_value: (*vkf).sem_value[0],
                queue_family: (*vkf).queue_family[0],
            }
        }
    }

    fn unlock(&self, update: Option<VulkanFrameUpdate>) {
        let mut locked = self.locked.lock().unwrap_or_else(PoisonError::into_inner);
        if !*locked {
            return;
        }
        let (fc, vkf, vkfc) = self.parts();
        // SAFETY: el fotograma está bloqueado por nosotros (`locked`); se anotan el layout, el
        // acceso, la familia y el valor del semáforo que señalarán nuestros comandos, como pide
        // `hwcontext_vulkan.h`, y se desbloquea.
        unsafe {
            if let Some(update) = update {
                (*vkf).layout[0] = vk::ImageLayout::from_raw(update.layout);
                (*vkf).access[0] = vk::AccessFlags::from_raw(update.access);
                (*vkf).sem_value[0] = update.semaphore_value;
                (*vkf).queue_family[0] = update.queue_family;
            }
            if let Some(unlock_frame) = (*vkfc).unlock_frame {
                unlock_frame(fc, vkf);
            }
        }
        *locked = false;
    }
}

impl Drop for VkFrameAccess {
    fn drop(&mut self) {
        // Un bloqueo olvidado (error entre `lock` y `unlock`) no puede quedarse tomado.
        self.unlock(None);
    }
}

/// Espera en CPU a que la GPU termine con una imagen de FFmpeg (`vkWaitSemaphores` sobre su
/// semáforo de línea temporal). La usa el decodificador para que la marca `decodificado` incluya
/// el trabajo de la GPU, también cuando la imagen no se baja a CPU.
pub(crate) struct SemaphoreWaiter {
    device: vk::Device,
    wait: vk::PFN_vkWaitSemaphores,
}

impl SemaphoreWaiter {
    /// Para el dispositivo Vulkan `device` (`None` si no es Vulkan o falta la función).
    pub(crate) fn new(device: &AVHWDeviceContext) -> Option<Self> {
        // SAFETY: `device` es un `AVHWDeviceContext` iniciado; si es Vulkan, su `hwctx` tiene un
        // `vkGetInstanceProcAddr`, la instancia y el dispositivo válidos mientras viva. Se carga
        // `vkGetDeviceProcAddr` y con él `vkWaitSemaphores` (Vulkan 1.2, obligatoria en FFmpeg).
        unsafe {
            let ctx = (*device.as_ptr()).data.cast::<ffi::AVHWDeviceContext>();
            if (*ctx).type_ != ffi::AV_HWDEVICE_TYPE_VULKAN {
                return None;
            }
            let hw = (*ctx).hwctx.cast::<AVVulkanDeviceContext>();
            let get_instance_proc_addr = (*hw).get_proc_addr?;
            let get_device_proc_addr =
                get_instance_proc_addr((*hw).inst, c"vkGetDeviceProcAddr".as_ptr())?;
            let get_device_proc_addr: vk::PFN_vkGetDeviceProcAddr =
                std::mem::transmute(get_device_proc_addr);
            let wait = get_device_proc_addr((*hw).act_dev, c"vkWaitSemaphores".as_ptr())?;
            Some(Self {
                device: (*hw).act_dev,
                wait: std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkWaitSemaphores>(
                    wait,
                ),
            })
        }
    }

    /// Espera como mucho `timeout_ns` a que `semaphore` llegue a `value`. Devuelve si llegó.
    pub(crate) fn wait(&self, semaphore: u64, value: u64, timeout_ns: u64) -> bool {
        if semaphore == 0 {
            return true;
        }
        let semaphores = [vk::Semaphore::from_raw(semaphore)];
        let values = [value];
        let info = vk::SemaphoreWaitInfo::default()
            .semaphores(&semaphores)
            .values(&values);
        // SAFETY: `wait` es `vkWaitSemaphores` del dispositivo que creó el semáforo; la
        // estructura y sus listas viven durante la llamada.
        unsafe { (self.wait)(self.device, &info, timeout_ns) == vk::Result::SUCCESS }
    }
}

/// Puntero opaco de un `AVBufferRef` (para comparar contextos).
pub(crate) fn buffer_key(buffer: *const ffi::AVBufferRef) -> usize {
    if buffer.is_null() {
        0
    } else {
        // SAFETY: `buffer` es una referencia válida; solo se lee `data`.
        unsafe { (*buffer).data as usize }
    }
}
