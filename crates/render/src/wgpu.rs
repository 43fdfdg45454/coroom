//! Dispositivo `wgpu` de la ventana ([`GpuContext`]) y superficie.
//!
//! Prefiere un adaptador de hardware con backend Vulkan y acepta cualquiera como respaldo
//! ([`select_adapter`]); presenta en modo `Immediate` si la superficie lo admite, si no `Mailbox` y
//! si no `Fifo`, con un solo fotograma en cola ([`GpuContext::surface_setup`]). El
//! mismo dispositivo dibuja la interfaz (`iced_wgpu`) y el vídeo ([`crate::layer::VideoLayer`]),
//! en la misma pasada.
//!
//! **Imágenes Vulkan sin copia.** Con el backend Vulkan, si el adaptador tiene una familia
//! de colas de decodificación de vídeo (`VK_KHR_video_decode_queue` con H.264, HEVC o AV1), otra
//! de cómputo o transferencia distinta de la de `wgpu` y Vulkan 1.3, el dispositivo se crea con
//! `wgpu-hal` (`open_with_callback`) añadiendo esas colas, las extensiones de vídeo (y las de
//! memoria externa y DMA-BUF si están) y `synchronization2`, y se describe en un
//! [`VulkanDeviceShare`] para que FFmpeg decodifique en él. Cada [`PresentImage::Vulkan`] se
//! importa con `texture_from_raw` (sin tomar posesión: la imagen es de FFmpeg y vive lo que el
//! `wgpu::Texture`), se pasa a `SHADER_READ_ONLY_OPTIMAL` con una barrera que se envía esperando
//! al semáforo de línea temporal de FFmpeg en su valor, la entrega siguiente (la que dibuja la
//! interfaz) lo señala al valor siguiente, y se anota ese valor y el layout en el `AVVkFrame`
//! (bloqueado mientras tanto, como pide `hwcontext_vulkan.h`): [`VideoImporter`]. Si algo falta,
//! el dispositivo es el de siempre y el decodificador baja la imagen a CPU. `COROOM_ZERO_COPY=0`
//! lo desactiva.
//!
//! Todo el `unsafe` del render está en este módulo, con su `// SAFETY:`.

// Interoperabilidad con `wgpu-hal` y Vulkan crudo (ver `CLAUDE.md`: backend nativo del render).
#![allow(unsafe_code)]

use std::ffi::{CStr, CString};
use std::fmt;
use std::sync::Arc;

use ash::vk::{self, Handle};

use crate::video::VideoPipeline;
use crate::{
    ExternalImageState, ExternalImageUpdate, PresentMode, RenderError, SharedQueueFamily,
    VulkanDeviceShare, VulkanFrame,
};

/// Crea una instancia de `wgpu` con los backends principales (Vulkan, DX12, Metal), que se pueden
/// cambiar con la variable `WGPU_BACKEND`. `display` es la conexión con el compositor (en `winit`,
/// el `OwnedDisplayHandle` del bucle de eventos).
pub fn create_instance(
    display: Option<Box<dyn ::wgpu::wgt::WgpuHasDisplayHandle>>,
) -> ::wgpu::Instance {
    let base = match display {
        Some(display) => ::wgpu::InstanceDescriptor::new_with_display_handle(display),
        None => ::wgpu::InstanceDescriptor::new_without_display_handle(),
    };
    ::wgpu::Instance::new(
        ::wgpu::InstanceDescriptor {
            backends: ::wgpu::Backends::PRIMARY,
            ..base
        }
        .with_env(),
    )
}

/// Orden de preferencia: hardware antes que CPU, Vulkan antes que el resto y GPU dedicada antes que
/// integrada.
fn adapter_rank(info: &::wgpu::AdapterInfo) -> (bool, bool, u8) {
    let device = match info.device_type {
        ::wgpu::DeviceType::DiscreteGpu => 0,
        ::wgpu::DeviceType::IntegratedGpu => 1,
        ::wgpu::DeviceType::VirtualGpu => 2,
        ::wgpu::DeviceType::Other => 3,
        ::wgpu::DeviceType::Cpu => 4,
    };
    (
        info.device_type == ::wgpu::DeviceType::Cpu,
        info.backend != ::wgpu::Backend::Vulkan,
        device,
    )
}

/// Elige el adaptador: Vulkan de hardware si existe, cualquiera compatible con `surface` si no.
pub async fn select_adapter(
    instance: &::wgpu::Instance,
    surface: Option<&::wgpu::Surface<'_>>,
) -> Result<::wgpu::Adapter, RenderError> {
    let best = instance
        .enumerate_adapters(::wgpu::Backends::all())
        .await
        .into_iter()
        .filter(|adapter| surface.is_none_or(|surface| adapter.is_surface_supported(surface)))
        .min_by_key(|adapter| adapter_rank(&adapter.get_info()));
    if let Some(adapter) = best {
        return Ok(adapter);
    }
    instance
        .request_adapter(&::wgpu::RequestAdapterOptions {
            power_preference: ::wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: surface,
            ..Default::default()
        })
        .await
        .map_err(|err| RenderError::Unsupported(format!("no hay adaptador gráfico: {err}")))
}

/// Características que se piden si el adaptador las tiene: texturas `R16Unorm`/`Rg16Unorm` para
/// el vídeo de 10 bits.
pub fn optional_features(adapter: &::wgpu::Adapter) -> ::wgpu::Features {
    adapter.features() & ::wgpu::Features::TEXTURE_FORMAT_16BIT_NORM
}

