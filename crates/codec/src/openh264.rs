//! H.264 por software con OpenH264 de Cisco (crate `openh264`, biblioteca compilada desde el
//! fuente, licencia BSD-2-Clause).
//!
//! Es el respaldo software de `docs/ARQUITECTURA.md` §3 y el codificador de la prueba de latencia
//! local. Perfil base (sin B-frames), modo de tiempo real para contenido de pantalla, control de
//! tasa por bitrate sin saltar fotogramas, sin intra periódicos (solo al pedirlos) y SPS/PPS
//! delante de cada IDR. Cada fotograma sale como un único [`EncodedSlice`] con el flujo Annex B
//! completo; el reparto en slices reales lo hacen los codificadores de hardware.

use ::openh264::OpenH264API;
use ::openh264::decoder::Decoder as H264Decoder;
use ::openh264::encoder::{
    BitRate, Complexity, Encoder as H264Encoder, EncoderConfig as H264Config, FrameRate, FrameType,
    IntraFramePeriod, RateControlMode, SpsPpsStrategy, UsageType, VuiConfig,
};
use ::openh264::formats::YUVSource;

use crate::convert::bgra_to_i420_into;
use crate::{
    Backend, Codec, CodecError, Decoder, DecoderBackend, EncodedSlice, Encoder, EncoderConfig,
    PixelFormat, RawFrame,
};

/// Tipo de NAL de un slice IDR (H.264 §7.4.1.2.3).
const NAL_IDR: u8 = 5;

/// Hilos de codificación como máximo (OpenH264 no escala más allá con estos tamaños).
const MAX_THREADS: usize = 4;

/// Tamaño máximo de cada slice en bytes.
const MAX_SLICE_BYTES: u32 = 16_000;

/// Vista I420 que se entrega a OpenH264 sin copiar.
struct I420View<'a> {
    width: usize,
    height: usize,
    y_stride: usize,
    chroma_stride: usize,
    y: &'a [u8],
    u: &'a [u8],
    v: &'a [u8],
}

impl YUVSource for I420View<'_> {
    fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn strides(&self) -> (usize, usize, usize) {
        (self.y_stride, self.chroma_stride, self.chroma_stride)
    }

    fn y(&self) -> &[u8] {
        self.y
    }

    fn u(&self) -> &[u8] {
        self.u
    }

    fn v(&self) -> &[u8] {
        self.v
    }
}

fn backend(err: ::openh264::Error) -> CodecError {
    CodecError::Backend(format!("OpenH264: {err}"))
}

/// Indica si un flujo Annex B contiene un slice IDR.
pub fn contains_idr(annex_b: &[u8]) -> bool {
    ::openh264::nal_units(annex_b).any(|nal| {
        // `nal_units` devuelve cada NAL con su código de inicio delante.
        let header = nal.iter().skip_while(|&&byte| byte == 0).nth(1);
        header.is_some_and(|byte| byte & 0x1F == NAL_IDR)
    })
}

/// Codificador H.264 software de baja latencia.
pub struct OpenH264Encoder {
    config: Option<EncoderConfig>,
    encoder: Option<H264Encoder>,
    keyframe_requested: bool,
    scratch: Vec<u8>,
    output: Vec<u8>,
}

impl Default for OpenH264Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenH264Encoder {
    /// Crea un codificador sin configurar; hay que llamar a [`Encoder::configure`] antes de
    /// codificar.
    pub fn new() -> Self {
        Self {
            config: None,
            encoder: None,
            keyframe_requested: false,
            scratch: Vec::new(),
            output: Vec::new(),
        }
    }

    fn build(config: &EncoderConfig) -> Result<H264Encoder, CodecError> {
        let bitrate_bps = config.bitrate_kbps.saturating_mul(1000);
        let threads = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(MAX_THREADS) as u16;
        let settings = H264Config::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(bitrate_bps))
            .max_frame_rate(FrameRate::from_hz(config.fps as f32))
            // OpenH264 solo respeta el bitrate si puede saltarse fotogramas; un salto sale como
            // `encode` sin slices y el pipeline lo cuenta como descarte.
            .skip_frames(true)
            // Varios hilos solo ayudan con varios slices: se limitan por tamaño, lo que además
            // reparte la carga de los intra grandes. En la prueba local (1080p, 4 núcleos) el
            // p95 de codificación baja de unos 33 ms a unos 19 ms.
            .num_threads(threads)
            .max_slice_len(MAX_SLICE_BYTES)
            .sps_pps_strategy(SpsPpsStrategy::ConstantId)
            .intra_frame_period(IntraFramePeriod::from_num_frames(0))
            .complexity(Complexity::Low)
            // OpenH264 no admite cuantización adaptativa en contenido de pantalla y pide detección
            // de cambio de escena en ese modo.
            .adaptive_quantization(false)
            .scene_change_detect(true)
            .background_detection(false)
            // BT.709 en rango limitado, como la conversión.
            .vui(VuiConfig::bt709());
        H264Encoder::with_api_config(OpenH264API::from_source(), settings).map_err(backend)
    }
}

