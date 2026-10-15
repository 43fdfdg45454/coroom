//! Importación sin copias de las imágenes de la captura en los *hw frames* del codificador.
//!
//! **Linux (DMA-BUF)**: la imagen se envuelve en un `AVFrame` `AV_PIX_FMT_DRM_PRIME` (con un
//! `AVDRMFrameDescriptor` construido desde sus planos, [`drm_descriptor`]) y se mapea con
//! `av_hwframe_map` en un contexto de *hw frames* del dispositivo del codificador. PipeWire
//! reutiliza sus 4 a 16 búferes (cada vez con un `dup` nuevo del descriptor), y mapear cuesta
//! (`vkImportMemoryFd`, `vkCreateImage` y su enlace por fotograma): el mapeo se guarda por
//! identidad del búfer ([`DmaBufKey`]: el inodo del DMA-BUF, único por búfer desde Linux 5.3, con
//! desplazamientos, pasos, modificador, formato y tamaño) en una caché LRU de
//! [`MAPPED_CACHE`] entradas que se vacía al cambiar el tamaño (se rehace el importador):
//!
//! | Codificador | Mapeo | Conversión de color (GPU) | Lo que recibe el codificador |
//! |---|---|---|---|
//! | Vulkan Video | DRM → `AV_PIX_FMT_VULKAN` (`VK_EXT_image_drm_format_modifier`) | `scale_vulkan=format=nv12` | imágenes Vulkan NV12 |
//! | VA-API | DRM → `AV_PIX_FMT_VAAPI` (superficie importada) | `scale_vaapi=format=nv12` (BT.709) | superficies VA NV12 |
//! | QSV | DRM → VA-API como arriba | `scale_vaapi`, luego `hwmap=derive_device=qsv` | superficies QSV NV12 |
//! | NVENC, AMF | — (FFmpeg 8.1 no importa DMA-BUF en CUDA ni en AMF): la captura usa memoria compartida | | |
//!
//! **Windows (D3D11)**: la textura de Windows.Graphics.Capture se copia **en la GPU**
//! (`CopySubresourceRegion`, sin pasar por CPU) a una textura del pool `AV_PIX_FMT_D3D11` del
//! codificador, creado sobre el mismo `ID3D11Device` de la captura. NVENC y AMF aceptan esas
//! texturas BGRA tal cual; QSV pasa por `hwmap=derive_device=qsv,scale_qsv=format=nv12`; Vulkan
//! Video por `hwmap=derive_device=vulkan,scale_vulkan=format=nv12` (si FFmpeg 8.1 no sabe mapear
//! D3D11 → Vulkan en esa máquina, el mapeo falla y se baja a CPU).
//!
//! Si el codificador no abre con los *hw frames* de la conversión (por ejemplo, porque las imágenes
//! de `scale_vulkan` no llevan el uso `VIDEO_ENCODE_SRC` en ese driver), el importador pasa al modo
//! de respaldo: la misma cadena termina en `hwdownload` y el codificador recibe NV12 en CPU (con
//! las copias contadas).

#[cfg(target_os = "linux")]
use std::sync::Arc;

use rsmpeg::avutil::{AVFrame, AVHWDeviceContext, AVHWFramesContext};
use rsmpeg::ffi;

use super::filter::HwFilter;
use crate::gpu::{GpuImage, GpuInput};
use crate::{Backend, PixelFormat, RawFrame};

/// Imágenes de GPU que acepta el codificador `backend` sin copia a CPU. `device` es su dispositivo,
/// si ya existe (en Vulkan se consultan los modificadores DRM que importa).
pub(crate) fn gpu_input_for(backend: Backend, device: Option<&AVHWDeviceContext>) -> GpuInput {
    let _ = device;
    #[cfg(target_os = "linux")]
    {
        use crate::gpu::{DRM_FORMAT_MOD_INVALID, DRM_FORMAT_MOD_LINEAR};
        match backend {
            Backend::VulkanVideo => {
                let owned;
                let device = match device {
                    Some(device) => device,
                    None => match super::create_device(ffi::AV_HWDEVICE_TYPE_VULKAN) {
                        Ok(device) => {
                            owned = device;
                            &owned
                        }
                        Err(_) => return GpuInput::None,
                    },
                };
                let modifiers = super::vulkan::drm_modifiers(device);
                if modifiers.is_empty() {
                    GpuInput::None
                } else {
                    GpuInput::DmaBuf { modifiers }
                }
            }
            // VA-API importa la disposición implícita del driver y la lineal.
            Backend::Vaapi | Backend::Qsv => GpuInput::DmaBuf {
                modifiers: vec![DRM_FORMAT_MOD_INVALID, DRM_FORMAT_MOD_LINEAR],
            },
            Backend::Nvenc | Backend::Amf | Backend::Software => GpuInput::None,
        }
    }
    #[cfg(target_os = "windows")]
    {
        match backend {
            Backend::Nvenc | Backend::Amf | Backend::Qsv => GpuInput::D3D11,
            // Importar texturas D3D11 en Vulkan cierra la aplicación con drivers actuales: con
            // Vulkan Video se copia salvo que se pida con `COROOM_ZERO_COPY=1`.
            Backend::VulkanVideo if crate::gpu::zero_copy_forced() => GpuInput::D3D11,
            Backend::VulkanVideo => GpuInput::None,
            Backend::Vaapi | Backend::Software => GpuInput::None,
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = backend;
        GpuInput::None
    }
}

/// Si el importador entrega al codificador *hw frames* (cero copias) o NV12 en CPU (respaldo).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportMode {
    /// El codificador recibe los *hw frames* de la conversión.
    Zero,
    /// La conversión termina en `hwdownload`: el codificador recibe NV12 en CPU.
    Download,
}

