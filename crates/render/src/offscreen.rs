//! Presentador sin ventana que dibuja en una textura y permite leer los píxeles.
//!
//! Usa el mismo pipeline y el mismo shader que [`crate::layer::VideoLayer`], así que sirve para
//! comprobar la conversión YUV → RGB y el escalado sin pantalla (por ejemplo, con lavapipe en la CI).

use crate::video::VideoPipeline;
use crate::wgpu::{AdapterSummary, create_instance, request_device, select_adapter};
use crate::{
    CursorImage, CursorOverlay, OutputMode, PresentFrame, PresentMode, Presenter, RenderError,
};

/// Formato de la textura de destino (sin sRGB, como la vista del presentador de ventana).
const FORMAT: ::wgpu::TextureFormat = ::wgpu::TextureFormat::Rgba8Unorm;

/// Presentador que dibuja en una textura `Rgba8Unorm` (o, para probar la salida HDR,
/// `Rgba16Float`) de tamaño fijo.
pub struct OffscreenPresenter {
    device: ::wgpu::Device,
    queue: ::wgpu::Queue,
    format: ::wgpu::TextureFormat,
    target: ::wgpu::Texture,
    width: u32,
    height: u32,
    video: VideoPipeline,
    cursor: Option<CursorOverlay>,
    cursor_image: Option<std::sync::Arc<CursorImage>>,
    adapter: AdapterSummary,
}

impl OffscreenPresenter {
    /// Crea el presentador con un destino de `width × height`. Falla con
    /// [`RenderError::Unsupported`] si no hay ningún adaptador gráfico.
    pub fn new(width: u32, height: u32) -> Result<Self, RenderError> {
        Self::with_format(width, height, FORMAT)
    }

    /// Como [`OffscreenPresenter::new`], con la salida HDR en sRGB extendido
    /// (`Rgba16Float`, ver [`crate::hdr`]) y el margen `headroom`.
    pub fn hdr(width: u32, height: u32, headroom: f32) -> Result<Self, RenderError> {
        let mut presenter = Self::with_format(width, height, crate::hdr::HDR_FORMAT)?;
        presenter.video.set_output(OutputMode::Hdr { headroom });
        Ok(presenter)
    }

    fn with_format(
        width: u32,
        height: u32,
        format: ::wgpu::TextureFormat,
    ) -> Result<Self, RenderError> {
        let instance = create_instance(None);
        let adapter = pollster::block_on(select_adapter(&instance, None))?;
        let (device, queue) = pollster::block_on(request_device(&adapter))?;
        let width = width.max(1);
        let height = height.max(1);
        let target = Self::create_target(&device, format, width, height);
        let video = VideoPipeline::new(&device, &queue, format);
        Ok(Self {
            device,
            queue,
            format,
            target,
            width,
            height,
            video,
            cursor: None,
            cursor_image: None,
            adapter: AdapterSummary::from_info(&adapter.get_info()),
        })
    }