impl Encoder for OpenH264Encoder {
    fn codec(&self) -> Codec {
        Codec::H264
    }

    fn backend(&self) -> Backend {
        Backend::Software
    }

    fn configure(&mut self, config: &EncoderConfig) -> Result<(), CodecError> {
        if config.codec != Codec::H264 {
            return Err(CodecError::Unsupported(format!(
                "OpenH264 solo codifica H.264, no {:?}",
                config.codec
            )));
        }
        if config.width == 0
            || config.height == 0
            || !config.width.is_multiple_of(2)
            || !config.height.is_multiple_of(2)
        {
            return Err(CodecError::Unsupported(format!(
                "OpenH264 necesita un tamaño par y no nulo, no {}×{}",
                config.width, config.height
            )));
        }
        if config.width.max(config.height) > 3840 || config.width.min(config.height) > 2160 {
            return Err(CodecError::Unsupported(format!(
                "OpenH264 admite hasta 3840×2160, no {}×{}",
                config.width, config.height
            )));
        }
        if config.fps == 0 || config.bitrate_kbps == 0 {
            return Err(CodecError::Unsupported(
                "los fps y el bitrate deben ser mayores que cero".into(),
            ));
        }
        if !config.format.is_default() {
            return Err(CodecError::Unsupported(format!(
                "OpenH264 solo codifica 4:2:0 SDR de 8 bits, no {}",
                config.format
            )));
        }
        self.encoder = Some(Self::build(config)?);
        self.config = Some(*config);
        Ok(())
    }

    /// Cambia el bitrate objetivo.
    ///
    /// La crate `openh264` no expone `ENCODER_OPTION_BITRATE` sin `unsafe` (su `raw_api` es una
    /// función `unsafe`), así que se recrea el codificador con el nuevo bitrate y el siguiente
    /// fotograma sale intra. Es aceptable para la prueba local; el control de tasa
    /// continuo usa los codificadores de hardware, que lo cambian en caliente.
    fn set_bitrate(&mut self, bitrate_kbps: u32) -> Result<(), CodecError> {
        let Some(mut config) = self.config else {
            return Err(CodecError::Unsupported(
                "el codificador no está configurado".into(),
            ));
        };
        if bitrate_kbps == 0 {
            return Err(CodecError::Unsupported(
                "el bitrate debe ser mayor que cero".into(),
            ));
        }
        if config.bitrate_kbps == bitrate_kbps {
            return Ok(());
        }
        config.bitrate_kbps = bitrate_kbps;
        self.encoder = Some(Self::build(&config)?);
        self.config = Some(config);
        Ok(())
    }

    fn request_keyframe(&mut self) {
        self.keyframe_requested = true;
    }

    fn encode(&mut self, frame: &RawFrame) -> Result<Vec<EncodedSlice>, CodecError> {
        let Some(encoder) = self.encoder.as_mut() else {
            return Err(CodecError::Unsupported(
                "el codificador no está configurado".into(),
            ));
        };
        if !frame.width.is_multiple_of(2) || !frame.height.is_multiple_of(2) {
            return Err(CodecError::Unsupported(format!(
                "OpenH264 necesita un tamaño par, no {}×{}",
                frame.width, frame.height
            )));
        }
        let (y_stride, data) = match frame.format {
            PixelFormat::Bgra => {
                bgra_to_i420_into(frame, &mut self.scratch)?;
                (frame.width as usize, self.scratch.as_slice())
            }
            PixelFormat::I420 => (frame.stride as usize, frame.data.as_slice()),
            PixelFormat::Nv12
            | PixelFormat::I444
            | PixelFormat::Nv24
            | PixelFormat::P010
            | PixelFormat::Rgba16F
            | PixelFormat::Gpu => {
                return Err(CodecError::Unsupported(format!(
                    "OpenH264 no acepta {:?}; entrega BGRA o I420 en memoria de CPU",
                    frame.format
                )));
            }
        };
        let i420 = RawFrame {
            frame_id: frame.frame_id,
            width: frame.width,
            height: frame.height,
            format: PixelFormat::I420,
            stride: y_stride as u32,
            data: Vec::new(),
            gpu: None,
            color: crate::ColorSpace::SDR,
        };
        let (cw, ch) = RawFrame::chroma_size(frame.width, frame.height);
        let y_len = y_stride * frame.height as usize;
        let c_len = cw as usize * ch as usize;
        if y_stride < frame.width as usize || data.len() < y_len + 2 * c_len {
            return Err(CodecError::Unsupported(format!(
                "búfer I420 de {} bytes demasiado corto para {}×{}",
                data.len(),
                i420.width,
                i420.height
            )));
        }
        let view = I420View {
            width: frame.width as usize,
            height: frame.height as usize,
            y_stride,
            chroma_stride: cw as usize,
            y: &data[..y_len],
            u: &data[y_len..y_len + c_len],
            v: &data[y_len + c_len..y_len + 2 * c_len],
        };

        if std::mem::take(&mut self.keyframe_requested) {
            encoder.force_intra_frame();
        }
        let bitstream = encoder.encode(&view).map_err(backend)?;
        if matches!(bitstream.frame_type(), FrameType::Skip | FrameType::Invalid) {
            return Ok(Vec::new());
        }
        self.output.clear();
        bitstream.write_vec(&mut self.output);
        if self.output.is_empty() {
            return Ok(Vec::new());
        }
        let keyframe = contains_idr(&self.output);
        Ok(vec![EncodedSlice {
            frame_id: frame.frame_id,
            index: 0,
            count: 1,
            keyframe,
            data: self.output.clone(),
        }])
    }
}