/// Lo que sale del importador para un fotograma.
pub(crate) enum Prepared {
    /// Fotograma de GPU listo para `avcodec_send_frame`.
    Hw(AVFrame),
    /// Imagen en CPU para la ruta con copias (respaldo).
    Cpu(RawFrame),
}

/// Formato de FFmpeg de un formato DRM de 32 bits de la captura.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn pix_fmt_for_fourcc(fourcc: u32) -> Option<ffi::AVPixelFormat> {
    use crate::gpu::{
        DRM_FORMAT_ABGR8888, DRM_FORMAT_ARGB8888, DRM_FORMAT_XBGR8888, DRM_FORMAT_XRGB8888,
    };
    Some(match fourcc {
        DRM_FORMAT_XRGB8888 => ffi::AV_PIX_FMT_BGR0,
        DRM_FORMAT_ARGB8888 => ffi::AV_PIX_FMT_BGRA,
        DRM_FORMAT_XBGR8888 => ffi::AV_PIX_FMT_RGB0,
        DRM_FORMAT_ABGR8888 => ffi::AV_PIX_FMT_RGBA,
        _ => return None,
    })
}

/// Cadena de conversión de color en la GPU de cada codificador (sin el `hwdownload` del respaldo).
pub(crate) fn conversion_chain(backend: Backend, source: SourceKind) -> Option<&'static str> {
    match (source, backend) {
        (SourceKind::DmaBuf, Backend::VulkanVideo) => {
            Some("scale_vulkan=format=nv12:out_range=limited")
        }
        (SourceKind::DmaBuf, Backend::Vaapi) => {
            Some("scale_vaapi=format=nv12:out_color_matrix=bt709:out_range=limited")
        }
        (SourceKind::DmaBuf, Backend::Qsv) => Some(
            "scale_vaapi=format=nv12:out_color_matrix=bt709:out_range=limited,\
             hwmap=derive_device=qsv,format=qsv",
        ),
        (SourceKind::D3D11, Backend::Qsv) => Some("hwmap=derive_device=qsv,scale_qsv=format=nv12"),
        (SourceKind::D3D11, Backend::VulkanVideo) => {
            Some("hwmap=derive_device=vulkan,scale_vulkan=format=nv12:out_range=limited")
        }
        // NVENC y AMF aceptan las texturas BGRA de D3D11 sin conversión.
        (SourceKind::D3D11, Backend::Nvenc | Backend::Amf) => Some(""),
        _ => None,
    }
}

/// Cadena completa para el modo `mode` (en el de respaldo, con `hwdownload` a NV12).
pub(crate) fn filter_chain(
    backend: Backend,
    source: SourceKind,
    mode: ImportMode,
) -> Option<String> {
    let conversion = conversion_chain(backend, source)?;
    Some(match (mode, conversion.is_empty()) {
        (ImportMode::Zero, _) => conversion.to_owned(),
        // La parte QSV no hace falta para bajar: se baja desde VA-API.
        (ImportMode::Download, false) => {
            let base = conversion
                .split(",hwmap=derive_device=qsv")
                .next()
                .unwrap_or(conversion);
            format!("{base},hwdownload,format=nv12")
        }
        (ImportMode::Download, true) => "hwdownload,format=bgra".to_owned(),
    })
}

/// Tipo de imagen de la captura.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceKind {
    /// DMA-BUF de PipeWire.
    #[cfg_attr(not(unix), allow(dead_code))]
    DmaBuf,
    /// Textura D3D11 de WGC.
    #[cfg_attr(not(windows), allow(dead_code))]
    D3D11,
}

impl SourceKind {
    /// Tipo de `image`, si se importa.
    pub(crate) fn of(image: &GpuImage) -> Option<SourceKind> {
        match image {
            #[cfg(unix)]
            GpuImage::DmaBuf(_) => Some(SourceKind::DmaBuf),
            #[cfg(windows)]
            GpuImage::D3D11(_) => Some(SourceKind::D3D11),
            _ => None,
        }
    }
}

/// Importador de un tamaño y un tipo de imagen.
pub(crate) struct GpuImporter {
    source: SourceKind,
    size: (u32, u32),
    mode: ImportMode,
    /// Contexto donde aterrizan las imágenes (mapeadas o copiadas en la GPU).
    landing: AVHWFramesContext,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    landing_format: ffi::AVPixelFormat,
    filter: Option<HwFilter>,
    /// Copias a CPU del último fotograma.
    copies: u32,
    /// Mapeos de DMA-BUF por identidad del búfer ([`MappedCache`]).
    #[cfg(target_os = "linux")]
    mapped: MappedCache<DmaBufKey, AVFrame>,
    #[cfg(windows)]
    d3d11: d3d11::Device,
    /// Lo que debe vivir tanto como el importador (dispositivos derivados).
    _devices: Vec<AVHWDeviceContext>,
}

impl std::fmt::Debug for GpuImporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuImporter")
            .field("source", &self.source)
            .field("size", &self.size)
            .field("mode", &self.mode)
            .field("filter", &self.filter)
            .finish_non_exhaustive()
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn check(what: &str, ret: i32) -> Result<(), String> {
    if ret < 0 {
        Err(format!("{what}: {}", super::describe_code(ret)))
    } else {
        Ok(())
    }
}

impl GpuImporter {
    /// Prepara la importación de imágenes como `image` para el codificador `backend`, cuyo
    /// dispositivo es `device`.
    pub(crate) fn new(
        backend: Backend,
        device: Option<&AVHWDeviceContext>,
        image: &GpuImage,
        size: (u32, u32),
        mode: ImportMode,
    ) -> Result<Self, String> {
        let source = SourceKind::of(image)
            .ok_or_else(|| format!("las imágenes {} no se importan", image.kind()))?;
        match source {
            SourceKind::DmaBuf => Self::new_dmabuf(backend, device, image, size, mode),
            SourceKind::D3D11 => Self::new_d3d11(backend, image, size, mode),
        }
    }

