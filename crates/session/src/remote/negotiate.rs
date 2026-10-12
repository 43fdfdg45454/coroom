//! Negociación del códec y traducción entre los tipos de `coroom-codec` y
//! `coroom-capture` y los mensajes de control de `proto` (que no depende de otras crates).
//!
//! El invitado anuncia lo que decodifica ([`ControlMessage::Capabilities`]) justo después del
//! `Hello`; el anfitrión elige con [`choose_codec`] el mejor códec que él codifica y todos los
//! invitados conectados decodifican, lo anuncia ([`ControlMessage::CodecSelected`]) y arranca o
//! cambia el codificador. Un solo flujo para todos (sin simulcast).
//!
//! Orden de preferencia: primero lo que el anfitrión codifica por hardware, después lo que todos
//! los invitados decodifican por hardware y, a igualdad, AV1 → HEVC → H.264 (`docs/ARQUITECTURA.md` §3).
//! Así un anfitrión con NVENC H.264 y SVT-AV1 por software elige H.264 por hardware, y dos equipos
//! sin GPU eligen AV1 (SVT-AV1 → dav1d) antes que H.264 (OpenH264).
//!
//! Formato ([`choose_format`]): con el códec elegido, el primero de lo que pide el
//! anfitrión (HDR10, 4:4:4, y siempre 4:2:0 SDR al final) que su codificador admite y **todos** los
//! invitados aceptan (lo dicen en `Capabilities`, según sus decodificadores y sus Ajustes).

use std::sync::Arc;

use coroom_capture::CursorShape;
use coroom_codec::color::{Matrix, Range, Transfer};
use coroom_codec::{
    Backend, Chroma, Codec, ColorSpace, DecoderBackend, DecoderSet, EncoderSet, VideoFormat,
};
use coroom_transport::proto::control::{
    self, CURSOR_MAX_SIDE, Capabilities as WireCapabilities, ControlMessage,
};

/// Formato de `coroom-codec` en el cable: `(croma, espacio de color)` de `CodecSelected`.
pub fn format_to_wire(format: VideoFormat) -> (control::Chroma, control::ColorSpace) {
    let chroma = match format.chroma {
        Chroma::Yuv420 => control::Chroma::Yuv420,
        Chroma::Yuv444 => control::Chroma::Yuv444,
    };
    let color = control::ColorSpace {
        matrix: match format.color.matrix {
            Matrix::Bt709 => control::ColorMatrix::Bt709,
            Matrix::Bt2020 => control::ColorMatrix::Bt2020,
        },
        range: match format.color.range {
            Range::Limited => control::ColorRange::Limited,
            Range::Full => control::ColorRange::Full,
        },
        transfer: match format.color.transfer {
            Transfer::Sdr => control::ColorTransfer::Sdr,
            Transfer::Pq => control::ColorTransfer::Pq,
        },
    };
    (chroma, color)
}

/// Formato del cable en `coroom-codec`.
pub fn format_from_wire(chroma: control::Chroma, color: control::ColorSpace) -> VideoFormat {
    VideoFormat {
        chroma: match chroma {
            control::Chroma::Yuv420 => Chroma::Yuv420,
            control::Chroma::Yuv444 => Chroma::Yuv444,
        },
        color: ColorSpace {
            matrix: match color.matrix {
                control::ColorMatrix::Bt709 => Matrix::Bt709,
                control::ColorMatrix::Bt2020 => Matrix::Bt2020,
            },
            range: match color.range {
                control::ColorRange::Limited => Range::Limited,
                control::ColorRange::Full => Range::Full,
            },
            transfer: match color.transfer {
                control::ColorTransfer::Sdr => Transfer::Sdr,
                control::ColorTransfer::Pq => Transfer::Pq,
            },
        },
    }
}

/// Códec de `coroom-codec` en el cable.
pub fn codec_to_wire(codec: Codec) -> control::Codec {
    match codec {
        Codec::H264 => control::Codec::H264,
        Codec::Hevc => control::Codec::Hevc,
        Codec::Av1 => control::Codec::Av1,
    }
}

/// Códec del cable en `coroom-codec`.
pub fn codec_from_wire(codec: control::Codec) -> Codec {
    match codec {
        control::Codec::H264 => Codec::H264,
        control::Codec::Hevc => Codec::Hevc,
        control::Codec::Av1 => Codec::Av1,
    }
}

fn decoder_to_wire(backend: DecoderBackend) -> control::DecoderBackend {
    match backend {
        DecoderBackend::Vulkan => control::DecoderBackend::Vulkan,
        DecoderBackend::Nvdec => control::DecoderBackend::Nvdec,
        DecoderBackend::D3D12va => control::DecoderBackend::D3D12va,
        DecoderBackend::D3D11va => control::DecoderBackend::D3D11va,
        DecoderBackend::Vaapi => control::DecoderBackend::Vaapi,
        DecoderBackend::Software => control::DecoderBackend::Software,
    }
}

