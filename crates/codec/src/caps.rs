//! Capacidades de vídeo de esta máquina: qué codificadores y decodificadores funcionan de verdad.
//!
//! [`Capabilities::probe`] abre cada combinación de códec y backend con un contexto real (ver
//! `ffmpeg::probe`) y guarda el resultado para todo el proceso. OpenH264 (codificar y decodificar
//! H.264 por software) está siempre. Sin la feature `ffmpeg` no hay nada más.

use std::fmt;
use std::sync::OnceLock;

use crate::color::{Chroma, VideoFormat};
use crate::{Backend, Codec, DecoderBackend, decoder_label, encoder_label};

/// Si el codificador `codec` con `backend` admite `format` en esta versión.
///
/// Tabla fija, comprobada contra los formatos de píxel y los perfiles de cada codificador de
/// FFmpeg 8.1 (`ffmpeg -h encoder=…`; el test `encoder_pixel_formats_back_the_table` lo vuelve a
/// mirar en los codificadores de la build) y contra lo que hace el hardware de cada fabricante:
///
/// | | 4:4:4 (8 bits) | HDR10 (10 bits 4:2:0) |
/// |---|---|---|
/// | OpenH264 (H.264 software) | no | no |
/// | SVT-AV1 (AV1 software) | no (solo perfil *main*) | sí (`yuv420p10le`) |
/// | NVENC | H.264 *High 4:4:4* y HEVC *Rext* (`yuv444p`) | HEVC *Main10* y AV1 (`p010le`) |
/// | Vulkan Video | H.264 `high444p`, HEVC `rext`, AV1 `high` (NV24; según el driver) | HEVC `main10` y AV1 (P010) |
/// | QSV, VA-API, AMF | no (el 4:4:4 de Intel en HEVC pide formatos empaquetados que coroom aún no convierte) | HEVC y AV1 (P010) |
///
/// 4:4:4 y HDR a la vez no se negocian (el 4:4:4 de 10 bits apenas existe en hardware): la sesión
/// elige uno de los dos.
pub fn encoder_supports(codec: Codec, backend: Backend, format: VideoFormat) -> bool {
    if format.is_hdr() && format.chroma == Chroma::Yuv444 {
        return false;
    }
    if format.is_default() {
        return true;
    }
    match format.chroma {
        Chroma::Yuv444 => matches!(
            (backend, codec),
            (Backend::Nvenc, Codec::H264 | Codec::Hevc) | (Backend::VulkanVideo, _)
        ),
        Chroma::Yuv420 => match backend {
            Backend::Software => codec == Codec::Av1,
            _ => codec != Codec::H264,
        },
    }
}

/// Si el decodificador `codec` con `backend` admite `format`. Tabla fija, como
/// [`encoder_supports`]:
///
/// | | 4:4:4 (8 bits) | HDR10 (10 bits 4:2:0) |
/// |---|---|---|
/// | OpenH264 (H.264 software) | no | no |
/// | dav1d (AV1) y `hevc` de FFmpeg (software) | sí | sí |
/// | NVDEC | HEVC (Turing y posteriores) | HEVC y AV1 |
/// | Vulkan Video, VA-API, D3D11/D3D12 | no (Vulkan Video no tiene perfiles 4:4:4 de decodificación estándar) | HEVC y AV1 |
pub fn decoder_supports(codec: Codec, backend: DecoderBackend, format: VideoFormat) -> bool {
    if format.is_hdr() && format.chroma == Chroma::Yuv444 {
        return false;
    }
    if format.is_default() {
        return true;
    }
    if codec == Codec::H264 {
        // OpenH264 y los decodificadores de hardware de H.264: solo 4:2:0 de 8 bits.
        return false;
    }
    match (format.chroma, backend) {
        (_, DecoderBackend::Software) => true,
        (Chroma::Yuv444, DecoderBackend::Nvdec) => codec == Codec::Hevc,
        (Chroma::Yuv444, _) => false,
        (Chroma::Yuv420, _) => true,
    }
}

/// Lo que esta máquina codifica y decodifica, en orden de preferencia (por códec, AV1 → HEVC →
/// H.264; dentro de cada uno, del backend preferido al de respaldo).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// Codificadores que pasaron el sondeo.
    pub encoders: Vec<(Codec, Backend)>,
    /// Decodificadores que pasaron el sondeo.
    pub decoders: Vec<(Codec, DecoderBackend)>,
    /// Versión de FFmpeg cargada (`None` sin la feature `ffmpeg`).
    pub ffmpeg_version: Option<String>,
    /// Directorio de las bibliotecas de FFmpeg con el que se compiló.
    pub ffmpeg_dir: Option<String>,
    /// Codificadores y decodificadores que abren pero fallaron la comprobación de imagen (su
    /// imagen no coincide con la original), con el motivo: `«codificar AV1 con Vulkan Video: …»`.
    pub rejected: Vec<String>,
}

static PROBED: OnceLock<Capabilities> = OnceLock::new();

fn sort<B: Ord + Copy>(entries: &mut Vec<(Codec, B)>) {
    let rank = |codec: Codec| Codec::PREFERENCE.iter().position(|c| *c == codec);
    entries.sort_by_key(|&(codec, backend)| (rank(codec), backend));
    entries.dedup();
}

/// Como [`sort`], con los codificadores en el orden de [`Backend::encoder_rank`] (en Windows,
/// los nativos del fabricante antes que Vulkan Video).
fn sort_encoders(entries: &mut Vec<(Codec, Backend)>) {
    let rank = |codec: Codec| Codec::PREFERENCE.iter().position(|c| *c == codec);
    entries.sort_by_key(|&(codec, backend)| (rank(codec), backend.encoder_rank()));
    entries.dedup();
}