    fn create_target(
        device: &::wgpu::Device,
        format: ::wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> ::wgpu::Texture {
        device.create_texture(&::wgpu::TextureDescriptor {
            label: Some("coroom-offscreen"),
            size: ::wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: ::wgpu::TextureDimension::D2,
            format,
            usage: ::wgpu::TextureUsages::RENDER_ATTACHMENT | ::wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    /// Adaptador en uso.
    pub fn adapter(&self) -> &AdapterSummary {
        &self.adapter
    }

    /// Si el dispositivo tiene texturas de 16 bits (P010 sin bajar a 8 bits).
    pub fn has_16bit_textures(&self) -> bool {
        self.device
            .features()
            .contains(::wgpu::Features::TEXTURE_FORMAT_16BIT_NORM)
    }

    /// Lee un destino `Rgba16Float` como RGBA en `f32`.
    pub fn read_pixels_f32(&self) -> Result<Vec<[f32; 4]>, RenderError> {
        if self.format != crate::hdr::HDR_FORMAT {
            return Err(RenderError::Unsupported(
                "el destino no es Rgba16Float".into(),
            ));
        }
        let bytes = self.read_bytes(8)?;
        Ok(bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|px| {
                let half =
                    |i: usize| crate::color::f16_to_f32(u16::from_le_bytes([px[i], px[i + 1]]));
                [half(0), half(2), half(4), half(6)]
            })
            .collect())
    }

    /// Lee el destino como filas RGBA compactas (`width × 4` bytes por fila).
    pub fn read_pixels(&self) -> Result<Vec<u8>, RenderError> {
        if self.format != FORMAT {
            return Err(RenderError::Unsupported(
                "el destino no es Rgba8Unorm".into(),
            ));
        }
        self.read_bytes(4)
    }

    fn read_bytes(&self, bytes_per_pixel: u32) -> Result<Vec<u8>, RenderError> {
        let row = self.width * bytes_per_pixel;
        let padded = row.div_ceil(::wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * ::wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&::wgpu::BufferDescriptor {
            label: Some("coroom-offscreen-readback"),
            size: u64::from(padded) * u64::from(self.height),
            usage: ::wgpu::BufferUsages::COPY_DST | ::wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&::wgpu::CommandEncoderDescriptor {
                label: Some("coroom-offscreen-readback"),
            });
        encoder.copy_texture_to_buffer(
            ::wgpu::TexelCopyTextureInfo {
                texture: &self.target,
                mip_level: 0,
                origin: ::wgpu::Origin3d::ZERO,
                aspect: ::wgpu::TextureAspect::All,
            },
            ::wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: ::wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(self.height),
                },
            },
            ::wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(::wgpu::MapMode::Read, move |result| {
            // Si el receptor ya no existe, nadie espera el resultado.
            let _ = tx.send(result);
        });
        self.device
            .poll(::wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .map_err(|err| RenderError::Backend(format!("espera de la GPU: {err}")))?;
        rx.recv()
            .map_err(|_| RenderError::Backend("la lectura de la textura no terminó".into()))?
            .map_err(|err| RenderError::Backend(format!("lectura de la textura: {err}")))?;
        let mapped = slice
            .get_mapped_range()
            .map_err(|err| RenderError::Backend(format!("lectura de la textura: {err:?}")))?;
        let mut pixels = Vec::with_capacity((row * self.height) as usize);
        for line in mapped.chunks(padded as usize).take(self.height as usize) {
            pixels.extend_from_slice(&line[..row as usize]);
        }
        drop(mapped);
        buffer.unmap();
        Ok(pixels)
    }
}

impl Presenter for OffscreenPresenter {
    fn present_mode(&self) -> PresentMode {
        PresentMode::Immediate
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        if width == 0 || height == 0 {
            return Err(RenderError::Unsupported("tamaño nulo".into()));
        }
        self.width = width;
        self.height = height;
        self.target = Self::create_target(&self.device, self.format, width, height);
        Ok(())
    }

    fn set_cursor(&mut self, cursor: CursorOverlay) {
        self.cursor = Some(cursor);
    }

    fn set_cursor_image(&mut self, image: Option<std::sync::Arc<CursorImage>>) {
        self.cursor_image = image;
    }

    fn present(&mut self, frame: &PresentFrame) -> Result<(), RenderError> {
        self.video.upload(&self.device, &self.queue, frame)?;
        self.video
            .set_cursor_image(&self.device, &self.queue, self.cursor_image.as_ref());
        self.video.update_params(
            &self.queue,
            [0.0, 0.0, self.width as f32, self.height as f32],
            self.cursor,
        );
        let view = self.target.create_view(&Default::default());
        let mut encoder = self
            .device
            .create_command_encoder(&::wgpu::CommandEncoderDescriptor {
                label: Some("coroom-offscreen"),
            });
        {
            let mut pass = encoder
                .begin_render_pass(&::wgpu::RenderPassDescriptor {
                    label: Some("coroom-offscreen"),
                    color_attachments: &[Some(::wgpu::RenderPassColorAttachment {
                        view: &view,
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
                })
                .forget_lifetime();
            self.video.draw(&mut pass);
        }
        self.queue.submit([encoder.finish()]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PresentImage;

    use crate::color::ColorInfo;

    /// BT.709 en rango limitado, la misma matriz que `coroom_codec::convert`.
    fn rgb_to_yuv([r, g, b]: [u8; 3]) -> [u8; 3] {
        let (r, g, b) = (f64::from(r), f64::from(g), f64::from(b));
        let (kr, kb) = (0.2126, 0.0722);
        let luma = kr * r + (1.0 - kr - kb) * g + kb * b;
        let y = 16.0 + 219.0 * luma / 255.0;
        let u = 128.0 + 224.0 * (b - luma) / (2.0 * (1.0 - kb)) / 255.0;
        let v = 128.0 + 224.0 * (r - luma) / (2.0 * (1.0 - kr)) / 255.0;
        [y.round() as u8, u.round() as u8, v.round() as u8]
    }

    fn flat_frame(width: u32, height: u32, [y, u, v]: [u8; 3]) -> PresentFrame {
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        PresentFrame {
            frame_id: 1,
            width,
            height,
            image: PresentImage::I420 {
                y: vec![y; (width * height) as usize],
                u: vec![u; (cw * ch) as usize],
                v: vec![v; (cw * ch) as usize],
                y_stride: width,
                uv_stride: cw,
            },
            color: ColorInfo::SDR,
        }
    }

    fn flat_nv12(width: u32, height: u32, [y, u, v]: [u8; 3]) -> PresentFrame {
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        PresentFrame {
            frame_id: 1,
            width,
            height,
            image: PresentImage::Nv12 {
                y: vec![y; (width * height) as usize],
                uv: [u, v].repeat((cw * ch) as usize),
                y_stride: width,
                uv_stride: 2 * cw,
            },
            color: ColorInfo::SDR,
        }
    }

    fn flat_i444(width: u32, height: u32, [y, u, v]: [u8; 3]) -> PresentFrame {
        let n = (width * height) as usize;
        PresentFrame {
            frame_id: 1,
            width,
            height,
            image: PresentImage::I444 {
                y: vec![y; n],
                u: vec![u; n],
                v: vec![v; n],
                y_stride: width,
                uv_stride: width,
            },
            color: ColorInfo::SDR,
        }
    }

    /// P010 plano con códigos de 10 bits `[y, u, v]` y el espacio de color HDR10.
    fn flat_p010(width: u32, height: u32, [y, u, v]: [u16; 3]) -> PresentFrame {
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        let word = |code: u16| (code << 6).to_le_bytes();
        PresentFrame {
            frame_id: 1,
            width,
            height,
            image: PresentImage::P010 {
                y: word(y).repeat((width * height) as usize),
                uv: [word(u), word(v)].concat().repeat((cw * ch) as usize),
                y_stride: 2 * width,
                uv_stride: 4 * cw,
            },
            color: ColorInfo::HDR10,
        }
    }

    /// Lo que debería dibujar el shader para un P010 plano (`crate::color::output`).
    fn expected_p010(codes: [u16; 3], mode: OutputMode) -> [f32; 3] {
        let m = crate::color::yuv_to_rgb(ColorInfo::HDR10, 10, 65535.0 / 64.0);
        let sample = codes.map(|c| f32::from(c << 6) / 65535.0);
        crate::color::output(crate::color::apply(&m, sample), ColorInfo::HDR10, mode)
    }

    fn presenter(width: u32, height: u32) -> Option<OffscreenPresenter> {
        match OffscreenPresenter::new(width, height) {
            Ok(presenter) => Some(presenter),
            Err(err) => {
                eprintln!("AVISO: test de render omitido, no hay adaptador gráfico ({err})");
                None
            }
        }
    }

    fn pixel(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 3] {
        let at = ((y * width + x) * 4) as usize;
        [pixels[at], pixels[at + 1], pixels[at + 2]]
    }

    fn assert_close(actual: [u8; 3], expected: [u8; 3], what: &str) {
        for (a, e) in actual.iter().zip(expected) {
            assert!(
                (i16::from(*a) - i16::from(e)).abs() <= 3,
                "{what}: {actual:?}, se esperaba {expected:?} ±3"
            );
        }
    }

    #[test]
    fn flat_i420_frames_convert_to_the_expected_rgb() {
        let Some(mut presenter) = presenter(64, 64) else {
            return;
        };
        eprintln!("adaptador: {}", presenter.adapter());
        for rgb in [
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [255, 255, 255],
            [0, 0, 0],
            [128, 128, 128],
            [200, 100, 50],
            [30, 160, 220],
        ] {
            presenter
                .present(&flat_frame(64, 64, rgb_to_yuv(rgb)))
                .unwrap();
            let pixels = presenter.read_pixels().unwrap();
            for (x, y) in [(0, 0), (31, 31), (63, 63), (10, 50)] {
                assert_close(
                    pixel(&pixels, 64, x, y),
                    rgb,
                    &format!("{rgb:?} en ({x}, {y})"),
                );
            }
        }
    }

    #[test]
    fn flat_nv12_frames_convert_like_i420() {
        let Some(mut presenter) = presenter(64, 64) else {
            return;
        };
        for rgb in [
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [200, 100, 50],
            [30, 160, 220],
        ] {
            presenter
                .present(&flat_nv12(64, 64, rgb_to_yuv(rgb)))
                .unwrap();
            let pixels = presenter.read_pixels().unwrap();
            for (x, y) in [(0, 0), (31, 31), (63, 63)] {
                assert_close(pixel(&pixels, 64, x, y), rgb, &format!("NV12 {rgb:?}"));
            }
            // Y vuelta a I420 en el mismo presentador (cambia la disposición de texturas).
            presenter
                .present(&flat_frame(64, 64, rgb_to_yuv(rgb)))
                .unwrap();
            let pixels = presenter.read_pixels().unwrap();
            assert_close(pixel(&pixels, 64, 10, 10), rgb, &format!("I420 {rgb:?}"));
        }
    }

    /// I444 con el mismo color que I420 da lo mismo (el croma, a resolución completa).
    #[test]
    fn flat_i444_frames_convert_like_i420() {
        let Some(mut presenter) = presenter(32, 32) else {
            return;
        };
        for rgb in [[255, 0, 0], [0, 0, 255], [200, 100, 50]] {
            presenter
                .present(&flat_i444(32, 32, rgb_to_yuv(rgb)))
                .unwrap();
            let pixels = presenter.read_pixels().unwrap();
            assert_close(pixel(&pixels, 32, 7, 21), rgb, &format!("I444 {rgb:?}"));
        }
    }

    /// HDR10 en un destino SDR: el blanco de referencia (código 573) sale casi blanco, el negro
    /// negro, y lo que dibuja el shader coincide con `crate::color::output` (tone mapping).
    #[test]
    fn p010_hdr10_is_tone_mapped_on_sdr_targets() {
        let Some(mut presenter) = presenter(32, 32) else {
            return;
        };
        eprintln!("texturas de 16 bits: {}", presenter.has_16bit_textures());
        for codes in [
            [573, 512, 512],
            [64, 512, 512],
            [723, 512, 512],
            [400, 450, 600],
        ] {
            presenter.present(&flat_p010(32, 32, codes)).unwrap();
            let pixels = presenter.read_pixels().unwrap();
            let expected = expected_p010(codes, OutputMode::Sdr).map(|c| (c * 255.0).round() as u8);
            assert_close(
                pixel(&pixels, 32, 16, 16),
                expected,
                &format!("P010 {codes:?}"),
            );
        }
        presenter
            .present(&flat_p010(32, 32, [573, 512, 512]))
            .unwrap();
        let white = pixel(&presenter.read_pixels().unwrap(), 32, 3, 3);
        assert!(white.iter().all(|c| *c >= 245), "{white:?}");
    }

    /// HDR10 en un destino HDR (`Rgba16Float`, sRGB extendido): 1000 nits salen por encima de 1 y
    /// el SDR de siempre igual que en SDR.
    #[test]
    fn p010_hdr10_extends_above_sdr_white_on_hdr_targets() {
        let mut presenter = match OffscreenPresenter::hdr(16, 16, 8.0) {
            Ok(presenter) => presenter,
            Err(err) => {
                eprintln!("AVISO: test de render omitido ({err})");
                return;
            }
        };
        presenter
            .present(&flat_p010(16, 16, [723, 512, 512]))
            .unwrap();
        let pixels = presenter.read_pixels_f32().unwrap();
        let expected = expected_p010([723, 512, 512], OutputMode::Hdr { headroom: 8.0 });
        let got = pixels[8 * 16 + 8];
        assert!(got[0] > 1.5, "{got:?}");
        for c in 0..3 {
            assert!(
                (got[c] - expected[c]).abs() < 0.03,
                "{got:?} frente a {expected:?}"
            );
        }
        presenter
            .present(&flat_frame(16, 16, rgb_to_yuv([200, 100, 50])))
            .unwrap();
        let sdr = presenter.read_pixels_f32().unwrap()[0];
        assert!((sdr[0] - 200.0 / 255.0).abs() < 0.015, "{sdr:?}");
    }

    #[test]
    fn the_cursor_shape_is_drawn_at_its_hotspot() {
        let Some(mut presenter) = presenter(64, 64) else {
            return;
        };
        // Forma de 4 × 4: mitad izquierda roja opaca, mitad derecha transparente; punto activo en
        // (1, 1).
        let mut rgba = Vec::new();
        for _ in 0..4 {
            rgba.extend_from_slice(&[255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0]);
        }
        presenter.set_cursor_image(Some(std::sync::Arc::new(crate::CursorImage {
            id: 7,
            width: 4,
            height: 4,
            hotspot_x: 1,
            hotspot_y: 1,
            rgba,
        })));
        presenter.set_cursor(CursorOverlay {
            x: 20,
            y: 30,
            visible: true,
        });
        presenter
            .present(&flat_frame(64, 64, rgb_to_yuv([0, 0, 255])))
            .unwrap();
        let pixels = presenter.read_pixels().unwrap();
        // La forma empieza en (20 − 1, 30 − 1): columnas 19 y 20 rojas, 21 y 22 transparentes.
        assert_close(
            pixel(&pixels, 64, 19, 29),
            [255, 0, 0],
            "esquina de la forma",
        );
        assert_close(pixel(&pixels, 64, 20, 32), [255, 0, 0], "interior opaco");
        assert_close(
            pixel(&pixels, 64, 21, 30),
            [0, 0, 255],
            "parte transparente",
        );
        assert_close(pixel(&pixels, 64, 18, 30), [0, 0, 255], "fuera de la forma");
        assert_close(
            pixel(&pixels, 64, 20, 34),
            [0, 0, 255],
            "debajo de la forma",
        );
        // Sin forma vuelve el cuadrado blanco.
        presenter.set_cursor_image(None);
        presenter
            .present(&flat_frame(64, 64, rgb_to_yuv([0, 0, 255])))
            .unwrap();
        let pixels = presenter.read_pixels().unwrap();
        assert_close(
            pixel(&pixels, 64, 20, 30),
            [255, 255, 255],
            "cuadrado de respaldo",
        );
    }

    #[test]
    fn letterboxing_and_cursor() {
        let Some(mut presenter) = presenter(64, 64) else {
            return;
        };
        // Vídeo de 64×32 en un destino cuadrado: bandas de 16 píxeles arriba y abajo.
        presenter.set_cursor(CursorOverlay {
            x: 48,
            y: 16,
            visible: true,
        });
        presenter
            .present(&flat_frame(64, 32, rgb_to_yuv([0, 0, 255])))
            .unwrap();
        let pixels = presenter.read_pixels().unwrap();
        assert_close(pixel(&pixels, 64, 5, 5), [0, 0, 0], "banda superior");
        assert_close(pixel(&pixels, 64, 5, 60), [0, 0, 0], "banda inferior");
        assert_close(pixel(&pixels, 64, 5, 32), [0, 0, 255], "vídeo");
        // El cursor (48, 16) del vídeo cae en (48, 32) del destino: cuadrado blanco de 8 píxeles.
        assert_close(pixel(&pixels, 64, 48, 32), [255, 255, 255], "cursor");
        assert_close(
            pixel(&pixels, 64, 44, 28),
            [255, 255, 255],
            "borde del cursor",
        );
        assert_close(pixel(&pixels, 64, 40, 32), [0, 0, 255], "fuera del cursor");
    }
}
