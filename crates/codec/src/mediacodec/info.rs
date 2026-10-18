//! Lo que la aplicación de Android cuenta de cada decodificador de `MediaCodecList`, la elección de
//! los que se anuncian al anfitrión y los parámetros con los que se configuran.
//!
//! La aplicación (Kotlin) recorre `MediaCodecList(REGULAR_CODECS)` y manda un
//! [`AndroidDecoderInfo`] por cada decodificador de vídeo y tipo MIME, con lo que dice la propia
//! plataforma (`MediaCodecInfo`, `CodecCapabilities`, `VideoCapabilities`): aquí no se deduce nada
//! del nombre del componente ni del fabricante.

use serde::{Deserialize, Serialize};

use crate::color::Chroma;
use crate::{Codec, DecodeLimits, DecoderBackend, StreamHint, VideoFormat, decoder_supports};

/// Tipo MIME de H.264 (`MediaFormat.MIMETYPE_VIDEO_AVC`).
pub const MIME_H264: &str = "video/avc";
/// Tipo MIME de HEVC (`MediaFormat.MIMETYPE_VIDEO_HEVC`).
pub const MIME_HEVC: &str = "video/hevc";
/// Tipo MIME de AV1 (`MediaFormat.MIMETYPE_VIDEO_AV1`).
pub const MIME_AV1: &str = "video/av01";

/// Tipo MIME de `codec`.
pub const fn mime(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => MIME_H264,
        Codec::Hevc => MIME_HEVC,
        Codec::Av1 => MIME_AV1,
    }
}

/// Códec de un tipo MIME de `MediaCodecList` (`None` si coroom no lo usa).
pub fn codec_from_mime(mime: &str) -> Option<Codec> {
    Codec::PREFERENCE
        .into_iter()
        .find(|&codec| self::mime(codec).eq_ignore_ascii_case(mime.trim()))
}

/// Un decodificador de `MediaCodecList` para un tipo MIME, tal como lo describe la plataforma.
///
/// Los campos con valor por defecto pueden faltar en el JSON que manda la aplicación.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AndroidDecoderInfo {
    /// Nombre del componente (`MediaCodecInfo.getName()`), con el que se crea
    /// (`AMediaCodec_createCodecByName`). Los alias (`isAlias()`) no hace falta mandarlos.
    pub name: String,
    /// Tipo MIME (`video/avc`, `video/hevc`, `video/av01`).
    pub mime: String,
    /// `MediaCodecInfo.isHardwareAccelerated()` (Android 10). Solo se anuncian los de hardware.
    pub hardware: bool,
    /// `CodecCapabilities.isFeatureRequired(FEATURE_SecurePlayback)`: solo decodifica en
    /// superficies protegidas, no sirve para el flujo de coroom.
    #[serde(default)]
    pub secure_required: bool,
    /// `CodecCapabilities.isFeatureSupported(FEATURE_LowLatency)` (Android 11): entrega cada
    /// fotograma en cuanto lo decodifica si se configura con `low-latency`.
    #[serde(default)]
    pub low_latency: bool,
    /// `CodecCapabilities.isFeatureSupported(FEATURE_AdaptivePlayback)`: cambia de resolución en
    /// marcha sin reconfigurarse (hasta `max-width × max-height`).
    #[serde(default)]
    pub adaptive_playback: bool,
    /// Ancho máximo que decodifica junto con [`Self::max_height`] (`VideoCapabilities`, un par
    /// que `isSizeSupported` acepta).
    pub max_width: u32,
    /// Alto máximo.
    pub max_height: u32,
    /// Fotogramas por segundo que admite a `max_width × max_height`
    /// (`getSupportedFrameRatesFor(w, h).getUpper()`).
    pub max_fps: u32,
    /// Lo que el fabricante midió a ese tamaño (`getAchievableFrameRatesFor(w, h).getUpper()`, que
    /// el CTS comprueba), si lo publica. Si está, manda sobre [`Self::max_fps`] cuando es menor.
    #[serde(default)]
    pub achievable_fps: Option<f64>,
    /// Admite un perfil de 10 bits 4:2:0 (`HEVCProfileMain10`, `AV1ProfileMain10`…): HDR10.
    #[serde(default)]
    pub main10: bool,
    /// Admite un perfil 4:4:4 de 8 bits (`AVCProfileHigh444`, `HEVCProfileMain444`…, raro).
    #[serde(default)]
    pub yuv444: bool,
}

