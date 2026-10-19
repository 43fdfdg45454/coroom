//! Sondeo real de los codificadores y decodificadores de FFmpeg en esta máquina.
//!
//! Cada combinación se prueba en su propio hilo con un tiempo límite ([`PROBE_TIMEOUT`]): un driver
//! que se cuelga o un pánico solo descartan esa combinación.
//!
//! - **Codificadores**: se crea el dispositivo de la GPU que toque, se abre el codificador con un
//!   contexto de [`ENCODE_SIDE`]² y se codifican fotogramas hasta que sale un paquete. Los de
//!   hardware pasan además la **comprobación de imagen** ([`check_encoder`]): codifican
//!   [`CHECK_FRAMES`] fotogramas de un patrón en movimiento (unos 10 s de vídeo, con sus fotogramas
//!   predichos), se decodifican por software y cada uno se compara con el original
//!   ([`MIN_PSNR_DB`]). Un driver que codifica mal (AV1 por Vulkan Video con algunos
//!   drivers de NVIDIA, por ejemplo) queda fuera en ese equipo, y se dice por qué
//!   ([`Probed::rejected`]); cuando el driver se arregle, el sondeo lo vuelve a aceptar solo.
//! - **Decodificadores**: se abre el decodificador (con el dispositivo y su `hwaccel`) y se
//!   decodifica una muestra real de [`SAMPLE_SIDE`]² hecha aquí mismo (OpenH264 para H.264,
//!   SVT-AV1 para AV1). Uno de hardware solo cuenta si el fotograma salió de verdad de la GPU (si
//!   el `hwaccel` falla, FFmpeg vuelve en silencio al software) y si su imagen coincide con la
//!   original, como en la comprobación de los codificadores. La muestra de HEVC es la del primer
//!   codificador HEVC por hardware que pasó la comprobación; sin él, el `hevc` por software cuenta
//!   si abre, y uno de hardware si el mismo backend decodificó H.264 de verdad y FFmpeg tiene
//!   `hwaccel` HEVC para ese dispositivo.
//!
//! En una máquina sin GPU (o con lavapipe, que crea el dispositivo Vulkan pero no tiene colas de
//! vídeo) solo queda el software.
//!
//! **Vulkan Video en un proceso hijo** ([`super::probe_child`]): un cargador Vulkan viejo
//! (`libvulkan1` 1.3.239 de Debian 12 con un driver de 2026) hace que `avcodec_open2` de
//! `av1_vulkan` **muera con SIGSEGV**, y un hilo con tiempo límite no protege de eso. Cuando el
//! binario lo ha activado ([`super::probe_child::run_if_requested`]), el primer `open` de cada
//! codificador y decodificador Vulkan se hace en un proceso hijo (el mismo ejecutable con una
//! variable de entorno); si el hijo muere, la combinación queda «no disponible» con el motivo y
//! el sondeo sigue.

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

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

/// Fotogramas de la comprobación de imagen de un codificador por hardware: 10 s a 30 fps, para
/// que salgan también los fallos que aparecen con los fotogramas predichos o al cabo de unos
/// segundos (al dar la vuelta el contador de orden de AV1, de 8 bits, por ejemplo).
pub(crate) const CHECK_FRAMES: u64 = 300;

/// Fotogramas de cada muestra de los decodificadores.
const SAMPLE_FRAMES: u64 = 30;

/// PSNR de luma mínimo de cada fotograma comprobado. El patrón a 1 Mbit/s sale por encima de
/// 35 dB con cualquier codificador sano; un flujo corrupto da en torno a 10.
pub(crate) const MIN_PSNR_DB: f64 = 25.0;

/// Tiempo límite de la comprobación de un codificador (abrirlo, 300 fotogramas y decodificarlos).
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

/// Lo que puede durar el bucle de la comprobación ([`check_encoder`]): menos que
/// [`CHECK_TIMEOUT`], para que un codificador lento (que no colgado) acabe con lo que se midió
/// (cuántos fotogramas y cuánto tardó cada uno) en vez de con un «no terminó» sin datos.
const CHECK_BUDGET: Duration = Duration::from_secs(17);