    #[cfg(target_os = "linux")]
    fn new_dmabuf(
        backend: Backend,
        device: Option<&AVHWDeviceContext>,
        image: &GpuImage,
        size: (u32, u32),
        mode: ImportMode,
    ) -> Result<Self, String> {
        let GpuImage::DmaBuf(dmabuf) = image else {
            return Err("se esperaba un DMA-BUF".to_owned());
        };
        let sw_format = pix_fmt_for_fourcc(dmabuf.fourcc).ok_or_else(|| {
            format!(
                "formato DMA-BUF no admitido: {}",
                crate::gpu::fourcc_name(dmabuf.fourcc)
            )
        })?;
        let chain = filter_chain(backend, SourceKind::DmaBuf, mode)
            .ok_or_else(|| format!("{backend} no importa DMA-BUF en FFmpeg 8.1"))?;
        let device =
            device.ok_or_else(|| format!("el codificador {backend} no tiene dispositivo"))?;
        // QSV mapea a través de VA-API (QSV en Linux se apoya en él).
        let mut devices = Vec::new();
        let (landing_device, landing_format) = match backend {
            Backend::VulkanVideo => (device.clone(), ffi::AV_PIX_FMT_VULKAN),
            Backend::Vaapi => (device.clone(), ffi::AV_PIX_FMT_VAAPI),
            Backend::Qsv => {
                let vaapi = device
                    .create_derived(ffi::AV_HWDEVICE_TYPE_VAAPI)
                    .map_err(|err| format!("VA-API desde QSV: {}", super::describe(&err)))?;
                devices.push(vaapi.clone());
                (vaapi, ffi::AV_PIX_FMT_VAAPI)
            }
            other => return Err(format!("{other} no importa DMA-BUF en FFmpeg 8.1")),
        };
        let mut landing = landing_device.hwframe_ctx_alloc();
        {
            let data = landing.data();
            data.format = landing_format;
            data.sw_format = sw_format;
            data.width = i32::try_from(size.0).unwrap_or(i32::MAX);
            data.height = i32::try_from(size.1).unwrap_or(i32::MAX);
            data.initial_pool_size = 0;
        }
        if landing_format == ffi::AV_PIX_FMT_VULKAN {
            // SAFETY: el `hwctx` de un contexto de fotogramas Vulkan sin iniciar es un
            // `AVVulkanFramesContext` a cero; se limitan los usos a los que necesita la
            // conversión (muestrear y copiar), que los modificadores ofrecidos admiten.
            unsafe {
                let vkfc = landing
                    .data()
                    .hwctx
                    .cast::<super::hw::AVVulkanFramesContext>();
                (*vkfc).usage =
                    ash::vk::ImageUsageFlags::SAMPLED | ash::vk::ImageUsageFlags::TRANSFER_SRC;
            }
        }
        landing
            .init()
            .map_err(|err| format!("fotogramas de destino: {}", super::describe(&err)))?;
        let filter = HwFilter::new(&landing, landing_format, size, &chain)?;
        devices.push(landing_device);
        Ok(Self {
            source: SourceKind::DmaBuf,
            size,
            mode,
            landing,
            landing_format,
            filter: Some(filter),
            copies: 0,
            mapped: MappedCache::new(MAPPED_CACHE, MAPPED_IDLE_FRAMES),
            _devices: devices,
        })
    }

    #[cfg(not(target_os = "linux"))]
    fn new_dmabuf(
        backend: Backend,
        _device: Option<&AVHWDeviceContext>,
        _image: &GpuImage,
        _size: (u32, u32),
        _mode: ImportMode,
    ) -> Result<Self, String> {
        Err(format!("{backend}: los DMA-BUF solo se importan en Linux"))
    }

    #[cfg(windows)]
    fn new_d3d11(
        backend: Backend,
        image: &GpuImage,
        size: (u32, u32),
        mode: ImportMode,
    ) -> Result<Self, String> {
        let GpuImage::D3D11(texture) = image else {
            return Err("se esperaba una textura D3D11".to_owned());
        };
        let chain = filter_chain(backend, SourceKind::D3D11, mode)
            .ok_or_else(|| format!("{backend} no recibe texturas D3D11"))?;
        let d3d11 = d3d11::Device::new(&texture.device)?;
        let landing = d3d11.pool(size)?;
        let filter = if chain.is_empty() {
            None
        } else {
            Some(HwFilter::new(
                &landing,
                ffi::AV_PIX_FMT_D3D11,
                size,
                &chain,
            )?)
        };
        Ok(Self {
            source: SourceKind::D3D11,
            size,
            mode,
            landing,
            landing_format: ffi::AV_PIX_FMT_D3D11,
            filter,
            copies: 0,
            _devices: vec![d3d11.context().clone()],
            d3d11,
        })
    }

    #[cfg(not(windows))]
    fn new_d3d11(
        backend: Backend,
        _image: &GpuImage,
        _size: (u32, u32),
        _mode: ImportMode,
    ) -> Result<Self, String> {
        Err(format!(
            "{backend}: las texturas D3D11 solo se importan en Windows"
        ))
    }

    /// Modo en uso.
    pub(crate) fn mode(&self) -> ImportMode {
        self.mode
    }

    /// Si sirve para `image` de `size`.
    pub(crate) fn matches(&self, image: &GpuImage, size: (u32, u32)) -> bool {
        SourceKind::of(image) == Some(self.source) && self.size == size
    }