fn decoder_from_wire(backend: control::DecoderBackend) -> DecoderBackend {
    match backend {
        control::DecoderBackend::Vulkan => DecoderBackend::Vulkan,
        control::DecoderBackend::Nvdec => DecoderBackend::Nvdec,
        control::DecoderBackend::D3D12va => DecoderBackend::D3D12va,
        control::DecoderBackend::D3D11va => DecoderBackend::D3D11va,
        control::DecoderBackend::Vaapi => DecoderBackend::Vaapi,
        control::DecoderBackend::Software => DecoderBackend::Software,
    }
}

/// Lo que decodifica un invitado y hasta qué tamaño.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestCapabilities {
    /// Decodificadores, en su orden de preferencia.
    pub decoders: Vec<(Codec, DecoderBackend)>,
    /// Ancho máximo.
    pub max_width: u32,
    /// Alto máximo.
    pub max_height: u32,
    /// Fotogramas por segundo máximos.
    pub max_fps: u32,
    /// Códecs que acepta en 4:4:4.
    pub yuv444: Vec<Codec>,
    /// Códecs que acepta en HDR10.
    pub hdr10: Vec<Codec>,
}

impl GuestCapabilities {
    /// Límites por defecto que anuncia el invitado: 4K a 240 fps.
    pub const MAX: (u32, u32, u32) = (3840, 2160, 240);

    /// Con los límites por defecto.
    pub fn new(decoders: Vec<(Codec, DecoderBackend)>) -> Self {
        let (max_width, max_height, max_fps) = Self::MAX;
        Self {
            decoders,
            max_width,
            max_height,
            max_fps,
            yuv444: Vec::new(),
            hdr10: Vec::new(),
        }
    }

    /// Lo que decodifica `set` y, si se piden (Ajustes del invitado), los códecs con los que
    /// acepta 4:4:4 y HDR10 ([`coroom_codec::decoder_supports`]).
    pub fn from_decoders(set: &DecoderSet, yuv444: bool, hdr10: bool) -> Self {
        let mut caps = Self::new(set.available().to_vec());
        let codecs = set_codecs(set);
        if yuv444 {
            caps.yuv444 = codecs
                .iter()
                .copied()
                .filter(|&codec| set.supports(codec, VideoFormat::SDR_444))
                .collect();
        }
        if hdr10 {
            caps.hdr10 = codecs
                .iter()
                .copied()
                .filter(|&codec| set.supports(codec, VideoFormat::HDR10_420))
                .collect();
        }
        caps
    }

    /// Si acepta `codec` en `format`.
    pub fn accepts(&self, codec: Codec, format: VideoFormat) -> bool {
        if format.is_default() {
            return true;
        }
        if format.is_hdr() {
            format.chroma == Chroma::Yuv420 && self.hdr10.contains(&codec)
        } else {
            self.yuv444.contains(&codec)
        }
    }

    /// Lo que se supone de un invitado que no anuncia nada en [`super::CAPABILITIES_TIMEOUT`]:
    /// H.264 por software, que decodifica cualquiera.
    pub fn h264_fallback() -> Self {
        Self::new(vec![(Codec::H264, DecoderBackend::Software)])
    }

    /// Mensaje de control.
    pub fn to_message(&self) -> ControlMessage {
        ControlMessage::Capabilities(WireCapabilities {
            decoders: self
                .decoders
                .iter()
                .map(|&(codec, backend)| (codec_to_wire(codec), decoder_to_wire(backend)))
                .collect(),
            max_width: self.max_width,
            max_height: self.max_height,
            max_fps: self.max_fps,
            yuv444: self.yuv444.iter().copied().map(codec_to_wire).collect(),
            hdr10: self.hdr10.iter().copied().map(codec_to_wire).collect(),
        })
    }

    /// Del mensaje de control.
    pub fn from_wire(caps: &WireCapabilities) -> Self {
        Self {
            decoders: caps
                .decoders
                .iter()
                .map(|&(codec, backend)| (codec_from_wire(codec), decoder_from_wire(backend)))
                .collect(),
            max_width: caps.max_width,
            max_height: caps.max_height,
            max_fps: caps.max_fps,
            yuv444: caps.yuv444.iter().copied().map(codec_from_wire).collect(),
            hdr10: caps.hdr10.iter().copied().map(codec_from_wire).collect(),
        }
    }
}

/// Códecs de un conjunto de decodificadores, sin repetir.
fn set_codecs(set: &DecoderSet) -> Vec<Codec> {
    let mut codecs: Vec<Codec> = set.available().iter().map(|(codec, _)| *codec).collect();
    codecs.dedup();
    codecs
}