/// Lo más que puede tardar [`probe`]: cada paso espera como mucho su tiempo límite (el hilo que
/// no responde se abandona), así que la suma de todos acota el sondeo entero. Lo usa el padre del
/// sondeo en proceso hijo ([`super::probe_child`]) para no esperar a un hijo colgado.
pub(crate) fn worst_case() -> Duration {
    let child = super::probe_child::CHILD_TIMEOUT;
    let combinations = |backends: usize, codecs: usize| (backends * codecs) as u32;
    let encoders = combinations(platform_encoders().len(), Codec::PREFERENCE.len());
    let decoders = combinations(platform_decoders().len(), Codec::PREFERENCE.len());
    // Codificador: el hijo de Vulkan Video, abrir y la comprobación de imagen. Muestras: H.264 y
    // AV1. Decodificador: el hijo de Vulkan, decodificar la muestra o abrir con `hwaccel`.
    (child + PROBE_TIMEOUT + CHECK_TIMEOUT) * encoders
        + PROBE_TIMEOUT * 2
        + (child + PROBE_TIMEOUT) * decoders
}

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
            // No es de FFmpeg: lo describe la aplicación de Android (`DecoderSet::android`).
            DecoderBackend::MediaCodec => false,
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
    with_timeout_of(PROBE_TIMEOUT, name, job)
}

/// Como [`with_timeout`], con otro tiempo límite.
fn with_timeout_of<T: Send + 'static>(
    timeout: Duration,
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
    rx.recv_timeout(timeout).ok().flatten()
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
        pool: None,
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