impl AndroidDecoderInfo {
    /// Códec que decodifica (`None` si el MIME no es de coroom).
    pub fn codec(&self) -> Option<Codec> {
        codec_from_mime(&self.mime)
    }

    /// Fotogramas por segundo al tamaño máximo: los admitidos, o los medidos por el fabricante si
    /// son menos.
    pub fn fps_at_max(&self) -> u32 {
        match self.achievable_fps {
            Some(measured) if measured.is_finite() && measured >= 1.0 => {
                // Truncar es lo prudente: 59,94 medidos no son 60.
                let measured = measured.min(f64::from(u32::MAX)) as u32;
                self.max_fps.min(measured)
            }
            _ => self.max_fps,
        }
    }

    /// Lo que decodifica al tamaño máximo.
    pub fn limits(&self) -> DecodeLimits {
        DecodeLimits {
            max_width: self.max_width,
            max_height: self.max_height,
            max_fps: self.fps_at_max(),
        }
    }

    /// Si decodifica `format`: la tabla de [`decoder_supports`] para MediaCodec y, además, los
    /// perfiles que anuncia este componente.
    pub fn supports(&self, format: VideoFormat) -> bool {
        let Some(codec) = self.codec() else {
            return false;
        };
        if !decoder_supports(codec, DecoderBackend::MediaCodec, format) {
            return false;
        }
        if format.is_default() {
            return true;
        }
        match format.chroma {
            Chroma::Yuv444 => self.yuv444,
            Chroma::Yuv420 => self.main10,
        }
    }

    /// Si se puede usar: de hardware, de un códec de coroom, sin superficie protegida, con un
    /// nombre que el NDK acepta y con tamaño y ritmo.
    fn usable(&self) -> bool {
        self.hardware
            && !self.secure_required
            && self.codec().is_some()
            && !self.name.trim().is_empty()
            && !self.name.contains('\0')
            && self.max_width >= 2
            && self.max_height >= 2
            && self.fps_at_max() >= 1
    }
}

/// Los decodificadores de un códec que se pueden abrir, del preferido al de respaldo.
#[derive(Debug, Clone, PartialEq)]
pub struct AndroidChoice {
    /// Códec.
    pub codec: Codec,
    /// Componentes, del preferido al de respaldo (el primero es el que se anuncia).
    pub components: Vec<AndroidDecoderInfo>,
}

impl AndroidChoice {
    /// Lo que se anuncia de este códec: los límites del componente preferido.
    pub fn limits(&self) -> DecodeLimits {
        self.components
            .first()
            .map_or(DecodeLimits::default(), AndroidDecoderInfo::limits)
    }

    /// Si algún componente decodifica `format`.
    pub fn supports(&self, format: VideoFormat) -> bool {
        self.components.iter().any(|info| info.supports(format))
    }
}

/// Elige, por códec y en el orden de [`Codec::PREFERENCE`], los decodificadores que se pueden
/// usar: solo los de hardware, sin repetir nombre (gana el primero que llega) y, dentro de cada
/// códec, primero los que anuncian `FEATURE_LowLatency` y después el orden de `MediaCodecList`
/// (el de preferencia de la plataforma). Un códec sin ninguno no aparece.
pub fn choose(decoders: Vec<AndroidDecoderInfo>) -> Vec<AndroidChoice> {
    let mut seen: Vec<String> = Vec::new();
    let mut usable: Vec<AndroidDecoderInfo> = Vec::new();
    for info in decoders {
        if !info.usable() || seen.contains(&info.name) {
            continue;
        }
        seen.push(info.name.clone());
        usable.push(info);
    }
    Codec::PREFERENCE
        .into_iter()
        .filter_map(|codec| {
            let mut components: Vec<AndroidDecoderInfo> = usable
                .iter()
                .filter(|info| info.codec() == Some(codec))
                .cloned()
                .collect();
            // Estable: a igualdad, el orden de la plataforma.
            components.sort_by_key(|info| !info.low_latency);
            (!components.is_empty()).then_some(AndroidChoice { codec, components })
        })
        .collect()
}

/// Valor de un parámetro de `AMediaFormat`.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    /// `AMediaFormat_setInt32`.
    I32(i32),
    /// `AMediaFormat_setFloat`.
    F32(f32),
    /// `AMediaFormat_setString`.
    Str(String),
}