/// Pide un dispositivo con los límites por defecto.
pub async fn request_device(
    adapter: &::wgpu::Adapter,
) -> Result<(::wgpu::Device, ::wgpu::Queue), RenderError> {
    adapter
        .request_device(&::wgpu::DeviceDescriptor {
            label: Some("coroom"),
            required_features: optional_features(adapter),
            required_limits: ::wgpu::Limits::default().using_resolution(adapter.limits()),
            ..Default::default()
        })
        .await
        .map_err(|err| RenderError::Backend(format!("no se pudo abrir el dispositivo: {err}")))
}

/// `COROOM_ZERO_COPY=0` (o `off`, `no`, `false`) desactiva el dispositivo compartido.
fn zero_copy_disabled() -> bool {
    std::env::var("COROOM_ZERO_COPY").is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "off" | "no" | "false"
        )
    })
}

/// Familia de colas de un dispositivo físico.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueFamilyInfo {
    /// Índice.
    pub index: u32,
    /// `VkQueueFlags`.
    pub flags: u32,
    /// Colas.
    pub count: u32,
    /// `VkVideoCodecOperationFlagsKHR`.
    pub video_caps: u32,
}

/// Familias que se le dan al decodificador: `(decodificación, cómputo, transferencia)`, ninguna la
/// de gráficos de `wgpu` (`graphics`). `None` si falta la de decodificación o una de cómputo o
/// transferencia que no sea la de `wgpu` (entonces FFmpeg enviaría trabajo a la cola de `wgpu`
/// desde otro hilo).
pub fn plan_queue_families(
    families: &[QueueFamilyInfo],
    graphics: u32,
) -> Option<Vec<SharedQueueFamily>> {
    let has = |family: &QueueFamilyInfo, flag: vk::QueueFlags| {
        vk::QueueFlags::from_raw(family.flags).contains(flag)
    };
    let others = || {
        families
            .iter()
            .filter(|f| f.index != graphics && f.count > 0)
    };
    let decode =
        others().find(|f| has(f, vk::QueueFlags::VIDEO_DECODE_KHR) && f.video_caps != 0)?;
    let compute = others().find(|f| f.index != decode.index && has(f, vk::QueueFlags::COMPUTE));
    let transfer = others().find(|f| {
        f.index != decode.index
            && Some(f.index) != compute.map(|c| c.index)
            && has(f, vk::QueueFlags::TRANSFER)
    });
    let transfer_covered = transfer.is_some()
        || compute.is_some_and(|c| has(c, vk::QueueFlags::TRANSFER))
        || has(decode, vk::QueueFlags::TRANSFER);
    if compute.is_none() || !transfer_covered {
        return None;
    }
    let entry = |family: &QueueFamilyInfo| SharedQueueFamily {
        index: family.index,
        count: 1,
        flags: family.flags,
        video_caps: family.video_caps,
    };
    let mut plan: Vec<SharedQueueFamily> = [Some(decode), compute, transfer]
        .into_iter()
        .flatten()
        .map(entry)
        .collect();
    // La de gráficos, la última: solo para que FFmpeg cree las imágenes compartidas con ella.
    if let Some(graphics) = families.iter().find(|f| f.index == graphics) {
        plan.push(entry(graphics));
    }
    Some(plan)
}

/// Lo que hace falta para importar las imágenes del decodificador en este dispositivo.
struct VulkanImporter {
    device: ash::Device,
    graphics_family: u32,
}

impl fmt::Debug for VulkanImporter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VulkanImporter")
            .field(
                "device",
                &format_args!("{:#x}", self.device.handle().as_raw()),
            )
            .field("graphics_family", &self.graphics_family)
            .finish()
    }
}

/// Familias de colas (con sus capacidades de vídeo) de `physical_device`.
///
/// # Safety
///
/// `instance` y `physical_device` deben ser válidos y la instancia de Vulkan ≥ 1.1.
unsafe fn queue_families(
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
) -> Vec<QueueFamilyInfo> {
    // SAFETY: consulta de propiedades con estructuras locales del tamaño que da la primera llamada;
    // la cadena `pNext` de cada entrada apunta a su `QueueFamilyVideoPropertiesKHR`, que vive
    // hasta después de leerla.
    unsafe {
        let count = instance.get_physical_device_queue_family_properties2_len(physical_device);
        let mut video = vec![vk::QueueFamilyVideoPropertiesKHR::default(); count];
        let basics: Vec<(vk::QueueFlags, u32)> = {
            let mut properties: Vec<vk::QueueFamilyProperties2> = video
                .iter_mut()
                .map(|video| vk::QueueFamilyProperties2::default().push_next(video))
                .collect();
            instance.get_physical_device_queue_family_properties2(physical_device, &mut properties);
            properties
                .iter()
                .map(|p| {
                    (
                        p.queue_family_properties.queue_flags,
                        p.queue_family_properties.queue_count,
                    )
                })
                .collect()
        };
        basics
            .into_iter()
            .zip(&video)
            .enumerate()
            .map(|(index, ((flags, count), video))| QueueFamilyInfo {
                index: index as u32,
                flags: flags.as_raw(),
                count,
                video_caps: if flags.contains(vk::QueueFlags::VIDEO_DECODE_KHR) {
                    video.video_codec_operations.as_raw()
                } else {
                    0
                },
            })
            .collect()
    }
}