/// Codifica fotogramas hasta que salen `frames`; devuelve los fotogramas codificados.
fn encode_some(encoder: &mut dyn Encoder, side: u32, frames: u64) -> Vec<Vec<EncodedSlice>> {
    let mut out = Vec::new();
    for frame_id in 0..ENCODE_ATTEMPTS + frames {
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

/// Resultado del sondeo de un codificador.
enum EncoderProbe {
    /// No está en esta build (no se dice nada).
    Missing,
    /// Está en la build pero no abre o no codifica aquí (lo normal sin esa GPU): por qué.
    Unavailable(String),
    /// Abre, pero su imagen no se decodifica bien: por qué.
    Rejected(String),
    /// Funciona; con hardware, los primeros fotogramas de la comprobación (muestra para los
    /// decodificadores).
    Works(Vec<Vec<EncodedSlice>>),
}

/// Abre el codificador con la configuración del sondeo y codifica hasta que sale un paquete.
pub(super) fn open_and_encode(codec: Codec, backend: Backend) -> Result<(), String> {
    // Primero el dispositivo por separado, para tener el motivo de FFmpeg si falla (sin CUDA, una
    // GPU sin el bloque de codificación…).
    if let Some(device) = super::encoder_device(backend) {
        super::create_device(device)?;
    }
    let mut encoder = FfmpegEncoder::new(codec, backend).map_err(|err| err.to_string())?;
    encoder
        .configure(&probe_config(codec, ENCODE_SIDE))
        .map_err(|err| format!("no abre: {err}"))?;
    if encode_some(&mut encoder, ENCODE_SIDE, 1).is_empty() {
        return Err("abre, pero no saca ningún fotograma".to_owned());
    }
    Ok(())
}

fn probe_encoder(codec: Codec, backend: Backend) -> EncoderProbe {
    let compiled = encoder_name(codec, backend)
        .and_then(AVCodec::find_encoder_by_name)
        .is_some();
    if !compiled {
        return EncoderProbe::Missing;
    }
    if backend == Backend::VulkanVideo
        && let Err(reason) = super::probe_child::isolated_encoder_open(codec, backend)
    {
        return EncoderProbe::Unavailable(reason);
    }
    let opened = with_timeout(&format!("{}-{}", codec.id(), backend.id()), move || {
        Some(open_and_encode(codec, backend))
    });
    match opened {
        Some(Ok(())) => {}
        Some(Err(reason)) => return EncoderProbe::Unavailable(reason),
        None => {
            return EncoderProbe::Unavailable(format!(
                "no respondió en {} s",
                PROBE_TIMEOUT.as_secs()
            ));
        }
    }
    if !backend.is_hardware() {
        return EncoderProbe::Works(Vec::new());
    }
    let checked = with_timeout_of(
        CHECK_TIMEOUT,
        &format!("comprueba-{}-{}", codec.id(), backend.id()),
        move || {
            let mut encoder = FfmpegEncoder::new(codec, backend).ok()?;
            encoder.configure(&probe_config(codec, ENCODE_SIDE)).ok()?;
            Some(check_encoder(
                &mut encoder,
                codec,
                Instant::now() + CHECK_BUDGET,
            ))
        },
    );
    match checked {
        Some(Ok(sample)) => EncoderProbe::Works(sample),
        Some(Err(reason)) => EncoderProbe::Rejected(reason),
        None => EncoderProbe::Rejected(format!(
            "la comprobación de imagen no terminó en {} s",
            CHECK_TIMEOUT.as_secs()
        )),
    }
}

/// Comprobación de imagen de un codificador (ver el módulo): codifica [`CHECK_FRAMES`]
/// fotogramas del patrón, los decodifica por software y compara cada uno con el original. Si
/// pasa, devuelve los [`SAMPLE_FRAMES`] primeros codificados; si no, por qué. Si llega `deadline`
/// antes de acabar, el motivo dice hasta dónde llegó y cuánto tardó en codificar cada fotograma.
fn check_encoder(
    encoder: &mut dyn Encoder,
    codec: Codec,
    deadline: Instant,
) -> Result<Vec<Vec<EncodedSlice>>, String> {
    let mut decoder = crate::select::create_decoder(codec, DecoderBackend::Software)
        .map_err(|err| format!("sin decodificador por software para comprobarlo: {err}"))?;
    let mut sample = Vec::new();
    let (mut encoded, mut decoded) = (0u64, 0u64);
    // Lo que tardó cada `encode` y cuántos no soltaron su fotograma a la primera.
    let mut times = Vec::with_capacity(CHECK_FRAMES as usize);
    let mut late = 0u64;
    let started = Instant::now();
    for frame_id in 0..CHECK_FRAMES {
        if Instant::now() >= deadline {
            times.sort_unstable();
            let ms = |d: Option<&Duration>| d.map_or(0.0, |d| d.as_secs_f64() * 1000.0);
            return Err(format!(
                "la comprobación de imagen no terminó: {frame_id} de {CHECK_FRAMES} fotogramas en \
                 {:.0} s (codificar uno tardó {:.0} ms de mediana y {:.0} ms como mucho; {late} no \
                 salieron a la primera)",
                started.elapsed().as_secs_f64(),
                ms(times.get(times.len() / 2)),
                ms(times.last()),
            ));
        }
        let at = Instant::now();
        let slices = encoder
            .encode(&test_pattern(ENCODE_SIDE, frame_id))
            .map_err(|err| format!("error al codificar el fotograma {frame_id}: {err}"))?;
        times.push(at.elapsed());
        if !slices.iter().any(|slice| slice.frame_id == frame_id) {
            late += 1;
        }
        for frame in split_frames(slices) {
            encoded += 1;
            let id = frame[0].frame_id;
            if (sample.len() as u64) < SAMPLE_FRAMES {
                sample.push(frame.clone());
            }
            match decoder.decode(&frame) {
                Ok(Some(image)) => {
                    decoded += 1;
                    check_image(&image)?;
                }
                Ok(None) => {}
                Err(err) => {
                    return Err(format!("su flujo no se decodifica (fotograma {id}): {err}"));
                }
            }
        }
    }
    // El decodificador puede quedarse uno o dos fotogramas; más es que no salen.
    if encoded < CHECK_FRAMES / 2 || decoded + 2 < encoded {
        return Err(format!(
            "de {CHECK_FRAMES} fotogramas salieron {encoded} codificados y {decoded} decodificados"
        ));
    }
    Ok(sample)
}

/// Separa los slices de varios fotogramas (los que soltó el codificador de una vez).
fn split_frames(slices: Vec<EncodedSlice>) -> Vec<Vec<EncodedSlice>> {
    let mut frames: Vec<Vec<EncodedSlice>> = Vec::new();
    for slice in slices {
        match frames.last_mut() {
            Some(frame) if frame[0].frame_id == slice.frame_id => frame.push(slice),
            _ => frames.push(vec![slice]),
        }
    }
    frames
}

/// Si `image` (decodificada) es el patrón de su `frame_id` ([`MIN_PSNR_DB`] de luma).
fn check_image(image: &RawFrame) -> Result<(), String> {
    match luma_psnr(image) {
        Some(psnr) if psnr < MIN_PSNR_DB => Err(format!(
            "su imagen no coincide con la original (fotograma {}: PSNR {psnr:.1} dB, mínimo \
             {MIN_PSNR_DB:.0})",
            image.frame_id
        )),
        _ => Ok(()),
    }
}

/// PSNR de la luma de `image` frente al patrón de su `frame_id`, convertido igual que lo
/// convierte el codificador (BT.709 en rango limitado). `None` si el formato no es YUV 4:2:0
/// de 8 bits en CPU o el tamaño no es el del patrón.
pub(crate) fn luma_psnr(image: &RawFrame) -> Option<f64> {
    if !matches!(image.format, PixelFormat::I420 | PixelFormat::Nv12)
        || image.width != image.height
        || image.width == 0
    {
        return None;
    }
    let side = image.width as usize;
    let stride = image.stride as usize;
    let expected = crate::convert::bgra_to_i420(&test_pattern(image.width, image.frame_id)).ok()?;
    let mut squared = 0f64;
    for y in 0..side {
        let got = image.data.get(y * stride..y * stride + side)?;
        let want = expected.data.get(y * side..(y + 1) * side)?;
        for (a, b) in got.iter().zip(want) {
            let d = f64::from(*a) - f64::from(*b);
            squared += d * d;
        }
    }
    let mse = squared / (side * side) as f64;
    Some(if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    })
}

/// Muestras para los decodificadores: [`SAMPLE_FRAMES`] fotogramas (un intra y predichos) de cada
/// códec que se puede codificar por software aquí.
fn samples() -> Vec<(Codec, Vec<Vec<EncodedSlice>>)> {
    let mut out = Vec::new();
    let h264 = with_timeout("muestra-h264", || {
        let mut encoder = OpenH264Encoder::new();
        encoder
            .configure(&probe_config(Codec::H264, SAMPLE_SIDE))
            .ok()?;
        Some(encode_some(&mut encoder, SAMPLE_SIDE, SAMPLE_FRAMES))
    });
    if let Some(frames) = h264.filter(|frames| !frames.is_empty()) {
        out.push((Codec::H264, frames));
    }
    let av1 = with_timeout("muestra-av1", || {
        let mut encoder = FfmpegEncoder::new(Codec::Av1, Backend::Software).ok()?;
        encoder
            .configure(&probe_config(Codec::Av1, SAMPLE_SIDE))
            .ok()?;
        Some(encode_some(&mut encoder, SAMPLE_SIDE, SAMPLE_FRAMES))
    });
    if let Some(frames) = av1.filter(|frames| !frames.is_empty()) {
        out.push((Codec::Av1, frames));
    }
    out
}

/// Resultado del sondeo de un decodificador.
enum DecoderProbe {
    /// No está o no decodifica en esta máquina (no se dice nada).
    Missing,
    /// Su apertura aislada falló o murió ([`super::probe_child`]): por qué.
    Unavailable(String),
    /// Decodifica, pero su imagen no coincide con la original: por qué.
    Rejected(String),
    /// Funciona.
    Works,
}

/// Decodifica la muestra; con un backend de hardware, solo vale si salió de la GPU y su imagen
/// es la del patrón ([`check_image`]).
fn probe_decoder(
    codec: Codec,
    backend: DecoderBackend,
    sample: Vec<Vec<EncodedSlice>>,
) -> DecoderProbe {
    if backend == DecoderBackend::Vulkan
        && let Err(reason) = super::probe_child::isolated_decoder_open(codec, backend)
    {
        return DecoderProbe::Unavailable(reason);
    }
    let result = with_timeout(&format!("dec-{}-{}", codec.id(), backend.id()), move || {
        let mut decoder = FfmpegDecoder::new(codec, backend).ok()?;
        let mut decoded = 0;
        let mut wrong = None;
        for frame in &sample {
            if let Ok(Some(image)) = decoder.decode(frame) {
                decoded += 1;
                if wrong.is_none()
                    && let Err(reason) = check_image(&image)
                {
                    wrong = Some(reason);
                }
            }
        }
        let (hw, _) = decoder.output_counts();
        let works = decoded > 0 && (backend == DecoderBackend::Software || hw > 0);
        Some(match (works, wrong) {
            (false, _) => DecoderProbe::Missing,
            (true, Some(reason)) => DecoderProbe::Rejected(reason),
            (true, None) => DecoderProbe::Works,
        })
    });
    result.unwrap_or(DecoderProbe::Missing)
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
        && (backend != DecoderBackend::Vulkan
            || super::probe_child::isolated_decoder_open(codec, backend).is_ok())
        && with_timeout(
            &format!("abre-{}-{}", codec.id(), backend.id()),
            move || FfmpegDecoder::new(codec, backend).ok().map(|_| ()),
        )
        .is_some()
}

/// Lo que dio el sondeo.
#[derive(Debug, Default)]
pub(crate) struct Probed {
    /// Codificadores que funcionan, en orden de preferencia.
    pub(crate) encoders: Vec<(Codec, Backend)>,
    /// Decodificadores que funcionan, en orden de preferencia.
    pub(crate) decoders: Vec<(Codec, DecoderBackend)>,
    /// Los que abren pero fallaron la comprobación de imagen: `«AV1/Vulkan Video: por qué»`.
    pub(crate) rejected: Vec<String>,
    /// Codificadores por hardware de la build que no abren aquí, con el motivo.
    pub(crate) unavailable: Vec<String>,
}

/// Codificadores y decodificadores de FFmpeg que funcionan aquí, en orden de preferencia.
pub(crate) fn probe() -> Probed {
    super::init_logging();
    // El sondeo provoca errores esperados (sin CUDA, sin VA-API…): sin registro mientras dura.
    super::set_log_level(ffi::AV_LOG_QUIET);
    let mut probed = Probed::default();
    let mut hevc_sample = None;
    for codec in Codec::PREFERENCE {
        for backend in platform_encoders() {
            match probe_encoder(codec, backend) {
                EncoderProbe::Missing => {}
                // El software siempre está; uno que falle es raro y ya lo dirá al usarlo.
                EncoderProbe::Unavailable(reason) if backend.is_hardware() => {
                    probed.unavailable.push(format!(
                        "codificar {codec} con {}: {reason}",
                        crate::encoder_label(codec, backend)
                    ));
                }
                EncoderProbe::Unavailable(_) => {}
                EncoderProbe::Rejected(reason) => probed.rejected.push(format!(
                    "codificar {codec} con {}: {reason}",
                    crate::encoder_label(codec, backend)
                )),
                EncoderProbe::Works(sample) => {
                    if codec == Codec::Hevc && hevc_sample.is_none() && !sample.is_empty() {
                        hevc_sample = Some(sample);
                    }
                    probed.encoders.push((codec, backend));
                }
            }
        }
    }
    let mut samples = samples();
    if let Some(sample) = hevc_sample {
        samples.push((Codec::Hevc, sample));
    }
    let decoders = &mut probed.decoders;
    // H.264 primero: el sondeo de HEVC por hardware se apoya en el de H.264 del mismo backend.
    for codec in [Codec::H264, Codec::Av1, Codec::Hevc] {
        let sample = samples
            .iter()
            .find(|(c, _)| *c == codec)
            .map(|(_, frames)| frames.clone());
        for backend in platform_decoders() {
            let works = match sample.clone() {
                Some(sample) => match probe_decoder(codec, backend, sample) {
                    DecoderProbe::Works => true,
                    DecoderProbe::Missing => false,
                    DecoderProbe::Unavailable(reason) => {
                        probed.unavailable.push(format!(
                            "decodificar {codec} con {}: {reason}",
                            crate::decoder_label(codec, backend)
                        ));
                        false
                    }
                    DecoderProbe::Rejected(reason) => {
                        probed.rejected.push(format!(
                            "decodificar {codec} con {}: {reason}",
                            crate::decoder_label(codec, backend)
                        ));
                        false
                    }
                },
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
    probed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CodecError;

    /// SVT-AV1 con la configuración del sondeo (sin hardware en la CI: es el que hay).
    fn svt_av1() -> FfmpegEncoder {
        let mut encoder = FfmpegEncoder::new(Codec::Av1, Backend::Software).expect("SVT-AV1");
        encoder
            .configure(&probe_config(Codec::Av1, ENCODE_SIDE))
            .expect("configurar");
        encoder
    }

    /// Un plazo que no llega durante la prueba.
    fn far() -> Instant {
        Instant::now() + Duration::from_secs(3600)
    }

    #[test]
    fn a_check_out_of_time_says_how_far_it_got() {
        let mut encoder = svt_av1();
        let deadline = Instant::now() + Duration::from_millis(300);
        let reason = check_encoder(&mut encoder, Codec::Av1, deadline)
            .err()
            .unwrap_or_default();
        // En una máquina rápida puede acabar los 300 a tiempo; si no, dice hasta dónde llegó.
        if !reason.is_empty() {
            assert!(reason.contains("no terminó"), "{reason}");
            assert!(
                reason.contains(&format!("de {CHECK_FRAMES} fotogramas")),
                "{reason}"
            );
            assert!(reason.contains("ms de mediana"), "{reason}");
        }
        let reason = check_encoder(&mut encoder, Codec::Av1, Instant::now())
            .expect_err("sin tiempo no comprueba nada");
        assert!(reason.contains("0 de 300 fotogramas"), "{reason}");
    }

    #[test]
    fn the_pattern_matches_itself_and_not_another_frame() {
        let mut image = crate::convert::bgra_to_i420(&test_pattern(ENCODE_SIDE, 7)).unwrap();
        assert_eq!(luma_psnr(&image), Some(f64::INFINITY));
        assert!(check_image(&image).is_ok());
        // La imagen de otro momento del patrón no pasa.
        image.frame_id = 40;
        let psnr = luma_psnr(&image).unwrap();
        assert!(psnr < MIN_PSNR_DB, "{psnr}");
        assert!(check_image(&image).unwrap_err().contains("no coincide"));
    }

    #[test]
    fn a_healthy_encoder_passes_the_image_check() {
        let sample = check_encoder(&mut svt_av1(), Codec::Av1, far()).expect("SVT-AV1 sano");
        assert_eq!(sample.len() as u64, SAMPLE_FRAMES);
        assert!(sample[0][0].keyframe, "la muestra empieza por un intra");
    }

    /// Codificador que estropea los fotogramas predichos, como el driver que daba artefactos.
    struct Corrupting(FfmpegEncoder);

    impl Encoder for Corrupting {
        fn codec(&self) -> Codec {
            self.0.codec()
        }

        fn backend(&self) -> Backend {
            self.0.backend()
        }

        fn configure(&mut self, config: &EncoderConfig) -> Result<(), CodecError> {
            self.0.configure(config)
        }

        fn set_bitrate(&mut self, bitrate_kbps: u32) -> Result<(), CodecError> {
            self.0.set_bitrate(bitrate_kbps)
        }

        fn request_keyframe(&mut self) {
            self.0.request_keyframe();
        }

        fn encode(&mut self, frame: &RawFrame) -> Result<Vec<EncodedSlice>, CodecError> {
            let mut slices = self.0.encode(frame)?;
            for slice in slices.iter_mut().filter(|slice| !slice.keyframe) {
                let len = slice.data.len();
                for byte in &mut slice.data[len / 2..] {
                    *byte ^= 0x5A;
                }
            }
            Ok(slices)
        }
    }

    #[test]
    fn an_encoder_with_damaged_frames_is_rejected() {
        let reason = check_encoder(&mut Corrupting(svt_av1()), Codec::Av1, far())
            .expect_err("el vídeo dañado no pasa");
        assert!(
            reason.contains("no coincide") || reason.contains("no se decodifica"),
            "{reason}"
        );
    }
}
