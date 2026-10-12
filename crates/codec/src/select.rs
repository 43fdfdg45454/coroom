//! Elección del codificador y del decodificador concretos de un códec.
//!
//! [`EncoderSet`] y [`DecoderSet`] guardan qué combinaciones de códec y backend se pueden usar (lo
//! que dio [`Capabilities::probe`], quizá restringido por la línea de órdenes) y abren la mejor de un
//! códec, pasando a la siguiente si una falla al configurarse.
//!
//! [`DecoderSet::with_output`] dice dónde dejan los decodificadores sus imágenes
//! ([`DecodeOutput`]) y [`EncoderSet::gpu_input`] qué imágenes de GPU acepta sin copia el
//! codificador preferido (lo que la sesión le pide a la captura).

use crate::openh264::{OpenH264Decoder, OpenH264Encoder};
use crate::{
    Backend, Capabilities, Codec, CodecError, DecodeOutput, Decoder, DecoderBackend, Encoder,
    EncoderConfig, GpuInput, VideoFormat, decoder_supports, encoder_supports,
};

/// Crea el codificador de `codec` con `backend` (sin configurar).
pub fn create_encoder(codec: Codec, backend: Backend) -> Result<Box<dyn Encoder>, CodecError> {
    match (codec, backend) {
        (Codec::H264, Backend::Software) => Ok(Box::new(OpenH264Encoder::new())),
        #[cfg(feature = "ffmpeg")]
        (codec, backend) => Ok(Box::new(crate::ffmpeg::FfmpegEncoder::new(codec, backend)?)),
        #[cfg(not(feature = "ffmpeg"))]
        (_, backend) => Err(CodecError::BackendUnavailable(backend)),
    }
}

/// Crea el decodificador de `codec` con `backend` y la salida en memoria de CPU. H.264 por
/// software es OpenH264.
pub fn create_decoder(
    codec: Codec,
    backend: DecoderBackend,
) -> Result<Box<dyn Decoder>, CodecError> {
    create_decoder_with(codec, backend, &DecodeOutput::Cpu)
}

/// Como [`create_decoder`], con la salida que se pida (los decodificadores por software dejan
/// siempre la imagen en CPU).
pub fn create_decoder_with(
    codec: Codec,
    backend: DecoderBackend,
    output: &DecodeOutput,
) -> Result<Box<dyn Decoder>, CodecError> {
    let _ = output;
    match (codec, backend) {
        (Codec::H264, DecoderBackend::Software) => Ok(Box::new(OpenH264Decoder::new()?)),
        #[cfg(feature = "ffmpeg")]
        (codec, backend) => Ok(Box::new(crate::ffmpeg::FfmpegDecoder::with_output(
            codec,
            backend,
            output.clone(),
        )?)),
        #[cfg(not(feature = "ffmpeg"))]
        (_, backend) => Err(CodecError::DecoderUnavailable(backend)),
    }
}

/// Imágenes de GPU que aceptaría sin copia el codificador `backend` (sin abrirlo: en Vulkan se
/// consultan los modificadores DRM del dispositivo). `COROOM_ZERO_COPY=0` lo desactiva.
pub fn gpu_input_for(backend: Backend) -> GpuInput {
    if crate::gpu::zero_copy_disabled() || !backend.is_hardware() {
        return GpuInput::None;
    }
    #[cfg(feature = "ffmpeg")]
    {
        crate::ffmpeg::import::gpu_input_for(backend, None)
    }
    #[cfg(not(feature = "ffmpeg"))]
    {
        GpuInput::None
    }
}

/// Codificadores utilizables, en orden de preferencia.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderSet {
    available: Vec<(Codec, Backend)>,
}

impl EncoderSet {
    /// Los que pasaron el sondeo de esta máquina.
    pub fn probe() -> Self {
        Self::new(Capabilities::probe().encoders)
    }

    /// Un conjunto dado (se ordena por preferencia).
    pub fn new(available: impl IntoIterator<Item = (Codec, Backend)>) -> Self {
        let caps = Capabilities::with(available.into_iter().collect(), Vec::new());
        Self {
            available: caps.encoders,
        }
    }

    /// Solo OpenH264 (siempre disponible).
    pub fn openh264() -> Self {
        Self::new([(Codec::H264, Backend::Software)])
    }

    /// Se queda con `codec` (si es `Some`).
    pub fn only_codec(mut self, codec: Option<Codec>) -> Self {
        if let Some(codec) = codec {
            self.available.retain(|(c, _)| *c == codec);
        }
        self
    }