/// Lo que hay que pedir al abrir el dispositivo para compartirlo con el decodificador.
/// Extensiones de mantenimiento de Vulkan Video (`ash` 0.38 solo trae la primera).
const VIDEO_MAINTENANCE: [&CStr; 2] = [
    ash::khr::video_maintenance1::NAME,
    c"VK_KHR_video_maintenance2",
];

struct VideoPlan {
    features: ::wgpu::Features,
    limits: ::wgpu::Limits,
    extensions: Vec<&'static CStr>,
    families: Vec<SharedQueueFamily>,
    codecs: Vec<&'static str>,
    /// Extensiones de mantenimiento de vídeo que el driver ofrece y este dispositivo no activa:
    /// FFmpeg las activa en su propio dispositivo y, sin ellas, decodifica AV1 por otro camino.
    unused_video_extensions: Vec<String>,
}

/// Comprueba si `adapter` puede compartir su dispositivo con el decodificador y qué pedir; si no,
/// por qué.
fn video_plan(adapter: &::wgpu::Adapter) -> Result<VideoPlan, String> {
    if zero_copy_disabled() {
        return Err("desactivada con COROOM_ZERO_COPY=0".into());
    }
    if adapter.get_info().backend != ::wgpu::Backend::Vulkan {
        return Err("el adaptador no usa Vulkan".into());
    }
    let features = adapter.features() & ::wgpu::Features::TEXTURE_FORMAT_NV12;
    if features.is_empty() {
        return Err("el adaptador no muestrea texturas NV12".into());
    }
    let features = features | optional_features(adapter);
    let limits = ::wgpu::Limits::default().using_resolution(adapter.limits());
    // SAFETY: solo se usa el adaptador de `wgpu-hal` para consultar su dispositivo físico.
    let hal_adapter = unsafe { adapter.as_hal::<::wgpu::hal::api::Vulkan>() }
        .ok_or_else(|| "el adaptador no es de Vulkan".to_owned())?;
    let instance = hal_adapter.shared_instance().raw_instance();
    let physical_device = hal_adapter.raw_physical_device();
    // SAFETY: consultas de propiedades del dispositivo físico de este adaptador.
    let (api_version, available, sync2) = unsafe {
        let properties = instance.get_physical_device_properties(physical_device);
        let available: Vec<CString> = instance
            .enumerate_device_extension_properties(physical_device)
            .map_err(|err| format!("no se pudieron leer las extensiones: {err}"))?
            .iter()
            .filter_map(|ext| ext.extension_name_as_c_str().ok().map(CStr::to_owned))
            .collect();
        let mut v13 = vk::PhysicalDeviceVulkan13Features::default();
        if properties.api_version >= vk::API_VERSION_1_3 {
            let mut features2 = vk::PhysicalDeviceFeatures2::default().push_next(&mut v13);
            instance.get_physical_device_features2(physical_device, &mut features2);
        }
        (
            properties.api_version,
            available,
            v13.synchronization2 == vk::TRUE,
        )
    };
    if api_version < vk::API_VERSION_1_3 || !sync2 {
        return Err("el dispositivo no tiene Vulkan 1.3 con synchronization2".into());
    }
    let has = |name: &CStr| available.iter().any(|ext| ext.as_c_str() == name);
    if !has(ash::khr::video_queue::NAME) || !has(ash::khr::video_decode_queue::NAME) {
        return Err("el driver no expone VK_KHR_video_decode_queue".into());
    }
    let codecs: Vec<(&'static CStr, &'static str)> = [
        (ash::khr::video_decode_h264::NAME, "H.264"),
        (ash::khr::video_decode_h265::NAME, "HEVC"),
        (ash::khr::video_decode_av1::NAME, "AV1"),
    ]
    .into_iter()
    .filter(|(name, _)| has(name))
    .collect();
    if codecs.is_empty() {
        return Err("el driver no decodifica H.264, HEVC ni AV1 con Vulkan Video".into());
    }
    let mut extensions: Vec<&'static CStr> = vec![
        ash::khr::video_queue::NAME,
        ash::khr::video_decode_queue::NAME,
    ];
    extensions.extend(codecs.iter().map(|(name, _)| *name));
    // Memoria y semáforos externos: para mapear VA-API → Vulkan con DMA-BUF (Linux).
    extensions.extend(
        [
            ash::khr::external_memory_fd::NAME,
            ash::ext::external_memory_dma_buf::NAME,
            ash::ext::image_drm_format_modifier::NAME,
            ash::khr::external_semaphore_fd::NAME,
        ]
        .into_iter()
        .filter(|name| has(name)),
    );
    // SAFETY: `instance` y `physical_device` son los del adaptador, con Vulkan ≥ 1.3.
    let families = unsafe { queue_families(instance, physical_device) };
    // `wgpu-hal` usa siempre la familia 0 (se comprueba con el dispositivo abierto).
    let families = plan_queue_families(&families, 0).ok_or_else(|| {
        "no hay una familia de decodificación y otra de cómputo aparte de la de wgpu".to_owned()
    })?;
    let unused_video_extensions = VIDEO_MAINTENANCE
        .iter()
        .filter(|name| has(name) && !extensions.contains(name))
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    Ok(VideoPlan {
        features,
        limits,
        extensions,
        families,
        codecs: codecs.into_iter().map(|(_, label)| label).collect(),
        unused_video_extensions,
    })
}