    /// Contexto de *hw frames* con el que hay que abrir el codificador (modo
    /// [`ImportMode::Zero`]); `None` en el de respaldo.
    pub(crate) fn encoder_frames(&self) -> Option<AVHWFramesContext> {
        if self.mode != ImportMode::Zero {
            return None;
        }
        match &self.filter {
            Some(filter) => filter.output_frames(),
            None => Some(self.landing.clone()),
        }
    }

    /// Copias a CPU del último fotograma (0 en el modo sin copias).
    pub(crate) fn copies(&self) -> u32 {
        self.copies
    }

    /// Lleva `image` a lo que recibe el codificador.
    pub(crate) fn prepare(
        &mut self,
        image: &GpuImage,
        frame_id: u64,
        pts: i64,
    ) -> Result<Vec<Prepared>, String> {
        let mut landed = self.land(image)?;
        landed.set_pts(pts);
        // SAFETY: `landed` es un fotograma propio; se rellenan las propiedades de color que usan
        // las conversiones (RGB de rango completo con la matriz BT.709 de destino:
        // `scale_vulkan` toma los coeficientes de la entrada y `scale_vaapi` los de
        // `out_color_matrix`).
        unsafe {
            let raw = landed.as_mut_ptr();
            (*raw).colorspace = ffi::AVCOL_SPC_BT709;
            (*raw).color_range = ffi::AVCOL_RANGE_JPEG;
        }
        let outputs = match self.filter.as_mut() {
            Some(filter) => filter.run(&mut landed)?,
            None => vec![landed],
        };
        self.copies = 0;
        let mut prepared = Vec::with_capacity(outputs.len());
        for mut out in outputs {
            out.set_pts(pts);
            if self.mode == ImportMode::Zero {
                prepared.push(Prepared::Hw(out));
            } else {
                self.copies = 2;
                prepared.push(Prepared::Cpu(to_raw_frame(&out, frame_id)?));
            }
        }
        Ok(prepared)
    }

    /// Mapea o copia `image` en el contexto de destino.
    fn land(&mut self, image: &GpuImage) -> Result<AVFrame, String> {
        match image {
            #[cfg(target_os = "linux")]
            GpuImage::DmaBuf(dmabuf) => self.map_dmabuf(dmabuf),
            #[cfg(windows)]
            GpuImage::D3D11(texture) => self.d3d11.copy_into_pool(&mut self.landing, texture),
            other => Err(format!("las imágenes {} no se importan", other.kind())),
        }
    }

    /// Mapeo de `image`: el guardado si el búfer ya pasó por aquí ([`DmaBufKey`]), si no uno
    /// nuevo que se guarda. Devuelve una referencia nueva (`av_frame_clone`) al mismo mapeo: el
    /// filtro y el codificador toman las suyas y el guardado no cambia.
    #[cfg(target_os = "linux")]
    fn map_dmabuf(&mut self, image: &Arc<crate::gpu::DmaBufImage>) -> Result<AVFrame, String> {
        let key = DmaBufKey::of(image);
        self.mapped.tick();
        if let Some(key) = &key
            && let Some(frame) = self.mapped.get(key)
        {
            return Ok(frame.clone());
        }
        let mapped = self.map_dmabuf_fresh(image)?;
        if let Some(key) = key {
            self.mapped.insert(key, mapped.clone());
        }
        Ok(mapped)
    }

    #[cfg(target_os = "linux")]
    fn map_dmabuf_fresh(
        &mut self,
        image: &Arc<crate::gpu::DmaBufImage>,
    ) -> Result<AVFrame, String> {
        let source = drm_frame(image)?;
        let mut mapped = AVFrame::new();
        mapped.set_format(self.landing_format);
        // SAFETY: `mapped` es un fotograma vacío nuestro; se le da una referencia propia del
        // contexto de destino (como pide `av_hwframe_map` para mapear «hacia» ese contexto) y se
        // mapea desde `source`, un `AV_PIX_FMT_DRM_PRIME` válido (ver `drm_frame`). El mapeo
        // guarda una referencia de `source`, así que su descriptor y sus fd viven lo que el
        // fotograma mapeado.
        let ret = unsafe {
            let raw = mapped.as_mut_ptr();
            (*raw).hw_frames_ctx = ffi::av_buffer_ref(self.landing.as_ptr());
            if (*raw).hw_frames_ctx.is_null() {
                return Err("sin memoria para el mapeo".to_owned());
            }
            ffi::av_hwframe_map(raw, source.as_ptr(), super::MAP_READ)
        };
        check("mapeo del DMA-BUF", ret)?;
        Ok(mapped)
    }
}

/// Mapeos de DMA-BUF que se recuerdan: PipeWire negocia como mucho 16 búferes.
#[cfg(target_os = "linux")]
const MAPPED_CACHE: usize = 16;

/// Fotogramas sin usarse tras los que un mapeo se suelta: cuando el compositor reserva búferes
/// nuevos (otro tamaño, otra lista), los viejos no vuelven a aparecer y, sin esto, quedarían
/// retenidos (y con ellos su memoria de GPU) hasta que 16 búferes distintos los desplazaran.
#[cfg(target_os = "linux")]
const MAPPED_IDLE_FRAMES: u64 = 64;

