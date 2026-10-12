//! Sondeo real de los codificadores y decodificadores de FFmpeg en esta máquina.
//!
//! Cada combinación se prueba en su propio hilo con un tiempo límite ([`PROBE_TIMEOUT`]): un driver
//! que se cuelga o un pánico solo descartan esa combinación.
//!
//! - **Codificadores**: se crea el dispositivo de la GPU que toque, se abre el codificador con un
//!   contexto de [`ENCODE_SIDE`]² y se codifican fotogramas hasta que sale un paquete.
//! - **Decodificadores**: se abre el decodificador (con el dispositivo y su `hwaccel`) y se
//!   decodifica una muestra real de [`SAMPLE_SIDE`]² hecha aquí mismo (OpenH264 para H.264,
//!   SVT-AV1 para AV1). Uno de hardware solo cuenta si el fotograma salió de verdad de la GPU (si
//!   el `hwaccel` falla, FFmpeg vuelve en silencio al software). Sin codificador HEVC por software
//!   no hay muestra de HEVC: el `hevc` por software cuenta si abre, y uno de hardware si el mismo
//!   backend decodificó H.264 de verdad y FFmpeg tiene `hwaccel` HEVC para ese dispositivo.
//!
//! En una máquina sin GPU (o con lavapipe, que crea el dispositivo Vulkan pero no tiene colas de
//! vídeo) solo queda el software.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rsmpeg::avcodec::AVCodec;
use rsmpeg::ffi;

use super::{FfmpegDecoder, FfmpegEncoder, decoder_device, decoder_name, encoder_name};
use crate::openh264::OpenH264Encoder;
use crate::{
    Backend, Codec, Decoder, DecoderBackend, EncodedSlice, Encoder, EncoderConfig, PixelFormat,
    RawFrame,
};

/// Tiempo límite de cada combinación.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Lado del contexto con el que se prueba cada codificador. Bastaría con 64×64, pero NVENC
/// y algunos drivers Vulkan rechazan anchos de menos de 129 o 145 píxeles en HEVC y H.264: con 256
/// se mide lo mismo sin falsos negativos.
pub(crate) const ENCODE_SIDE: u32 = 256;

/// Lado de las muestras de decodificación.
pub(crate) const SAMPLE_SIDE: u32 = 128;

/// Fotogramas que se entregan, como mucho, hasta que el codificador suelta un paquete.
const ENCODE_ATTEMPTS: u64 = 8;

/// Backends de codificación que tienen sentido en este sistema.
fn platform_encoders() -> Vec<Backend> {
    Backend::ALL
        .into_iter()
        .filter(|backend| cfg!(target_os = "linux") || *backend != Backend::Vaapi)
        .collect()
}

/// Backends de decodificación que tienen sentido en este sistema.
fn platform_decoders() -> Vec<DecoderBackend> {
    DecoderBackend::ALL
        .into_iter()
        .filter(|backend| match backend {
            DecoderBackend::D3D11va | DecoderBackend::D3D12va => cfg!(target_os = "windows"),
            DecoderBackend::Vaapi => cfg!(target_os = "linux"),
            _ => true,
        })
        .collect()
}

/// Ejecuta `job` en un hilo y espera como mucho [`PROBE_TIMEOUT`]. `None` si falló, tardó o hizo
/// pánico (el hilo colgado se abandona).
fn with_timeout<T: Send + 'static>(
    name: &str,
    job: impl FnOnce() -> Option<T> + Send + 'static,
) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name(format!("sondeo-{name}"))
        .spawn(move || {
            let _ = tx.send(job());
        });
    if spawned.is_err() {
        return None;
    }
    rx.recv_timeout(PROBE_TIMEOUT).ok().flatten()
}

/// Fotograma BGRA de prueba (degradado con movimiento según `frame_id`).
pub(crate) fn test_pattern(side: u32, frame_id: u64) -> RawFrame {
    let side_usize = side as usize;
    let mut data = vec![0u8; side_usize * side_usize * 4];
    for (i, pixel) in data.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = (i % side_usize, i / side_usize);
        let shift = frame_id as usize * 3;
        *pixel = [
            ((x + shift) * 255 / side_usize) as u8,
            (y * 255 / side_usize) as u8,
            (((x + y) / 2 + shift) % 256) as u8,
            255,
        ];
    }
    RawFrame {
        frame_id,
        width: side,
        height: side,
        format: PixelFormat::Bgra,
        stride: side * 4,
        data,
        gpu: None,
        color: crate::ColorSpace::SDR,
    }
}

fn probe_config(codec: Codec, side: u32) -> EncoderConfig {
    EncoderConfig {
        codec,
        width: side,
        height: side,
        fps: 30,
        bitrate_kbps: 1_000,
        slices: 1,
        format: crate::VideoFormat::SDR_420,
    }
}