    /// Se queda con `backend` (si es `Some`).
    pub fn only_backend(mut self, backend: Option<Backend>) -> Self {
        if let Some(backend) = backend {
            self.available.retain(|(_, b)| *b == backend);
        }
        self
    }

    /// Combinaciones disponibles.
    pub fn available(&self) -> &[(Codec, Backend)] {
        &self.available
    }

    /// Si no queda ninguna.
    pub fn is_empty(&self) -> bool {
        self.available.is_empty()
    }

    /// Códecs disponibles, del preferido al de respaldo.
    pub fn codecs(&self) -> Vec<Codec> {
        let mut codecs: Vec<Codec> = self.available.iter().map(|(codec, _)| *codec).collect();
        codecs.dedup();
        codecs
    }

    /// Imágenes de GPU que acepta sin copia el codificador preferido (el primero de hardware, que
    /// es el que se abrirá si funciona); la sesión se las pide a la captura antes de saber qué
    /// códec se negocia.
    pub fn gpu_input(&self) -> GpuInput {
        self.available
            .iter()
            .map(|(_, backend)| *backend)
            .find(|backend| backend.is_hardware())
            .map_or(GpuInput::None, gpu_input_for)
    }

    /// Si algún backend de `codec` admite `format` ([`encoder_supports`]).
    pub fn supports(&self, codec: Codec, format: VideoFormat) -> bool {
        self.available
            .iter()
            .any(|&(c, backend)| c == codec && encoder_supports(c, backend, format))
    }

    /// Abre y configura el mejor codificador de `config.codec` que admite `config.format`; si un
    /// backend falla, prueba el siguiente.
    pub fn open(&self, config: &EncoderConfig) -> Result<Box<dyn Encoder>, CodecError> {
        let mut last = None;
        for (_, backend) in self.available.iter().filter(|&&(c, backend)| {
            c == config.codec && encoder_supports(c, backend, config.format)
        }) {
            match create_encoder(config.codec, *backend).and_then(|mut encoder| {
                encoder.configure(config)?;
                Ok(encoder)
            }) {
                Ok(encoder) => return Ok(encoder),
                Err(err) => last = Some(err),
            }
        }
        Err(last.unwrap_or_else(|| {
            CodecError::Unsupported(format!(
                "no hay codificador de {} en {} disponible",
                config.codec, config.format
            ))
        }))
    }
}

/// Decodificadores utilizables, en orden de preferencia, y dónde dejan las imágenes.
#[derive(Debug, Clone)]
pub struct DecoderSet {
    available: Vec<(Codec, DecoderBackend)>,
    output: DecodeOutput,
}

/// Dos conjuntos son iguales si ofrecen lo mismo y dejan las imágenes en el mismo sitio (CPU o
/// GPU; el dispositivo concreto no se compara).
impl PartialEq for DecoderSet {
    fn eq(&self, other: &Self) -> bool {
        self.available == other.available && self.output.is_gpu() == other.output.is_gpu()
    }
}

impl Eq for DecoderSet {}

impl DecoderSet {
    /// Los que pasaron el sondeo de esta máquina.
    pub fn probe() -> Self {
        Self::new(Capabilities::probe().decoders)
    }

    /// Un conjunto dado (se ordena por preferencia).
    pub fn new(available: impl IntoIterator<Item = (Codec, DecoderBackend)>) -> Self {
        let caps = Capabilities::with(Vec::new(), available.into_iter().collect());
        Self {
            available: caps.decoders,
            output: DecodeOutput::Cpu,
        }
    }

    /// Cambia dónde dejan los decodificadores las imágenes (por defecto, en CPU).
    /// `COROOM_ZERO_COPY=0` fuerza la salida en CPU.
    pub fn with_output(mut self, output: DecodeOutput) -> Self {
        self.output = if crate::gpu::zero_copy_disabled() {
            DecodeOutput::Cpu
        } else {
            output
        };
        self
    }

    /// Dónde dejan los decodificadores las imágenes.
    pub fn output(&self) -> &DecodeOutput {
        &self.output
    }

    /// Solo OpenH264 (siempre disponible).
    pub fn openh264() -> Self {
        Self::new([(Codec::H264, DecoderBackend::Software)])
    }

    /// Se queda con `codec` (si es `Some`).
    pub fn only_codec(mut self, codec: Option<Codec>) -> Self {
        if let Some(codec) = codec {
            self.available.retain(|(c, _)| *c == codec);
        }
        self
    }

