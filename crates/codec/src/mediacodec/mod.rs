//! Decodificación por hardware con MediaCodec de Android, directa a la `Surface` de la vista del
//! vídeo (feature `mediacodec`).
//!
//! La ruta de menor latencia de Android: MediaCodec decodifica en la `Surface` de la
//! `SurfaceView` (sin copia a CPU, plano superpuesto del compositor) y la presentación suelta cada
//! fotograma a la pantalla con `AMediaCodec_releaseOutputBuffer(render = true)` o
//! `releaseOutputBufferAtTime` ([`SurfaceFrame::present`], [`SurfaceFrame::present_at`]).
//!
//! - [`info`]: lo que la aplicación cuenta de cada decodificador de `MediaCodecList`
//!   ([`AndroidDecoderInfo`]), la elección de los que se anuncian y los parámetros de
//!   configuración (solo claves documentadas: `low-latency`, `priority`, `operating-rate`).
//! - [`surface`]: [`SurfaceSlot`], el sitio de la superficie que actualiza la aplicación (giros,
//!   segundo plano), y [`SurfaceFrame`], el fotograma que entrega el decodificador.
//! - `decoder`: la máquina de estados del decodificador, independiente de la plataforma y
//!   probada en el equipo de desarrollo con un códec falso; la espera de la salida está
//!   explicada allí.
//! - `android` (solo `target_os = "android"`): la capa fina sobre la crate `ndk`.
//!
//! Para la sesión: [`crate::DecoderSet::android`] con la lista de la aplicación y el sitio de la
//! superficie anuncia solo lo que el teléfono decodifica por hardware
//! ([`crate::DecoderSet::limits`] da hasta qué tamaño y ritmo) y abre estos decodificadores con
//! [`crate::DecoderSet::open_format`] o [`crate::DecoderSet::open_stream`] como cualquier otro.

#[cfg(target_os = "android")]
mod android;
// Fuera de Android solo lo usan las pruebas (con un códec falso).
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
mod decoder;
pub mod info;
pub mod surface;

pub use decoder::{OUTPUT_WAIT_LIMIT, PIPELINE_EVIDENCE};
pub use info::{AndroidChoice, AndroidDecoderInfo, codec_from_mime, mime};
pub use surface::{OutputSurface, PresentOutcome, SurfaceFrame, SurfaceSlot};

use crate::{Codec, CodecError, DecodeLimits, Decoder, DecoderBackend, StreamHint, VideoFormat};

/// Lo que [`crate::DecoderSet::android`] guarda para abrir los decodificadores.
#[derive(Debug, Clone)]
pub struct AndroidSetup {
    choices: Vec<AndroidChoice>,
    slot: SurfaceSlot,
}

impl AndroidSetup {
    /// Elige entre `decoders` ([`info::choose`]) los que se usan con la superficie de `slot`.
    pub fn new(decoders: Vec<AndroidDecoderInfo>, slot: SurfaceSlot) -> Self {
        Self {
            choices: info::choose(decoders),
            slot,
        }
    }

    /// Por códec, los componentes elegidos.
    pub fn choices(&self) -> &[AndroidChoice] {
        &self.choices
    }

    /// El sitio de la superficie.
    pub fn slot(&self) -> &SurfaceSlot {
        &self.slot
    }

    /// Los códecs que decodifica, del preferido al de respaldo.
    pub fn codecs(&self) -> Vec<Codec> {
        self.choices.iter().map(|choice| choice.codec).collect()
    }

    fn choice(&self, codec: Codec) -> Option<&AndroidChoice> {
        self.choices.iter().find(|choice| choice.codec == codec)
    }

    /// Si algún componente de `codec` decodifica `format`.
    pub fn supports(&self, codec: Codec, format: VideoFormat) -> bool {
        self.choice(codec)
            .is_some_and(|choice| choice.supports(format))
    }

    /// Hasta qué tamaño y ritmo decodifica `codec`.
    pub fn limits_for(&self, codec: Codec) -> Option<DecodeLimits> {
        self.choice(codec).map(AndroidChoice::limits)
    }

    /// Lo que se puede anunciar sea cual sea el códec que elija el anfitrión, entre `codecs`: el
    /// mínimo de cada límite (el mensaje `Capabilities` lleva un solo tamaño y ritmo).
    pub fn limits_among(&self, codecs: &[Codec]) -> Option<DecodeLimits> {
        codecs
            .iter()
            .filter_map(|&codec| self.limits_for(codec))
            .reduce(|a, b| DecodeLimits {
                max_width: a.max_width.min(b.max_width),
                max_height: a.max_height.min(b.max_height),
                max_fps: a.max_fps.min(b.max_fps),
            })
    }