/// `KEY_MIME`.
pub const KEY_MIME: &str = "mime";
/// `KEY_WIDTH`.
pub const KEY_WIDTH: &str = "width";
/// `KEY_HEIGHT`.
pub const KEY_HEIGHT: &str = "height";
/// `KEY_MAX_WIDTH`: tope de la reproducción adaptativa.
pub const KEY_MAX_WIDTH: &str = "max-width";
/// `KEY_MAX_HEIGHT`.
pub const KEY_MAX_HEIGHT: &str = "max-height";
/// `KEY_PRIORITY` (Android 6): 0 es tiempo real.
pub const KEY_PRIORITY: &str = "priority";
/// `KEY_LOW_LATENCY` (Android 11): 1 para que entregue cada fotograma en cuanto lo decodifica.
pub const KEY_LOW_LATENCY: &str = "low-latency";
/// `KEY_OPERATING_RATE` (Android 6): fotogramas por segundo que tiene que sostener.
pub const KEY_OPERATING_RATE: &str = "operating-rate";
/// `KEY_MAX_INPUT_SIZE`: bytes del búfer de entrada.
pub const KEY_MAX_INPUT_SIZE: &str = "max-input-size";
/// `KEY_CROP_LEFT` del formato de salida (la zona visible).
pub const KEY_CROP_LEFT: &str = "crop-left";
/// `KEY_CROP_TOP`.
pub const KEY_CROP_TOP: &str = "crop-top";
/// `KEY_CROP_RIGHT` (incluido).
pub const KEY_CROP_RIGHT: &str = "crop-right";
/// `KEY_CROP_BOTTOM` (incluido).
pub const KEY_CROP_BOTTOM: &str = "crop-bottom";

/// Parámetros de `AMediaCodec_configure`, en el orden en que se ponen.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FormatParams {
    entries: Vec<(&'static str, ParamValue)>,
}

impl FormatParams {
    /// Pone (o cambia) `key`.
    pub fn set(&mut self, key: &'static str, value: ParamValue) {
        match self.entries.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 = value,
            None => self.entries.push((key, value)),
        }
    }

    /// El valor de `key`.
    pub fn get(&self, key: &str) -> Option<&ParamValue> {
        self.entries
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, value)| value)
    }

    /// El entero de `key`.
    pub fn i32(&self, key: &str) -> Option<i32> {
        match self.get(key)? {
            ParamValue::I32(value) => Some(*value),
            _ => None,
        }
    }

    /// Todos, en orden.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &ParamValue)> {
        self.entries.iter().map(|(key, value)| (*key, value))
    }

    /// Tamaño configurado (`width × height`).
    pub fn size(&self) -> Option<(u32, u32)> {
        let width = u32::try_from(self.i32(KEY_WIDTH)?).ok()?;
        let height = u32::try_from(self.i32(KEY_HEIGHT)?).ok()?;
        Some((width, height))
    }
}

fn to_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// Parámetros para decodificar con `info` lo mínimo posible de latencia, solo con claves
/// documentadas de `MediaFormat` (ninguna del fabricante):
///
/// - `width × height`: el tamaño del flujo si se sabe (`hint`, de `CodecSelected`) y si no, el
///   máximo del componente (el flujo trae el suyo en la cabecera de secuencia y el formato de
///   salida lo dice; el decodificador solo reserva con él).
/// - `max-width × max-height`: el máximo del componente, si tiene `FEATURE_AdaptivePlayback`
///   (cambia de tamaño sin reconfigurarse).
/// - `priority` = 0 (tiempo real) siempre.
/// - `low-latency` = 1 si el componente anuncia `FEATURE_LowLatency`: sin ella, muchos
///   decodificadores retienen fotogramas hasta recibir más entrada.
/// - `operating-rate` = los fps del flujo, si se saben (sin pasarse de lo que admite): con
///   prioridad de tiempo real, el decodificador ajusta sus relojes para sostener ese ritmo. No
///   se infla por encima del flujo (eso sería un truco sin respaldo en la documentación).
/// - `max-input-size`: solo si una unidad de acceso no cupo en el búfer por defecto
///   (`max_input`, medido; ver `MediaCodecDecoder::reset`).
pub fn format_params(
    info: &AndroidDecoderInfo,
    codec: Codec,
    hint: Option<StreamHint>,
    max_input: Option<usize>,
) -> FormatParams {
    let mut params = FormatParams::default();
    params.set(KEY_MIME, ParamValue::Str(mime(codec).to_owned()));
    let (width, height) = hint
        .filter(|hint| hint.width >= 2 && hint.height >= 2)
        .map_or((info.max_width, info.max_height), |hint| {
            (
                hint.width.min(info.max_width),
                hint.height.min(info.max_height),
            )
        });
    params.set(KEY_WIDTH, ParamValue::I32(to_i32(width)));
    params.set(KEY_HEIGHT, ParamValue::I32(to_i32(height)));
    if info.adaptive_playback {
        params.set(KEY_MAX_WIDTH, ParamValue::I32(to_i32(info.max_width)));
        params.set(KEY_MAX_HEIGHT, ParamValue::I32(to_i32(info.max_height)));
    }
    params.set(KEY_PRIORITY, ParamValue::I32(0));
    if info.low_latency {
        params.set(KEY_LOW_LATENCY, ParamValue::I32(1));
    }
    if let Some(hint) = hint.filter(|hint| hint.fps > 0) {
        let fps = hint.fps.min(info.max_fps.max(1));
        params.set(KEY_OPERATING_RATE, ParamValue::F32(fps as f32));
    }
    if let Some(bytes) = max_input {
        params.set(
            KEY_MAX_INPUT_SIZE,
            ParamValue::I32(i32::try_from(bytes).unwrap_or(i32::MAX)),
        );
    }
    params
}

