//! Salida HDR de la ventana.
//!
//! `wgpu` 30 configura la superficie con un espacio de color HDR (`SurfaceColorSpace`); coroom usa
//! `Rgba16Float` con **`ExtendedSrgbLinear`** (scRGB), el que ofrecen DX12 siempre y Vulkan cuando
//! el compositor tiene `VK_EXT_swapchain_colorspace` con HDR (en Linux, Mesa con el protocolo de
//! gestión de color de Wayland).
//!
//! La interfaz (`iced_wgpu`, sin corrección gamma) escribe valores en espacio sRGB, y el vídeo HDR
//! en sRGB extendido ([`crate::OutputMode::Hdr`]). Para no tocar `iced`, las dos cosas se dibujan
//! en una textura intermedia `Rgba16Float` ([`HdrOutput::target`]) y una pasada final
//! ([`HdrOutput::composite`]) la lleva a la superficie lineal: `srgb_to_linear` con signo y escala
//! al blanco SDR de la pantalla ([`sdr_white_scale`]: en Windows scRGB 1,0 son 80 nits y el blanco
//! SDR es el del ajuste «Brillo del contenido SDR»; donde no se sabe, 1,0). Coste: una lectura y
//! una escritura de la textura intermedia por fotograma.

use bytemuck::{Pod, Zeroable};

/// Formato de la superficie y de la textura intermedia en HDR.
pub const HDR_FORMAT: ::wgpu::TextureFormat = ::wgpu::TextureFormat::Rgba16Float;

/// Escala de la pasada final: cuántas veces 1,0 (scRGB, 80 nits en Windows) es el blanco SDR de la
/// pantalla, si `wgpu` lo sabe; si no, 1,0 (la convención de `wgpu`: 1,0 es el blanco SDR).
pub fn sdr_white_scale(info: &::wgpu::DisplayHdrInfo) -> f32 {
    info.luminance
        .and_then(|luminance| luminance.sdr_white_nits)
        .filter(|nits| nits.is_finite() && *nits > 0.0)
        .map_or(1.0, |nits| nits / 80.0)
}

/// Margen de la pantalla en múltiplos del blanco SDR (1,0 si no se sabe).
pub fn headroom(info: &::wgpu::DisplayHdrInfo) -> f32 {
    info.tone_map_headroom()
        .filter(|h| h.is_finite())
        .map_or(1.0, |h| h.max(1.0))
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct Params {
    /// x: escala al blanco SDR; y, z, w: sin uso.
    scale: [f32; 4],
}

const SHADER: &str = r"
struct Params { scale: vec4<f32> };
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: Params;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let x = f32(i32(index & 1u) * 4 - 1);
    let y = f32(i32(index & 2u) * 2 - 1);
    return vec4<f32>(x, y, 0.0, 1.0);
}

fn srgb_to_linear(v: vec3<f32>) -> vec3<f32> {
    let a = abs(v);
    let low = a / 12.92;
    let high = pow((a + 0.055) / 1.055, vec3<f32>(2.4));
    return sign(v) * select(high, low, a <= vec3<f32>(0.04045));
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = textureLoad(source, vec2<i32>(position.xy), 0);
    return vec4<f32>(srgb_to_linear(texel.rgb) * params.scale.x, 1.0);
}
";

/// Textura intermedia y pasada final a la superficie HDR (ver el módulo).
pub struct HdrOutput {
    pipeline: ::wgpu::RenderPipeline,
    layout: ::wgpu::BindGroupLayout,
    params: ::wgpu::Buffer,
    target: Option<(::wgpu::Texture, ::wgpu::TextureView, ::wgpu::BindGroup)>,
    size: (u32, u32),
}