/// Si el presentador podrá importar las imágenes del decodificador sin copia en el adaptador que
/// elegiría, y por qué no; para `coroom doctor`.
pub fn probe_zero_copy() -> String {
    let instance = create_instance(None);
    match pollster::block_on(select_adapter(&instance, None)) {
        Ok(adapter) => match video_plan(&adapter) {
            Ok(plan) => format!(
                "dispositivo Vulkan compartido con el decodificador (Vulkan Video: {}; colas {:?}; \
                 ofrecidas sin activar: {})",
                plan.codecs.join(", "),
                plan.families.iter().map(|f| f.index).collect::<Vec<_>>(),
                unused_label(&plan.unused_video_extensions)
            ),
            Err(reason) => format!("sin importación de imágenes Vulkan: {reason}"),
        },
        Err(err) => format!("sin adaptador gráfico ({err})"),
    }
}

/// Las extensiones ofrecidas sin activar, o «ninguna».
pub fn unused_label(names: &[String]) -> String {
    if names.is_empty() {
        "ninguna".to_owned()
    } else {
        names.join(", ")
    }
}

/// Abre el dispositivo de `adapter` con lo necesario para que FFmpeg decodifique en él (ver el
/// módulo). `None` si el adaptador no lo permite: entonces se abre el de siempre.
fn open_video_device(
    adapter: &::wgpu::Adapter,
) -> Option<(
    ::wgpu::Device,
    ::wgpu::Queue,
    VulkanDeviceShare,
    VulkanImporter,
)> {
    let VideoPlan {
        features,
        limits,
        extensions,
        families: plan,
        unused_video_extensions,
        ..
    } = video_plan(adapter).ok()?;
    let extra_families: Vec<u32> = plan
        .iter()
        .map(|family| family.index)
        .filter(|index| *index != 0)
        .collect();
    // SAFETY: solo se usa el adaptador de `wgpu-hal` para abrir el dispositivo con
    // `open_with_callback`, que se entrega enseguida a `create_device_from_hal` del mismo
    // adaptador.
    let hal_adapter = unsafe { adapter.as_hal::<::wgpu::hal::api::Vulkan>() }?;

    let mut sync2 = vk::PhysicalDeviceSynchronization2Features::default().synchronization2(true);
    let priorities: &[f32] = &[1.0];
    let callback_extensions = extensions.clone();
    let sync2_ref = &mut sync2;
    let callback: Box<::wgpu::hal::vulkan::CreateDeviceCallback<'_>> = Box::new(move |args| {
        for name in callback_extensions {
            if !args.extensions.contains(&name) {
                args.extensions.push(name);
            }
        }
        for family in extra_families {
            args.queue_create_infos.push(
                vk::DeviceQueueCreateInfo::default()
                    .queue_family_index(family)
                    .queue_priorities(priorities),
            );
        }
        *args.create_info = std::mem::take(args.create_info).push_next(sync2_ref);
    });
    // SAFETY: el callback solo añade extensiones que el dispositivo admite (comprobado arriba),
    // colas de familias que existen y la característica `synchronization2`, que admite; no quita
    // nada de lo que pide `wgpu`.
    let open = unsafe {
        hal_adapter.open_with_callback(
            features,
            &limits,
            &::wgpu::MemoryHints::Performance,
            Some(callback),
        )
    }
    .ok()?;
    drop(hal_adapter);
    // SAFETY: `open` sale de este mismo adaptador y se pide exactamente lo que se abrió.
    let (device, queue) = unsafe {
        adapter.create_device_from_hal(
            open,
            &::wgpu::DeviceDescriptor {
                label: Some("coroom"),
                required_features: features,
                required_limits: limits,
                memory_hints: ::wgpu::MemoryHints::Performance,
                ..Default::default()
            },
        )
    }
    .ok()?;
    // SAFETY: solo se leen manijas y listas del dispositivo de `wgpu-hal`, que vive mientras viva
    // `device` (el `keepalive` del reparto).
    let (share, importer) = unsafe {
        let hal_device = device.as_hal::<::wgpu::hal::api::Vulkan>()?;
        if hal_device.queue_family_index() != 0 {
            return None;
        }
        let shared = hal_device.shared_instance();
        let share = VulkanDeviceShare {
            get_instance_proc_addr: shared.entry().static_fn().get_instance_proc_addr as usize,
            instance: shared.raw_instance().handle().as_raw(),
            physical_device: hal_device.raw_physical_device().as_raw(),
            device: hal_device.raw_device().handle().as_raw(),
            instance_extensions: shared
                .extensions()
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            device_extensions: hal_device
                .enabled_device_extensions()
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            queue_families: plan,
            timeline_semaphore: true,
            synchronization2: true,
            sampler_ycbcr_conversion: false,
            unused_video_extensions: unused_video_extensions.clone(),
            keepalive: Arc::new(device.clone()),
        };
        let importer = VulkanImporter {
            device: hal_device.raw_device().clone(),
            graphics_family: hal_device.queue_family_index(),
        };
        (share, importer)
    };
    Some((device, queue, share, importer))
}

/// Descripción legible de un adaptador.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterSummary {
    /// Nombre del dispositivo.
    pub name: String,
    /// Backend (`Vulkan`, `Dx12`, …).
    pub backend: String,
    /// Tipo de dispositivo.
    pub device_type: String,
    /// Controlador e información del controlador.
    pub driver: String,
}

