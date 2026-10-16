//! Elección del codificador y del decodificador concretos de un códec.
//!
//! [`EncoderSet`] y [`DecoderSet`] guardan qué combinaciones de códec y backend se pueden usar (lo
//! que dio [`Capabilities::probe`], quizá restringido por la línea de órdenes) y abren la mejor de un
//! códec, pasando a la siguiente si una falla al configurarse.
//!
//! [`DecoderSet::with_output`] dice dónde dejan los decodificadores sus imágenes
//! ([`DecodeOutput`]) y [`EncoderSet::gpu_input`] qué imágenes de GPU acepta sin copia el
//! codificador preferido (lo que la sesión le pide a la captura).

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::calibrate::{self, CachedChoice, CalibrationCache, CalibrationKey, CalibrationResult};
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
    /// Fichero JSON de la calibración medida ([`crate::calibrate`]); sin él, solo en memoria.
    calibration_cache: Option<PathBuf>,
}

/// Cómo se eligió el backend en [`EncoderSet::open_calibrated`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Calibrated {
    /// El backend elegido.
    pub backend: Backend,
    /// p95 de la latencia entrega → paquete medida (o guardada).
    pub p95: Duration,
    /// Mediana de esa latencia.
    pub p50: Duration,
    /// Bitrate real medido en kbit/s.
    pub kbit_s: u32,
    /// Si salió de la caché (misma GPU, driver, códec, resolución y formato) o se midió ahora.
    pub from_cache: bool,
    /// Todas las medidas de esta vez (vacío con acierto de caché), para el registro.
    pub results: Vec<CalibrationResult>,
}