impl std::fmt::Debug for HdrOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HdrOutput")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl HdrOutput {
    /// Pasada final hacia una superficie de formato `surface_format` (normalmente [`HDR_FORMAT`]).
    pub fn new(device: &::wgpu::Device, surface_format: ::wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(::wgpu::ShaderModuleDescriptor {
            label: Some("coroom-hdr"),
            source: ::wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&::wgpu::BindGroupLayoutDescriptor {
            label: Some("coroom-hdr"),
            entries: &[
                ::wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: ::wgpu::ShaderStages::FRAGMENT,
                    ty: ::wgpu::BindingType::Texture {
                        sample_type: ::wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: ::wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                ::wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: ::wgpu::ShaderStages::FRAGMENT,
                    ty: ::wgpu::BindingType::Buffer {
                        ty: ::wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&::wgpu::PipelineLayoutDescriptor {
            label: Some("coroom-hdr"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&::wgpu::RenderPipelineDescriptor {
            label: Some("coroom-hdr"),
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
                    format: surface_format,
                    blend: None,
                    write_mask: ::wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let params = device.create_buffer(&::wgpu::BufferDescriptor {
            label: Some("coroom-hdr-params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: ::wgpu::BufferUsages::UNIFORM | ::wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            layout,
            params,
            target: None,
            size: (0, 0),
        }
    }

    /// Escala al blanco SDR de la pantalla ([`sdr_white_scale`]).
    pub fn set_scale(&self, queue: &::wgpu::Queue, scale: f32) {
        let params = Params {
            scale: [scale.max(0.0), 0.0, 0.0, 0.0],
        };
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
    }

    /// Vista de la textura intermedia de `size` (se recrea si cambia) donde dibujar la interfaz y
    /// el vídeo, en [`HDR_FORMAT`].
    pub fn target(&mut self, device: &::wgpu::Device, size: (u32, u32)) -> &::wgpu::TextureView {
        let size = (size.0.max(1), size.1.max(1));
        if self.target.is_none() || self.size != size {
            let texture = device.create_texture(&::wgpu::TextureDescriptor {
                label: Some("coroom-hdr-intermedia"),
                size: ::wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: ::wgpu::TextureDimension::D2,
                format: HDR_FORMAT,
                usage: ::wgpu::TextureUsages::RENDER_ATTACHMENT
                    | ::wgpu::TextureUsages::TEXTURE_BINDING
                    | ::wgpu::TextureUsages::COPY_SRC
                    | ::wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let bind_group = device.create_bind_group(&::wgpu::BindGroupDescriptor {
                label: Some("coroom-hdr"),
                layout: &self.layout,
                entries: &[
                    ::wgpu::BindGroupEntry {
                        binding: 0,
                        resource: ::wgpu::BindingResource::TextureView(&view),
                    },
                    ::wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.params.as_entire_binding(),
                    },
                ],
            });
            self.target = Some((texture, view, bind_group));
            self.size = size;
        }
        match &self.target {
            Some((_, view, _)) => view,
            None => unreachable!("la textura intermedia se acaba de crear"),
        }
    }

    /// Textura intermedia (para leerla en los tests).
    pub fn texture(&self) -> Option<&::wgpu::Texture> {
        self.target.as_ref().map(|(texture, _, _)| texture)
    }

    /// Graba la pasada final de la textura intermedia a `surface` (del mismo tamaño).
    pub fn composite(&self, encoder: &mut ::wgpu::CommandEncoder, surface: &::wgpu::TextureView) {
        let Some((_, _, bind_group)) = &self.target else {
            return;
        };
        let mut pass = encoder.begin_render_pass(&::wgpu::RenderPassDescriptor {
            label: Some("coroom-hdr"),
            color_attachments: &[Some(::wgpu::RenderPassColorAttachment {
                view: surface,
                depth_slice: None,
                resolve_target: None,
                ops: ::wgpu::Operations {
                    load: ::wgpu::LoadOp::Clear(::wgpu::Color::BLACK),
                    store: ::wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{f16_to_f32, srgb_to_linear};
    use crate::wgpu::{create_instance, request_device, select_adapter};

    fn f32_to_f16(value: f32) -> u16 {
        // Solo valores normales y positivos o negativos sencillos (los del test).
        let bits = value.to_bits();
        let sign = ((bits >> 16) & 0x8000) as u16;
        let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
        let mantissa = ((bits & 0x7f_ffff) + 0x1000) >> 13;
        if value == 0.0 {
            return sign;
        }
        sign | (((exponent as u32) << 10) + mantissa) as u16
    }

    #[test]
    fn white_scale_comes_from_the_display() {
        let mut info = ::wgpu::DisplayHdrInfo::default();
        assert_eq!(sdr_white_scale(&info), 1.0);
        assert_eq!(headroom(&info), 1.0);
        info.luminance = Some(::wgpu::DisplayLuminance {
            max_nits: Some(1000.0),
            sdr_white_nits: Some(240.0),
            ..Default::default()
        });
        assert!((sdr_white_scale(&info) - 3.0).abs() < 1e-6);
        assert!((headroom(&info) - 1000.0 / 240.0).abs() < 1e-4);
    }

    /// La pasada final pasa el sRGB extendido de la textura intermedia a lineal con la escala del
    /// blanco SDR, conservando los valores por encima de 1 (lavapipe en el contenedor).
    #[test]
    fn composite_linearises_extended_srgb() {
        let instance = create_instance(None);
        let Ok(adapter) = pollster::block_on(select_adapter(&instance, None)) else {
            eprintln!("AVISO: test de render omitido, no hay adaptador gráfico");
            return;
        };
        let Ok((device, queue)) = pollster::block_on(request_device(&adapter)) else {
            return;
        };
        let mut output = HdrOutput::new(&device, HDR_FORMAT);
        output.set_scale(&queue, 2.5);
        let _ = output.target(&device, (2, 1));
        let values = [[0.5f32, 1.0, 1.8, 1.0], [0.0, 0.2, -0.1, 1.0]];
        let data: Vec<u8> = values
            .iter()
            .flatten()
            .flat_map(|v| f32_to_f16(*v).to_le_bytes())
            .collect();
        let Some(source) = output.texture() else {
            panic!("sin textura intermedia");
        };
        queue.write_texture(
            ::wgpu::TexelCopyTextureInfo {
                texture: source,
                mip_level: 0,
                origin: ::wgpu::Origin3d::ZERO,
                aspect: ::wgpu::TextureAspect::All,
            },
            &data,
            ::wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(16),
                rows_per_image: Some(1),
            },
            ::wgpu::Extent3d {
                width: 2,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let surface = device.create_texture(&::wgpu::TextureDescriptor {
            label: Some("coroom-hdr-test"),
            size: ::wgpu::Extent3d {
                width: 2,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: ::wgpu::TextureDimension::D2,
            format: HDR_FORMAT,
            usage: ::wgpu::TextureUsages::RENDER_ATTACHMENT | ::wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = surface.create_view(&Default::default());
        let readback = device.create_buffer(&::wgpu::BufferDescriptor {
            label: Some("coroom-hdr-test"),
            size: 256,
            usage: ::wgpu::BufferUsages::COPY_DST | ::wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        output.composite(&mut encoder, &view);
        encoder.copy_texture_to_buffer(
            ::wgpu::TexelCopyTextureInfo {
                texture: &surface,
                mip_level: 0,
                origin: ::wgpu::Origin3d::ZERO,
                aspect: ::wgpu::TextureAspect::All,
            },
            ::wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: ::wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(1),
                },
            },
            ::wgpu::Extent3d {
                width: 2,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(::wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        device
            .poll(::wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .unwrap();
        rx.recv().unwrap().unwrap();
        let mapped = slice.get_mapped_range().unwrap();
        for (pixel, value) in values.iter().enumerate() {
            for (c, channel) in value.iter().take(3).enumerate() {
                let at = pixel * 8 + c * 2;
                let got = f16_to_f32(u16::from_le_bytes([mapped[at], mapped[at + 1]]));
                let want = srgb_to_linear(*channel) * 2.5;
                assert!(
                    (got - want).abs() < 0.01 * want.abs().max(1.0),
                    "píxel {pixel}, canal {c}: {got} frente a {want}"
                );
            }
        }
    }
}
