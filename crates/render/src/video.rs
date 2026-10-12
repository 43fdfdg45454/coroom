//! Pipeline de GPU compartido por [`crate::layer::VideoLayer`] (el vídeo dentro de la interfaz) y
//! [`crate::offscreen::OffscreenPresenter`]: texturas del fotograma (tres `R8Unorm` en I420 e I444,
//! `R8Unorm` + `Rg8Unorm` en NV12, `R16Unorm` + `Rg16Unorm` en P010), la textura `Rgba8Unorm` del
//! cursor, un triángulo que cubre el *viewport* de la pasada y el shader `yuv.wgsl`, que recorta al
//! rectángulo del vídeo y convierte con la matriz y la transferencia del fotograma
//! ([`crate::color`]).
//!
//! P010 necesita `TEXTURE_FORMAT_16BIT_NORM` (lo tienen Vulkan y D3D12 de escritorio; el
//! dispositivo lo pide si existe). Sin ella, los 10 bits se bajan a 8 en CPU (NV12) y se avisa una
//! vez: se pierde precisión, no el HDR.
//!
//! También dibuja una textura NV12 importada (la imagen Vulkan del decodificador,
//! [`VideoPipeline::show_imported`]) con vistas de sus dos planos y el recorte de la parte visible
//! (el decodificador alinea el tamaño de la imagen).

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};

use crate::color::{ColorInfo, OutputMode, yuv_to_rgb};
use crate::{CursorImage, CursorOverlay, PresentFrame, PresentImage, RenderError};

/// Lado del cursor de respaldo (cuadrado blanco) en píxeles del destino, cuando todavía no ha
/// llegado la forma real.
pub const CURSOR_SIDE: f32 = 8.0;

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct Params {
    video_rect: [f32; 4],
    /// Cursor de respaldo: centro (x, y), semilado y visibilidad.
    cursor: [f32; 4],
    /// Cursor con forma: origen (x, y) y tamaño (w, h) en píxeles del destino.
    cursor_rect: [f32; 4],
    /// x: 1 si el fotograma es semiplanar (NV12, P010); y: 0 sin cursor, 1 cuadrado, 2 con forma;
    /// z, w: parte visible de la textura (1 salvo en las imágenes importadas, que pueden ser más
    /// grandes).
    mode: [f32; 4],
    /// Matriz afín Y′CbCr → R′G′B′ por filas ([`crate::color::yuv_to_rgb`]).
    yuv_r: [f32; 4],
    /// Segunda fila.
    yuv_g: [f32; 4],
    /// Tercera fila.
    yuv_b: [f32; 4],
    /// x: 1 con PQ; y: 1 con salida HDR (sRGB extendido); z: margen de la pantalla; w: sin uso.
    color: [f32; 4],
}

/// Rectángulo `[x, y, ancho, alto]` en píxeles del destino donde cabe el vídeo sin deformarse,
/// centrado, con bandas negras a los lados o arriba y abajo.
pub fn video_rect(frame: (u32, u32), target: (u32, u32)) -> [f32; 4] {
    let (fw, fh) = (frame.0.max(1) as f32, frame.1.max(1) as f32);
    let (tw, th) = (target.0.max(1) as f32, target.1.max(1) as f32);
    let scale = (tw / fw).min(th / fh);
    let (w, h) = (fw * scale, fh * scale);
    [(tw - w) / 2.0, (th - h) / 2.0, w, h]
}

/// Disposición de los planos del fotograma en la GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    I420,
    Nv12,
    I444,
    P010,
}

impl Layout {
    /// Si el croma va entrelazado en una textura de dos canales.
    fn semiplanar(self) -> bool {
        matches!(self, Layout::Nv12 | Layout::P010)
    }

    /// Bits por muestra y escala de la muestra normalizada al código.
    fn depth(self) -> (u8, f32) {
        match self {
            Layout::P010 => (10, 65535.0 / 64.0),
            _ => (8, 255.0),
        }
    }
}

struct Planes {
    layout: Layout,
    width: u32,
    height: u32,
    y: ::wgpu::Texture,
    /// U (I420, I444) o UV entrelazado (NV12, P010).
    u: ::wgpu::Texture,
    /// V (solo I420 e I444).
    v: Option<::wgpu::Texture>,
}