impl AdapterSummary {
    /// Resume la información de un adaptador.
    pub fn from_info(info: &::wgpu::AdapterInfo) -> Self {
        let device_type = match info.device_type {
            ::wgpu::DeviceType::DiscreteGpu => "GPU dedicada",
            ::wgpu::DeviceType::IntegratedGpu => "GPU integrada",
            ::wgpu::DeviceType::VirtualGpu => "GPU virtual",
            ::wgpu::DeviceType::Cpu => "CPU (software)",
            ::wgpu::DeviceType::Other => "otro",
        };
        let driver = [info.driver.as_str(), info.driver_info.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            name: info.name.clone(),
            backend: format!("{:?}", info.backend),
            device_type: device_type.to_owned(),
            driver,
        }
    }
}

impl fmt::Display for AdapterSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}, {}", self.name, self.backend, self.device_type)?;
        if !self.driver.is_empty() {
            write!(f, ", {}", self.driver)?;
        }
        f.write_str(")")
    }
}

/// Adaptador que elegiría el presentador, sin ventana; para `coroom doctor`.
pub fn probe_adapter() -> Result<AdapterSummary, RenderError> {
    let instance = create_instance(None);
    let adapter = pollster::block_on(select_adapter(&instance, None))?;
    Ok(AdapterSummary::from_info(&adapter.get_info()))
}

/// Modo de presentación de menor latencia que admite la superficie.
pub fn choose_present_mode(available: &[::wgpu::PresentMode]) -> PresentMode {
    if available.contains(&::wgpu::PresentMode::Immediate) {
        PresentMode::Immediate
    } else if available.contains(&::wgpu::PresentMode::Mailbox) {
        PresentMode::Mailbox
    } else {
        PresentMode::Fifo
    }
}

fn to_wgpu_mode(mode: PresentMode) -> ::wgpu::PresentMode {
    match mode {
        PresentMode::Immediate => ::wgpu::PresentMode::Immediate,
        PresentMode::Mailbox => ::wgpu::PresentMode::Mailbox,
        PresentMode::Fifo => ::wgpu::PresentMode::Fifo,
    }
}

/// Dispositivo `wgpu` de la ventana, compartido por la interfaz (`iced_wgpu`) y el vídeo
/// ([`crate::layer::VideoLayer`]): un solo `wgpu::Device` para todo.
///
/// Con el backend Vulkan y colas de vídeo, el dispositivo se abre para que el decodificador
/// decodifique en él ([`GpuContext::vulkan_share`]) y las imágenes se importan sin copia con
/// [`GpuContext::importer`]; si no, es el dispositivo de siempre y el vídeo llega en CPU.
pub struct GpuContext {
    /// Adaptador elegido ([`select_adapter`]).
    pub adapter: ::wgpu::Adapter,
    /// Dispositivo.
    pub device: ::wgpu::Device,
    /// Cola.
    pub queue: ::wgpu::Queue,
    summary: AdapterSummary,
    share: Option<Arc<VulkanDeviceShare>>,
    importer: Option<VideoImporter>,
}

impl fmt::Debug for GpuContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GpuContext")
            .field("adapter", &self.summary)
            .field("vulkan_share", &self.share.is_some())
            .finish_non_exhaustive()
    }
}

impl GpuContext {
    /// Abre el dispositivo del mejor adaptador compatible con `surface` (o sin superficie, para
    /// dibujar fuera de pantalla).
    pub fn new(
        instance: &::wgpu::Instance,
        surface: Option<&::wgpu::Surface<'_>>,
    ) -> Result<Self, RenderError> {
        let adapter = pollster::block_on(select_adapter(instance, surface))?;
        let (device, queue, share, importer) = match open_video_device(&adapter) {
            Some((device, queue, share, importer)) => (
                device,
                queue,
                Some(Arc::new(share)),
                Some(VideoImporter(Arc::new(importer))),
            ),
            None => {
                let (device, queue) = pollster::block_on(request_device(&adapter))?;
                (device, queue, None, None)
            }
        };
        Ok(Self {
            summary: AdapterSummary::from_info(&adapter.get_info()),
            adapter,
            device,
            queue,
            share,
            importer,
        })
    }

    /// Adaptador en uso.
    pub fn adapter_summary(&self) -> &AdapterSummary {
        &self.summary
    }

    /// Dispositivo Vulkan para que el decodificador lo use, si se pudo crear con colas
    /// de vídeo.
    pub fn vulkan_share(&self) -> Option<Arc<VulkanDeviceShare>> {
        self.share.clone()
    }

    /// Importador de las imágenes Vulkan del decodificador, si el dispositivo es compartido.
    pub fn importer(&self) -> Option<VideoImporter> {
        self.importer.clone()
    }

    /// Configuración de `surface` para este dispositivo: formato sin sRGB (la interfaz y el
    /// vídeo escriben valores en espacio gamma), el modo de menor latencia y un solo fotograma en
    /// cola.
    pub fn surface_setup(
        &self,
        surface: &::wgpu::Surface<'_>,
        width: u32,
        height: u32,
    ) -> Result<SurfaceSetup, RenderError> {
        self.surface_setup_with(surface, width, height, false)
    }

    /// Si la superficie admite la salida HDR de [`crate::hdr`] (`Rgba16Float` con
    /// `ExtendedSrgbLinear`).
    pub fn hdr_available(&self, surface: &::wgpu::Surface<'_>) -> bool {
        surface
            .get_capabilities(&self.adapter)
            .color_spaces(crate::hdr::HDR_FORMAT)
            .contains(::wgpu::SurfaceColorSpaces::EXTENDED_SRGB_LINEAR)
    }