/// Caché de mapeos por identidad del búfer con dos salidas: la de menos uso reciente cuando se
/// llena, y la que lleva [`MAPPED_IDLE_FRAMES`] fotogramas sin usarse ([`MappedCache::tick`] cuenta
/// uno por imagen que llega). Genérica para probarla sin DMA-BUF.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct MappedCache<K, V> {
    entries: Vec<(K, V, u64)>,
    capacity: usize,
    idle: u64,
    now: u64,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl<K: PartialEq, V> MappedCache<K, V> {
    fn new(capacity: usize, idle: u64) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
            capacity,
            idle,
            now: 0,
        }
    }

    /// Llega una imagen: avanza el contador y suelta los mapeos que llevan `idle` sin usarse.
    fn tick(&mut self) {
        self.now += 1;
        let (now, idle) = (self.now, self.idle);
        self.entries
            .retain(|(_, _, last_used)| now.saturating_sub(*last_used) <= idle);
    }

    /// El mapeo de `key`, marcándolo como usado ahora.
    fn get(&mut self, key: &K) -> Option<&V> {
        let now = self.now;
        self.entries
            .iter_mut()
            .find(|(known, _, _)| known == key)
            .map(|(_, value, last_used)| {
                *last_used = now;
                &*value
            })
    }

    /// Guarda un mapeo nuevo; si no cabe, fuera el de menos uso reciente.
    fn insert(&mut self, key: K, value: V) {
        if self.entries.len() >= self.capacity
            && let Some(oldest) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, (_, _, last_used))| *last_used)
                .map(|(index, _)| index)
        {
            self.entries.swap_remove(oldest);
        }
        self.entries.push((key, value, self.now));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Identidad de un DMA-BUF de la captura: inodo de cada plano (el mismo objeto con distinto `fd`
/// tras cada `dup`), desplazamientos y pasos, modificador, formato y tamaño.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct DmaBufKey {
    planes: Vec<(u64, u32, u32)>,
    modifier: u64,
    fourcc: u32,
    size: (u32, u32),
}

#[cfg(target_os = "linux")]
impl DmaBufKey {
    /// `None` si algún inodo no se puede leer (entonces no se guarda el mapeo).
    fn of(image: &crate::gpu::DmaBufImage) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        let planes = image
            .planes
            .iter()
            .map(|plane| {
                let file = std::fs::File::from(plane.fd.try_clone().ok()?);
                let inode = file.metadata().ok()?.ino();
                (inode != 0).then_some((inode, plane.offset, plane.stride))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            planes,
            modifier: image.modifier,
            fourcc: image.fourcc,
            size: (image.width, image.height),
        })
    }
}

/// Copia un fotograma NV12 o BGRA de CPU (salida del respaldo) en un [`RawFrame`] compacto.
fn to_raw_frame(frame: &AVFrame, frame_id: u64) -> Result<RawFrame, String> {
    let format = match frame.format {
        ffi::AV_PIX_FMT_NV12 => PixelFormat::Nv12,
        ffi::AV_PIX_FMT_BGRA | ffi::AV_PIX_FMT_BGR0 => PixelFormat::Bgra,
        other => {
            return Err(format!(
                "la conversión entregó un formato inesperado ({other})"
            ));
        }
    };
    let (Ok(width), Ok(height)) = (u32::try_from(frame.width), u32::try_from(frame.height)) else {
        return Err("tamaño de imagen fuera de rango".to_owned());
    };
    let len = frame
        .image_get_buffer_size(1)
        .map_err(|err| super::describe(&err))?;
    let mut data = vec![0u8; len];
    frame
        .image_copy_to_buffer(&mut data, 1)
        .map_err(|err| super::describe(&err))?;
    let stride = if format == PixelFormat::Bgra {
        width * 4
    } else {
        width
    };
    Ok(RawFrame::cpu(
        frame_id,
        (width, height),
        format,
        stride,
        data,
    ))
}

/// Descriptor DRM de una imagen DMA-BUF: un objeto por descriptor distinto y una capa con un plano
/// por plano de la imagen.
#[cfg(unix)]
pub(crate) fn drm_descriptor(
    image: &crate::gpu::DmaBufImage,
) -> Result<super::hw::AVDRMFrameDescriptor, String> {
    use std::os::fd::AsRawFd;

    use super::hw::{AV_DRM_MAX_PLANES, AVDRMFrameDescriptor, AVDRMObjectDescriptor};

    if image.planes.is_empty() || image.planes.len() > AV_DRM_MAX_PLANES {
        return Err(format!(
            "un DMA-BUF de {} planos no cabe en un descriptor DRM",
            image.planes.len()
        ));
    }
    let mut desc = AVDRMFrameDescriptor::default();
    let mut fds: Vec<i32> = Vec::new();
    for plane in &image.planes {
        let fd = plane.fd.as_raw_fd();
        if !fds.contains(&fd) {
            fds.push(fd);
            desc.objects[fds.len() - 1] = AVDRMObjectDescriptor {
                fd,
                size: object_size(&plane.fd),
                format_modifier: image.modifier,
            };
        }
    }
    desc.nb_objects = fds.len() as i32;
    desc.nb_layers = 1;
    let layer = &mut desc.layers[0];
    layer.format = image.fourcc;
    layer.nb_planes = image.planes.len() as i32;
    for (slot, plane) in layer.planes.iter_mut().zip(&image.planes) {
        let fd = plane.fd.as_raw_fd();
        slot.object_index = fds.iter().position(|known| *known == fd).unwrap_or(0) as i32;
        slot.offset = plane.offset as isize;
        slot.pitch = plane.stride as isize;
    }
    Ok(desc)
}

/// Tamaño de un objeto DMA-BUF (`lseek(fd, 0, SEEK_END)` sobre un duplicado), o 0 si no se sabe.
#[cfg(unix)]
fn object_size(fd: &std::os::fd::OwnedFd) -> usize {
    use std::io::{Seek, SeekFrom};
    fd.try_clone()
        .ok()
        .map(std::fs::File::from)
        .and_then(|mut file| file.seek(SeekFrom::End(0)).ok())
        .and_then(|size| usize::try_from(size).ok())
        .unwrap_or(0)
}