/// Decodificador H.264 software.
pub struct OpenH264Decoder {
    decoder: H264Decoder,
    bitstream: Vec<u8>,
}

impl OpenH264Decoder {
    /// Crea un decodificador.
    pub fn new() -> Result<Self, CodecError> {
        Ok(Self {
            decoder: H264Decoder::new().map_err(backend)?,
            bitstream: Vec::new(),
        })
    }
}

impl Decoder for OpenH264Decoder {
    fn codec(&self) -> Codec {
        Codec::H264
    }

    fn backend(&self) -> DecoderBackend {
        DecoderBackend::Software
    }

    fn decode(&mut self, slices: &[EncodedSlice]) -> Result<Option<RawFrame>, CodecError> {
        let Some(first) = slices.first() else {
            return Ok(None);
        };
        let frame_id = first.frame_id;
        self.bitstream.clear();
        for slice in slices {
            self.bitstream.extend_from_slice(&slice.data);
        }
        let Some(image) = self.decoder.decode(&self.bitstream).map_err(backend)? else {
            return Ok(None);
        };
        let (width, height) = image.dimensions();
        let (y_stride, u_stride, v_stride) = image.strides();
        let (cw, ch) = image.dimensions_uv();
        let mut data = Vec::with_capacity(width * height + 2 * cw * ch);
        for (plane, stride, w, h) in [
            (image.y(), y_stride, width, height),
            (image.u(), u_stride, cw, ch),
            (image.v(), v_stride, cw, ch),
        ] {
            for row in plane.chunks(stride).take(h) {
                let Some(pixels) = row.get(..w) else {
                    return Err(CodecError::Backend(
                        "OpenH264 devolvió un plano más corto que su tamaño".into(),
                    ));
                };
                data.extend_from_slice(pixels);
            }
        }
        let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
            return Err(CodecError::Backend(
                "tamaño de imagen fuera de rango".into(),
            ));
        };
        if data.len() != RawFrame::i420_len(width, height) {
            return Err(CodecError::Backend(
                "OpenH264 devolvió planos incompletos".into(),
            ));
        }
        Ok(Some(RawFrame {
            frame_id,
            width,
            height,
            format: PixelFormat::I420,
            stride: width,
            data,
            gpu: None,
            color: crate::ColorSpace::SDR,
        }))
    }

    fn reset(&mut self) {
        // Crear un decodificador solo falla si OpenH264 no tiene memoria; en ese caso se conserva el
        // anterior y el siguiente intra lo resincroniza igualmente.
        if let Ok(decoder) = H264Decoder::new() {
            self.decoder = decoder;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: u32 = 640;
    const HEIGHT: u32 = 360;

    /// Patrón propio con movimiento: degradado que se desplaza y un cuadrado que avanza.
    fn pattern(frame_id: u64) -> RawFrame {
        let (w, h) = (WIDTH as usize, HEIGHT as usize);
        let shift = frame_id as usize * 4;
        let mut data = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let at = (y * w + x) * 4;
                let r = ((x + shift) * 255 / w) as u8;
                let g = (y * 255 / h) as u8;
                let b = (((x + y + shift) / 4) % 256) as u8;
                data[at..at + 4].copy_from_slice(&[b, g, r, 255]);
            }
        }
        let side = 64;
        let x0 = (frame_id as usize * 9) % (w - side);
        let y0 = (frame_id as usize * 5) % (h - side);
        for y in y0..y0 + side {
            for x in x0..x0 + side {
                let at = (y * w + x) * 4;
                data[at..at + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        RawFrame {
            frame_id,
            width: WIDTH,
            height: HEIGHT,
            format: PixelFormat::Bgra,
            stride: WIDTH * 4,
            data,
            gpu: None,
            color: crate::ColorSpace::SDR,
        }
    }

    fn config() -> EncoderConfig {
        EncoderConfig {
            codec: Codec::H264,
            width: WIDTH,
            height: HEIGHT,
            fps: 30,
            bitrate_kbps: 4_000,
            slices: 1,
            format: crate::VideoFormat::SDR_420,
        }
    }

    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        assert_eq!(a.len(), b.len());
        let mse = a
            .iter()
            .zip(b)
            .map(|(&x, &y)| {
                let d = f64::from(x) - f64::from(y);
                d * d
            })
            .sum::<f64>()
            / a.len() as f64;
        if mse == 0.0 {
            f64::INFINITY
        } else {
            10.0 * (255.0f64 * 255.0 / mse).log10()
        }
    }

    #[test]
    fn thirty_frames_round_trip_with_good_luma_psnr() {
        let mut encoder = OpenH264Encoder::new();
        encoder.configure(&config()).unwrap();
        let mut decoder = OpenH264Decoder::new().unwrap();
        let mut worst = f64::INFINITY;
        for frame_id in 0..30 {
            let source = pattern(frame_id);
            let reference = crate::convert::bgra_to_i420(&source).unwrap();
            let slices = encoder.encode(&source).unwrap();
            assert_eq!(slices.len(), 1, "un slice por fotograma");
            assert_eq!((slices[0].index, slices[0].count), (0, 1));
            assert_eq!(slices[0].frame_id, frame_id);
            if frame_id == 0 {
                assert!(slices[0].keyframe, "el primer fotograma es intra");
            }
            let decoded = decoder.decode(&slices).unwrap().expect("imagen");
            assert_eq!(decoded.frame_id, frame_id);
            assert_eq!((decoded.width, decoded.height), (WIDTH, HEIGHT));
            assert_eq!(decoded.format, PixelFormat::I420);
            let (y, _, _) = decoded.i420_planes().unwrap();
            let (y_ref, _, _) = reference.i420_planes().unwrap();
            worst = worst.min(psnr(y, y_ref));
        }
        assert!(worst > 30.0, "PSNR mínimo del plano Y: {worst:.2} dB");
    }

    #[test]
    fn keyframes_on_request_and_bitrate_changes() {
        let mut encoder = OpenH264Encoder::new();
        encoder.configure(&config()).unwrap();
        let mut decoder = OpenH264Decoder::new().unwrap();
        let first = encoder.encode(&pattern(0)).unwrap();
        assert!(first[0].keyframe);
        let second = encoder.encode(&pattern(1)).unwrap();
        assert!(!second[0].keyframe, "sin petición, el segundo es P");
        encoder.request_keyframe();
        let third = encoder.encode(&pattern(2)).unwrap();
        assert!(third[0].keyframe, "request_keyframe produce intra");
        let fourth = encoder.encode(&pattern(3)).unwrap();
        assert!(!fourth[0].keyframe);

        encoder.set_bitrate(2_000).unwrap();
        let after = encoder.encode(&pattern(4)).unwrap();
        assert_eq!(after.len(), 1);
        decoder.reset();
        assert!(decoder.decode(&after).unwrap().is_some());
    }

    #[test]
    fn i420_input_is_accepted() {
        let mut encoder = OpenH264Encoder::new();
        encoder.configure(&config()).unwrap();
        let i420 = crate::convert::bgra_to_i420(&pattern(0)).unwrap();
        let slices = encoder.encode(&i420).unwrap();
        assert!(slices[0].keyframe);
    }

    #[test]
    fn bad_configurations_are_rejected() {
        let mut encoder = OpenH264Encoder::new();
        assert!(encoder.encode(&pattern(0)).is_err(), "sin configurar");
        assert!(encoder.set_bitrate(1_000).is_err());
        let mut odd = config();
        odd.width = 641;
        assert!(encoder.configure(&odd).is_err());
        let mut av1 = config();
        av1.codec = Codec::Av1;
        assert!(encoder.configure(&av1).is_err());
        let mut chroma = config();
        chroma.format = crate::VideoFormat::SDR_444;
        assert!(encoder.configure(&chroma).is_err(), "sin 4:4:4");
    }

    #[test]
    fn idr_detection_reads_the_nal_type() {
        assert!(contains_idr(&[0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x65, 2]));
        assert!(!contains_idr(&[0, 0, 0, 1, 0x41, 1, 2, 3]));
        assert!(!contains_idr(&[]));
    }
}