/// Formato del flujo de `codec` (ver el módulo): el primero de `candidates` (del preferido al de
/// siempre) que `host` codifica y todos los `guests` aceptan; 4:2:0 SDR si no.
pub fn choose_format(
    codec: Codec,
    candidates: &[VideoFormat],
    host: &EncoderSet,
    guests: &[GuestCapabilities],
) -> VideoFormat {
    candidates
        .iter()
        .copied()
        .find(|&format| {
            host.supports(codec, format) && guests.iter().all(|guest| guest.accepts(codec, format))
        })
        .unwrap_or(VideoFormat::SDR_420)
}

/// Elige el códec del flujo: el mejor que el anfitrión codifica (`host`) y todos los invitados
/// (`guests`) decodifican, con el orden del módulo. `None` si no hay invitados o no hay ninguno
/// común.
pub fn choose_codec(
    host: &[(Codec, Backend)],
    guests: &[Vec<(Codec, DecoderBackend)>],
) -> Option<Codec> {
    if guests.is_empty() {
        return None;
    }
    Codec::PREFERENCE
        .iter()
        .enumerate()
        .filter_map(|(rank, &codec)| {
            let host_hw = host
                .iter()
                .filter(|(c, _)| *c == codec)
                .map(|(_, backend)| backend.is_hardware())
                .reduce(|a, b| a || b)?;
            let mut guests_hw = true;
            for decoders in guests {
                let hw = decoders
                    .iter()
                    .filter(|(c, _)| *c == codec)
                    .map(|(_, backend)| backend.is_hardware())
                    .reduce(|a, b| a || b)?;
                guests_hw &= hw;
            }
            Some(((host_hw, guests_hw, std::cmp::Reverse(rank)), codec))
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, codec)| codec)
}

/// Forma del cursor de la captura como mensaje de control (`None` si no cabe en el protocolo).
pub fn shape_to_message(shape: &CursorShape) -> Option<ControlMessage> {
    let side = |value: u32| u16::try_from(value).ok().filter(|v| *v <= CURSOR_MAX_SIDE);
    Some(ControlMessage::CursorShape {
        shape_id: shape.shape_id,
        width: side(shape.width)?,
        height: side(shape.height)?,
        hotspot_x: side(shape.hotspot_x)?,
        hotspot_y: side(shape.hotspot_y)?,
        rgba: shape.rgba.clone(),
    })
}