    /// Como [`GpuContext::surface_setup`] y, con `hdr` y si la superficie lo admite
    /// ([`GpuContext::hdr_available`]), en HDR: superficie `Rgba16Float` con
    /// `ExtendedSrgbLinear` y la interfaz dibujada en la textura intermedia de
    /// [`crate::hdr::HdrOutput`] ([`SurfaceSetup::hdr`]).
    pub fn surface_setup_with(
        &self,
        surface: &::wgpu::Surface<'_>,
        width: u32,
        height: u32,
        hdr: bool,
    ) -> Result<SurfaceSetup, RenderError> {
        let caps = surface.get_capabilities(&self.adapter);
        if hdr && self.hdr_available(surface) {
            let info = surface.display_hdr_info(&self.adapter);
            let mode = choose_present_mode(&caps.present_modes);
            let alpha_mode = caps
                .alpha_modes
                .first()
                .copied()
                .unwrap_or(::wgpu::CompositeAlphaMode::Auto);
            return Ok(SurfaceSetup {
                config: ::wgpu::SurfaceConfiguration {
                    usage: ::wgpu::TextureUsages::RENDER_ATTACHMENT,
                    format: crate::hdr::HDR_FORMAT,
                    color_space: ::wgpu::SurfaceColorSpace::ExtendedSrgbLinear,
                    width: width.max(1),
                    height: height.max(1),
                    present_mode: to_wgpu_mode(mode),
                    desired_maximum_frame_latency: 1,
                    alpha_mode,
                    view_formats: Vec::new(),
                },
                render_format: crate::hdr::HDR_FORMAT,
                mode,
                hdr: Some(HdrSurface {
                    sdr_white_scale: crate::hdr::sdr_white_scale(&info),
                    headroom: crate::hdr::headroom(&info),
                }),
            });
        }
        let Some(&format) = caps
            .formats
            .iter()
            .find(|format| !format.is_srgb())
            .or_else(|| caps.formats.first())
        else {
            return Err(RenderError::Unsupported(
                "la superficie no ofrece ningún formato".into(),
            ));
        };
        let render_format = format.remove_srgb_suffix();
        let mode = choose_present_mode(&caps.present_modes);
        let alpha_mode = caps
            .alpha_modes
            .first()
            .copied()
            .unwrap_or(::wgpu::CompositeAlphaMode::Auto);
        Ok(SurfaceSetup {
            config: ::wgpu::SurfaceConfiguration {
                usage: ::wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                color_space: ::wgpu::SurfaceColorSpace::Auto,
                width: width.max(1),
                height: height.max(1),
                present_mode: to_wgpu_mode(mode),
                desired_maximum_frame_latency: 1,
                alpha_mode,
                view_formats: if render_format == format {
                    Vec::new()
                } else {
                    vec![render_format]
                },
            },
            render_format,
            mode,
            hdr: None,
        })
    }
}

/// Superficie HDR.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HdrSurface {
    /// Escala de la pasada final al blanco SDR ([`crate::hdr::sdr_white_scale`]).
    pub sdr_white_scale: f32,
    /// Margen de la pantalla ([`crate::hdr::headroom`]).
    pub headroom: f32,
}

/// Cómo se configura la superficie de la ventana ([`GpuContext::surface_setup`]).
#[derive(Debug, Clone)]
pub struct SurfaceSetup {
    /// Configuración (`surface.configure(&device, &config)`).
    pub config: ::wgpu::SurfaceConfiguration,
    /// Formato de la vista en la que se dibuja (sin sRGB; en HDR, el de la textura intermedia).
    pub render_format: ::wgpu::TextureFormat,
    /// Modo de presentación.
    pub mode: PresentMode,
    /// Con superficie HDR, sus datos: se dibuja en la textura intermedia de
    /// [`crate::hdr::HdrOutput`] y se compone a la superficie.
    pub hdr: Option<HdrSurface>,
}

/// Resultado de pedir la imagen de la superficie ([`acquire`]).
#[derive(Debug)]
pub enum Acquired {
    /// Imagen lista.
    Frame(::wgpu::SurfaceTexture),
    /// Ahora no (ventana oculta, ocupada o desactualizada tras reconfigurar): otra vez luego.
    Skip,
}

/// Configura `surface` para `device`. Si el adaptador ya no puede presentar en ella (el escritorio
/// cerró la conexión con la ventana, por ejemplo porque no pudo importar sus imágenes), devuelve
/// [`RenderError::SurfaceRejected`] en vez del pánico con el que `wgpu` trata el error de
/// validación.
pub fn configure_surface(
    surface: &::wgpu::Surface<'_>,
    device: &::wgpu::Device,
    config: &::wgpu::SurfaceConfiguration,
) -> Result<(), RenderError> {
    let scope = device.push_error_scope(::wgpu::ErrorFilter::Validation);
    surface.configure(device, config);
    match pollster::block_on(scope.pop()) {
        None => Ok(()),
        Some(err) => Err(RenderError::SurfaceRejected(err.to_string())),
    }
}