    /// Se queda con `backend` (si es `Some`).
    pub fn only_backend(mut self, backend: Option<DecoderBackend>) -> Self {
        if let Some(backend) = backend {
            self.available.retain(|(_, b)| *b == backend);
        }
        self
    }

    /// Combinaciones disponibles (lo que se anuncia al anfitrión).
    pub fn available(&self) -> &[(Codec, DecoderBackend)] {
        &self.available
    }

    /// Si no queda ninguna.
    pub fn is_empty(&self) -> bool {
        self.available.is_empty()
    }

    /// Si algún backend de `codec` admite `format` ([`decoder_supports`]).
    pub fn supports(&self, codec: Codec, format: VideoFormat) -> bool {
        self.available
            .iter()
            .any(|&(c, backend)| c == codec && decoder_supports(c, backend, format))
    }

    /// Abre el mejor decodificador de `codec` (4:2:0 SDR); si un backend falla, prueba el
    /// siguiente.
    pub fn open(&self, codec: Codec) -> Result<Box<dyn Decoder>, CodecError> {
        self.open_format(codec, VideoFormat::SDR_420)
    }

    /// Abre el mejor decodificador de `codec` que admite `format`. Fuera del 4:2:0 SDR
    /// la imagen sale en CPU aunque se pidiera en el dispositivo de la ventana: la importación sin
    /// copias es solo de NV12.
    pub fn open_format(
        &self,
        codec: Codec,
        format: VideoFormat,
    ) -> Result<Box<dyn Decoder>, CodecError> {
        let output = match &self.output {
            DecodeOutput::Gpu { vulkan: Some(_) } if !format.is_default() => DecodeOutput::Cpu,
            other => other.clone(),
        };
        let mut last = None;
        for (_, backend) in self
            .available
            .iter()
            .filter(|&&(c, backend)| c == codec && decoder_supports(c, backend, format))
        {
            match create_decoder_with(codec, *backend, &output) {
                Ok(decoder) => return Ok(decoder),
                Err(err) => last = Some(err),
            }
        }
        Err(last.unwrap_or_else(|| {
            CodecError::Unsupported(format!(
                "no hay decodificador de {codec} en {format} disponible"
            ))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(codec: Codec) -> EncoderConfig {
        EncoderConfig {
            codec,
            width: 320,
            height: 180,
            fps: 30,
            bitrate_kbps: 1_000,
            slices: 1,
            format: VideoFormat::SDR_420,
        }
    }

    #[test]
    fn gpu_input_follows_the_preferred_hardware_backend() {
        assert_eq!(EncoderSet::openh264().gpu_input(), GpuInput::None);
        assert_eq!(gpu_input_for(Backend::Software), GpuInput::None);
        // Sin GPU, NVENC no importa nada en Linux (no hay DMA-BUF → CUDA en FFmpeg) y en Windows
        // pediría texturas D3D11.
        let nvenc = gpu_input_for(Backend::Nvenc);
        assert_eq!(nvenc == GpuInput::D3D11, cfg!(windows), "{nvenc:?}");
        let set = DecoderSet::openh264().with_output(DecodeOutput::Gpu { vulkan: None });
        assert!(set.output().is_gpu() != crate::gpu::zero_copy_disabled());
        assert_eq!(
            set.open(Codec::H264).unwrap().cpu_copies(),
            1,
            "OpenH264 copia a CPU"
        );
    }

    #[test]
    fn sets_filter_and_open_the_best_backend() {
        let set = EncoderSet::new([
            (Codec::H264, Backend::Software),
            (Codec::H264, Backend::Nvenc),
            (Codec::Av1, Backend::Software),
        ]);
        assert_eq!(set.codecs(), vec![Codec::Av1, Codec::H264]);
        let h264 = set.clone().only_codec(Some(Codec::H264));
        assert_eq!(
            h264.available(),
            &[
                (Codec::H264, Backend::Nvenc),
                (Codec::H264, Backend::Software)
            ]
        );
        // Sin GPU, NVENC falla y se abre OpenH264.
        let encoder = h264.open(&config(Codec::H264)).unwrap();
        assert_eq!(encoder.codec(), Codec::H264);
        assert!(set.clone().only_backend(Some(Backend::Amf)).is_empty());
        assert!(EncoderSet::new([]).open(&config(Codec::Hevc)).is_err());

        let decoders = DecoderSet::openh264();
        assert_eq!(decoders.open(Codec::H264).unwrap().codec(), Codec::H264);
        assert!(decoders.open(Codec::Av1).is_err());
    }
}
