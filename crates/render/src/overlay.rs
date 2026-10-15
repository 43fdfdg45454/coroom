//! La interfaz encima del vídeo: una textura con alfa premultiplicado dibujada sobre el destino.
//!
//! El compositor de la ventana dibuja primero el fondo y las capas de vídeo y, al final, la textura
//! donde `iced` dejó la interfaz (limpiada a transparente antes de dibujarla, así que lo que hay en
//! ella ya está premultiplicado por su alfa). [`OverlayBlit`] la copia encima con la mezcla
//! `(1, 1 − α)` con un triángulo que cubre todo el destino; las dos texturas tienen el mismo tamaño
//! y se leen por píxel, sin muestreador.

/// Pasada que dibuja una textura con alfa premultiplicado sobre el destino.
pub struct OverlayBlit {
    pipeline: ::wgpu::RenderPipeline,
    layout: ::wgpu::BindGroupLayout,
}

impl std::fmt::Debug for OverlayBlit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayBlit").finish_non_exhaustive()
    }
}

const SHADER: &str = r"
@group(0) @binding(0) var source: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let x = f32(i32(index & 1u) * 4 - 1);
    let y = f32(i32(index & 2u) * 2 - 1);
    return vec4<f32>(x, y, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(source, vec2<i32>(position.xy), 0);
}
";

impl OverlayBlit {
    /// Pasada hacia destinos de formato `format` (el de la textura de la interfaz).
    pub fn new(device: &::wgpu::Device, format: ::wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(::wgpu::ShaderModuleDescriptor {
            label: Some("coroom-overlay"),
            source: ::wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&::wgpu::BindGroupLayoutDescriptor {
            label: Some("coroom-overlay"),
            entries: &[::wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: ::wgpu::ShaderStages::FRAGMENT,
                ty: ::wgpu::BindingType::Texture {
                    sample_type: ::wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: ::wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&::wgpu::PipelineLayoutDescriptor {
            label: Some("coroom-overlay"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&::wgpu::RenderPipelineDescriptor {
            label: Some("coroom-overlay"),
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
                    format,
                    blend: Some(::wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: ::wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self { pipeline, layout }
    }

    /// Enlace a la textura que se va a dibujar encima (uno por textura; se reutiliza mientras no
    /// cambie).
    pub fn bind(&self, device: &::wgpu::Device, source: &::wgpu::TextureView) -> ::wgpu::BindGroup {
        device.create_bind_group(&::wgpu::BindGroupDescriptor {
            label: Some("coroom-overlay"),
            layout: &self.layout,
            entries: &[::wgpu::BindGroupEntry {
                binding: 0,
                resource: ::wgpu::BindingResource::TextureView(source),
            }],
        })
    }

    /// Dibuja la textura de `bind_group` sobre la pasada (del mismo tamaño que ella).
    pub fn draw(&self, pass: &mut ::wgpu::RenderPass<'_>, bind_group: &::wgpu::BindGroup) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}