    /// Abre el decodificador de `codec` en `format` con el primer componente que lo admite y
    /// abre (si uno falla, el siguiente).
    pub fn open(
        &self,
        codec: Codec,
        format: VideoFormat,
        hint: Option<StreamHint>,
    ) -> Result<Box<dyn Decoder>, CodecError> {
        let Some(choice) = self.choice(codec) else {
            return Err(CodecError::DecoderUnavailable(DecoderBackend::MediaCodec));
        };
        let mut last = None;
        for info in choice
            .components
            .iter()
            .filter(|info| info.supports(format))
        {
            match open_component(codec, info, hint, &self.slot) {
                Ok(decoder) => return Ok(decoder),
                Err(err) => last = Some(err),
            }
        }
        Err(last.unwrap_or_else(|| {
            CodecError::Unsupported(format!(
                "ningún decodificador de {codec} por MediaCodec admite {format}"
            ))
        }))
    }
}

#[cfg(target_os = "android")]
fn open_component(
    codec: Codec,
    info: &AndroidDecoderInfo,
    hint: Option<StreamHint>,
    slot: &SurfaceSlot,
) -> Result<Box<dyn Decoder>, CodecError> {
    android::open(codec, info.clone(), hint, slot)
}

#[cfg(not(target_os = "android"))]
fn open_component(
    _codec: Codec,
    _info: &AndroidDecoderInfo,
    _hint: Option<StreamHint>,
    _slot: &SurfaceSlot,
) -> Result<Box<dyn Decoder>, CodecError> {
    Err(CodecError::DecoderUnavailable(DecoderBackend::MediaCodec))
}

// Con superficies y códecs falsos: solo en el equipo de desarrollo.
#[cfg(test)]
#[cfg(not(target_os = "android"))]
mod tests {
    use super::info::tests::info;
    use super::info::{MIME_AV1, MIME_H264, MIME_HEVC};
    use super::*;

    fn phone() -> AndroidSetup {
        let mut av1 = info("c2.example.av1.decoder", MIME_AV1);
        av1.max_width = 1920;
        av1.max_height = 1080;
        av1.max_fps = 120;
        av1.main10 = true;
        let mut hevc = info("c2.example.hevc.decoder", MIME_HEVC);
        hevc.achievable_fps = Some(90.0);
        hevc.max_fps = 120;
        let avc = info("c2.example.avc.decoder", MIME_H264);
        AndroidSetup::new(vec![avc, hevc, av1], SurfaceSlot::new())
    }

    #[test]
    fn the_setup_reports_codecs_formats_and_limits() {
        let setup = phone();
        assert_eq!(setup.codecs(), vec![Codec::Av1, Codec::Hevc, Codec::H264]);
        assert!(setup.supports(Codec::Av1, VideoFormat::HDR10_420));
        assert!(!setup.supports(Codec::Hevc, VideoFormat::HDR10_420));
        assert_eq!(
            setup.limits_for(Codec::Hevc),
            Some(DecodeLimits {
                max_width: 3840,
                max_height: 2160,
                max_fps: 90
            })
        );
        assert_eq!(
            setup.limits_among(&setup.codecs()),
            Some(DecodeLimits {
                max_width: 1920,
                max_height: 1080,
                max_fps: 60
            })
        );
        assert_eq!(setup.limits_among(&[]), None);
    }

    #[test]
    fn opening_off_android_says_the_backend_is_missing() {
        let setup = phone();
        assert_eq!(
            setup.open(Codec::Av1, VideoFormat::SDR_420, None).err(),
            Some(CodecError::DecoderUnavailable(DecoderBackend::MediaCodec))
        );
        // Un formato que ningún componente admite no llega a abrir nada.
        assert!(matches!(
            setup.open(Codec::Hevc, VideoFormat::HDR10_420, None),
            Err(CodecError::Unsupported(_))
        ));
        let empty = AndroidSetup::new(Vec::new(), SurfaceSlot::new());
        assert!(empty.codecs().is_empty());
        assert_eq!(
            empty.open(Codec::H264, VideoFormat::SDR_420, None).err(),
            Some(CodecError::DecoderUnavailable(DecoderBackend::MediaCodec))
        );
    }
}