/// Tamaño del búfer de entrada que se pide cuando una unidad de acceso de `needed` bytes no cupo:
/// la siguiente potencia de dos, para que un intra algo mayor que el que falló quepa sin volver
/// a reconfigurar (crece como mucho al doble de lo medido).
pub fn grown_input_size(needed: usize) -> usize {
    needed.checked_next_power_of_two().unwrap_or(needed)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn info(name: &str, mime: &str) -> AndroidDecoderInfo {
        AndroidDecoderInfo {
            name: name.to_owned(),
            mime: mime.to_owned(),
            hardware: true,
            secure_required: false,
            low_latency: false,
            adaptive_playback: false,
            max_width: 3840,
            max_height: 2160,
            max_fps: 60,
            achievable_fps: None,
            main10: false,
            yuv444: false,
        }
    }

    #[test]
    fn mime_types_map_to_codecs() {
        for codec in Codec::PREFERENCE {
            assert_eq!(codec_from_mime(mime(codec)), Some(codec));
        }
        assert_eq!(codec_from_mime("VIDEO/AVC"), Some(Codec::H264));
        assert_eq!(codec_from_mime("video/x-vnd.on2.vp9"), None);
    }

    /// Solo hardware, sin superficie protegida, sin repetir; por códec, primero los de baja
    /// latencia y luego el orden de la plataforma; los códecs en el orden de preferencia.
    #[test]
    fn only_hardware_decoders_are_chosen_and_low_latency_goes_first() {
        let mut software = info("c2.android.av1.decoder", MIME_AV1);
        software.hardware = false;
        let mut secure = info("c2.example.hevc.decoder.secure", MIME_HEVC);
        secure.secure_required = true;
        let mut low = info("c2.example.avc.lowlatency", MIME_H264);
        low.low_latency = true;
        let mut zero = info("c2.example.hevc.broken", MIME_HEVC);
        zero.max_fps = 0;
        let decoders = vec![
            software,
            info("c2.example.avc.decoder", MIME_H264),
            low.clone(),
            secure,
            zero,
            info("c2.example.hevc.decoder", MIME_HEVC),
            info("c2.example.hevc.decoder", MIME_HEVC),
            info("c2.example.vp9.decoder", "video/x-vnd.on2.vp9"),
            info("bad\0name", MIME_HEVC),
        ];
        let chosen = choose(decoders);
        let summary: Vec<(Codec, Vec<&str>)> = chosen
            .iter()
            .map(|choice| {
                (
                    choice.codec,
                    choice
                        .components
                        .iter()
                        .map(|info| info.name.as_str())
                        .collect(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (Codec::Hevc, vec!["c2.example.hevc.decoder"]),
                (
                    Codec::H264,
                    vec!["c2.example.avc.lowlatency", "c2.example.avc.decoder"]
                ),
            ]
        );
        assert!(choose(Vec::new()).is_empty());
    }

    #[test]
    fn limits_use_the_measured_rate_when_it_is_lower() {
        let mut av1 = info("c2.example.av1.decoder", MIME_AV1);
        assert_eq!(av1.fps_at_max(), 60);
        av1.achievable_fps = Some(47.9);
        assert_eq!(av1.fps_at_max(), 47);
        av1.achievable_fps = Some(240.0);
        assert_eq!(av1.fps_at_max(), 60, "lo admitido manda si es menor");
        av1.achievable_fps = Some(f64::NAN);
        assert_eq!(av1.fps_at_max(), 60);
        av1.achievable_fps = Some(0.5);
        assert_eq!(av1.fps_at_max(), 60, "una medida sin sentido no cuenta");
        av1.achievable_fps = Some(30.0);
        assert_eq!(
            av1.limits(),
            DecodeLimits {
                max_width: 3840,
                max_height: 2160,
                max_fps: 30
            }
        );
    }

    #[test]
    fn formats_follow_the_profiles_the_component_announces() {
        let mut hevc = info("c2.example.hevc.decoder", MIME_HEVC);
        assert!(hevc.supports(VideoFormat::SDR_420));
        assert!(!hevc.supports(VideoFormat::HDR10_420));
        assert!(!hevc.supports(VideoFormat::SDR_444));
        hevc.main10 = true;
        hevc.yuv444 = true;
        assert!(hevc.supports(VideoFormat::HDR10_420));
        // La tabla común manda: 4:4:4 por hardware no se negocia.
        assert!(!hevc.supports(VideoFormat::SDR_444));
        let mut avc = info("c2.example.avc.decoder", MIME_H264);
        avc.main10 = true;
        assert!(
            !avc.supports(VideoFormat::HDR10_420),
            "H.264 solo 4:2:0 SDR"
        );
    }

    #[test]
    fn parameters_ask_for_realtime_and_low_latency_only_when_announced() {
        let mut avc = info("c2.example.avc.decoder", MIME_H264);
        let plain = format_params(&avc, Codec::H264, None, None);
        assert_eq!(
            plain.get(KEY_MIME),
            Some(&ParamValue::Str("video/avc".into()))
        );
        assert_eq!(plain.size(), Some((3840, 2160)));
        assert_eq!(plain.i32(KEY_PRIORITY), Some(0));
        assert_eq!(plain.get(KEY_LOW_LATENCY), None);
        assert_eq!(plain.get(KEY_OPERATING_RATE), None);
        assert_eq!(plain.get(KEY_MAX_WIDTH), None);
        assert_eq!(plain.get(KEY_MAX_INPUT_SIZE), None);

        avc.low_latency = true;
        avc.adaptive_playback = true;
        let hint = StreamHint {
            width: 1920,
            height: 1080,
            fps: 120,
        };
        let tuned = format_params(&avc, Codec::H264, Some(hint), Some(4 << 20));
        assert_eq!(tuned.size(), Some((1920, 1080)));
        assert_eq!(tuned.i32(KEY_LOW_LATENCY), Some(1));
        assert_eq!(tuned.i32(KEY_MAX_WIDTH), Some(3840));
        assert_eq!(tuned.i32(KEY_MAX_HEIGHT), Some(2160));
        assert_eq!(
            tuned.get(KEY_OPERATING_RATE),
            Some(&ParamValue::F32(60.0)),
            "sin pasarse de lo que admite"
        );
        assert_eq!(tuned.i32(KEY_MAX_INPUT_SIZE), Some(4 << 20));
        // Un flujo mayor que el máximo se recorta al máximo.
        let big = StreamHint {
            width: 7680,
            height: 4320,
            fps: 30,
        };
        assert_eq!(
            format_params(&avc, Codec::H264, Some(big), None).size(),
            Some((3840, 2160))
        );
        let mut params = FormatParams::default();
        params.set(KEY_WIDTH, ParamValue::I32(1));
        params.set(KEY_WIDTH, ParamValue::I32(2));
        assert_eq!(params.iter().count(), 1);
        assert_eq!(params.i32(KEY_WIDTH), Some(2));
    }

    #[test]
    fn the_input_buffer_grows_to_the_next_power_of_two() {
        assert_eq!(grown_input_size(1_000_000), 1 << 20);
        assert_eq!(grown_input_size(1 << 20), 1 << 20);
        assert_eq!(grown_input_size(usize::MAX), usize::MAX);
    }

    #[test]
    fn the_app_can_send_the_list_as_json() {
        let json = r#"[{"name":"c2.example.av1.decoder","mime":"video/av01","hardware":true,
            "low_latency":true,"max_width":3840,"max_height":2160,"max_fps":60}]"#;
        let list: Vec<AndroidDecoderInfo> = serde_json::from_str(json).expect("JSON");
        assert_eq!(list[0].codec(), Some(Codec::Av1));
        assert!(list[0].low_latency && !list[0].main10 && list[0].achievable_fps.is_none());
    }
}