impl Capabilities {
    /// Sondea la máquina la primera vez (unos cientos de milisegundos; hasta unos segundos si un
    /// driver tarda) y devuelve el resultado guardado las siguientes.
    pub fn probe() -> Capabilities {
        PROBED.get_or_init(Self::run_probe).clone()
    }

    fn run_probe() -> Capabilities {
        let mut caps = Self::openh264_only();
        #[cfg(feature = "ffmpeg")]
        {
            let probed = crate::ffmpeg::probe::probe();
            caps.encoders.extend(probed.encoders);
            caps.decoders.extend(probed.decoders);
            caps.rejected = probed.rejected;
            caps.ffmpeg_version = Some(crate::ffmpeg::version());
            caps.ffmpeg_dir = Some(crate::ffmpeg::library_dir().to_owned());
        }
        sort_encoders(&mut caps.encoders);
        sort(&mut caps.decoders);
        caps
    }

    /// Solo H.264 por software con OpenH264, sin sondear nada.
    pub fn openh264_only() -> Capabilities {
        Capabilities {
            encoders: vec![(Codec::H264, Backend::Software)],
            decoders: vec![(Codec::H264, DecoderBackend::Software)],
            ffmpeg_version: None,
            ffmpeg_dir: None,
            rejected: Vec::new(),
        }
    }

    /// Capacidades con listas dadas (para pruebas y para limitar lo que se anuncia).
    pub fn with(
        mut encoders: Vec<(Codec, Backend)>,
        mut decoders: Vec<(Codec, DecoderBackend)>,
    ) -> Capabilities {
        sort_encoders(&mut encoders);
        sort(&mut decoders);
        Capabilities {
            encoders,
            decoders,
            ffmpeg_version: None,
            ffmpeg_dir: None,
            rejected: Vec::new(),
        }
    }

    /// Si algún codificador o decodificador usa la GPU.
    pub fn has_hardware(&self) -> bool {
        self.encoders.iter().any(|(_, b)| b.is_hardware())
            || self.decoders.iter().any(|(_, b)| b.is_hardware())
    }
}

impl fmt::Display for Capabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.ffmpeg_version, &self.ffmpeg_dir) {
            (Some(version), Some(dir)) => writeln!(f, "FFmpeg {version} (bibliotecas en {dir})")?,
            (Some(version), None) => writeln!(f, "FFmpeg {version}")?,
            _ => writeln!(f, "FFmpeg: no compilado en este binario (solo OpenH264)")?,
        }
        if !self.has_hardware() {
            writeln!(
                f,
                "Solo software: sin GPU detectada o sin códecs acelerados."
            )?;
        }
        writeln!(f, "{:<6} {:<12} Decodificar", "Códec", "Codificar")?;
        for codec in Codec::PREFERENCE {
            let join = |names: Vec<String>| {
                if names.is_empty() {
                    "—".to_owned()
                } else {
                    names.join(", ")
                }
            };
            let encoders = join(
                self.encoders
                    .iter()
                    .filter(|(c, _)| *c == codec)
                    .map(|(c, b)| encoder_label(*c, *b))
                    .collect(),
            );
            let decoders = join(
                self.decoders
                    .iter()
                    .filter(|(c, _)| *c == codec)
                    .map(|(c, b)| decoder_label(*c, *b))
                    .collect(),
            );
            writeln!(f, "{:<6} {encoders:<12} {decoders}", codec.to_string())?;
        }
        if !self.rejected.is_empty() {
            writeln!(
                f,
                "Descartados por la comprobación de imagen (abren, pero su vídeo sale dañado):"
            )?;
            for line in &self.rejected {
                writeln!(f, "  {line}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probing_never_fails_and_lists_the_software_codecs() {
        let caps = Capabilities::probe();
        eprintln!("{caps}");
        assert!(caps.encoders.contains(&(Codec::H264, Backend::Software)));
        assert!(
            caps.decoders
                .contains(&(Codec::H264, DecoderBackend::Software))
        );
        #[cfg(feature = "ffmpeg")]
        {
            assert!(caps.ffmpeg_version.is_some());
            assert!(
                caps.encoders.contains(&(Codec::Av1, Backend::Software)),
                "{caps}"
            );
            assert!(
                caps.decoders
                    .contains(&(Codec::Av1, DecoderBackend::Software)),
                "{caps}"
            );
            assert!(
                caps.decoders
                    .contains(&(Codec::Hevc, DecoderBackend::Software)),
                "{caps}"
            );
        }
        // Cacheado: la segunda vez es lo mismo, sin volver a sondear.
        assert_eq!(Capabilities::probe(), caps);
    }

    #[test]
    fn entries_are_sorted_by_preference() {
        let caps = Capabilities::with(
            vec![
                (Codec::H264, Backend::Software),
                (Codec::Av1, Backend::Software),
                (Codec::Av1, Backend::Nvenc),
            ],
            vec![(Codec::H264, DecoderBackend::Software)],
        );
        assert_eq!(
            caps.encoders,
            vec![
                (Codec::Av1, Backend::Nvenc),
                (Codec::Av1, Backend::Software),
                (Codec::H264, Backend::Software)
            ]
        );
        assert!(caps.has_hardware());
        let text = caps.to_string();
        assert!(text.contains("NVENC, SVT-AV1"), "{text}");
        assert!(
            Capabilities::openh264_only()
                .to_string()
                .contains("Solo software")
        );
    }
}