impl fmt::Display for Calibrated {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} elegido por calibración{}: p95 {:.1} ms, p50 {:.1} ms",
            self.backend,
            if self.from_cache { " (guardada)" } else { "" },
            self.p95.as_secs_f64() * 1000.0,
            self.p50.as_secs_f64() * 1000.0
        )
    }
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
            calibration_cache: None,
        }
    }

    /// Dónde guardar la calibración medida del modo automático (el directorio de datos de la
    /// aplicación); `None`, solo en memoria durante el proceso.
    pub fn with_calibration_cache(mut self, path: Option<PathBuf>) -> Self {
        self.calibration_cache = path;
        self
    }

    /// Fichero de la calibración, si se dio.
    pub fn calibration_cache(&self) -> Option<&Path> {
        self.calibration_cache.as_deref()
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
                Err(err) => {
                    // El sondeo lo dio por bueno y no abre: que el próximo arranque vuelva a
                    // sondear en vez de fiarse de la caché.
                    if backend.is_hardware() {
                        Capabilities::forget_cache();
                    }
                    last = Some(err);
                }
            }
        }
        Err(last.unwrap_or_else(|| {
            CodecError::Unsupported(format!(
                "no hay codificador de {} en {} disponible",
                config.codec, config.format
            ))
        }))
    }

    /// Backends de `config.codec` que admiten `config.format`, en orden de preferencia.
    fn candidates(&self, config: &EncoderConfig) -> Vec<Backend> {
        self.available
            .iter()
            .filter(|&&(c, backend)| {
                c == config.codec && encoder_supports(c, backend, config.format)
            })
            .map(|(_, backend)| *backend)
            .collect()
    }

    /// Como [`EncoderSet::open`], pero con **varios** backends para el códec elige el de menor
    /// latencia medida ([`crate::calibrate`]) en vez del primero de la lista: con un acierto de
    /// caché (misma GPU y driver, códec, clase de resolución y formato) abre el guardado; si no,
    /// o si el guardado ya no abre, mide todos (unas décimas de segundo; fuera del hilo de la
    /// interfaz) y guarda al ganador. Con un solo backend (ajuste explícito) no calibra nada y
    /// devuelve `None` de segundo.
    pub fn open_calibrated(
        &self,
        config: &EncoderConfig,
    ) -> Result<(Box<dyn Encoder>, Option<Calibrated>), CodecError> {
        let candidates = self.candidates(config);
        if candidates.len() < 2 {
            return self.open(config).map(|encoder| (encoder, None));
        }
        let open_one = |backend: Backend| -> Result<Box<dyn Encoder>, CodecError> {
            let mut encoder = create_encoder(config.codec, backend)?;
            encoder.configure(config)?;
            Ok(encoder)
        };
        let key = CalibrationKey::of(config);
        let mut cache = CalibrationCache::load(self.calibration_cache.as_deref());
        let cached = calibrate::remembered(&key).or_else(|| cache.lookup(&key).cloned());
        if let Some(cached) = cached
            && let Some(backend) = cached.backend()
            && candidates.contains(&backend)
        {
            match open_one(backend) {
                Ok(encoder) => {
                    calibrate::remember(&cached);
                    return Ok((
                        encoder,
                        Some(Calibrated {
                            backend,
                            p95: cached.p95(),
                            p50: cached.p50(),
                            kbit_s: cached.kbit_s,
                            from_cache: true,
                            results: Vec::new(),
                        }),
                    ));
                }
                Err(_) => {
                    // El guardado ya no abre (otro driver, GPU ocupada…): se vuelve a medir.
                    calibrate::forget_remembered(&key);
                    cache.forget(&key);
                }
            }
        }
        let results = calibrate::calibrate(&candidates, config);
        if results.iter().all(|result| !result.opened) {
            // Nada midió: se dice por qué (registro de FFmpeg, `COROOM_FFMPEG_LOG=warning`) y se
            // sigue con la lista de preferencia.
            let notes: Vec<String> = results.iter().map(ToString::to_string).collect();
            #[cfg(feature = "ffmpeg")]
            crate::ffmpeg::log_warning(&format!(
                "calibración de {} en {}: ningún candidato midió ({})",
                config.codec,
                config.format,
                notes.join("; ")
            ));
            #[cfg(not(feature = "ffmpeg"))]
            drop(notes);
        }
        let mut ranked: Vec<&CalibrationResult> =
            results.iter().filter(|result| result.opened).collect();
        // Primero los que respetan el techo por fotograma, luego la latencia ([`calibrate::rank`]).
        ranked.sort_by_key(|result| calibrate::rank(result));
        let mut last = None;
        for result in ranked {
            match open_one(result.backend) {
                Ok(encoder) => {
                    let choice = calibrate::choice_from(key.clone(), result);
                    calibrate::remember(&choice);
                    // Sin poder escribir la caché se sigue igual: solo se pierde el ahorro.
                    let _ = cache.store(choice);
                    return Ok((
                        encoder,
                        Some(Calibrated {
                            backend: result.backend,
                            p95: result.p95,
                            p50: result.p50,
                            kbit_s: result.kbit_s,
                            from_cache: false,
                            results: results.clone(),
                        }),
                    ));
                }
                Err(err) => last = Some(err),
            }
        }
        // Ninguno midió, o los que midieron ya no abren: la lista de preferencia, como siempre
        // (su error es el que explica por qué, si tampoco abre).
        drop(last);
        self.open(config).map(|encoder| (encoder, None))
    }

    /// Como [`EncoderSet::open_calibrated`], pero **sin medir nunca** (con la pantalla ya en el
    /// aire, abrir y medir varios codificadores de GPU la haría tartamudear): abre lo que se
    /// calibró para esta clave (misma GPU y driver, códec, clase de resolución y formato) y, si
    /// ese códec no se calibró, el backend que ganó para otro códec en la misma GPU y formato (la
    /// latencia depende sobre todo del motor: si NVENC ganó en AV1, gana en H.264); sin nada
    /// guardado, la lista de preferencia como [`EncoderSet::open`]. Antes, al renegociar a otro
    /// códec con la sala abierta se abría siempre el primero de la lista (Vulkan Video en Linux),
    /// aunque la calibración hubiera dado otro.
    pub fn open_remembered(
        &self,
        config: &EncoderConfig,
    ) -> Result<(Box<dyn Encoder>, Option<Calibrated>), CodecError> {
        let candidates = self.candidates(config);
        if candidates.len() < 2 {
            return self.open(config).map(|encoder| (encoder, None));
        }
        let key = CalibrationKey::of(config);
        let mut choices = calibrate::remembered_all();
        choices.extend(
            CalibrationCache::load(self.calibration_cache.as_deref())
                .entries()
                .iter()
                .cloned(),
        );
        // 0: esta misma clave; 1: otro códec, misma clase de resolución; 2: otra clase.
        let mut ranked: Vec<(u8, CachedChoice)> = choices
            .into_iter()
            .filter(|choice| {
                choice.key.adapter == key.adapter && choice.key.pixel_format == key.pixel_format
            })
            .map(|choice| {
                let rank = if choice.key == key {
                    0
                } else if choice.key.resolution_class == key.resolution_class {
                    1
                } else {
                    2
                };
                (rank, choice)
            })
            .collect();
        // Dentro de cada grupo, primero lo que se midió que respeta el techo por fotograma.
        ranked.sort_by_key(|(rank, choice)| {
            let fits = choice
                .peak_ratio_milli
                .is_some_and(|milli| f64::from(milli) / 1000.0 <= calibrate::PEAK_TOLERANCE);
            (*rank, !fits, choice.p95_us)
        });
        let mut tried = Vec::new();
        for (rank, choice) in ranked {
            let Some(backend) = choice.backend() else {
                continue;
            };
            if !candidates.contains(&backend) || tried.contains(&backend) {
                continue;
            }
            tried.push(backend);
            let opened = create_encoder(config.codec, backend).and_then(|mut encoder| {
                encoder.configure(config)?;
                Ok(encoder)
            });
            if let Ok(encoder) = opened {
                let calibrated = (rank == 0).then(|| Calibrated {
                    backend,
                    p95: choice.p95(),
                    p50: choice.p50(),
                    kbit_s: choice.kbit_s,
                    from_cache: true,
                    results: Vec::new(),
                });
                return Ok((encoder, calibrated));
            }
        }
        self.open(config).map(|encoder| (encoder, None))
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

    /// El mismo conjunto sin las combinaciones de `excluded` (los decodificadores que fallaron en
    /// la sesión: el siguiente que se abra es el que viene después en el orden).
    pub fn without(&self, excluded: &[(Codec, DecoderBackend)]) -> Self {
        let mut out = self.clone();
        out.available.retain(|pair| !excluded.contains(pair));
        out
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
                Err(err) => {
                    // Como en los codificadores: el próximo arranque vuelve a sondear.
                    if *backend != DecoderBackend::Software {
                        Capabilities::forget_cache();
                    }
                    last = Some(err);
                }
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
        let without = decoders.without(&[(Codec::H264, DecoderBackend::Software)]);
        assert!(without.is_empty());
        assert!(without.open(Codec::H264).is_err());
    }
}