/// Forma del cursor recibida, validada.
pub fn shape_from_message(
    shape_id: u64,
    (width, height): (u16, u16),
    (hotspot_x, hotspot_y): (u16, u16),
    rgba: Vec<u8>,
) -> Option<Arc<CursorShape>> {
    let shape = CursorShape {
        shape_id,
        width: u32::from(width),
        height: u32::from(height),
        hotspot_x: u32::from(hotspot_x),
        hotspot_y: u32::from(hotspot_y),
        rgba,
    };
    shape.is_valid().then(|| Arc::new(shape))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SW: DecoderBackend = DecoderBackend::Software;

    #[test]
    fn the_common_codec_wins_and_av1_is_preferred() {
        let host = [
            (Codec::Av1, Backend::Software),
            (Codec::H264, Backend::Software),
        ];
        // El invitado solo decodifica H.264: H.264.
        assert_eq!(
            choose_codec(&host, &[vec![(Codec::H264, SW)]]),
            Some(Codec::H264)
        );
        // Los dos con AV1: AV1.
        assert_eq!(
            choose_codec(&host, &[vec![(Codec::Av1, SW), (Codec::H264, SW)]]),
            Some(Codec::Av1)
        );
        // Un segundo invitado sin AV1 baja a H.264.
        assert_eq!(
            choose_codec(
                &host,
                &[
                    vec![(Codec::Av1, SW), (Codec::H264, SW)],
                    vec![(Codec::H264, SW)]
                ]
            ),
            Some(Codec::H264)
        );
        // Sin invitados o sin nada común, nada.
        assert_eq!(choose_codec(&host, &[]), None);
        assert_eq!(choose_codec(&host, &[vec![(Codec::Hevc, SW)]]), None);
        assert_eq!(choose_codec(&[], &[vec![(Codec::H264, SW)]]), None);
    }

    #[test]
    fn hardware_beats_the_codec_order() {
        // NVENC H.264 y SVT-AV1: gana H.264 por hardware.
        let host = [
            (Codec::Av1, Backend::Software),
            (Codec::H264, Backend::Nvenc),
            (Codec::H264, Backend::Software),
        ];
        let guest = vec![(Codec::Av1, SW), (Codec::H264, SW)];
        assert_eq!(choose_codec(&host, &[guest]), Some(Codec::H264));
        // Con HEVC y AV1 por hardware en los dos extremos, AV1.
        let host = [
            (Codec::Av1, Backend::VulkanVideo),
            (Codec::Hevc, Backend::VulkanVideo),
            (Codec::H264, Backend::Software),
        ];
        let guest = vec![
            (Codec::Av1, DecoderBackend::Vulkan),
            (Codec::Hevc, DecoderBackend::Nvdec),
            (Codec::H264, SW),
        ];
        assert_eq!(choose_codec(&host, &[guest]), Some(Codec::Av1));
        // Si el invitado solo decodifica HEVC por hardware, HEVC antes que AV1 por software.
        let guest = vec![
            (Codec::Av1, SW),
            (Codec::Hevc, DecoderBackend::Vaapi),
            (Codec::H264, SW),
        ];
        assert_eq!(choose_codec(&host, &[guest]), Some(Codec::Hevc));
    }

    /// 4:4:4 y HDR solo si el anfitrión los codifica y todos los invitados los aceptan; si no, el
    /// de siempre.
    #[test]
    fn the_format_needs_everyone() {
        let wanted = [
            VideoFormat::HDR10_420,
            VideoFormat::SDR_444,
            VideoFormat::SDR_420,
        ];
        let nvenc = EncoderSet::new([
            (Codec::Hevc, Backend::Nvenc),
            (Codec::Av1, Backend::Software),
        ]);
        let mut full = GuestCapabilities::new(vec![(Codec::Hevc, SW), (Codec::Av1, SW)]);
        full.yuv444 = vec![Codec::Hevc, Codec::Av1];
        full.hdr10 = vec![Codec::Hevc, Codec::Av1];
        let plain = GuestCapabilities::new(vec![(Codec::Hevc, SW)]);
        assert_eq!(
            choose_format(Codec::Hevc, &wanted, &nvenc, std::slice::from_ref(&full)),
            VideoFormat::HDR10_420
        );
        assert_eq!(
            choose_format(
                Codec::Hevc,
                &wanted[1..],
                &nvenc,
                std::slice::from_ref(&full)
            ),
            VideoFormat::SDR_444
        );
        assert_eq!(
            choose_format(Codec::Hevc, &wanted, &nvenc, &[full.clone(), plain]),
            VideoFormat::SDR_420,
            "un invitado sin 4:4:4 ni HDR baja a todos"
        );
        // SVT-AV1 hace HDR pero no 4:4:4.
        assert_eq!(
            choose_format(
                Codec::Av1,
                &wanted[1..],
                &nvenc,
                std::slice::from_ref(&full)
            ),
            VideoFormat::SDR_420
        );
        assert_eq!(
            choose_format(Codec::Av1, &wanted, &nvenc, &[full]),
            VideoFormat::HDR10_420
        );
        // Lo que anuncia un invitado sale de sus decodificadores y de sus Ajustes.
        let set = DecoderSet::new([(Codec::Av1, SW), (Codec::H264, SW)]);
        let caps = GuestCapabilities::from_decoders(&set, true, true);
        assert_eq!(caps.yuv444, vec![Codec::Av1], "OpenH264 no hace 4:4:4");
        assert_eq!(caps.hdr10, vec![Codec::Av1]);
        let off = GuestCapabilities::from_decoders(&set, false, false);
        assert!(off.yuv444.is_empty() && off.hdr10.is_empty());
        for format in wanted {
            let (chroma, color) = format_to_wire(format);
            assert_eq!(format_from_wire(chroma, color), format);
        }
    }

    #[test]
    fn capabilities_and_shapes_round_trip_through_the_wire() {
        let mut caps = GuestCapabilities::new(vec![
            (Codec::Av1, DecoderBackend::Vulkan),
            (Codec::H264, SW),
        ]);
        caps.hdr10 = vec![Codec::Av1];
        let ControlMessage::Capabilities(wire) = caps.to_message() else {
            panic!("se esperaba Capabilities");
        };
        assert_eq!(GuestCapabilities::from_wire(&wire), caps);
        for codec in Codec::PREFERENCE {
            assert_eq!(codec_from_wire(codec_to_wire(codec)), codec);
        }

        let shape = coroom_capture::synthetic::cursor_shape();
        let Some(ControlMessage::CursorShape {
            shape_id,
            width,
            height,
            hotspot_x,
            hotspot_y,
            rgba,
        }) = shape_to_message(&shape)
        else {
            panic!("se esperaba CursorShape");
        };
        let back = shape_from_message(shape_id, (width, height), (hotspot_x, hotspot_y), rgba)
            .expect("forma válida");
        assert_eq!(*back, shape);
        assert!(shape_from_message(1, (2, 2), (0, 0), vec![0; 3]).is_none());
    }
}
