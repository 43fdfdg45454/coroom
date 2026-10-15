//! El vídeo como capa de una pasada de dibujo.
//!
//! [`VideoLayer`] vive en el compositor de la ventana (una por fuente de vídeo; la pasada que
//! compone el vídeo y, encima, la textura de la interfaz) o, sin compositor, dentro del `Pipeline`
//! del *widget* de vídeo de la interfaz (`iced_wgpu`), siempre con el mismo `wgpu::Device` y la
//! misma cola que la interfaz. Antes de la pasada sube los planos del fotograma (I420 o NV12, una
//! copia de CPU a GPU) o importa la imagen Vulkan del decodificador sin copia
//! ([`crate::wgpu::VideoImporter`]); en la pasada dibuja el vídeo ajustado y el cursor del
//! anfitrión dentro del rectángulo que le dieron ([`VideoLayer::place`], [`VideoLayer::draw`]).

use std::sync::Arc;

use crate::video::VideoPipeline;
use crate::wgpu::VideoImporter;
use crate::{CursorImage, CursorOverlay, OutputMode, PresentFrame, PresentImage, RenderError};

/// Vídeo y cursor del anfitrión para dibujar dentro de otra pasada.
pub struct VideoLayer {
    video: VideoPipeline,
    cursor: Option<CursorOverlay>,
    cursor_image: Option<Arc<CursorImage>>,
    copies: u32,
    last_frame: Option<u64>,
}

impl std::fmt::Debug for VideoLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoLayer")
            .field("frame_size", &self.video.frame_size())
            .field("last_frame", &self.last_frame)
            .field("copies", &self.copies)
            .finish_non_exhaustive()
    }
}

impl VideoLayer {
    /// Capa para dibujar en vistas de formato `format` (con [`crate::hdr::HDR_FORMAT`], la salida
    /// es HDR en sRGB extendido; si no, SDR con tone mapping del HDR).
    pub fn new(
        device: &::wgpu::Device,
        queue: &::wgpu::Queue,
        format: ::wgpu::TextureFormat,
    ) -> Self {
        Self {
            video: VideoPipeline::new(device, queue, format),
            cursor: None,
            cursor_image: None,
            copies: 0,
            last_frame: None,
        }
    }

    /// Sube o importa `frame` si es nuevo. Una imagen Vulkan necesita el `importer` del dispositivo
    /// compartido y deja la señal de su semáforo para la próxima entrega de la cola: hay que dibujar
    /// y entregar en el mismo fotograma de la interfaz.
    pub fn upload(
        &mut self,
        device: &::wgpu::Device,
        queue: &::wgpu::Queue,
        importer: Option<&VideoImporter>,
        frame: &PresentFrame,
    ) -> Result<(), RenderError> {
        if self.last_frame == Some(frame.frame_id) {
            return Ok(());
        }
        match &frame.image {
            PresentImage::Vulkan(image) => {
                let Some(importer) = importer else {
                    return Err(RenderError::Unsupported(
                        "la ventana no comparte su dispositivo Vulkan con el decodificador".into(),
                    ));
                };
                importer.import(device, queue, &mut self.video, image)?;
                self.copies = 0;
            }
            _ => {
                self.video.upload(device, queue, frame)?;
                self.copies = 1;
            }
        }
        self.last_frame = Some(frame.frame_id);
        Ok(())
    }

    /// Cambia el destino: SDR o HDR con el margen de la pantalla ([`crate::hdr::headroom`]).
    pub fn set_output(&mut self, output: OutputMode) {
        self.video.set_output(output);
    }

    /// Cursor del anfitrión (posición en píxeles del fotograma y forma; `None` sin forma: el
    /// cuadrado de respaldo).
    pub fn set_cursor(
        &mut self,
        device: &::wgpu::Device,
        queue: &::wgpu::Queue,
        cursor: Option<CursorOverlay>,
        image: Option<Arc<CursorImage>>,
    ) {
        self.cursor = cursor;
        self.cursor_image = image;
        self.video
            .set_cursor_image(device, queue, self.cursor_image.as_ref());
    }

    /// Coloca el vídeo en `area` (`[x, y, ancho, alto]` en píxeles físicos del destino).
    pub fn place(&self, queue: &::wgpu::Queue, area: [f32; 4]) {
        self.video.update_params(queue, area, self.cursor);
    }

    /// Dibuja en la pasada (con el *viewport* en el área de [`VideoLayer::place`]).
    pub fn draw(&self, pass: &mut ::wgpu::RenderPass<'_>) {
        self.video.draw(pass);
    }

    /// Tamaño del fotograma mostrado (`None` si todavía no hay).
    pub fn frame_size(&self) -> Option<(u32, u32)> {
        let size = self.video.frame_size();
        (size.0 > 0 && size.1 > 0).then_some(size)
    }

    /// Último fotograma mostrado.
    pub fn last_frame(&self) -> Option<u64> {
        self.last_frame
    }

    /// Copias de CPU del último fotograma (1 al subir planos, 0 al importar).
    pub fn cpu_copies(&self) -> u32 {
        self.copies
    }
}