/// `AVFrame` `AV_PIX_FMT_DRM_PRIME` que apunta a `image`: `data[0]` es su descriptor y `buf[0]` un
/// búfer que mantiene vivos el descriptor y duplicados de los fd de sus planos mientras FFmpeg use
/// el fotograma. No retiene el búfer de PipeWire (`keepalive`): los filtros de Vulkan de FFmpeg
/// sueltan su entrada al reutilizar su contexto de ejecución (decenas de fotogramas después), y
/// con ella retenida la captura se quedaba sin búferes y se paraba a los pocos fotogramas. El búfer
/// lo retiene el codificador hasta que sale el paquete del fotograma, cuando la GPU ya lo leyó.
#[cfg(target_os = "linux")]
fn drm_frame(image: &Arc<crate::gpu::DmaBufImage>) -> Result<AVFrame, String> {
    use super::hw::AVDRMFrameDescriptor;

    /// Dueño del descriptor y de la imagen.
    struct Owner {
        desc: AVDRMFrameDescriptor,
        _fds: Vec<std::os::fd::OwnedFd>,
    }

    unsafe extern "C" fn free_owner(opaque: *mut std::ffi::c_void, _data: *mut u8) {
        // SAFETY: `opaque` es el `Box<Owner>` que puso `drm_frame`; FFmpeg llama a esta función
        // una sola vez, al soltar la última referencia del búfer.
        unsafe { drop(Box::from_raw(opaque.cast::<Owner>())) };
    }

    let planes = image
        .planes
        .iter()
        .map(|plane| {
            Ok(crate::gpu::DmaBufPlane {
                fd: plane
                    .fd
                    .try_clone()
                    .map_err(|err| format!("no se pudo duplicar el DMA-BUF: {err}"))?,
                offset: plane.offset,
                stride: plane.stride,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let own = crate::gpu::DmaBufImage {
        planes,
        modifier: image.modifier,
        fourcc: image.fourcc,
        width: image.width,
        height: image.height,
        keepalive: None,
    };
    let desc = drm_descriptor(&own)?;
    let owner = Box::into_raw(Box::new(Owner {
        desc,
        _fds: own.planes.into_iter().map(|plane| plane.fd).collect(),
    }));
    let mut frame = AVFrame::new();
    frame.set_format(ffi::AV_PIX_FMT_DRM_PRIME);
    frame.set_width(i32::try_from(image.width).unwrap_or(i32::MAX));
    frame.set_height(i32::try_from(image.height).unwrap_or(i32::MAX));
    // SAFETY: `owner` es nuestro hasta que `av_buffer_create` lo adopta (con `free_owner` como
    // destructor); si falla, se recupera aquí. `data[0]` apunta al descriptor dentro del dueño,
    // que vive lo que el búfer.
    unsafe {
        let data = std::ptr::addr_of_mut!((*owner).desc).cast::<u8>();
        let buffer = ffi::av_buffer_create(
            data,
            std::mem::size_of::<AVDRMFrameDescriptor>(),
            Some(free_owner),
            owner.cast(),
            0,
        );
        if buffer.is_null() {
            drop(Box::from_raw(owner));
            return Err("sin memoria para el descriptor DRM".to_owned());
        }
        let raw = frame.as_mut_ptr();
        (*raw).buf[0] = buffer;
        (*raw).data[0] = data;
    }
    Ok(frame)
}

#[cfg(windows)]
mod d3d11 {
    //! Dispositivo D3D11VA de FFmpeg sobre el `ID3D11Device` de la captura y copia en la GPU de
    //! cada textura capturada al pool del codificador.

    use rsmpeg::avutil::{AVFrame, AVHWDeviceContext, AVHWFramesContext};
    use rsmpeg::ffi;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, ID3D11Device,
        ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
    };
    use windows::core::Interface;

    use super::super::hw::{AVD3D11VADeviceContext, AVD3D11VAFramesContext};
    use crate::gpu::D3D11Image;

    /// Texturas del pool (fijo: QSV necesita una matriz de tamaño conocido).
    const POOL_SIZE: i32 = 8;

    /// Dispositivo D3D11VA de FFmpeg que envuelve el de la captura.
    pub(super) struct Device {
        context: AVHWDeviceContext,
        device: ID3D11Device,
    }

    impl Device {
        /// Envuelve `device` (con la protección multihilo del contexto inmediato activada: la
        /// captura y el codificador lo usan desde hilos distintos).
        pub(super) fn new(device: &ID3D11Device) -> Result<Self, String> {
            let multithread: ID3D11Multithread = device
                .cast()
                .map_err(|err| format!("ID3D11Multithread: {err}"))?;
            // SAFETY: `SetMultithreadProtected` solo cambia una bandera del dispositivo.
            let _ = unsafe { multithread.SetMultithreadProtected(true) };
            let mut context = AVHWDeviceContext::alloc(ffi::AV_HWDEVICE_TYPE_D3D11VA);
            // SAFETY: el `hwctx` de un dispositivo D3D11VA sin iniciar es un
            // `AVD3D11VADeviceContext` a cero; FFmpeg suelta en `uninit` la referencia del
            // dispositivo que se le da (de ahí el `clone` + `into_raw`, que suma una) y rellena
            // el resto (contexto inmediato, vídeo, bloqueo) en `av_hwdevice_ctx_init`.
            unsafe {
                let ctx = (*context.as_ptr()).data.cast::<ffi::AVHWDeviceContext>();
                let hw = (*ctx).hwctx.cast::<AVD3D11VADeviceContext>();
                (*hw).device = device.clone().into_raw();
            }
            context.init().map_err(|err| {
                format!(
                    "FFmpeg no aceptó el dispositivo D3D11 de la captura: {}",
                    super::super::describe(&err)
                )
            })?;
            Ok(Self {
                context,
                device: device.clone(),
            })
        }

        pub(super) fn context(&self) -> &AVHWDeviceContext {
            &self.context
        }

        /// Pool BGRA de `size` para el codificador.
        pub(super) fn pool(&self, size: (u32, u32)) -> Result<AVHWFramesContext, String> {
            let mut pool = self.context.hwframe_ctx_alloc();
            {
                let data = pool.data();
                data.format = ffi::AV_PIX_FMT_D3D11;
                data.sw_format = ffi::AV_PIX_FMT_BGRA;
                data.width = i32::try_from(size.0).unwrap_or(i32::MAX);
                data.height = i32::try_from(size.1).unwrap_or(i32::MAX);
                data.initial_pool_size = POOL_SIZE;
                // SAFETY: el `hwctx` de un contexto de fotogramas D3D11 sin iniciar es un
                // `AVD3D11VAFramesContext` a cero; se piden texturas que se puedan copiar y
                // muestrear (NVENC, AMF y los filtros de vídeo de D3D11).
                unsafe {
                    let hw = data.hwctx.cast::<AVD3D11VAFramesContext>();
                    (*hw).bind_flags =
                        (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
                }
            }
            pool.init().map_err(|err| {
                format!(
                    "pool D3D11 del codificador: {}",
                    super::super::describe(&err)
                )
            })?;
            Ok(pool)
        }

        /// Copia en la GPU `image` a una textura nueva del pool.
        pub(super) fn copy_into_pool(
            &self,
            pool: &mut AVHWFramesContext,
            image: &D3D11Image,
        ) -> Result<AVFrame, String> {
            if image.device.as_raw() != self.device.as_raw() {
                return Err("la textura es de otro dispositivo D3D11".to_owned());
            }
            let mut frame = AVFrame::new();
            pool.get_buffer(&mut frame)
                .map_err(|err| format!("textura del pool: {}", super::super::describe(&err)))?;
            let source_box = D3D11_BOX {
                left: 0,
                top: 0,
                front: 0,
                right: image.width,
                bottom: image.height,
                back: 1,
            };
            // SAFETY: un fotograma del pool D3D11 lleva en `data[0]` la textura (matriz) del
            // pool y en `data[1]` su índice; las dos texturas son del mismo dispositivo (comprobado
            // arriba), del mismo formato BGRA y el rectángulo cabe en las dos. El contexto
            // inmediato se usa con el bloqueo de FFmpeg tomado y con la protección multihilo del
            // dispositivo, porque la captura lo usa desde su hilo.
            unsafe {
                let raw = frame.as_mut_ptr();
                let texture_ptr = (*raw).data[0].cast::<std::ffi::c_void>();
                let index = (*raw).data[1] as usize as u32;
                let Some(destination) = ID3D11Texture2D::from_raw_borrowed(&texture_ptr) else {
                    return Err("textura del pool nula".to_owned());
                };
                let ctx = (*self.context.as_ptr())
                    .data
                    .cast::<ffi::AVHWDeviceContext>();
                let hw = (*ctx).hwctx.cast::<AVD3D11VADeviceContext>();
                let immediate_ptr = (*hw).device_context;
                let Some(immediate) = ID3D11DeviceContext::from_raw_borrowed(&immediate_ptr) else {
                    return Err("sin contexto inmediato de D3D11".to_owned());
                };
                if let Some(lock) = (*hw).lock {
                    lock((*hw).lock_ctx);
                }
                immediate.CopySubresourceRegion(
                    destination,
                    index,
                    0,
                    0,
                    0,
                    &image.texture,
                    image.subresource,
                    Some(std::ptr::from_ref(&source_box)),
                );
                if let Some(unlock) = (*hw).unlock {
                    unlock((*hw).lock_ctx);
                }
            }
            Ok(frame)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La caché de mapeos suelta lo que lleva demasiados fotogramas sin usarse y lo de menos uso
    /// cuando se llena, y reconoce lo que vuelve.
    #[test]
    fn mapped_cache_evicts_idle_and_least_recent_entries() {
        let mut cache: MappedCache<u32, &str> = MappedCache::new(3, 4);
        cache.tick();
        cache.insert(1, "uno");
        cache.tick();
        cache.insert(2, "dos");
        assert_eq!(cache.get(&1), Some(&"uno"), "acierto por identidad");
        assert_eq!(cache.get(&9), None);
        // Se llena: fuera el de menos uso reciente (el 2, que no se ha vuelto a tocar).
        cache.tick();
        cache.insert(3, "tres");
        cache.get(&1);
        cache.tick();
        cache.insert(4, "cuatro");
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.get(&2), None);
        assert!(cache.get(&1).is_some() && cache.get(&3).is_some());
        // Cuatro fotogramas sin tocar el 4: se suelta solo; el 1 y el 3, usados, siguen.
        for _ in 0..5 {
            cache.tick();
            cache.get(&1);
            cache.get(&3);
        }
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get(&4), None);
        // Sin usar nada, al cabo del tiempo la caché queda vacía (búferes que no volverán).
        for _ in 0..5 {
            cache.tick();
        }
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn every_import_path_has_its_chain() {
        assert_eq!(
            filter_chain(Backend::VulkanVideo, SourceKind::DmaBuf, ImportMode::Zero).as_deref(),
            Some("scale_vulkan=format=nv12:out_range=limited")
        );
        assert_eq!(
            filter_chain(Backend::Qsv, SourceKind::DmaBuf, ImportMode::Download).as_deref(),
            Some(
                "scale_vaapi=format=nv12:out_color_matrix=bt709:out_range=limited,hwdownload,\
                 format=nv12"
            )
        );
        assert_eq!(
            filter_chain(Backend::Nvenc, SourceKind::D3D11, ImportMode::Zero).as_deref(),
            Some("")
        );
        assert_eq!(
            filter_chain(Backend::Nvenc, SourceKind::DmaBuf, ImportMode::Zero),
            None,
            "FFmpeg 8.1 no importa DMA-BUF en NVENC"
        );
        assert_eq!(
            filter_chain(Backend::Software, SourceKind::D3D11, ImportMode::Zero),
            None
        );
        assert_eq!(
            pix_fmt_for_fourcc(crate::gpu::DRM_FORMAT_XRGB8888),
            Some(ffi::AV_PIX_FMT_BGR0)
        );
        assert_eq!(pix_fmt_for_fourcc(0), None);
    }

    /// Los filtros de las cadenas existen en la build de FFmpeg (los de Windows, solo allí).
    #[test]
    fn the_conversion_filters_exist() {
        let mut names = vec![
            "scale_vulkan",
            "scale_vaapi",
            "hwmap",
            "hwdownload",
            "format",
        ];
        if cfg!(windows) {
            names.push("scale_qsv");
        }
        for name in names {
            let name = std::ffi::CString::new(name).unwrap_or_default();
            // SAFETY: consulta de la tabla estática de filtros con una cadena válida.
            let found = unsafe { !ffi::avfilter_get_by_name(name.as_ptr()).is_null() };
            assert!(found, "falta el filtro {name:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn drm_descriptors_come_from_the_planes() {
        use std::os::fd::{AsRawFd, OwnedFd};

        use crate::gpu::{DRM_FORMAT_XRGB8888, DmaBufImage, DmaBufPlane};

        let file = tempfile_with(4096);
        let fd: OwnedFd = file.into();
        let raw = fd.as_raw_fd();
        let image = DmaBufImage {
            planes: vec![DmaBufPlane {
                fd,
                offset: 0,
                stride: 256 * 4,
            }],
            modifier: 0x0100_0000_0000_0001,
            fourcc: DRM_FORMAT_XRGB8888,
            width: 256,
            height: 4,
            keepalive: None,
        };
        let desc = drm_descriptor(&image).expect("descriptor");
        assert_eq!(desc.nb_objects, 1);
        assert_eq!(desc.objects[0].fd, raw);
        assert_eq!(desc.objects[0].size, 4096, "lseek del objeto");
        assert_eq!(desc.objects[0].format_modifier, 0x0100_0000_0000_0001);
        assert_eq!(desc.nb_layers, 1);
        assert_eq!(desc.layers[0].format, DRM_FORMAT_XRGB8888);
        assert_eq!(desc.layers[0].nb_planes, 1);
        assert_eq!(desc.layers[0].planes[0].pitch, 1024);
        assert_eq!(desc.layers[0].planes[0].object_index, 0);

        // Dos planos en dos objetos (por ejemplo, NV12 con un fd por plano).
        let a: OwnedFd = tempfile_with(100).into();
        let b: OwnedFd = tempfile_with(50).into();
        let image = DmaBufImage {
            planes: vec![
                DmaBufPlane {
                    fd: a,
                    offset: 0,
                    stride: 64,
                },
                DmaBufPlane {
                    fd: b,
                    offset: 16,
                    stride: 64,
                },
            ],
            modifier: crate::gpu::DRM_FORMAT_MOD_LINEAR,
            fourcc: crate::gpu::fourcc(b"NV12"),
            width: 64,
            height: 1,
            keepalive: None,
        };
        let desc = drm_descriptor(&image).expect("descriptor");
        assert_eq!(desc.nb_objects, 2);
        assert_eq!(desc.layers[0].planes[1].object_index, 1);
        assert_eq!(desc.layers[0].planes[1].offset, 16);
        assert_eq!((desc.objects[0].size, desc.objects[1].size), (100, 50));

        let empty = DmaBufImage {
            planes: Vec::new(),
            ..image
        };
        assert!(drm_descriptor(&empty).is_err());
    }

    /// El fotograma DRM que se da a FFmpeg no retiene el búfer de PipeWire: los filtros de Vulkan
    /// sueltan su entrada decenas de fotogramas después, y la captura se quedaba sin búferes.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_drm_frame_does_not_hold_the_capture_buffer() {
        use std::os::fd::{AsRawFd, OwnedFd};

        use crate::gpu::{DRM_FORMAT_XRGB8888, DmaBufImage, DmaBufPlane};

        let keepalive: crate::gpu::KeepAlive = Arc::new(());
        let fd: OwnedFd = tempfile_with(4096).into();
        let raw = fd.as_raw_fd();
        let image = Arc::new(DmaBufImage {
            planes: vec![DmaBufPlane {
                fd,
                offset: 0,
                stride: 256 * 4,
            }],
            modifier: 0,
            fourcc: DRM_FORMAT_XRGB8888,
            width: 256,
            height: 4,
            keepalive: Some(Arc::clone(&keepalive)),
        });
        let frame = drm_frame(&image).expect("fotograma DRM");
        assert_eq!(Arc::strong_count(&keepalive), 2, "solo la imagen y el test");
        drop(image);
        assert_eq!(Arc::strong_count(&keepalive), 1, "se suelta con la imagen");
        // El descriptor sigue apuntando a un fd abierto (un duplicado propio).
        // SAFETY: `data[0]` de un fotograma de `drm_frame` es su `AVDRMFrameDescriptor`.
        let desc = unsafe { &*(frame.data[0] as *const super::super::hw::AVDRMFrameDescriptor) };
        assert_ne!(desc.objects[0].fd, raw);
        assert_eq!(desc.objects[0].size, 4096, "el duplicado sigue abierto");
    }

    /// Fichero temporal de `len` bytes (hace de objeto DMA-BUF: lo que importa es el fd).
    #[cfg(unix)]
    fn tempfile_with(len: u64) -> std::fs::File {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "coroom-dmabuf-{}-{len}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .expect("fichero temporal");
        let _ = std::fs::remove_file(&path);
        file.set_len(len).expect("tamaño");
        file
    }
}