/// Codifica fotogramas hasta que sale algo; devuelve los fotogramas codificados.
fn encode_some(encoder: &mut dyn Encoder, side: u32, frames: u64) -> Vec<Vec<EncodedSlice>> {
    let mut out = Vec::new();
    for frame_id in 0..ENCODE_ATTEMPTS.max(frames) {
        match encoder.encode(&test_pattern(side, frame_id)) {
            Ok(slices) if !slices.is_empty() => {
                out.push(slices);
                if out.len() as u64 >= frames {
                    break;
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    out
}

fn probe_encoder(codec: Codec, backend: Backend) -> bool {
    let compiled = encoder_name(codec, backend)
        .and_then(AVCodec::find_encoder_by_name)
        .is_some();
    if !compiled {
        return false;
    }
    with_timeout(&format!("{}-{}", codec.id(), backend.id()), move || {
        let mut encoder = FfmpegEncoder::new(codec, backend).ok()?;
        encoder.configure(&probe_config(codec, ENCODE_SIDE)).ok()?;
        (!encode_some(&mut encoder, ENCODE_SIDE, 1).is_empty()).then_some(())
    })
    .is_some()
}

/// Muestras para los decodificadores: dos fotogramas (intra y P) de cada códec que se puede
/// codificar por software aquí.
fn samples() -> Vec<(Codec, Vec<Vec<EncodedSlice>>)> {
    let mut out = Vec::new();
    let h264 = with_timeout("muestra-h264", || {
        let mut encoder = OpenH264Encoder::new();
        encoder
            .configure(&probe_config(Codec::H264, SAMPLE_SIDE))
            .ok()?;
        Some(encode_some(&mut encoder, SAMPLE_SIDE, 2))
    });
    if let Some(frames) = h264.filter(|frames| !frames.is_empty()) {
        out.push((Codec::H264, frames));
    }
    let av1 = with_timeout("muestra-av1", || {
        let mut encoder = FfmpegEncoder::new(Codec::Av1, Backend::Software).ok()?;
        encoder
            .configure(&probe_config(Codec::Av1, SAMPLE_SIDE))
            .ok()?;
        Some(encode_some(&mut encoder, SAMPLE_SIDE, 2))
    });
    if let Some(frames) = av1.filter(|frames| !frames.is_empty()) {
        out.push((Codec::Av1, frames));
    }
    out
}

/// Decodifica la muestra; con un backend de hardware, solo vale si salió de la GPU.
fn probe_decoder(codec: Codec, backend: DecoderBackend, sample: Vec<Vec<EncodedSlice>>) -> bool {
    with_timeout(&format!("dec-{}-{}", codec.id(), backend.id()), move || {
        let mut decoder = FfmpegDecoder::new(codec, backend).ok()?;
        let mut decoded = 0;
        for frame in &sample {
            if let Ok(Some(_)) = decoder.decode(frame) {
                decoded += 1;
            }
        }
        let (hw, _) = decoder.output_counts();
        let ok = decoded > 0 && (backend == DecoderBackend::Software || hw > 0);
        ok.then_some(())
    })
    .is_some()
}

/// Si FFmpeg tiene `hwaccel` de `codec` para el dispositivo de `backend` y lo puede abrir.
fn opens_with_hwaccel(codec: Codec, backend: DecoderBackend) -> bool {
    let Some(device) = decoder_device(backend) else {
        return false;
    };
    let Some(av_codec) = AVCodec::find_decoder_by_name(decoder_name(codec, backend)) else {
        return false;
    };
    let has_config = (0..)
        .map_while(|index| av_codec.hw_config(index))
        .any(|config| {
            config.device_type == device && config.methods & super::HW_CONFIG_DEVICE_CTX != 0
        });
    has_config
        && with_timeout(
            &format!("abre-{}-{}", codec.id(), backend.id()),
            move || FfmpegDecoder::new(codec, backend).ok().map(|_| ()),
        )
        .is_some()
}

/// Codificadores y decodificadores que pasaron el sondeo.
pub(crate) type Probed = (Vec<(Codec, Backend)>, Vec<(Codec, DecoderBackend)>);

/// Codificadores y decodificadores de FFmpeg que funcionan aquí, en orden de preferencia.
pub(crate) fn probe() -> Probed {
    super::init_logging();
    // El sondeo provoca errores esperados (sin CUDA, sin VA-API…): sin registro mientras dura.
    super::set_log_level(ffi::AV_LOG_QUIET);
    let mut encoders = Vec::new();
    for codec in Codec::PREFERENCE {
        for backend in platform_encoders() {
            if probe_encoder(codec, backend) {
                encoders.push((codec, backend));
            }
        }
    }
    let samples = samples();
    let mut decoders = Vec::new();
    // H.264 primero: el sondeo de HEVC por hardware se apoya en el de H.264 del mismo backend.
    for codec in [Codec::H264, Codec::Av1, Codec::Hevc] {
        let sample = samples
            .iter()
            .find(|(c, _)| *c == codec)
            .map(|(_, frames)| frames.clone());
        for backend in platform_decoders() {
            let works = match sample.clone() {
                Some(sample) => probe_decoder(codec, backend, sample),
                None if backend == DecoderBackend::Software => {
                    FfmpegDecoder::new(codec, backend).is_ok()
                }
                None if codec == Codec::Hevc => {
                    decoders.contains(&(Codec::H264, backend)) && opens_with_hwaccel(codec, backend)
                }
                None => false,
            };
            if works {
                decoders.push((codec, backend));
            }
        }
    }
    let rank = |codec: Codec| Codec::PREFERENCE.iter().position(|c| *c == codec);
    decoders.sort_by_key(|&(codec, backend)| (rank(codec), backend));
    super::set_log_level(super::default_log_level());
    (encoders, decoders)
}