/// Baja un P010 a NV12 (los 8 bits altos) cuando el dispositivo no tiene texturas de 16 bits.
fn p010_to_nv12(
    y: &[u8],
    y_stride: u32,
    uv: &[u8],
    uv_stride: u32,
    (w, h): (u32, u32),
) -> (Vec<u8>, Vec<u8>) {
    let (cw, ch) = (w.div_ceil(2) as usize, h.div_ceil(2) as usize);
    let high = |plane: &[u8], stride: u32, words: usize, rows: usize| -> Vec<u8> {
        let mut out = Vec::with_capacity(words * rows);
        for row in plane.chunks(stride as usize).take(rows) {
            out.extend(
                row.as_chunks::<2>()
                    .0
                    .iter()
                    .take(words)
                    .map(|pair| pair[1]),
            );
        }
        out
    };
    (
        high(y, y_stride, w as usize, h as usize),
        high(uv, uv_stride, 2 * cw, ch),
    )
}

struct CursorTexture {
    image: Arc<CursorImage>,
    texture: ::wgpu::Texture,
}

/// Recursos de GPU para dibujar un fotograma YUV 4:2:0 y el cursor.
pub(crate) struct VideoPipeline {
    pipeline: ::wgpu::RenderPipeline,
    layout: ::wgpu::BindGroupLayout,
    sampler: ::wgpu::Sampler,
    params: ::wgpu::Buffer,
    /// Texturas de 1 × 1 para los huecos que no usa el formato (o sin cursor).
    blank_r8: ::wgpu::Texture,
    blank_rg8: ::wgpu::Texture,
    blank_rgba: ::wgpu::Texture,
    planes: Option<Planes>,
    /// Textura NV12 importada del fotograma actual y parte visible.
    imported: Option<(::wgpu::Texture, [f32; 2])>,
    cursor: Option<CursorTexture>,
    bind_group: Option<::wgpu::BindGroup>,
    frame_size: (u32, u32),
    /// Espacio de color del último fotograma.
    color: ColorInfo,
    /// Destino SDR o HDR.
    output: OutputMode,
    /// Si el dispositivo tiene texturas `R16Unorm` y `Rg16Unorm`.
    sixteen_bit: bool,
    /// Ya se avisó de que P010 baja a 8 bits.
    warned_p010: bool,
}