/// Pide la imagen de la superficie; reconfigura si está desactualizada.
pub fn acquire(
    surface: &::wgpu::Surface<'_>,
    device: &::wgpu::Device,
    config: &::wgpu::SurfaceConfiguration,
) -> Result<Acquired, RenderError> {
    for _ in 0..2 {
        match surface.get_current_texture() {
            ::wgpu::CurrentSurfaceTexture::Success(texture) => {
                return Ok(Acquired::Frame(texture));
            }
            ::wgpu::CurrentSurfaceTexture::Suboptimal(texture) => {
                drop(texture);
                configure_surface(surface, device, config)?;
            }
            ::wgpu::CurrentSurfaceTexture::Outdated => configure_surface(surface, device, config)?,
            ::wgpu::CurrentSurfaceTexture::Timeout | ::wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(Acquired::Skip);
            }
            ::wgpu::CurrentSurfaceTexture::Lost => return Err(RenderError::SurfaceLost),
            ::wgpu::CurrentSurfaceTexture::Validation => {
                return Err(RenderError::Backend(
                    "error de validación al pedir la imagen de la superficie".into(),
                ));
            }
        }
    }
    Ok(Acquired::Skip)
}

/// Importador de las imágenes Vulkan del decodificador en el dispositivo de un [`GpuContext`]
/// (clonable: lo comparten la ventana y el vídeo).
#[derive(Clone)]
pub struct VideoImporter(Arc<VulkanImporter>);

impl fmt::Debug for VideoImporter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl VideoImporter {
    /// Importa `frame` sin copia en `video` (ver el módulo): bloquea la imagen, la envuelve en un
    /// `wgpu::Texture` (`texture_from_raw`), graba y envía la barrera a
    /// `SHADER_READ_ONLY_OPTIMAL` esperando al semáforo de FFmpeg y deja la señal del valor
    /// siguiente para la **próxima entrega de la cola** (la que dibuja la interfaz con el vídeo),
    /// y desbloquea anotando ese valor y el layout.
    pub(crate) fn import(
        &self,
        device: &::wgpu::Device,
        queue: &::wgpu::Queue,
        video: &mut VideoPipeline,
        frame: &VulkanFrame,
    ) -> Result<(), RenderError> {
        let importer = &self.0;
        if frame.device != importer.device.handle().as_raw()
            || frame.format != vk::Format::G8_B8R8_2PLANE_420_UNORM.as_raw()
        {
            return Err(RenderError::Unsupported(
                "la imagen decodificada no es NV12 del dispositivo de la ventana".into(),
            ));
        }
        let state = frame.image.lock();
        match self.submit_import(device, queue, video, frame, &state) {
            Ok(update) => {
                frame.image.unlock(Some(update));
                Ok(())
            }
            Err(err) => {
                frame.image.unlock(None);
                Err(err)
            }
        }
    }

    fn submit_import(
        &self,
        device: &::wgpu::Device,
        queue: &::wgpu::Queue,
        video: &mut VideoPipeline,
        frame: &VulkanFrame,
        state: &ExternalImageState,
    ) -> Result<ExternalImageUpdate, RenderError> {
        let importer = &self.0;
        if state.image == 0 || state.semaphore == 0 {
            return Err(RenderError::Backend(
                "imagen Vulkan sin manija o sin semáforo".into(),
            ));
        }
        let size = ::wgpu::Extent3d {
            width: frame.image_width,
            height: frame.image_height,
            depth_or_array_layers: 1,
        };
        let image = vk::Image::from_raw(state.image);
        // El `wgpu::Texture` mantiene viva la imagen (y el `AVFrame`) hasta que la GPU termina.
        let keepalive = Arc::clone(&frame.image);
        let hal_desc = ::wgpu::hal::TextureDescriptor {
            label: Some("coroom-vulkan"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: ::wgpu::TextureDimension::D2,
            format: ::wgpu::TextureFormat::NV12,
            usage: ::wgpu::TextureUses::RESOURCE,
            memory_flags: ::wgpu::hal::MemoryFlags::empty(),
            view_formats: Vec::new(),
        };
        // SAFETY: `image` es una `VkImage` NV12 multiplano de `image_width × image_height` creada
        // por FFmpeg en este mismo `VkDevice` (comprobado en `import`) con uso de muestreo; con
        // `drop_callback` `wgpu-hal` no la destruye ni libera su memoria (`External`), y la
        // retrollamada suelta el `keepalive` que mantiene vivo el `AVFrame` hasta entonces.
        let hal_texture = unsafe {
            let hal_device = device
                .as_hal::<::wgpu::hal::api::Vulkan>()
                .ok_or_else(|| RenderError::Backend("el dispositivo no es Vulkan".into()))?;
            hal_device.texture_from_raw(
                image,
                &hal_desc,
                Some(Box::new(move || drop(keepalive))),
                ::wgpu::hal::vulkan::TextureMemory::External,
            )
        };
        // SAFETY: la textura es de este dispositivo y respeta la descripción; su estado inicial
        // es `RESOURCE` (`SHADER_READ_ONLY_OPTIMAL`) porque la barrera que se graba abajo y se
        // envía antes que cualquier uso la deja en ese layout.
        let texture = unsafe {
            device.create_texture_from_hal::<::wgpu::hal::api::Vulkan>(
                hal_texture,
                &::wgpu::TextureDescriptor {
                    label: Some("coroom-vulkan"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: ::wgpu::TextureDimension::D2,
                    format: ::wgpu::TextureFormat::NV12,
                    usage: ::wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                ::wgpu::TextureUses::RESOURCE,
            )
        };
        video.show_imported(
            device,
            texture,
            (frame.width, frame.height),
            (frame.image_width, frame.image_height),
        );
        let mut encoder = device.create_command_encoder(&::wgpu::CommandEncoderDescriptor {
            label: Some("coroom-importar"),
        });
        let barrier = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
            .dst_access_mask(vk::AccessFlags::SHADER_READ)
            .old_layout(vk::ImageLayout::from_raw(state.layout))
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            // Imágenes concurrentes entre las familias de FFmpeg y la de `wgpu`: sin traspaso.
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        let raw_device = importer.device.clone();
        // SAFETY: se graba una barrera de imagen en el búfer de comandos abierto de este
        // `CommandEncoder`, que no se usa con `wgpu` mientras tanto.
        let recorded = unsafe {
            encoder.as_hal_mut::<::wgpu::hal::api::Vulkan, _, _>(|hal| {
                let Some(hal) = hal else {
                    return false;
                };
                raw_device.cmd_pipeline_barrier(
                    hal.raw_handle(),
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[barrier],
                );
                true
            })
        };
        if !recorded {
            return Err(RenderError::Backend(
                "no se pudo grabar la barrera de la imagen importada".into(),
            ));
        }
        let semaphore = vk::Semaphore::from_raw(state.semaphore);
        let signal = state.semaphore_value + 1;
        // SAFETY: el semáforo de línea temporal es de este dispositivo. La entrega de la barrera
        // espera (en todas las etapas, así la transición de layout no se adelanta) a que FFmpeg
        // termine de decodificar (`semaphore_value`); la siguiente entrega de la cola, la que
        // muestrea la imagen al dibujar la interfaz, lo señala al valor siguiente al acabar, que
        // es lo que se anota en el `AVVkFrame` (FFmpeg espera a ese valor antes de reutilizarla).
        unsafe {
            let hal_queue = queue
                .as_hal::<::wgpu::hal::api::Vulkan>()
                .ok_or_else(|| RenderError::Backend("la cola no es Vulkan".into()))?;
            hal_queue.add_wait_semaphore(
                semaphore,
                Some(state.semaphore_value),
                vk::PipelineStageFlags::ALL_COMMANDS,
            );
        }
        queue.submit([encoder.finish()]);
        // SAFETY: como arriba; la señal queda para la próxima entrega de esta cola.
        unsafe {
            let hal_queue = queue
                .as_hal::<::wgpu::hal::api::Vulkan>()
                .ok_or_else(|| RenderError::Backend("la cola no es Vulkan".into()))?;
            hal_queue.add_signal_semaphore(semaphore, Some(signal));
        }
        Ok(ExternalImageUpdate {
            layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL.as_raw(),
            access: vk::AccessFlags::SHADER_READ.as_raw(),
            semaphore_value: signal,
            queue_family: importer.graphics_family,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn family(index: u32, flags: vk::QueueFlags, video_caps: u32) -> QueueFamilyInfo {
        QueueFamilyInfo {
            index,
            flags: flags.as_raw(),
            count: 1,
            video_caps,
        }
    }

    /// Reparto de colas sin GPU, con las familias típicas de cada fabricante.
    #[test]
    fn queue_plans_never_hand_over_the_wgpu_queue() {
        let gfx = vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER;
        let compute = vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER;
        let decode = vk::QueueFlags::VIDEO_DECODE_KHR;
        // NVIDIA: gráficos, transferencia, cómputo, decodificación.
        let nvidia = [
            family(0, gfx, 0),
            family(1, vk::QueueFlags::TRANSFER, 0),
            family(2, compute, 0),
            family(3, decode | vk::QueueFlags::TRANSFER, 0b111),
        ];
        let plan = plan_queue_families(&nvidia, 0).expect("NVIDIA");
        let order: Vec<u32> = plan.iter().map(|f| f.index).collect();
        assert_eq!(
            order,
            vec![3, 2, 1, 0],
            "decodificación primero, gráficos el último"
        );
        assert!(plan.iter().all(|f| f.count == 1));
        // AMD (RADV): gráficos, cómputo, decodificación.
        let amd = [
            family(0, gfx, 0),
            family(1, compute, 0),
            family(2, decode, 0b1),
        ];
        let plan = plan_queue_families(&amd, 0).expect("AMD");
        assert_eq!(
            plan.iter().map(|f| f.index).collect::<Vec<_>>(),
            vec![2, 1, 0]
        );
        // Sin otra familia de cómputo, FFmpeg usaría la cola de `wgpu`: no se comparte.
        let lonely = [family(0, gfx, 0), family(1, decode, 0b1)];
        assert_eq!(plan_queue_families(&lonely, 0), None);
        // Sin decodificación de vídeo, nada.
        assert_eq!(plan_queue_families(&[family(0, gfx, 0)], 0), None);
        // Una familia de decodificación sin códecs no cuenta.
        let empty = [
            family(0, gfx, 0),
            family(1, compute, 0),
            family(2, decode, 0),
        ];
        assert_eq!(plan_queue_families(&empty, 0), None);
    }

    #[test]
    fn present_mode_prefers_lowest_latency() {
        use ::wgpu::PresentMode as M;
        assert_eq!(
            choose_present_mode(&[M::Fifo, M::Mailbox, M::Immediate]),
            PresentMode::Immediate
        );
        assert_eq!(
            choose_present_mode(&[M::Fifo, M::Mailbox]),
            PresentMode::Mailbox
        );
        assert_eq!(choose_present_mode(&[M::Fifo]), PresentMode::Fifo);
        assert_eq!(choose_present_mode(&[]), PresentMode::Fifo);
    }
}