fn texture(
    device: &::wgpu::Device,
    label: &str,
    format: ::wgpu::TextureFormat,
    (width, height): (u32, u32),
) -> ::wgpu::Texture {
    device.create_texture(&::wgpu::TextureDescriptor {
        label: Some(label),
        size: ::wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: ::wgpu::TextureDimension::D2,
        format,
        usage: ::wgpu::TextureUsages::TEXTURE_BINDING | ::wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn write(
    queue: &::wgpu::Queue,
    texture: &::wgpu::Texture,
    data: &[u8],
    bytes_per_row: u32,
    (width, height): (u32, u32),
) {
    queue.write_texture(
        ::wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: ::wgpu::Origin3d::ZERO,
            aspect: ::wgpu::TextureAspect::All,
        },
        data,
        ::wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(height),
        },
        ::wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

/// Comprueba que un plano de `rows` filas de `row_bytes` útiles cabe en `len` bytes con `stride`.
fn plane_fits(len: usize, stride: u32, row_bytes: u32, rows: u32) -> bool {
    rows > 0
        && stride >= row_bytes
        && len >= stride as usize * (rows as usize - 1) + row_bytes as usize
}

impl VideoPipeline {
    pub(crate) fn new(
        device: &::wgpu::Device,
        queue: &::wgpu::Queue,
        target_format: ::wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(::wgpu::ShaderModuleDescriptor {
            label: Some("coroom-yuv"),
            source: ::wgpu::ShaderSource::Wgsl(include_str!("yuv.wgsl").into()),
        });
        let texture_entry = |binding| ::wgpu::BindGroupLayoutEntry {
            binding,
            visibility: ::wgpu::ShaderStages::FRAGMENT,
            ty: ::wgpu::BindingType::Texture {
                sample_type: ::wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: ::wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&::wgpu::BindGroupLayoutDescriptor {
            label: Some("coroom-yuv"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                texture_entry(2),
                ::wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: ::wgpu::ShaderStages::FRAGMENT,
                    ty: ::wgpu::BindingType::Sampler(::wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                ::wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: ::wgpu::ShaderStages::FRAGMENT,
                    ty: ::wgpu::BindingType::Buffer {
                        ty: ::wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_entry(5),
                texture_entry(6),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&::wgpu::PipelineLayoutDescriptor {
            label: Some("coroom-yuv"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&::wgpu::RenderPipelineDescriptor {
            label: Some("coroom-yuv"),
            layout: Some(&pipeline_layout),
            vertex: ::wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: ::wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: ::wgpu::MultisampleState::default(),
            fragment: Some(::wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(::wgpu::ColorTargetState {
                    format: target_format,
                    blend: None,
                    write_mask: ::wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&::wgpu::SamplerDescriptor {
            label: Some("coroom-yuv"),
            address_mode_u: ::wgpu::AddressMode::ClampToEdge,
            address_mode_v: ::wgpu::AddressMode::ClampToEdge,
            mag_filter: ::wgpu::FilterMode::Linear,
            min_filter: ::wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let params = device.create_buffer(&::wgpu::BufferDescriptor {
            label: Some("coroom-yuv-params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: ::wgpu::BufferUsages::UNIFORM | ::wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let blank_r8 = texture(
            device,
            "coroom-blank-r8",
            ::wgpu::TextureFormat::R8Unorm,
            (1, 1),
        );
        let blank_rg8 = texture(
            device,
            "coroom-blank-rg8",
            ::wgpu::TextureFormat::Rg8Unorm,
            (1, 1),
        );
        let blank_rgba = texture(
            device,
            "coroom-blank-rgba",
            ::wgpu::TextureFormat::Rgba8Unorm,
            (1, 1),
        );
        let sixteen_bit = device
            .features()
            .contains(::wgpu::Features::TEXTURE_FORMAT_16BIT_NORM);
        let output = if target_format == crate::hdr::HDR_FORMAT {
            OutputMode::Hdr { headroom: 1.0 }
        } else {
            OutputMode::Sdr
        };
        write(queue, &blank_r8, &[128], 1, (1, 1));
        write(queue, &blank_rg8, &[128, 128], 2, (1, 1));
        write(queue, &blank_rgba, &[0, 0, 0, 0], 4, (1, 1));
        Self {
            pipeline,
            layout,
            sampler,
            params,
            blank_r8,
            blank_rg8,
            blank_rgba,
            planes: None,
            imported: None,
            cursor: None,
            bind_group: None,
            frame_size: (0, 0),
            color: ColorInfo::SDR,
            output,
            sixteen_bit,
            warned_p010: false,
        }
    }

    /// Cambia el destino (SDR o HDR con su margen).
    pub(crate) fn set_output(&mut self, output: OutputMode) {
        self.output = output;
    }

    fn rebuild_bind_group(&mut self, device: &::wgpu::Device) {
        let view = |texture: &::wgpu::Texture| texture.create_view(&Default::default());
        let plane = |texture: &::wgpu::Texture, format, aspect| {
            texture.create_view(&::wgpu::TextureViewDescriptor {
                label: Some("coroom-plano-importado"),
                format: Some(format),
                aspect,
                ..Default::default()
            })
        };
        let (y, u, v, uv) = if let Some((texture, _)) = &self.imported {
            (
                plane(
                    texture,
                    ::wgpu::TextureFormat::R8Unorm,
                    ::wgpu::TextureAspect::Plane0,
                ),
                view(&self.blank_r8),
                view(&self.blank_r8),
                plane(
                    texture,
                    ::wgpu::TextureFormat::Rg8Unorm,
                    ::wgpu::TextureAspect::Plane1,
                ),
            )
        } else {
            let Some(planes) = &self.planes else {
                self.bind_group = None;
                return;
            };
            match planes.layout {
                Layout::I420 | Layout::I444 => (
                    view(&planes.y),
                    view(&planes.u),
                    view(planes.v.as_ref().unwrap_or(&self.blank_r8)),
                    view(&self.blank_rg8),
                ),
                Layout::Nv12 | Layout::P010 => (
                    view(&planes.y),
                    view(&self.blank_r8),
                    view(&self.blank_r8),
                    view(&planes.u),
                ),
            }
        };
        let cursor = view(
            self.cursor
                .as_ref()
                .map_or(&self.blank_rgba, |cursor| &cursor.texture),
        );
        let entry = |binding, resource| ::wgpu::BindGroupEntry { binding, resource };
        self.bind_group = Some(device.create_bind_group(&::wgpu::BindGroupDescriptor {
            label: Some("coroom-yuv"),
            layout: &self.layout,
            entries: &[
                entry(0, ::wgpu::BindingResource::TextureView(&y)),
                entry(1, ::wgpu::BindingResource::TextureView(&u)),
                entry(2, ::wgpu::BindingResource::TextureView(&v)),
                entry(3, ::wgpu::BindingResource::Sampler(&self.sampler)),
                entry(4, self.params.as_entire_binding()),
                entry(5, ::wgpu::BindingResource::TextureView(&uv)),
                entry(6, ::wgpu::BindingResource::TextureView(&cursor)),
            ],
        }));
    }

    fn create_planes(device: &::wgpu::Device, layout: Layout, width: u32, height: u32) -> Planes {
        let chroma = (width.div_ceil(2), height.div_ceil(2));
        let r8 = ::wgpu::TextureFormat::R8Unorm;
        match layout {
            Layout::I444 => Planes {
                layout,
                width,
                height,
                y: texture(device, "coroom-y", r8, (width, height)),
                u: texture(device, "coroom-u", r8, (width, height)),
                v: Some(texture(device, "coroom-v", r8, (width, height))),
            },
            Layout::P010 => Planes {
                layout,
                width,
                height,
                y: texture(
                    device,
                    "coroom-y16",
                    ::wgpu::TextureFormat::R16Unorm,
                    (width, height),
                ),
                u: texture(
                    device,
                    "coroom-uv16",
                    ::wgpu::TextureFormat::Rg16Unorm,
                    chroma,
                ),
                v: None,
            },
            Layout::I420 => Planes {
                layout,
                width,
                height,
                y: texture(device, "coroom-y", r8, (width, height)),
                u: texture(device, "coroom-u", r8, chroma),
                v: Some(texture(device, "coroom-v", r8, chroma)),
            },
            Layout::Nv12 => Planes {
                layout,
                width,
                height,
                y: texture(device, "coroom-y", r8, (width, height)),
                u: texture(device, "coroom-uv", ::wgpu::TextureFormat::Rg8Unorm, chroma),
                v: None,
            },
        }
    }

    /// Sube los planos del fotograma; recrea las texturas si cambió el tamaño o el formato.
    pub(crate) fn upload(
        &mut self,
        device: &::wgpu::Device,
        queue: &::wgpu::Queue,
        frame: &PresentFrame,
    ) -> Result<(), RenderError> {
        let (width, height) = (frame.width, frame.height);
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        let incoherent = |what: &str| {
            Err(RenderError::Unsupported(format!(
                "planos {what} incoherentes con {width}×{height}"
            )))
        };
        let layout = match &frame.image {
            PresentImage::I420 {
                y,
                u,
                v,
                y_stride,
                uv_stride,
            } => {
                if width == 0
                    || !plane_fits(y.len(), *y_stride, width, height)
                    || !plane_fits(u.len(), *uv_stride, cw, ch)
                    || !plane_fits(v.len(), *uv_stride, cw, ch)
                {
                    return incoherent("I420");
                }
                Layout::I420
            }
            PresentImage::Nv12 {
                y,
                uv,
                y_stride,
                uv_stride,
            } => {
                if width == 0
                    || !plane_fits(y.len(), *y_stride, width, height)
                    || !plane_fits(uv.len(), *uv_stride, 2 * cw, ch)
                {
                    return incoherent("NV12");
                }
                Layout::Nv12
            }
            PresentImage::I444 {
                y,
                u,
                v,
                y_stride,
                uv_stride,
            } => {
                if width == 0
                    || !plane_fits(y.len(), *y_stride, width, height)
                    || !plane_fits(u.len(), *uv_stride, width, height)
                    || !plane_fits(v.len(), *uv_stride, width, height)
                {
                    return incoherent("I444");
                }
                Layout::I444
            }
            PresentImage::P010 {
                y,
                uv,
                y_stride,
                uv_stride,
            } => {
                if width == 0
                    || !plane_fits(y.len(), *y_stride, 2 * width, height)
                    || !plane_fits(uv.len(), *uv_stride, 4 * cw, ch)
                {
                    return incoherent("P010");
                }
                if self.sixteen_bit {
                    Layout::P010
                } else {
                    if !std::mem::replace(&mut self.warned_p010, true) {
                        tracing_warn(
                            "el adaptador no tiene texturas de 16 bits: el vídeo de 10 bits se \
                             muestra con 8",
                        );
                    }
                    Layout::Nv12
                }
            }
            PresentImage::Vulkan(_) | PresentImage::Gpu => {
                return Err(RenderError::Unsupported(
                    "este presentador no importa imágenes de GPU; hace falta el de la ventana \
                     con el dispositivo Vulkan compartido"
                        .into(),
                ));
            }
        };
        let was_imported = self.imported.take().is_some();
        let stale = self.planes.as_ref().is_none_or(|planes| {
            (planes.layout, planes.width, planes.height) != (layout, width, height)
        });
        if stale {
            self.planes = Some(Self::create_planes(device, layout, width, height));
        }
        if stale || was_imported {
            self.rebuild_bind_group(device);
        }
        let Some(planes) = self.planes.as_ref() else {
            return Err(RenderError::Backend("sin texturas de vídeo".into()));
        };
        match &frame.image {
            PresentImage::I420 {
                y,
                u,
                v,
                y_stride,
                uv_stride,
            } => {
                write(queue, &planes.y, y, *y_stride, (width, height));
                write(queue, &planes.u, u, *uv_stride, (cw, ch));
                if let Some(plane) = &planes.v {
                    write(queue, plane, v, *uv_stride, (cw, ch));
                }
            }
            PresentImage::I444 {
                y,
                u,
                v,
                y_stride,
                uv_stride,
            } => {
                write(queue, &planes.y, y, *y_stride, (width, height));
                write(queue, &planes.u, u, *uv_stride, (width, height));
                if let Some(plane) = &planes.v {
                    write(queue, plane, v, *uv_stride, (width, height));
                }
            }
            PresentImage::Nv12 {
                y,
                uv,
                y_stride,
                uv_stride,
            } => {
                write(queue, &planes.y, y, *y_stride, (width, height));
                write(queue, &planes.u, uv, *uv_stride, (cw, ch));
            }
            PresentImage::P010 {
                y,
                uv,
                y_stride,
                uv_stride,
            } => {
                if layout == Layout::P010 {
                    write(queue, &planes.y, y, *y_stride, (width, height));
                    write(queue, &planes.u, uv, *uv_stride, (cw, ch));
                } else {
                    let (y8, uv8) = p010_to_nv12(y, *y_stride, uv, *uv_stride, (width, height));
                    write(queue, &planes.y, &y8, width, (width, height));
                    write(queue, &planes.u, &uv8, 2 * cw, (cw, ch));
                }
            }
            PresentImage::Vulkan(_) | PresentImage::Gpu => {}
        }
        self.frame_size = (width, height);
        self.color = frame.color;
        Ok(())
    }

    /// Dibuja `texture` (NV12 importada, de `image_size` píxeles) con su parte visible de `size`.
    pub(crate) fn show_imported(
        &mut self,
        device: &::wgpu::Device,
        texture: ::wgpu::Texture,
        size: (u32, u32),
        image_size: (u32, u32),
    ) {
        let crop = [
            size.0 as f32 / image_size.0.max(1) as f32,
            size.1 as f32 / image_size.1.max(1) as f32,
        ];
        self.imported = Some((texture, crop));
        self.frame_size = size;
        self.color = ColorInfo::SDR;
        self.rebuild_bind_group(device);
    }

    /// Cambia la forma del cursor (`None`: cuadrado de respaldo). Solo sube la textura si cambió.
    pub(crate) fn set_cursor_image(
        &mut self,
        device: &::wgpu::Device,
        queue: &::wgpu::Queue,
        image: Option<&Arc<CursorImage>>,
    ) {
        let same = match (&self.cursor, image) {
            (Some(current), Some(image)) => {
                Arc::ptr_eq(&current.image, image) || current.image.id == image.id
            }
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.cursor = image.filter(|image| image.is_valid()).map(|image| {
            let size = (image.width, image.height);
            let texture = texture(
                device,
                "coroom-cursor",
                ::wgpu::TextureFormat::Rgba8Unorm,
                size,
            );
            write(queue, &texture, &image.rgba, image.width * 4, size);
            CursorTexture {
                image: Arc::clone(image),
                texture,
            }
        });
        self.rebuild_bind_group(device);
    }

    /// Actualiza la posición del vídeo y del cursor para el área `[x, y, ancho, alto]` del destino
    /// (en píxeles físicos del destino entero: el `@builtin(position)` del fragmento lo está).
    pub(crate) fn update_params(
        &self,
        queue: &::wgpu::Queue,
        area: [f32; 4],
        cursor: Option<CursorOverlay>,
    ) {
        let fitted = video_rect(
            self.frame_size,
            (area[2].max(1.0) as u32, area[3].max(1.0) as u32),
        );
        let rect = [
            area[0] + fitted[0],
            area[1] + fitted[1],
            fitted[2],
            fitted[3],
        ];
        let layout = if self.imported.is_some() {
            Layout::Nv12
        } else {
            self.planes
                .as_ref()
                .map_or(Layout::I420, |planes| planes.layout)
        };
        let crop = self.imported.as_ref().map_or([1.0, 1.0], |(_, crop)| *crop);
        let (bits, scale) = layout.depth();
        let [yuv_r, yuv_g, yuv_b] = yuv_to_rgb(self.color, bits, scale);
        let (hdr, headroom) = match self.output {
            OutputMode::Sdr => (0.0, 1.0),
            OutputMode::Hdr { headroom } => (1.0, headroom.max(1.0)),
        };
        let mut params = Params {
            video_rect: rect,
            cursor: [0.0; 4],
            cursor_rect: [0.0; 4],
            mode: [
                if layout.semiplanar() { 1.0 } else { 0.0 },
                0.0,
                crop[0],
                crop[1],
            ],
            yuv_r,
            yuv_g,
            yuv_b,
            color: [if self.color.pq { 1.0 } else { 0.0 }, hdr, headroom, 0.0],
        };
        if let Some(cursor) = cursor
            && cursor.visible
            && self.frame_size.0 > 0
            && self.frame_size.1 > 0
        {
            let sx = rect[2] / self.frame_size.0 as f32;
            let sy = rect[3] / self.frame_size.1 as f32;
            let (x, y) = (
                rect[0] + cursor.x as f32 * sx,
                rect[1] + cursor.y as f32 * sy,
            );
            match &self.cursor {
                Some(shape) => {
                    let image = &shape.image;
                    params.cursor_rect = [
                        x - image.hotspot_x as f32 * sx,
                        y - image.hotspot_y as f32 * sy,
                        image.width as f32 * sx,
                        image.height as f32 * sy,
                    ];
                    params.mode[1] = 2.0;
                }
                None => {
                    params.cursor = [x, y, CURSOR_SIDE / 2.0, 1.0];
                    params.mode[1] = 1.0;
                }
            }
        }
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
    }

    /// Tamaño del último fotograma (`(0, 0)` si todavía no hay).
    pub(crate) fn frame_size(&self) -> (u32, u32) {
        self.frame_size
    }

    /// Dibuja el vídeo (o nada si todavía no hay fotograma).
    pub(crate) fn draw(&self, pass: &mut ::wgpu::RenderPass<'_>) {
        if let Some(bind_group) = &self.bind_group {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

/// Aviso por `stderr` (`coroom-render` no depende de `tracing`).
fn tracing_warn(message: &str) {
    eprintln!("AVISO: {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p010_drops_to_the_high_byte_without_16_bit_textures() {
        // 2 × 2: Y = 573 (0x8F40 con los 10 bits arriba) y UV = (512, 512).
        let y = (573u16 << 6).to_le_bytes().repeat(4);
        let uv = [(512u16 << 6).to_le_bytes(), (512u16 << 6).to_le_bytes()].concat();
        let (y8, uv8) = p010_to_nv12(&y, 4, &uv, 4, (2, 2));
        assert_eq!(y8, vec![(573 >> 2) as u8; 4]);
        assert_eq!(uv8, vec![128, 128]);
    }

    #[test]
    fn letterboxing_keeps_the_aspect_ratio() {
        assert_eq!(
            video_rect((1920, 1080), (1920, 1080)),
            [0.0, 0.0, 1920.0, 1080.0]
        );
        // Ventana más alta: bandas arriba y abajo.
        assert_eq!(
            video_rect((1920, 1080), (1920, 1440)),
            [0.0, 180.0, 1920.0, 1080.0]
        );
        // Ventana más ancha: bandas a los lados.
        assert_eq!(
            video_rect((1280, 720), (1600, 720)),
            [160.0, 0.0, 1280.0, 720.0]
        );
    }
}
