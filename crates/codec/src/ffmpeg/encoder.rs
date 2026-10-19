//! Codificador de FFmpeg ([`FfmpegEncoder`]) con opciones de baja latencia por backend.
//!
//! Ruta sin copias:
//!
//! - Una imagen de GPU de la captura ([`crate::GpuImage::DmaBuf`] en Linux,
//!   [`crate::GpuImage::D3D11`] en Windows) entra por [`super::import`] sin pasar por la CPU; el
//!   codificador se abre con los *hw frames* de la importación. Si no puede (o si la importación no
//!   existe para ese backend), se baja a CPU y se sigue por la ruta con copias, con las copias
//!   contadas en [`Encoder::cpu_copies`].
//! - Por el pipeline ([`Encoder::encode_into`]) el paquete se copia una sola vez, entero, al búfer
//!   del llamante; [`Encoder::encode`] lo reparte además en sus slices reales ([`crate::slices`]).
//! - Ningún backend repinta por su cuenta (*intra-refresh* apagado): GOP infinito e IDR solo al
//!   empezar o cuando el invitado lo pide; con la imagen quieta solo viajan diferencias.
//!
//! Tope de resolución: `EncoderConfig.width × height` es un máximo ([`crate::EncoderConfig`]). La
//! captura de pantalla trae su propio tamaño y el codificador se reabre al de cada fotograma
//! **reducido a lo que cabe** ([`coroom_proto::video::fit`]): en la ruta sin copias, la reducción
//! va en el mismo filtro de la GPU que la conversión de formato ([`super::import`]); en la de CPU,
//! libswscale reduce y convierte en un solo paso ([`super::scale`]). El tope se guarda aparte del
//! tamaño abierto, así que vale para la fuente que venga después.

use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, Instant};

use rsmpeg::avcodec::{AVCodec, AVCodecContext, AVCodecRef, AVPacket};
use rsmpeg::avutil::{AVFrame, AVHWDeviceContext, AVHWFramesContext};
use rsmpeg::error::RsmpegError;
use rsmpeg::ffi;

use coroom_proto::video::fit;

use super::import::{GpuImporter, ImportMode, Prepared};
use super::scale::{Scaler, tag_color};
use super::{backend_error, create_device, encoder_device, encoder_name, hw_pixel_format};
use crate::color::{Chroma, VideoFormat};
use crate::convert::{
    TenBitPlanes, bgra_to_i420_planes, bgra_to_i444_planes, bgra_to_nv12_planes,
    bgra_to_nv24_planes, i420_to_nv12_planes, rgba16f_to_bgra, to_hdr10_planes,
};
use crate::gpu::GpuImage;
use crate::{
    Backend, Codec, CodecError, CpuScale, EncodedFrame, EncodedSlice, Encoder, EncoderConfig,
    GpuInput, PixelFormat, RateMode, RawFrame,
};

/// Periodo de GOP «infinito»: solo hay intra al pedirlo (o al empezar). Cabe en los 16 bits de
/// QSV y son más de nueve minutos a 60 fps.
const LONG_GOP: u32 = 32_767;

/// Espera máxima a que salga el paquete del fotograma recién entregado, en periodos de fotograma
/// (SVT-AV1 codifica en sus propios hilos y entrega el paquete de forma asíncrona).
const OUTPUT_WAIT_FRAMES: u32 = 3;

/// Espera mínima al paquete, aunque los fps sean altos.
const OUTPUT_WAIT_MIN: Duration = Duration::from_millis(50);

/// Pausa entre dos sondeos del paquete de salida, pasado [`OUTPUT_SPIN`].
const OUTPUT_POLL: Duration = Duration::from_micros(200);

/// Tiempo de espera al paquete en que se sondea sin dormir (cediendo el hilo con `yield_now`).
/// Solo en Windows: allí un `sleep` de 200 µs dura lo que el reloj del sistema (1 ms con
/// `timeBeginPeriod`, 15,6 ms si el sistema ignora la petición con la ventana oculta o
/// minimizada): con NVENC o QSV el paquete sale en 1 a 3 ms y cada `sleep` de más se come un
/// fotograma a 60 fps. Pasado este tiempo (un codificador lento, como SVT-AV1 en un equipo justo)
/// se duerme para no gastar un núcleo. En Linux `nanosleep` despierta con decenas de µs de error,
/// así que se duerme desde el principio y no se quema un núcleo por fotograma.
const OUTPUT_SPIN: Duration = if cfg!(windows) {
    Duration::from_millis(4)
} else {
    Duration::ZERO
};

/// Tamaño del búfer VBV en bits para `bitrate_kbps` a `fps`: lo que entra en **un** intervalo de
/// fotograma. Se fija al abrir y se vuelve a fijar con cada cambio de bitrate
/// ([`Encoder::set_bitrate`]): un búfer calculado con el bitrate de arranque dejaba al intra ese
/// tamaño aunque el objetivo hubiera bajado a la cuarta parte. SVT-AV1 no admite un búfer menor
/// de [`SVT_MIN_VBV_MS`] (lo rechaza al abrir): con él, el mayor de los dos.
pub fn vbv_buffer_bits(bitrate_kbps: u32, fps: u32, backend: Backend) -> u64 {
    let frame = u64::from(bitrate_kbps) * 1000 / u64::from(fps.max(1));
    let floor = if backend == Backend::Software {
        u64::from(bitrate_kbps) * SVT_MIN_VBV_MS
    } else {
        0
    };
    frame.max(floor).max(1)
}

/// Búfer VBV mínimo que acepta SVT-AV1, en milisegundos («The maximum buffer size must be between
/// [20, 10000]»).
pub const SVT_MIN_VBV_MS: u64 = 20;

/// Opciones de baja latencia de cada backend, tal como las acepta `avcodec_open2` (genéricas de
/// `AVCodecContext` y privadas del codificador). Comprobadas contra la ayuda de cada codificador de
/// FFmpeg 8.1 (`ffmpeg -h encoder=…`) y, en los que no hay en la build de desarrollo (AMF), contra su
/// fuente; el test `low_latency_options_exist` lo vuelve a mirar con `av_opt_find`.
///
/// Así salen en tasa constante ([`RateMode::Constant`]); con techo ([`RateMode::Capped`], lo
/// normal, [`low_latency_options_with`]), cada uno gasta lo que pide la imagen con el objetivo de
/// techo: VBR o CBR con suelo de QP ([`min_qp`]) y sin relleno, según el backend
/// ([`supports_capped`]).
///
/// Comunes: CBR (`b` = `maxrate`) con un búfer VBV de **un fotograma** ([`vbv_buffer_bits`]: así
/// ningún fotograma, intra incluido, pesa mucho más que lo que el enlace pasa en un intervalo; con
/// tres, un intra tras un cambio de escena salía con tres fotogramas de bits y llenaba la cola del
/// camino), sin B-frames, una referencia,
/// GOP muy largo (intra solo al pedirlo), el espacio de color del flujo en el VUI (BT.709
/// en rango limitado, o BT.2020 con PQ en HDR; ver [`color_options`]), el perfil de 4:4:4 o de
/// 10 bits si hace falta ([`profile_option`]) y `slices` si se piden varios.
///
/// - **SVT-AV1**: `preset` 11 (hasta 720p) o 12, y `svtav1-params` con estructura de baja latencia
///   (`pred-struct=1`), CBR (`rc=2`), modo de tiempo real (`rtc=1`), sin *lookahead* ni detección de
///   cambio de escena. SVT-AV1 no tiene *intra-refresh*; los intra salen al pedirlos (con
///   `pred-struct=1` admite forzarlos en cualquier fotograma).
/// - **NVENC**: `preset=p1`, `tune=ull`, `rc=cbr`, `zerolatency`, `delay=0`, `rc-lookahead=0`,
///   `forced-idr`, `intra-refresh=0` y `no-scenecut`.
/// - **AMF**: `usage=ultralowlatency`, `rc=cbr`, `latency` (en AV1, `lowest_latency`),
///   `preencode=0`, `async_depth=1`, `forced_idr` e `intra_refresh_mb=0` en H.264.
/// - **QSV**: `preset=veryfast`, `async_depth=1`, `low_delay_brc`, `forced_idr` y, en H.264 y
///   HEVC, `scenario=remotegaming` e `int_ref_type=none`.
/// - **VA-API**: `rc_mode=CBR`, `async_depth=1`. FFmpeg 8.1 no expone *intra-refresh* en VA-API.
/// - **Vulkan Video**: `rc_mode=cbr`, `tune=ull`, `usage=stream`, `content=rendered`,
///   `async_depth=1`. FFmpeg 8.1 no expone *intra-refresh* en Vulkan.
pub fn low_latency_options(
    codec: Codec,
    backend: Backend,
    config: &EncoderConfig,
) -> Vec<(&'static str, String)> {
    low_latency_options_with(codec, backend, config, RateMode::Constant)
}

/// Suelo de QP de [`RateMode::Capped`]: por debajo, la imagen ya no mejora a la vista y el
/// codificador solo gastaría bits. En la escala de cada códec: H.264 y HEVC de 0 a 51; AV1, el
/// `qindex` de 0 a 255 en NVENC y de 0 a 63 en SVT-AV1.
pub fn min_qp(codec: Codec, backend: Backend) -> u32 {
    match (codec, backend) {
        (Codec::Av1, Backend::Software) => CAPPED_MIN_QP_AV1_SVT,
        (Codec::Av1, Backend::VulkanVideo | Backend::Vaapi) => CAPPED_MIN_QP_AV1_GENERIC,
        (Codec::Av1, _) => CAPPED_MIN_QP_AV1,
        _ => CAPPED_MIN_QP,
    }
}

/// Techo de QP de cada códec (el máximo de su escala), que acompaña al suelo de [`min_qp`].
pub fn max_qp(codec: Codec) -> u32 {
    if codec == Codec::Av1 { 255 } else { 51 }
}

/// Suelo de QP de H.264 y HEVC ([`min_qp`]).
pub const CAPPED_MIN_QP: u32 = 20;

/// Suelo de `qindex` de AV1 por hardware ([`min_qp`]).
pub const CAPPED_MIN_QP_AV1: u32 = 80;

/// Suelo de `qindex` de AV1 en Vulkan Video y VA-API ([`min_qp`]), que lo toman del `qmin`
/// genérico de FFmpeg, que no pasa de 69.
pub const CAPPED_MIN_QP_AV1_GENERIC: u32 = 60;

/// Suelo de QP de SVT-AV1 ([`min_qp`]).
pub const CAPPED_MIN_QP_AV1_SVT: u32 = 14;

/// Si `backend` tiene [`RateMode::Capped`]: todos. Cómo (opciones comprobadas en el código de
/// FFmpeg 8.1):
///
/// - **NVENC**: VBR con `b` = `maxrate` (el objetivo) y suelo de QP ([`min_qp`]).
/// - **SVT-AV1**: CBR de tiempo real con `min-qp`.
/// - **AMF**: CBR sin relleno (`filler_data=0`) y con suelo de QP (`qmin` en H.264, `min_qp_i` y
///   `min_qp_p` en HEVC y AV1).
/// - **Vulkan Video**: CBR con suelo de QP (`qmin`, que pasa a `minQp`/`minQIndex`) en AV1 y H.264
///   (Vulkan no tiene relleno); en HEVC, VBR con la media a la mitad del techo y el control por
///   fotograma ([`per_frame_ceiling`]).
/// - **VA-API**: QVBR ([`CAPPED_QUALITY`]) con el objetivo de techo y `max_frame_size`, y el
///   control por fotograma, que también sube la calidad ([`qvbr_quality`]).
/// - **QSV**: en H.264 y HEVC, CBR sin relleno (`strict=-1` quita la conformidad HRD de NAL) y con
///   suelo de QP (`min_qp_i`, `min_qp_p`); en AV1, que no pasa el suelo al runtime, VBR con la
///   media a la mitad del techo.
///
/// Si un controlador no respeta el techo, [`FfmpegEncoder`] le baja el objetivo hasta que los
/// fotogramas caben (ver [`CEILING_TOLERANCE`]); nunca vuelve a tasa constante por eso. Solo si no
/// abre con estas opciones abre sin suelo.
pub fn supports_capped(backend: Backend) -> bool {
    let _ = backend;
    true
}

/// Si el control del techo de `codec` con `backend` reacciona en cada fotograma (y no solo al
/// cerrar cada segundo): en los modos en que el controlador puede no respetar `maxrate` (QVBR de
/// VA-API, VBR de HEVC en Vulkan Video), un fotograma que no es intra y se pasa del presupuesto de
/// un intervalo baja el objetivo antes del siguiente.
pub fn per_frame_ceiling(codec: Codec, backend: Backend) -> bool {
    matches!(
        (backend, codec),
        (Backend::Vaapi, _) | (Backend::VulkanVideo, Codec::Hevc)
    )
}

/// Calidad de QVBR de VA-API con techo, de 1 (la mejor) a 51, como el suelo de QP de H.264 y HEVC
/// ([`CAPPED_MIN_QP`]). Sube con el recorte del control por fotograma ([`qvbr_quality`]).
pub const CAPPED_QUALITY: u32 = 20;

/// Calidad de QVBR con el objetivo recortado a `trim` del pedido: seis puntos más (la mitad de
/// bits en H.264 y HEVC) por cada vez que se reduce a la mitad, hasta 51. En QVBR el controlador
/// manda por la calidad, y bajar solo `maxrate` no basta si no la respeta.
pub fn qvbr_quality(trim: f64) -> u32 {
    let halvings = (1.0 / trim.clamp(MIN_CEILING_TRIM, 1.0)).log2();
    (f64::from(CAPPED_QUALITY) + 6.0 * halvings)
        .round()
        .min(51.0) as u32
}

/// Fotogramas que espera el control por fotograma tras un recorte antes de volver a mirar (el
/// primero tras reabrir sale intra y el controlador tarda uno más en asentarse).
pub const PER_FRAME_SETTLE: u32 = 2;

/// Veces el presupuesto que puede gastar un codificador con techo ([`RateMode::Capped`]) antes de
/// contar como que no lo respeta: en un segundo, más que esto por el objetivo; o más de
/// [`LATE_FRAME_SHARE`] de sus fotogramas (sin contar los intra) por encima de esto por lo que le
/// toca a un intervalo. Entonces el objetivo que se le pasa baja (como mucho a
/// [`MIN_CEILING_TRIM`] del pedido) hasta que caben.
pub const CEILING_TOLERANCE: f64 = 1.1;

/// Fracción de fotogramas de un segundo que pueden pasarse del presupuesto de un intervalo.
pub const LATE_FRAME_SHARE: f64 = 0.1;

/// Lo más que se baja el objetivo que se le pasa a un codificador que no respeta el techo, por el
/// pedido.
pub const MIN_CEILING_TRIM: f64 = 0.25;

/// Segundos seguidos en que los fotogramas caben con holgura ([`CEILING_RECOVER_ROOM`]) antes de
/// devolver el objetivo recortado.
pub const CEILING_RECOVER_SECONDS: u32 = 2;

/// Fracción del techo pedido por debajo de la cual se vuelve de una vez al objetivo pedido. Cada
/// cambio de bitrate cuesta un intra en todos los backends (VA-API, Vulkan Video, AMF y SVT-AV1 se
/// reabren; NVENC se reconfigura con un IDR, ver [`supports_live_bitrate`]), así que no se devuelve
/// en pasos. FFmpeg 8.1 no deja cambiar el control de tasa en marcha ni en VA-API (los parámetros
/// globales solo van con los IDR y se calculan al abrir) ni en Vulkan
/// (`vkCmdControlVideoCodingKHR` solo al empezar la sesión).
pub const CEILING_RECOVER_ROOM: f64 = 0.5;

/// [`low_latency_options`] con el modo de tasa `mode` (donde el backend lo tiene,
/// [`supports_capped`]; si no, tasa constante).
pub fn low_latency_options_with(
    codec: Codec,
    backend: Backend,
    config: &EncoderConfig,
    mode: RateMode,
) -> Vec<(&'static str, String)> {
    let capped = mode == RateMode::Capped && supports_capped(backend);
    let bitrate = u64::from(config.bitrate_kbps) * 1000;
    let fps = config.fps.max(1);
    let mut options: Vec<(&'static str, String)> = vec![
        ("b", bitrate.to_string()),
        ("maxrate", bitrate.to_string()),
        (
            "bufsize",
            vbv_buffer_bits(config.bitrate_kbps, fps, backend).to_string(),
        ),
        ("bf", "0".into()),
        ("refs", "1".into()),
        ("g", LONG_GOP.to_string()),
    ];
    options.extend(
        color_options(config.format)
            .into_iter()
            .map(|(key, value)| (key, value.to_owned())),
    );
    if let Some(profile) = profile_option(codec, backend, config.format) {
        options.push(("profile", profile.to_owned()));
    }
    // SVT-AV1 no tiene slices (sus *tiles* no salen como unidades independientes del flujo).
    if config.slices > 1 && backend != Backend::Software {
        options.push(("slices", config.slices.to_string()));
    }
    let mut set = |key: &'static str, value: String| {
        if let Some(entry) = options.iter_mut().find(|(k, _)| *k == key) {
            entry.1 = value;
        } else {
            options.push((key, value));
        }
    };
    match backend {
        Backend::Software => {
            let pixels = u64::from(config.width) * u64::from(config.height);
            set(
                "preset",
                if pixels <= 1280 * 720 { "11" } else { "12" }.into(),
            );
            set(
                "svtav1-params",
                if capped {
                    format!(
                        "pred-struct=1:rc=2:rtc=1:lookahead=0:scd=0:min-qp={}",
                        min_qp(codec, backend)
                    )
                } else {
                    "pred-struct=1:rc=2:rtc=1:lookahead=0:scd=0".into()
                },
            );
        }
        Backend::Nvenc => {
            set("preset", "p1".into());
            set("tune", "ull".into());
            if capped {
                // VBR con el objetivo de media y de techo (`b` = `maxrate`) y el VBV de un
                // fotograma, con un suelo de QP: más calidad que esa no se ve, y sin ella el
                // codificador gasta menos. FFmpeg solo pasa el suelo a NVENC en VBR y si también
                // hay techo de QP.
                set("rc", "vbr".into());
                set("qmin", min_qp(codec, backend).to_string());
                set("qmax", max_qp(codec).to_string());
            } else {
                set("rc", "cbr".into());
            }
            set("zerolatency", "1".into());
            set("delay", "0".into());
            set("rc-lookahead", "0".into());
            set("forced-idr", "1".into());
            set("intra-refresh", "0".into());
            set("no-scenecut", "1".into());
        }
        Backend::Amf => {
            set("usage", "ultralowlatency".into());
            set("rc", "cbr".into());
            if capped {
                // CBR sin relleno y con suelo de QP: con la imagen quieta el suelo frena antes del
                // objetivo y no se rellena hasta él.
                let (min, max) = (min_qp(codec, backend), max_qp(codec));
                set("filler_data", "0".into());
                if codec == Codec::H264 {
                    // `h264_amf` toma el suelo de las genéricas.
                    set("qmin", min.to_string());
                    set("qmax", max.to_string());
                } else {
                    set("min_qp_i", min.to_string());
                    set("min_qp_p", min.to_string());
                    set("max_qp_i", max.to_string());
                    set("max_qp_p", max.to_string());
                }
            }
            set(
                "latency",
                if codec == Codec::Av1 {
                    "lowest_latency"
                } else {
                    "1"
                }
                .into(),
            );
            set("preencode", "0".into());
            set("async_depth", "1".into());
            set("forced_idr", "1".into());
            if codec == Codec::H264 {
                // -1 deja el valor del driver: apagado explícito.
                set("intra_refresh_mb", "0".into());
            }
        }
        Backend::Qsv => {
            set("preset", "veryfast".into());
            set("async_depth", "1".into());
            set("low_delay_brc", "1".into());
            set("forced_idr", "1".into());
            if capped {
                if codec == Codec::Av1 {
                    // `av1_qsv` no pasa el suelo de QP al runtime (solo H.264 y HEVC leen `qmin`
                    // y `min_qp_*`): VBR con la media a la mitad del techo.
                    set("b", (bitrate / 2).to_string());
                } else {
                    // CBR sin relleno (sin conformidad HRD de NAL, `strict` < normal) y con
                    // suelo de QP.
                    let min = min_qp(codec, backend).to_string();
                    set("strict", "-1".into());
                    set("min_qp_i", min.clone());
                    set("min_qp_p", min);
                }
            }
            if codec != Codec::Av1 {
                set("scenario", "remotegaming".into());
                // -1 deja el valor del runtime: apagado explícito.
                set("int_ref_type", "none".into());
            }
        }
        Backend::Vaapi => {
            if capped {
                // QVBR: calidad fija ([`CAPPED_QUALITY`], que sube con el recorte del control
                // por fotograma) con el objetivo de techo. En CBR hay controladores que no hacen
                // caso del suelo de QP (`min_qp`) y rellenan hasta el objetivo; en QVBR algunos no
                // hacen caso de `maxrate` en los picos, y por eso el control por fotograma
                // ([`per_frame_ceiling`]).
                set("rc_mode", "QVBR".into());
                set("global_quality", CAPPED_QUALITY.to_string());
                set(
                    "max_frame_size",
                    vbv_buffer_bits(config.bitrate_kbps, fps, backend)
                        .div_ceil(8)
                        .to_string(),
                );
            } else {
                set("rc_mode", "CBR".into());
            }
            set("async_depth", "1".into());
        }
        Backend::VulkanVideo => {
            set("rc_mode", "cbr".into());
            if capped && codec == Codec::Hevc {
                // Hay controladores que no hacen caso del suelo de QP en HEVC (gastan lo mismo
                // que en CBR): VBR con la media a la mitad del techo y el control por fotograma.
                set("rc_mode", "vbr".into());
                set("b", (bitrate / 2).to_string());
            } else if capped {
                // `minQp` (`minQIndex` en AV1) del control de tasa de Vulkan sale de `qmin`; en
                // AV1, `qmax` se queda en el del codificador (255).
                set("qmin", min_qp(codec, backend).to_string());
                if codec != Codec::Av1 {
                    set("qmax", max_qp(codec).to_string());
                }
            }
            set("tune", "ull".into());
            set("usage", "stream".into());
            set("content", "rendered".into());
            set("async_depth", "1".into());
        }
    }
    options
}

/// Señalización del espacio de color en el VUI (H.264/HEVC) o la cabecera de secuencia (AV1):
/// BT.709 en rango limitado en SDR, BT.2020 sin luminancia constante con PQ (SMPTE ST 2084) en HDR.
pub fn color_options(format: VideoFormat) -> [(&'static str, &'static str); 4] {
    if format.is_hdr() {
        [
            ("color_range", "tv"),
            ("colorspace", "bt2020nc"),
            ("color_primaries", "bt2020"),
            ("color_trc", "smpte2084"),
        ]
    } else {
        [
            ("color_range", "tv"),
            ("colorspace", "bt709"),
            ("color_primaries", "bt709"),
            ("color_trc", "bt709"),
        ]
    }
}

/// Perfil que pide `format` a cada codificador (`None`: el de siempre). 4:4:4: *High 4:4:4
/// Predictive* en H.264 (`high444p`), *Rext* en HEVC (`rext`) y *High* en AV1 (`high`, solo
/// Vulkan); HDR: *Main10* en HEVC (`main10`). AV1 *main* ya admite 10 bits y SVT-AV1 no tiene
/// opción de perfil (lo deduce del formato de píxel).
pub fn profile_option(codec: Codec, backend: Backend, format: VideoFormat) -> Option<&'static str> {
    if backend == Backend::Software {
        return None;
    }
    match (format.chroma, format.is_hdr(), codec) {
        (Chroma::Yuv444, _, Codec::H264) => Some("high444p"),
        (Chroma::Yuv444, _, Codec::Hevc) => Some("rext"),
        (Chroma::Yuv444, _, Codec::Av1) => Some("high"),
        (Chroma::Yuv420, true, Codec::Hevc) => Some("main10"),
        _ => None,
    }
}

/// Formato de píxel en CPU que recibe el codificador (o el *pool* de la GPU) para `format`.
pub fn software_format(backend: Backend, format: VideoFormat) -> ffi::AVPixelFormat {
    match (backend, format.chroma, format.is_hdr()) {
        (Backend::Software, _, false) => ffi::AV_PIX_FMT_YUV420P,
        (Backend::Software, _, true) => ffi::AV_PIX_FMT_YUV420P10LE,
        (_, Chroma::Yuv420, true) => ffi::AV_PIX_FMT_P010LE,
        (Backend::VulkanVideo, Chroma::Yuv444, _) => ffi::AV_PIX_FMT_NV24,
        (_, Chroma::Yuv444, _) => ffi::AV_PIX_FMT_YUV444P,
        _ => ffi::AV_PIX_FMT_NV12,
    }
}

/// Si el backend acepta un bitrate nuevo sin reabrir el codificador: NVENC lo reconfigura en el
/// siguiente fotograma (si la GPU anuncia `NV_ENC_CAPS_SUPPORT_DYN_BITRATE_CHANGE`) y QSV con
/// `MFXVideoENCODE_Reset`. SVT-AV1, AMF, VA-API y Vulkan de FFmpeg 8.1 solo lo leen al abrir.
///
/// Sin reabrir no quiere decir sin intra: FFmpeg 8.1 reconfigura NVENC con `resetEncoder` y
/// `forceIDR` (`reconfig_encoder` de `nvenc.c`), y cada cambio es un IDR. Por eso ningún backend
/// dice que cambia el bitrate en marcha ([`Encoder::live_bitrate`] falso): la sesión espacia los
/// cambios igual en todos.
pub fn supports_live_bitrate(backend: Backend) -> bool {
    matches!(backend, Backend::Nvenc | Backend::Qsv)
}

/// Recorte del objetivo tras un segundo medido (`window`, con `budget` bytes por intervalo al
/// objetivo pedido): `(recorte, segundos seguidos que caben)`. Si gastó más de
/// [`CEILING_TOLERANCE`] veces lo que le tocaba o se pasaron más de [`LATE_FRAME_SHARE`] de sus
/// fotogramas, baja en proporción (al menos un 10 %, hasta [`MIN_CEILING_TRIM`]); si cupo con
/// holgura [`CEILING_RECOVER_SECONDS`] seguidos (menos de [`CEILING_RECOVER_ROOM`] del techo),
/// vuelve a 1 de una vez.
fn next_trim(trim: f64, fits: u32, window: &CeilingWindow, budget: f64) -> (f64, u32) {
    let allowed = budget * f64::from(window.frames.max(1));
    let spent = window.bytes as f64;
    let late = f64::from(window.late) > LATE_FRAME_SHARE * f64::from(window.frames);
    // Cada paso cuesta un intra: se vuelve de una vez, y solo con holgura de sobra.
    if spent > allowed * CEILING_TOLERANCE || late {
        let over = (spent / allowed).max(1.0 / 0.9);
        ((trim / over).max(MIN_CEILING_TRIM), 0)
    } else if window.late == 0 && spent < allowed * CEILING_RECOVER_ROOM && trim < 1.0 {
        if fits + 1 < CEILING_RECOVER_SECONDS {
            (trim, fits + 1)
        } else {
            (1.0, 0)
        }
    } else {
        (trim, 0)
    }
}

/// Lo que lleva medido el segundo en curso de [`FfmpegEncoder::watch_ceiling`].
#[derive(Debug, Default, Clone, Copy)]
struct CeilingWindow {
    bytes: u64,
    frames: u32,
    /// Fotogramas (no intra) por encima del presupuesto de un intervalo.
    late: u32,
    largest: usize,
}

/// Codificador abierto con un tamaño concreto (el del flujo: la captura reducida al tope).
struct Session {
    ctx: AVCodecContext,
    frames: Option<AVHWFramesContext>,
    sw: AVFrame,
    sw_format: ffi::AVPixelFormat,
    width: u32,
    height: u32,
    /// Flujo HDR (BT.2020 PQ de 10 bits).
    hdr: bool,
    /// Con *hw frames* de la importación: identidad de su contexto.
    imported: Option<usize>,
    /// Control de tasa leído del contexto ya abierto ([`RateControl`]).
    rate_control: RateControl,
    /// Reducción en CPU de los fotogramas más grandes que la sesión (se crea con el primero).
    scaler: Option<Scaler>,
    /// HDR desde RGB: la conversión propia al tamaño de la captura, antes de reducir.
    converted: Option<AVFrame>,
}

/// Lo que el contexto de FFmpeg dice de su control de tasa **después** de abrirse: `bit_rate`,
/// `rc_max_rate` y `rc_buffer_size` tal como quedaron (lo que el codificador usa de verdad), y la
/// opción privada de modo de tasa que se pidió (`rc_mode`, `rc`…) si el codificador la reconoció.
/// Un `bit_rate` a cero con Vulkan Video sería «No rate control settings specified, using fixed
/// QP = 18» en el registro de FFmpeg: QP fijo e ignorando el bitrate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateControl {
    /// Bitrate objetivo en bit/s (0: sin control de tasa, QP fijo).
    pub bit_rate: i64,
    /// Bitrate máximo en bit/s.
    pub max_rate: i64,
    /// Búfer VBV en bits.
    pub buffer_bits: i32,
    /// Modo de tasa pedido (`cbr`, `CBR`, …) si el backend tiene esa opción y la aceptó.
    pub mode: Option<String>,
}

impl fmt::Display for RateControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.bit_rate <= 0 {
            return f
                .write_str("sin control de tasa (QP fijo): el bitrate no llegó al codificador");
        }
        match &self.mode {
            Some(mode) => write!(f, "{} ", mode.to_uppercase())?,
            None => f.write_str("tasa ")?,
        }
        write!(f, "{} kbit/s", self.bit_rate / 1000)?;
        if self.max_rate > 0 && self.max_rate != self.bit_rate {
            write!(f, " (máx. {} kbit/s)", self.max_rate / 1000)?;
        }
        if self.buffer_bits > 0 {
            write!(f, ", VBV {} kbit", i64::from(self.buffer_bits) / 1000)?;
        }
        Ok(())
    }
}

/// Lee el control de tasa de `ctx`, ya abierto, y el modo pedido en `options` si el codificador
/// lo reconoció (`unused` son las opciones que devolvió `avcodec_open2`).
fn read_rate_control(
    ctx: &AVCodecContext,
    options: &[(&str, String)],
    unused: &[String],
) -> RateControl {
    // SAFETY: `ctx` es un contexto abierto que poseemos; solo se leen tres enteros.
    let (bit_rate, max_rate, buffer_bits) = unsafe {
        let raw = ctx.as_ptr();
        ((*raw).bit_rate, (*raw).rc_max_rate, (*raw).rc_buffer_size)
    };
    let mode = options
        .iter()
        .find(|(key, _)| matches!(*key, "rc_mode" | "rc"))
        .filter(|(key, _)| !unused.iter().any(|u| u == key))
        .map(|(_, value)| value.clone());
    RateControl {
        bit_rate,
        max_rate,
        buffer_bits,
        mode,
    }
}

/// Codificador de vídeo de FFmpeg de baja latencia (ver [`low_latency_options`]).
pub struct FfmpegEncoder {
    codec: Codec,
    backend: Backend,
    av_codec: AVCodecRef<'static>,
    device: Option<AVHWDeviceContext>,
    session: Option<Session>,
    config: Option<EncoderConfig>,
    keyframe_requested: bool,
    next_pts: i64,
    /// `(pts, frame_id, imagen)` de los fotogramas entregados cuyo paquete no ha salido. La imagen
    /// de GPU de la captura (y el búfer de PipeWire que retiene) se suelta cuando sale el paquete:
    /// entonces la GPU ya la leyó ([`import`](super::import), `drm_frame`).
    in_flight: VecDeque<(i64, u64, Option<GpuImage>)>,
    unused_options: Vec<String>,
    reopened: u64,
    /// Paquetes que no correspondían a ningún fotograma pendiente (descartados).
    orphan_packets: u64,
    /// Último fotograma de GPU entregado que no retiene nada de la captura (la copia D3D11 en el
    /// pool propio) y su id, para [`Encoder::repeat_last`].
    last_hw: Option<(AVFrame, u64)>,
    /// Importación de imágenes de GPU.
    importer: Option<GpuImporter>,
    /// Modo de tasa ([`FfmpegEncoder::set_rate_mode`]).
    rate_mode: RateMode,
    /// El controlador no abrió con techo: sigue en tasa constante.
    capped_refused: bool,
    /// El segundo en curso ([`FfmpegEncoder::watch_ceiling`]).
    ceiling_window: CeilingWindow,
    /// Lo que se le pasa al codificador por el objetivo pedido (baja si no respeta el techo).
    ceiling_trim: f64,
    /// Segundos seguidos en que los fotogramas caben con holgura.
    ceiling_fits: u32,
    /// Objetivo pedido (sin recortar), en kbit/s.
    requested_kbps: Option<u32>,
    /// Objetivo recortado pendiente de aplicar antes del siguiente fotograma.
    ceiling_pending: Option<u32>,
    /// Fotogramas que faltan para que el control por fotograma vuelva a mirar.
    ceiling_settle: u32,
    /// La importación sin copias falló una vez: se usa el modo de respaldo.
    zero_copy_failed: bool,
    /// Por qué no se usa la ruta sin copias, si no se usa.
    gpu_note: Option<String>,
    /// Copias a CPU del último fotograma.
    copies: u32,
}

impl std::fmt::Debug for FfmpegEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfmpegEncoder")
            .field("codec", &self.codec)
            .field("backend", &self.backend)
            .field("config", &self.config)
            .field("importer", &self.importer)
            .finish_non_exhaustive()
    }
}

impl FfmpegEncoder {
    /// Modo de tasa ([`RateMode`]) desde la próxima vez que se abra el codificador (si el backend
    /// no tiene [`RateMode::Capped`], sigue en tasa constante).
    pub fn set_rate_mode(&mut self, mode: RateMode) {
        self.rate_mode = mode;
        self.capped_refused = false;
        self.ceiling_trim = 1.0;
        self.ceiling_fits = 0;
        self.ceiling_window = CeilingWindow::default();
    }

    /// Si codifica con calidad fija y techo.
    fn capped(&self) -> bool {
        self.rate_mode == RateMode::Capped && supports_capped(self.backend) && !self.capped_refused
    }

    /// Prepara el codificador de `codec` con `backend` (crea el dispositivo de GPU si hace falta).
    /// Falla con [`CodecError::BackendUnavailable`] si esta build de FFmpeg no lo trae o si no hay
    /// dispositivo; hay que llamar a [`Encoder::configure`] antes de codificar.
    pub fn new(codec: Codec, backend: Backend) -> Result<Self, CodecError> {
        super::init_logging();
        let av_codec = encoder_name(codec, backend)
            .and_then(AVCodec::find_encoder_by_name)
            .ok_or(CodecError::BackendUnavailable(backend))?;
        let device = match encoder_device(backend) {
            Some(device) => {
                Some(create_device(device).map_err(|_| CodecError::BackendUnavailable(backend))?)
            }
            None => None,
        };
        Ok(Self {
            codec,
            backend,
            av_codec,
            device,
            session: None,
            config: None,
            keyframe_requested: false,
            next_pts: 0,
            in_flight: VecDeque::new(),
            unused_options: Vec::new(),
            reopened: 0,
            orphan_packets: 0,
            last_hw: None,
            importer: None,
            rate_mode: RateMode::default(),
            capped_refused: false,
            ceiling_window: CeilingWindow::default(),
            ceiling_trim: 1.0,
            ceiling_fits: 0,
            requested_kbps: None,
            ceiling_pending: None,
            ceiling_settle: 0,
            zero_copy_failed: false,
            gpu_note: None,
            copies: 0,
        })
    }

    /// Por qué las imágenes de GPU no van sin copias, si es el caso (para `doctor` y los
    /// registros).
    pub fn gpu_note(&self) -> Option<&str> {
        self.gpu_note.as_deref()
    }

    /// Si el último fotograma de GPU entró sin copias a CPU.
    pub fn zero_copy(&self) -> bool {
        self.importer
            .as_ref()
            .is_some_and(|importer| importer.mode() == ImportMode::Zero)
    }

    /// Opciones que el codificador no reconoció al abrirse (deberían ser ninguna).
    pub fn unused_options(&self) -> &[String] {
        &self.unused_options
    }

    /// Veces que se reabrió el codificador después de configurarlo (por bitrate o por tamaño).
    pub fn reopened(&self) -> u64 {
        self.reopened
    }

    /// Paquetes descartados por no corresponder a ningún fotograma pendiente (tras cada uno, el
    /// siguiente fotograma sale intra).
    pub fn orphan_packets(&self) -> u64 {
        self.orphan_packets
    }

    fn open(&mut self, config: &EncoderConfig) -> Result<Session, CodecError> {
        self.open_with(config, None)
    }

    /// Abre el codificador; con `imported`, sobre los *hw frames* de la importación. Con techo
    /// ([`RateMode::Capped`]), si el controlador no admite ese modo de tasa, vuelve a abrir en
    /// tasa constante y no lo intenta más.
    fn open_with(
        &mut self,
        config: &EncoderConfig,
        imported: Option<AVHWFramesContext>,
    ) -> Result<Session, CodecError> {
        if self.capped() {
            match self.open_mode(config, imported.clone(), RateMode::Capped) {
                Ok(session) => return Ok(session),
                Err(err) => {
                    super::log_warning(&format!(
                        "{} no abre con techo ({err}); sigue en tasa constante",
                        self.av_codec.name().to_string_lossy()
                    ));
                    self.capped_refused = true;
                }
            }
        }
        self.open_mode(config, imported, RateMode::Constant)
    }

    /// Abre el codificador con el modo de tasa `mode`.
    fn open_mode(
        &mut self,
        config: &EncoderConfig,
        imported: Option<AVHWFramesContext>,
        mode: RateMode,
    ) -> Result<Session, CodecError> {
        let fail = |what: &str, err: &RsmpegError| backend_error(what, err);
        let mut ctx = AVCodecContext::new(&self.av_codec);
        let (width, height) = (config.width, config.height);
        let fps = i32::try_from(config.fps.max(1)).unwrap_or(i32::MAX);
        ctx.set_width(i32::try_from(width).unwrap_or(i32::MAX));
        ctx.set_height(i32::try_from(height).unwrap_or(i32::MAX));
        ctx.set_time_base(ffi::AVRational { num: 1, den: fps });
        ctx.set_framerate(ffi::AVRational { num: fps, den: 1 });
        ctx.set_max_b_frames(0);

        let device_type = encoder_device(self.backend);
        let hw_frames = matches!(
            self.backend,
            Backend::VulkanVideo | Backend::Nvenc | Backend::Vaapi
        );
        let sw_format = software_format(self.backend, config.format);
        let mut frames = None;
        let imported_key = imported
            .as_ref()
            .map(|frames| super::vulkan::buffer_key(frames.as_ptr()));
        match (&self.device, device_type, imported) {
            (_, _, Some(mut imported)) => {
                let format = imported.data().format;
                ctx.set_pix_fmt(format);
                ctx.set_hw_frames_ctx(imported);
            }
            (Some(device), Some(device_type), None) if hw_frames => {
                let mut pool = device.hwframe_ctx_alloc();
                {
                    let data = pool.data();
                    data.format = hw_pixel_format(device_type);
                    data.sw_format = sw_format;
                    data.width = i32::try_from(width).unwrap_or(i32::MAX);
                    data.height = i32::try_from(height).unwrap_or(i32::MAX);
                    data.initial_pool_size = if self.backend == Backend::Vaapi { 8 } else { 0 };
                }
                pool.init()
                    .map_err(|err| fail("marcos de la GPU para el codificador", &err))?;
                ctx.set_pix_fmt(hw_pixel_format(device_type));
                ctx.set_hw_frames_ctx(pool.clone());
                frames = Some(pool);
            }
            (Some(device), _, None) => {
                ctx.set_pix_fmt(sw_format);
                ctx.set_hw_device_ctx(device.clone());
            }
            (None, _, None) => ctx.set_pix_fmt(sw_format),
        }

        let mut options = low_latency_options_with(self.codec, self.backend, config, mode);
        if mode == RateMode::Capped && self.backend == Backend::Vaapi {
            let quality = qvbr_quality(self.ceiling_trim).to_string();
            if let Some(entry) = options.iter_mut().find(|(key, _)| *key == "global_quality") {
                entry.1 = quality;
            }
        }
        self.unused_options = super::open_codec(&mut ctx, &options).map_err(|err| {
            CodecError::Unsupported(format!(
                "{} no abre {}×{} a {} fps: {}",
                self.av_codec.name().to_string_lossy(),
                width,
                height,
                config.fps,
                super::describe(&err)
            ))
        })?;
        let rate_control = read_rate_control(&ctx, &options, &self.unused_options);
        // QSV con techo va en ICQ, sin bitrate.
        if rate_control.bit_rate <= 0 && !(mode == RateMode::Capped && self.backend == Backend::Qsv)
        {
            super::log_warning(&format!(
                "{} abrió sin bitrate ({rate_control}); opciones no reconocidas: {:?}",
                self.av_codec.name().to_string_lossy(),
                self.unused_options
            ));
        }

        let mut sw = AVFrame::new();
        sw.set_format(sw_format);
        sw.set_width(i32::try_from(width).unwrap_or(i32::MAX));
        sw.set_height(i32::try_from(height).unwrap_or(i32::MAX));
        sw.alloc_buffer()
            .map_err(|err| fail("búfer del fotograma", &err))?;
        // Lo que lleva el VUI: libswscale toma de aquí la matriz y el rango al reducir.
        tag_color(&mut sw, config.format.is_hdr(), false);
        self.in_flight.clear();
        self.next_pts = 0;
        Ok(Session {
            ctx,
            frames,
            sw,
            sw_format,
            width,
            height,
            hdr: config.format.is_hdr(),
            imported: imported_key,
            rate_control,
            scaler: None,
            converted: None,
        })
    }

    /// Control de tasa con el que quedó abierto el codificador ([`RateControl`]).
    pub fn rate_control_info(&self) -> Option<&RateControl> {
        self.session.as_ref().map(|session| &session.rate_control)
    }

    /// Reabre con `config` (bitrate nuevo) conservando el tamaño abierto y el modo de entrada
    /// (CPU o *hw frames* importados).
    fn reopen(&mut self, config: EncoderConfig) -> Result<(), CodecError> {
        let (imported, config) = match &self.session {
            Some(session) => {
                let imported = match &self.importer {
                    Some(importer) if session.imported.is_some() => importer.encoder_frames(),
                    _ => None,
                };
                let open = EncoderConfig {
                    width: session.width,
                    height: session.height,
                    ..config
                };
                (imported, open)
            }
            None => (None, config),
        };
        self.reopen_as(config, imported)
    }

    /// Reabre con `config` (con el tamaño del flujo), sobre los *hw frames* `imported` o con
    /// entrada en CPU. El tope de resolución configurado no cambia: solo lo demás de `config`.
    fn reopen_as(
        &mut self,
        config: EncoderConfig,
        imported: Option<AVHWFramesContext>,
    ) -> Result<(), CodecError> {
        self.session = None;
        self.last_hw = None;
        let session = self.open_with(&config, imported)?;
        self.session = Some(session);
        let (width, height) = self
            .config
            .map_or((config.width, config.height), |cap| (cap.width, cap.height));
        self.config = Some(EncoderConfig {
            width,
            height,
            ..config
        });
        self.reopened += 1;
        Ok(())
    }

    /// Recoge los paquetes que haya y los deja seguidos en `out` (vaciado antes); espera al del
    /// fotograma `pts` como mucho `wait`. `None` si no salió ninguno.
    fn drain_into(
        &mut self,
        pts: i64,
        wait: Duration,
        out: &mut Vec<u8>,
    ) -> Result<Option<EncodedFrame>, CodecError> {
        out.clear();
        let started = Instant::now();
        let deadline = started + wait;
        let mut encoded: Option<EncodedFrame> = None;
        let mut orphans = 0u32;
        let Some(session) = self.session.as_mut() else {
            return Ok(None);
        };
        loop {
            match session.ctx.receive_packet() {
                Ok(packet) => {
                    let (packet_pts, keyframe, data) = packet_bytes(&packet);
                    // El fotograma de un paquete se identifica por su pts.
                    let mut frame_id = None;
                    while let Some(&(front_pts, id, _)) = self.in_flight.front() {
                        if front_pts > packet_pts {
                            break;
                        }
                        self.in_flight.pop_front();
                        frame_id = Some(id);
                    }
                    match frame_id.or(encoded.map(|e| e.frame_id)) {
                        Some(id) => {
                            out.extend_from_slice(data);
                            encoded = Some(EncodedFrame {
                                frame_id: encoded.map_or(id, |e| e.frame_id),
                                keyframe: keyframe || encoded.is_some_and(|e| e.keyframe),
                            });
                        }
                        // Un paquete de ningún fotograma pendiente (pts inesperado): se pierde, y
                        // con él las referencias de lo que venga detrás.
                        None => orphans += 1,
                    }
                }
                Err(RsmpegError::EncoderDrainError) => {
                    let done = self
                        .in_flight
                        .front()
                        .is_none_or(|(front, _, _)| *front > pts);
                    let now = Instant::now();
                    if done || now >= deadline {
                        break;
                    }
                    if now.duration_since(started) < OUTPUT_SPIN {
                        std::thread::yield_now();
                    } else {
                        std::thread::sleep(OUTPUT_POLL);
                    }
                }
                Err(RsmpegError::EncoderFlushedError) => break,
                Err(err) => return Err(backend_error("recibir el paquete", &err)),
            }
        }
        if orphans > 0 {
            // El invitado no tendrá la referencia de ese paquete: el siguiente fotograma, intra.
            self.keyframe_requested = true;
            self.orphan_packets += u64::from(orphans);
            super::log_warning(&format!(
                "{} entregó {orphans} paquete(s) sin fotograma pendiente; se descartan y se pide un \
                 intra",
                self.backend
            ));
        }
        Ok(encoded)
    }

    /// Como [`FfmpegEncoder::drain_into`], repartido en sus slices reales (NAL u OBU), en orden.
    fn drain(&mut self, pts: i64, wait: Duration) -> Result<Vec<EncodedSlice>, CodecError> {
        let mut data = Vec::new();
        let Some(encoded) = self.drain_into(pts, wait, &mut data)? else {
            return Ok(Vec::new());
        };
        let ranges = crate::slices::split_slices(self.codec, &data);
        let count = u8::try_from(ranges.len()).unwrap_or(u8::MAX);
        Ok(ranges
            .into_iter()
            .enumerate()
            .map(|(index, range)| EncodedSlice {
                frame_id: encoded.frame_id,
                index: u8::try_from(index).unwrap_or(u8::MAX),
                count,
                keyframe: encoded.keyframe,
                data: data[range].to_vec(),
            })
            .collect())
    }
}

/// Pts, marca de intra y datos (prestados) de un paquete.
fn packet_bytes(packet: &AVPacket) -> (i64, bool, &[u8]) {
    let len = usize::try_from(packet.size).unwrap_or(0);
    let data: &[u8] = if packet.data.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: un `AVPacket` devuelto por `avcodec_receive_packet` tiene `data` apuntando a
        // `size` bytes válidos mientras viva el paquete, al que queda atado el préstamo.
        unsafe { std::slice::from_raw_parts(packet.data, len) }
    };
    let keyframe = packet.flags & ffi::AV_PKT_FLAG_KEY as i32 != 0;
    (packet.pts, keyframe, data)
}

/// Lee pts, marca de intra y una copia de los datos de un paquete.
#[cfg(test)]
fn read_packet(packet: &AVPacket) -> (i64, bool, Vec<u8>) {
    let (pts, keyframe, data) = packet_bytes(packet);
    (pts, keyframe, data.to_vec())
}

/// Planos escribibles de un `AVFrame` de CPU ya reservado: `(plano, bytes por fila)`.
fn frame_planes(frame: &mut AVFrame, rows: [usize; 3]) -> Vec<(&mut [u8], u32)> {
    let linesize = *frame.linesize_mut();
    let data = *frame.data_mut();
    let mut planes = Vec::with_capacity(3);
    for (plane, rows) in rows.into_iter().enumerate() {
        let (ptr, stride) = (data[plane], linesize[plane]);
        if ptr.is_null() || stride <= 0 || rows == 0 {
            break;
        }
        let stride = stride as usize;
        // SAFETY: `av_frame_get_buffer` reservó para el plano `plane` al menos `linesize × filas`
        // bytes (las filas del plano para el formato y el alto del fotograma), el fotograma es
        // escribible (`make_writable`) y nadie más lo toca mientras vivan estos préstamos, que
        // están atados al `&mut AVFrame`. Los planos no se solapan.
        let slice = unsafe { std::slice::from_raw_parts_mut(ptr, stride * rows) };
        planes.push((slice, stride as u32));
    }
    planes
}

/// Deja `frame` en el `AVFrame` de CPU del codificador, en el formato de la sesión y a su tamaño:
/// si mide lo mismo, la conversión de color propia ([`convert_into`]); si es más grande (la
/// captura por encima del tope), libswscale lo reduce y convierte en un solo paso
/// ([`Scaler`]), salvo en HDR desde RGB, donde la conversión propia va primero (al tamaño de la
/// captura) y la reducción después. Devuelve las pasadas por la imagen que hizo (1 o 2).
fn fill(session: &mut Session, frame: &RawFrame) -> Result<u32, CodecError> {
    session
        .sw
        .make_writable()
        .map_err(|err| backend_error("fotograma escribible", &err))?;
    if frame.format == PixelFormat::Gpu {
        return Err(gpu_in_cpu_path());
    }
    let sw_format = session.sw_format;
    if (frame.width, frame.height) == (session.width, session.height) {
        if let Some(scaler) = session.scaler.as_mut() {
            scaler.clear_last();
        }
        convert_into(&mut session.sw, sw_format, frame)?;
        return Ok(1);
    }
    if frame.width < session.width || frame.height < session.height {
        return Err(CodecError::Unsupported(format!(
            "el fotograma de {}×{} es más pequeño que la sesión de {}×{}",
            frame.width, frame.height, session.width, session.height
        )));
    }
    let scaler = match session.scaler.as_mut() {
        Some(scaler) => scaler,
        None => session.scaler.insert(Scaler::new()?),
    };
    let from_rgb = matches!(frame.format, PixelFormat::Bgra | PixelFormat::Rgba16F);
    if session.hdr && from_rgb {
        // BT.2020 PQ de 10 bits desde RGB: lo hace la conversión propia (libswscale no), al
        // tamaño de la captura; después se reduce de 10 bits a 10 bits.
        let size = (
            i32::try_from(frame.width).unwrap_or(i32::MAX),
            i32::try_from(frame.height).unwrap_or(i32::MAX),
        );
        let stale = session
            .converted
            .as_ref()
            .is_none_or(|c| (c.width, c.height) != size || c.format != sw_format);
        if stale {
            let mut converted = AVFrame::new();
            converted.set_format(sw_format);
            converted.set_width(size.0);
            converted.set_height(size.1);
            converted
                .alloc_buffer()
                .map_err(|err| backend_error("búfer de la conversión HDR", &err))?;
            tag_color(&mut converted, true, false);
            session.converted = Some(converted);
        }
        let Some(converted) = session.converted.as_mut() else {
            return Err(CodecError::Backend("sin búfer de la conversión HDR".into()));
        };
        converted
            .make_writable()
            .map_err(|err| backend_error("fotograma escribible", &err))?;
        convert_into(converted, sw_format, frame)?;
        scaler.scale_frame(converted, &mut session.sw)?;
        return Ok(2);
    }
    scaler.scale(frame, &mut session.sw)?;
    Ok(1)
}

/// Copia un fotograma (BGRA, RGBA de coma flotante, I420, NV12, I444, NV24 o P010) en `dst`, un
/// `AVFrame` de CPU de su mismo tamaño, convirtiendo al formato `sw_format`.
fn convert_into(
    dst: &mut AVFrame,
    sw_format: ffi::AVPixelFormat,
    frame: &RawFrame,
) -> Result<(), CodecError> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (cw, ch) = RawFrame::chroma_size(frame.width, frame.height);
    let (cw, ch) = (cw as usize, ch as usize);
    let short = || CodecError::Unsupported("búfer del fotograma más corto que su tamaño".into());
    let mismatch = || {
        CodecError::Unsupported(format!(
            "el codificador no convierte {:?} a {}",
            frame.format,
            rsmpeg::avutil::get_pix_fmt_name(sw_format).map_or_else(
                || "?".to_owned(),
                |name| name.to_string_lossy().into_owned()
            )
        ))
    };
    if frame.format == PixelFormat::Gpu {
        return Err(gpu_in_cpu_path());
    }
    if sw_format == ffi::AV_PIX_FMT_YUV420P {
        let mut planes = frame_planes(dst, [h, ch, ch]);
        let [y, u, v] = planes.as_mut_slice() else {
            return Err(CodecError::Backend(
                "el fotograma no tiene tres planos".into(),
            ));
        };
        match frame.format {
            PixelFormat::Bgra => bgra_to_i420_planes(
                frame,
                [(&mut *y.0, y.1), (&mut *u.0, u.1), (&mut *v.0, v.1)],
            ),
            PixelFormat::I420 => {
                let (sy, su, sv) = frame.i420_planes().ok_or_else(short)?;
                copy_plane(sy, frame.stride as usize, y, w, h)?;
                copy_plane(su, cw, u, cw, ch)?;
                copy_plane(sv, cw, v, cw, ch)
            }
            PixelFormat::Nv12 => {
                let (sy, suv) = frame.nv12_planes().ok_or_else(short)?;
                copy_plane(sy, frame.stride as usize, y, w, h)?;
                for row in 0..ch {
                    let src = suv
                        .get(row * 2 * cw..(row + 1) * 2 * cw)
                        .ok_or_else(short)?;
                    let du = &mut u.0[row * u.1 as usize..][..cw];
                    let dv = &mut v.0[row * v.1 as usize..][..cw];
                    for (x, &[pu, pv]) in src.as_chunks::<2>().0.iter().enumerate() {
                        du[x] = pu;
                        dv[x] = pv;
                    }
                }
                Ok(())
            }
            _ => Err(mismatch()),
        }
    } else if sw_format == ffi::AV_PIX_FMT_NV12 {
        let mut planes = frame_planes(dst, [h, ch, 0]);
        let [y, uv] = planes.as_mut_slice() else {
            return Err(CodecError::Backend(
                "el fotograma no tiene dos planos".into(),
            ));
        };
        match frame.format {
            PixelFormat::Bgra => bgra_to_nv12_planes(frame, [(&mut *y.0, y.1), (&mut *uv.0, uv.1)]),
            PixelFormat::I420 => i420_to_nv12_planes(frame, [(&mut *y.0, y.1), (&mut *uv.0, uv.1)]),
            PixelFormat::Nv12 => {
                let (sy, suv) = frame.nv12_planes().ok_or_else(short)?;
                copy_plane(sy, frame.stride as usize, y, w, h)?;
                copy_plane(suv, 2 * cw, uv, 2 * cw, ch)
            }
            _ => Err(mismatch()),
        }
    } else if sw_format == ffi::AV_PIX_FMT_YUV444P {
        let mut planes = frame_planes(dst, [h, h, h]);
        let [y, u, v] = planes.as_mut_slice() else {
            return Err(CodecError::Backend(
                "el fotograma no tiene tres planos".into(),
            ));
        };
        match frame.format {
            PixelFormat::Bgra => bgra_to_i444_planes(
                frame,
                [(&mut *y.0, y.1), (&mut *u.0, u.1), (&mut *v.0, v.1)],
            ),
            PixelFormat::I444 => {
                let (sy, su, sv) = frame.i444_planes().ok_or_else(short)?;
                copy_plane(sy, frame.stride as usize, y, w, h)?;
                copy_plane(su, w, u, w, h)?;
                copy_plane(sv, w, v, w, h)
            }
            _ => Err(mismatch()),
        }
    } else if sw_format == ffi::AV_PIX_FMT_NV24 {
        let mut planes = frame_planes(dst, [h, h, 0]);
        let [y, uv] = planes.as_mut_slice() else {
            return Err(CodecError::Backend(
                "el fotograma no tiene dos planos".into(),
            ));
        };
        match frame.format {
            PixelFormat::Bgra => bgra_to_nv24_planes(frame, [(&mut *y.0, y.1), (&mut *uv.0, uv.1)]),
            PixelFormat::Nv24 => {
                let (sy, suv) = frame.nv24_planes().ok_or_else(short)?;
                copy_plane(sy, frame.stride as usize, y, w, h)?;
                copy_plane(suv, 2 * w, uv, 2 * w, h)
            }
            _ => Err(mismatch()),
        }
    } else if sw_format == ffi::AV_PIX_FMT_P010LE {
        let mut planes = frame_planes(dst, [h, ch, 0]);
        let [y, uv] = planes.as_mut_slice() else {
            return Err(CodecError::Backend(
                "el fotograma no tiene dos planos".into(),
            ));
        };
        match frame.format {
            PixelFormat::Bgra | PixelFormat::Rgba16F => to_hdr10_planes(
                frame,
                TenBitPlanes::P010 {
                    y: (&mut *y.0, y.1),
                    uv: (&mut *uv.0, uv.1),
                },
            ),
            PixelFormat::P010 => {
                let (sy, suv) = frame.p010_planes().ok_or_else(short)?;
                copy_plane(sy, frame.stride as usize, y, 2 * w, h)?;
                copy_plane(suv, 4 * cw, uv, 4 * cw, ch)
            }
            _ => Err(mismatch()),
        }
    } else if sw_format == ffi::AV_PIX_FMT_YUV420P10LE {
        let mut planes = frame_planes(dst, [h, ch, ch]);
        let [y, u, v] = planes.as_mut_slice() else {
            return Err(CodecError::Backend(
                "el fotograma no tiene tres planos".into(),
            ));
        };
        match frame.format {
            PixelFormat::Bgra | PixelFormat::Rgba16F => to_hdr10_planes(
                frame,
                TenBitPlanes::Planar {
                    y: (&mut *y.0, y.1),
                    u: (&mut *u.0, u.1),
                    v: (&mut *v.0, v.1),
                },
            ),
            PixelFormat::P010 => {
                // P010 → tres planos de 10 bits en la parte baja.
                let (sy, suv) = frame.p010_planes().ok_or_else(short)?;
                let word = |src: &[u8], at: usize| -> Result<[u8; 2], CodecError> {
                    let bytes = src.get(at..at + 2).ok_or_else(short)?;
                    Ok((u16::from_le_bytes([bytes[0], bytes[1]]) >> 6).to_le_bytes())
                };
                for row in 0..h {
                    for x in 0..w {
                        let value = word(sy, row * frame.stride as usize + 2 * x)?;
                        y.0[row * y.1 as usize + 2 * x..][..2].copy_from_slice(&value);
                    }
                }
                for row in 0..ch {
                    for x in 0..cw {
                        let cb = word(suv, row * 4 * cw + 4 * x)?;
                        let cr = word(suv, row * 4 * cw + 4 * x + 2)?;
                        u.0[row * u.1 as usize + 2 * x..][..2].copy_from_slice(&cb);
                        v.0[row * v.1 as usize + 2 * x..][..2].copy_from_slice(&cr);
                    }
                }
                Ok(())
            }
            _ => Err(mismatch()),
        }
    } else {
        Err(mismatch())
    }
}

fn gpu_in_cpu_path() -> CodecError {
    CodecError::Unsupported("una imagen de GPU no se puede copiar por la ruta de CPU".into())
}

fn copy_plane(
    src: &[u8],
    src_stride: usize,
    (dst, dst_stride): &mut (&mut [u8], u32),
    width: usize,
    rows: usize,
) -> Result<(), CodecError> {
    let dst_stride = *dst_stride as usize;
    for row in 0..rows {
        let from = src
            .get(row * src_stride..row * src_stride + width)
            .ok_or_else(|| CodecError::Unsupported("plano de entrada incompleto".into()))?;
        dst[row * dst_stride..][..width].copy_from_slice(from);
    }
    Ok(())
}

impl Drop for FfmpegEncoder {
    /// Vacía el codificador antes de cerrarlo (SVT-AV1 se queja si se cierra sin fin de flujo).
    fn drop(&mut self) {
        if let Some(session) = self.session.as_mut()
            && session.ctx.send_frame(None).is_ok()
        {
            for _ in 0..64 {
                if session.ctx.receive_packet().is_err() {
                    break;
                }
            }
        }
    }
}

impl Encoder for FfmpegEncoder {
    fn codec(&self) -> Codec {
        self.codec
    }

    fn backend(&self) -> Backend {
        self.backend
    }

    fn configure(&mut self, config: &EncoderConfig) -> Result<(), CodecError> {
        if config.codec != self.codec {
            return Err(CodecError::Unsupported(format!(
                "este codificador es de {}, no de {}",
                self.codec, config.codec
            )));
        }
        if config.width == 0
            || config.height == 0
            || !config.width.is_multiple_of(2)
            || !config.height.is_multiple_of(2)
        {
            return Err(CodecError::Unsupported(format!(
                "el códec necesita un tamaño par y no nulo, no {}×{}",
                config.width, config.height
            )));
        }
        if config.fps == 0 || config.bitrate_kbps == 0 {
            return Err(CodecError::Unsupported(
                "los fps y el bitrate deben ser mayores que cero".into(),
            ));
        }
        self.session = None;
        self.last_hw = None;
        let session = self.open(config)?;
        self.session = Some(session);
        self.config = Some(*config);
        self.reopened = 0;
        self.requested_kbps = Some(config.bitrate_kbps);
        self.ceiling_trim = 1.0;
        self.ceiling_fits = 0;
        self.ceiling_window = CeilingWindow::default();
        self.ceiling_pending = None;
        self.ceiling_settle = 0;
        Ok(())
    }

    /// Cambia el bitrate en caliente donde el backend lo admite ([`supports_live_bitrate`]);
    /// donde no, reabre el codificador y el siguiente fotograma sale intra.
    fn set_bitrate(&mut self, bitrate_kbps: u32) -> Result<(), CodecError> {
        if bitrate_kbps == 0 {
            return Err(CodecError::Unsupported(
                "el bitrate debe ser mayor que cero".into(),
            ));
        }
        self.requested_kbps = Some(bitrate_kbps);
        let trimmed = ((f64::from(bitrate_kbps) * self.ceiling_trim).round() as u32).max(1);
        self.apply_bitrate(trimmed)
    }

    fn repeat_last(&mut self) -> Option<Result<Vec<EncodedSlice>, CodecError>> {
        let (last, frame_id) = self.last_hw.as_ref()?;
        let (mut hw, frame_id) = (last.clone(), *frame_id);
        let fps = self.config?.fps;
        let pts = self.next_pts;
        self.next_pts += 1;
        self.keyframe_requested = false;
        hw.set_pts(pts);
        hw.set_pict_type(picture_type(true));
        let session = self.session.as_mut()?;
        Some(match session.ctx.send_frame(Some(&hw)) {
            Ok(()) | Err(RsmpegError::SendFrameAgainError) => {
                self.copies = 0;
                self.finish_frame(Submitted {
                    pts,
                    frame_id,
                    image: None,
                    fps,
                })
            }
            Err(err) => Err(backend_error("entregar el fotograma repetido", &err)),
        })
    }

    fn request_keyframe(&mut self) {
        self.keyframe_requested = true;
    }

    fn encode(&mut self, frame: &RawFrame) -> Result<Vec<EncodedSlice>, CodecError> {
        self.apply_ceiling_trim()?;
        let slices = match self.submit(frame)? {
            Some(submitted) => self.finish_frame(submitted)?,
            None => Vec::new(),
        };
        if !slices.is_empty() {
            let keyframe = slices.iter().any(|slice| slice.keyframe);
            self.watch_ceiling(slices.iter().map(|slice| slice.data.len()).sum(), keyframe);
        }
        Ok(slices)
    }

    fn encode_into(
        &mut self,
        frame: &RawFrame,
        out: &mut Vec<u8>,
    ) -> Result<Option<EncodedFrame>, CodecError> {
        self.apply_ceiling_trim()?;
        let encoded = match self.submit(frame)? {
            Some(submitted) => self.finish_frame_into(submitted, out)?,
            None => {
                out.clear();
                None
            }
        };
        if let Some(encoded) = &encoded {
            self.watch_ceiling(out.len(), encoded.keyframe);
        }
        Ok(encoded)
    }

    fn gpu_input(&self) -> GpuInput {
        if crate::gpu::zero_copy_disabled() {
            GpuInput::None
        } else {
            super::import::gpu_input_for(self.backend, self.device.as_ref())
        }
    }

    fn rate_control(&self) -> Option<String> {
        self.rate_control_info().map(ToString::to_string)
    }

    fn cpu_copies(&self) -> u32 {
        self.copies
    }

    fn output_size(&self) -> Option<(u32, u32)> {
        self.session
            .as_ref()
            .filter(|_| self.next_pts > 0)
            .map(|session| (session.width, session.height))
    }

    fn cpu_scale(&self) -> Option<CpuScale> {
        // Solo cuenta si el último fotograma pasó por la ruta de CPU con reducción: el escalador
        // guarda la última y la olvida con cada fotograma que no la necesita ([`fill`]).
        self.session
            .as_ref()
            .and_then(|session| session.scaler.as_ref())
            .and_then(Scaler::last)
    }
}

/// Fotograma entregado al codificador, pendiente de su paquete.
struct Submitted {
    pts: i64,
    frame_id: u64,
    /// Imagen de GPU que hay que retener hasta que salga el paquete.
    image: Option<GpuImage>,
    fps: u32,
}

impl FfmpegEncoder {
    /// Entrega `frame` al codificador (por la ruta de CPU o la de GPU). `None` si no se entregó
    /// nada (el importador no sacó imagen).
    fn submit(&mut self, frame: &RawFrame) -> Result<Option<Submitted>, CodecError> {
        let Some(config) = self.config else {
            return Err(CodecError::Unsupported(
                "el codificador no está configurado".into(),
            ));
        };
        if !frame.width.is_multiple_of(2) || !frame.height.is_multiple_of(2) || frame.width == 0 {
            return Err(CodecError::Unsupported(format!(
                "el códec necesita un tamaño par, no {}×{}",
                frame.width, frame.height
            )));
        }
        // La captura de pantalla puede traer otro tamaño que el configurado: se reabre al del
        // fotograma reducido a lo que cabe en el tope (lo que ya cabe, tal cual).
        let (width, height) = fit((frame.width, frame.height), (config.width, config.height));
        let config = EncoderConfig {
            width,
            height,
            ..config
        };
        match frame.gpu.clone() {
            Some(_) if !config.format.is_default() => Err(CodecError::Unsupported(format!(
                "las imágenes de GPU de la captura van solo en 4:2:0 SDR, no en {} (la sesión \
                 pide la captura en memoria de CPU para 4:4:4 y HDR)",
                config.format
            ))),
            Some(image) => self.encode_gpu(frame, &image, config),
            None if frame.format == PixelFormat::Rgba16F && !config.format.is_hdr() => {
                // Captura HDR y flujo SDR: tone mapping a BGRA antes de convertir.
                let sdr = rgba16f_to_bgra(frame)?;
                self.encode_cpu(&sdr, config).map(Some)
            }
            None => self.encode_cpu(frame, config).map(Some),
        }
    }

    /// Reabre si la sesión no es de CPU o no tiene el tamaño de `config`.
    fn ensure_cpu_session(&mut self, config: EncoderConfig) -> Result<(), CodecError> {
        let stale = self.session.as_ref().is_none_or(|s| {
            s.imported.is_some() || (s.width, s.height) != (config.width, config.height)
        });
        if stale {
            self.reopen_as(config, None)?;
        }
        Ok(())
    }

    /// Ruta con copias: imagen en CPU, conversión de color y, con *hw frames*, subida.
    fn encode_cpu(
        &mut self,
        frame: &RawFrame,
        config: EncoderConfig,
    ) -> Result<Submitted, CodecError> {
        self.ensure_cpu_session(config)?;
        self.last_hw = None;
        let pts = self.next_pts;
        self.next_pts += 1;
        let keyframe = std::mem::take(&mut self.keyframe_requested);
        let copies = self.send_cpu(frame, pts, keyframe)?;
        self.copies = copies;
        Ok(Submitted {
            pts,
            frame_id: frame.frame_id,
            image: None,
            fps: config.fps,
        })
    }

    /// Copia `frame` en el `AVFrame` de CPU de la sesión (convertido y, si es más grande,
    /// reducido), lo sube a la GPU si hace falta y lo entrega. Devuelve las copias a CPU que hizo.
    fn send_cpu(&mut self, frame: &RawFrame, pts: i64, keyframe: bool) -> Result<u32, CodecError> {
        let Some(session) = self.session.as_mut() else {
            return Err(CodecError::Backend("sin sesión del codificador".into()));
        };
        let passes = fill(session, frame)?;
        let pict_type = picture_type(keyframe);
        session.sw.set_pts(pts);
        session.sw.set_pict_type(pict_type);
        let (sent, copies) = match session.frames.as_mut() {
            Some(pool) => {
                let mut hw = AVFrame::new();
                pool.get_buffer(&mut hw)
                    .map_err(|err| backend_error("marco de la GPU", &err))?;
                hw.hwframe_transfer_data(&session.sw)
                    .map_err(|err| backend_error("subida a la GPU", &err))?;
                hw.set_pts(pts);
                hw.set_pict_type(pict_type);
                (session.ctx.send_frame(Some(&hw)), passes + 1)
            }
            None => (session.ctx.send_frame(Some(&session.sw)), passes),
        };
        match sent {
            Ok(()) | Err(RsmpegError::SendFrameAgainError) => Ok(copies),
            Err(err) => Err(backend_error("entregar el fotograma", &err)),
        }
    }

    /// Ruta sin copias: importación de la imagen de GPU de la captura.
    fn encode_gpu(
        &mut self,
        frame: &RawFrame,
        image: &GpuImage,
        config: EncoderConfig,
    ) -> Result<Option<Submitted>, CodecError> {
        let size = (frame.width, frame.height);
        // Lo que sale de la importación: la captura reducida al tope si no cabe.
        let output = (config.width, config.height);
        if self
            .importer
            .as_ref()
            .is_none_or(|importer| !importer.matches(image, size, output))
        {
            self.importer = None;
            let importer = self.build_importer(image, size, output)?;
            self.importer = Some(importer);
        }
        let wanted = self.importer.as_ref().and_then(GpuImporter::encoder_frames);
        match wanted {
            Some(frames) => {
                let key = super::vulkan::buffer_key(frames.as_ptr());
                if self
                    .session
                    .as_ref()
                    .is_none_or(|s| s.imported != Some(key))
                    && let Err(err) = self.reopen_as(config, Some(frames))
                {
                    // El codificador no acepta esos *hw frames*: respaldo con descarga.
                    self.note(format!(
                        "{} no abre con las imágenes importadas ({err}); se bajan a CPU",
                        self.backend
                    ));
                    self.zero_copy_failed = true;
                    self.importer = None;
                    let importer = self.build_importer(image, size, output)?;
                    self.importer = Some(importer);
                    self.ensure_cpu_session(config)?;
                }
            }
            None => self.ensure_cpu_session(config)?,
        }
        let pts = self.next_pts;
        self.next_pts += 1;
        let keyframe = std::mem::take(&mut self.keyframe_requested);
        let Some(importer) = self.importer.as_mut() else {
            return Err(CodecError::Backend("sin importador".into()));
        };
        let prepared = importer
            .prepare(image, frame.frame_id, pts)
            .map_err(|err| {
                CodecError::GpuImport(format!(
                    "no se pudo importar la imagen {} en {}: {err}",
                    image.kind(),
                    self.backend
                ))
            })?;
        let mut copies = importer.copies();
        let mut sent = false;
        for item in prepared {
            match item {
                Prepared::Hw(mut hw) => {
                    hw.set_pict_type(picture_type(keyframe));
                    let Some(session) = self.session.as_mut() else {
                        return Err(CodecError::Backend("sin sesión del codificador".into()));
                    };
                    match session.ctx.send_frame(Some(&hw)) {
                        Ok(()) | Err(RsmpegError::SendFrameAgainError) => {}
                        Err(err) => return Err(backend_error("entregar el fotograma", &err)),
                    }
                    // Una textura D3D11 se copió a una del pool propio (8): guardar esa no retiene
                    // nada de la captura. Un DMA-BUF mapeado sí retendría el búfer de PipeWire.
                    #[cfg(windows)]
                    {
                        self.last_hw =
                            matches!(image, GpuImage::D3D11(_)).then(|| (hw, frame.frame_id));
                    }
                    #[cfg(not(windows))]
                    {
                        drop(hw);
                        self.last_hw = None;
                    }
                }
                Prepared::Cpu(raw) => {
                    self.last_hw = None;
                    copies += self.send_cpu(&raw, pts, keyframe)?;
                }
            }
            sent = true;
        }
        self.copies = copies;
        if !sent {
            return Ok(None);
        }
        // Un DMA-BUF se retiene hasta su paquete (el mapeo de FFmpeg no lo retiene); una textura
        // D3D11 ya se copió al pool propio.
        #[cfg(unix)]
        let held = matches!(image, GpuImage::DmaBuf(_)).then(|| image.clone());
        #[cfg(not(unix))]
        let held = None;
        Ok(Some(Submitted {
            pts,
            frame_id: frame.frame_id,
            image: held,
            fps: config.fps,
        }))
    }

    /// Importador para `image` de `size`, que entrega `output` (la imagen reducida al tope si no
    /// cabe): sin copias si no falló antes; si no se puede, el de respaldo.
    fn build_importer(
        &mut self,
        image: &GpuImage,
        size: (u32, u32),
        output: (u32, u32),
    ) -> Result<GpuImporter, CodecError> {
        if !self.zero_copy_failed {
            match GpuImporter::new(
                self.backend,
                self.device.as_ref(),
                image,
                size,
                output,
                ImportMode::Zero,
            ) {
                Ok(importer) => return Ok(importer),
                Err(err) => {
                    self.note(err);
                    self.zero_copy_failed = true;
                }
            }
        }
        GpuImporter::new(
            self.backend,
            self.device.as_ref(),
            image,
            size,
            output,
            ImportMode::Download,
        )
        .map_err(|err| {
            CodecError::GpuImport(format!(
                "{} no puede leer imágenes {} de la captura: {err}",
                self.backend,
                image.kind()
            ))
        })
    }

    fn note(&mut self, note: impl Into<String>) {
        let note = note.into();
        super::log_warning(&note);
        self.gpu_note = Some(note);
    }

    /// Anota el fotograma en vuelo y devuelve cuánto esperar a su paquete.
    fn track(&mut self, submitted: Submitted) -> (i64, Duration) {
        let Submitted {
            pts,
            frame_id,
            image,
            fps,
        } = submitted;
        self.in_flight.push_back((pts, frame_id, image));
        let period = Duration::from_secs(1) / fps.max(1);
        (pts, (period * OUTPUT_WAIT_FRAMES).max(OUTPUT_WAIT_MIN))
    }

    /// Anota el fotograma en vuelo y recoge lo que haya salido, en slices.
    /// Pasa `bitrate_kbps` al codificador: en marcha si el backend lo admite
    /// ([`supports_live_bitrate`]); si no, reabriéndolo.
    fn apply_bitrate(&mut self, bitrate_kbps: u32) -> Result<(), CodecError> {
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
        if supports_live_bitrate(self.backend)
            && let Some(session) = self.session.as_mut()
        {
            let bitrate = i64::from(bitrate_kbps) * 1000;
            let vbv = i32::try_from(vbv_buffer_bits(bitrate_kbps, config.fps, self.backend))
                .unwrap_or(i32::MAX);
            session.ctx.set_bit_rate(bitrate);
            // SAFETY: `as_mut_ptr` apunta al `AVCodecContext` abierto que posee `session.ctx`;
            // `rc_max_rate` y `rc_buffer_size` son enteros que el codificador lee al
            // reconfigurarse en el siguiente `avcodec_send_frame`, y nadie más usa el contexto
            // ahora (`&mut self`). El VBV sigue al bitrate: un fotograma al objetivo nuevo.
            unsafe {
                let ctx = session.ctx.as_mut_ptr();
                (*ctx).rc_max_rate = bitrate;
                (*ctx).rc_buffer_size = vbv;
            }
            self.config = Some(config);
            return Ok(());
        }
        self.reopen(config)
    }

    /// Con techo, mira que el codificador lo respete (ver [`CEILING_TOLERANCE`]): si en un segundo
    /// gasta de más o se pasan demasiados fotogramas del presupuesto de un intervalo, le baja el
    /// objetivo en proporción; si caben con holgura ([`CEILING_RECOVER_ROOM`])
    /// [`CEILING_RECOVER_SECONDS`] seguidos, se lo devuelve de una vez. En [`per_frame_ceiling`], el
    /// recorte va en el mismo fotograma que se pasa.
    fn watch_ceiling(&mut self, bytes: usize, keyframe: bool) {
        let Some(config) = self.config.filter(|_| self.capped()) else {
            return;
        };
        let requested = self.requested_kbps.unwrap_or(config.bitrate_kbps);
        let fps = config.fps.max(1);
        // Lo que le toca a un intervalo al objetivo pedido (el techo del enlace).
        let budget = f64::from(requested) * 1000.0 / 8.0 / f64::from(fps);
        if per_frame_ceiling(self.codec, self.backend) {
            self.ceiling_settle = self.ceiling_settle.saturating_sub(1);
            if !keyframe && self.ceiling_settle == 0 && bytes as f64 > budget * CEILING_TOLERANCE {
                // En el mismo fotograma: en proporción a lo que se pasó (al menos un 10 %).
                let over = (bytes as f64 / budget).max(1.0 / 0.9);
                let trim = (self.ceiling_trim / over).max(MIN_CEILING_TRIM);
                if trim < self.ceiling_trim {
                    self.ceiling_trim = trim;
                    self.ceiling_fits = 0;
                    self.ceiling_settle = PER_FRAME_SETTLE + 1;
                    self.ceiling_pending =
                        Some(((f64::from(requested) * trim).round() as u32).max(1));
                }
            }
        }
        let window = &mut self.ceiling_window;
        window.bytes += bytes as u64;
        window.frames += 1;
        window.largest = window.largest.max(bytes);
        if !keyframe && bytes as f64 > budget * CEILING_TOLERANCE {
            window.late += 1;
        }
        if window.frames < fps {
            return;
        }
        let window = std::mem::take(&mut self.ceiling_window);
        let spent = window.bytes as f64;
        let before = self.ceiling_trim;
        let (trim, fits) = next_trim(self.ceiling_trim, self.ceiling_fits, &window, budget);
        // Con control por fotograma, los recortes ya los hizo él; del segundo queda la vuelta.
        if trim >= before || !per_frame_ceiling(self.codec, self.backend) {
            (self.ceiling_trim, self.ceiling_fits) = (trim, fits);
        }
        if (self.ceiling_trim - before).abs() > f64::EPSILON {
            let trimmed = ((f64::from(requested) * self.ceiling_trim).round() as u32).max(1);
            if self.ceiling_trim < before {
                super::log_warning(&format!(
                    "{} se pasa del techo de {requested} kbit/s ({:.0} kbit/s, fotograma mayor {} KB); \
                     se le pide {trimmed}",
                    self.av_codec.name().to_string_lossy(),
                    spent * 8.0 / 1000.0 * f64::from(fps) / f64::from(window.frames),
                    window.largest / 1000
                ));
            }
            self.ceiling_pending = Some(trimmed);
        }
    }

    /// Aplica el objetivo recortado por [`FfmpegEncoder::watch_ceiling`] antes del siguiente
    /// fotograma.
    fn apply_ceiling_trim(&mut self) -> Result<(), CodecError> {
        match self.ceiling_pending.take() {
            Some(kbps) => self.apply_bitrate(kbps),
            None => Ok(()),
        }
    }

    /// Objetivo pedido (el techo), sin el recorte de [`FfmpegEncoder::watch_ceiling`].
    pub fn requested_kbps(&self) -> Option<u32> {
        self.requested_kbps
    }

    /// Lo que se le pasa al codificador por el objetivo pedido (1 si respeta el techo).
    pub fn ceiling_trim(&self) -> f64 {
        self.ceiling_trim
    }

    fn finish_frame(&mut self, submitted: Submitted) -> Result<Vec<EncodedSlice>, CodecError> {
        let (pts, wait) = self.track(submitted);
        self.drain(pts, wait)
    }

    /// Anota el fotograma en vuelo y recoge lo que haya salido, entero, en `out`.
    fn finish_frame_into(
        &mut self,
        submitted: Submitted,
        out: &mut Vec<u8>,
    ) -> Result<Option<EncodedFrame>, CodecError> {
        let (pts, wait) = self.track(submitted);
        self.drain_into(pts, wait, out)
    }
}

/// Tipo de imagen pedido para un fotograma.
fn picture_type(keyframe: bool) -> ffi::AVPictureType {
    if keyframe {
        ffi::AV_PICTURE_TYPE_I
    } else {
        ffi::AV_PICTURE_TYPE_NONE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    fn config(codec: Codec) -> EncoderConfig {
        EncoderConfig {
            codec,
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_kbps: 10_000,
            slices: 4,
            format: VideoFormat::SDR_420,
        }
    }

    /// Indica si `name` es una opción del contexto genérico o de la clase privada del codificador.
    fn option_exists(codec: &AVCodec, name: &str) -> bool {
        let Ok(name) = CString::new(name) else {
            return false;
        };
        let generic = rsmpeg::avcodec::AVCodecContext::new(codec);
        // SAFETY: `av_opt_find` recorre las tablas de opciones de un objeto con `AVClass` sin
        // modificarlo: el contexto recién creado (genéricas) y, con `AV_OPT_SEARCH_FAKE_OBJ`, un
        // puntero a la `priv_class` del codificador (privadas), que vive lo que FFmpeg.
        unsafe {
            let ctx = generic.as_ptr() as *mut std::ffi::c_void;
            if !ffi::av_opt_find(ctx, name.as_ptr(), std::ptr::null(), 0, 0).is_null() {
                return true;
            }
            let class = codec.priv_class;
            if class.is_null() {
                return false;
            }
            let fake = &class as *const *const ffi::AVClass as *mut std::ffi::c_void;
            !ffi::av_opt_find(
                fake,
                name.as_ptr(),
                std::ptr::null(),
                0,
                ffi::AV_OPT_SEARCH_FAKE_OBJ as i32,
            )
            .is_null()
        }
    }

    #[test]
    fn low_latency_options_exist() {
        let mut checked = Vec::new();
        for codec in Codec::PREFERENCE {
            for backend in Backend::ALL {
                let Some(av_codec) =
                    encoder_name(codec, backend).and_then(AVCodec::find_encoder_by_name)
                else {
                    continue;
                };
                for mode in [RateMode::Constant, RateMode::Capped] {
                    for (key, _) in low_latency_options_with(codec, backend, &config(codec), mode) {
                        assert!(
                            option_exists(&av_codec, key),
                            "{} no tiene la opción `{key}` ({mode:?})",
                            av_codec.name().to_string_lossy()
                        );
                    }
                }
                checked.push(av_codec.name().to_string_lossy().into_owned());
            }
        }
        eprintln!("opciones comprobadas en: {checked:?}");
        assert!(
            checked.iter().any(|name| name == "libsvtav1"),
            "{checked:?}"
        );
    }

    /// Por defecto ningún codificador por hardware va en tasa constante pura: en CBR llevan suelo
    /// de QP (con la imagen quieta frena antes del objetivo) y sin relleno donde FFmpeg 8.1 deja
    /// quitarlo (AMF, QSV); si no, VBR.
    #[test]
    fn no_hardware_encoder_spends_a_constant_rate_by_default() {
        for codec in Codec::PREFERENCE {
            for backend in Backend::ALL {
                if backend == Backend::Software {
                    continue;
                }
                let options =
                    low_latency_options_with(codec, backend, &config(codec), RateMode::default());
                let value = |key: &str| {
                    options
                        .iter()
                        .find(|(k, _)| *k == key)
                        .map(|(_, v)| v.to_ascii_lowercase())
                };
                let mode = value("rc").or_else(|| value("rc_mode"));
                let floor = ["qmin", "min_qp_i", "min_qp_p"]
                    .iter()
                    .any(|key| value(key).is_some());
                let variable = match backend {
                    // QSV elige VBR cuando `maxrate` pasa de `b`.
                    Backend::Qsv => value("b") != value("maxrate"),
                    _ => matches!(mode.as_deref(), Some("vbr" | "qvbr")),
                };
                assert!(
                    variable || floor,
                    "{codec:?}/{backend:?} en tasa constante sin suelo: {options:?}"
                );
                if mode.as_deref() == Some("cbr") || backend == Backend::Qsv && !variable {
                    let no_filler = match backend {
                        Backend::Amf => value("filler_data").as_deref() == Some("0"),
                        Backend::Qsv => value("strict").as_deref() == Some("-1"),
                        _ => true,
                    };
                    assert!(no_filler, "{codec:?}/{backend:?} con relleno: {options:?}");
                }
            }
        }
    }

    /// La calidad de QVBR sube seis puntos por cada vez que el objetivo se reduce a la mitad.
    #[test]
    fn qvbr_quality_follows_the_trim() {
        assert_eq!(qvbr_quality(1.0), CAPPED_QUALITY);
        assert_eq!(qvbr_quality(0.5), CAPPED_QUALITY + 6);
        assert_eq!(qvbr_quality(MIN_CEILING_TRIM), CAPPED_QUALITY + 12);
        assert_eq!(
            qvbr_quality(0.01),
            CAPPED_QUALITY + 12,
            "no baja de MIN_CEILING_TRIM"
        );
        assert!(per_frame_ceiling(Codec::Hevc, Backend::Vaapi));
        assert!(per_frame_ceiling(Codec::Hevc, Backend::VulkanVideo));
        assert!(!per_frame_ceiling(Codec::Av1, Backend::VulkanVideo));
        assert!(!per_frame_ceiling(Codec::Av1, Backend::Nvenc));
    }

    /// Un codificador que se pasa del techo recibe menos objetivo en proporción, nunca por debajo de
    /// [`MIN_CEILING_TRIM`] ni vuelta a tasa constante; si después cabe con holgura, lo recupera.
    #[test]
    fn an_encoder_over_its_ceiling_gets_a_lower_target_until_it_fits() {
        let budget = 1000.0;
        let second = |bytes: u64, late: u32| CeilingWindow {
            bytes,
            frames: 60,
            late,
            largest: 0,
        };
        // Gastó el doble: la mitad de objetivo.
        let (trim, fits) = next_trim(1.0, 0, &second(120_000, 0), budget);
        assert!((trim - 0.5).abs() < 1e-9 && fits == 0, "{trim}");
        // Gasta lo de un segundo pero con muchos fotogramas pasados: baja un 10 %.
        let (trim, _) = next_trim(1.0, 0, &second(60_000, 10), budget);
        assert!((trim - 0.9).abs() < 1e-9, "{trim}");
        // Por mucho que se pase, no baja de MIN_CEILING_TRIM.
        let mut trim = 1.0;
        for _ in 0..20 {
            trim = next_trim(trim, 0, &second(600_000, 60), budget).0;
        }
        assert!((trim - MIN_CEILING_TRIM).abs() < 1e-9, "{trim}");
        // Justo en el techo no se mueve.
        assert_eq!(next_trim(0.8, 3, &second(60_000, 2), budget), (0.8, 0));
        // Vuelve de una vez, y solo con menos de CEILING_RECOVER_ROOM del techo.
        let (mut trim, mut fits) = (MIN_CEILING_TRIM, 0);
        for _ in 0..CEILING_RECOVER_SECONDS {
            (trim, fits) = next_trim(trim, fits, &second(40_000, 0), budget);
        }
        assert!(
            (trim - MIN_CEILING_TRIM).abs() < 1e-9,
            "con poca holgura se queda: {trim}"
        );
        for _ in 0..CEILING_RECOVER_SECONDS {
            (trim, fits) = next_trim(trim, fits, &second(20_000, 0), budget);
        }
        assert!((trim - 1.0).abs() < 1e-9 && fits == 0, "{trim}");
    }

    /// El VBV es de un fotograma al bitrate configurado, en todos los backends.
    #[test]
    fn the_vbv_buffer_holds_one_frame() {
        assert_eq!(vbv_buffer_bits(10_000, 60, Backend::Nvenc), 166_666);
        assert_eq!(vbv_buffer_bits(10_000, 120, Backend::Vaapi), 83_333);
        assert_eq!(
            vbv_buffer_bits(10_000, 0, Backend::Nvenc),
            10_000_000,
            "sin fps, un segundo"
        );
        assert_eq!(vbv_buffer_bits(0, 60, Backend::Nvenc), 1);
        // SVT-AV1 no baja de 20 ms: a 60 fps son 1,2 fotogramas; a 30, un fotograma.
        assert_eq!(vbv_buffer_bits(10_000, 60, Backend::Software), 200_000);
        assert_eq!(vbv_buffer_bits(10_000, 30, Backend::Software), 333_333);
        for codec in Codec::PREFERENCE {
            for backend in Backend::ALL {
                let options = low_latency_options(codec, backend, &config(codec));
                let bufsize = options
                    .iter()
                    .find(|(key, _)| *key == "bufsize")
                    .map(|(_, value)| value.as_str());
                let expected = if backend == Backend::Software {
                    "200000"
                } else {
                    "166666"
                };
                assert_eq!(bufsize, Some(expected), "{codec:?} {backend:?}");
            }
        }
    }

    #[test]
    fn colour_is_signalled_in_bt709_or_bt2020_pq() {
        let get = |options: &[(&str, String)], key: &str| {
            options
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.clone())
        };
        let sdr = low_latency_options(Codec::Hevc, Backend::Nvenc, &config(Codec::Hevc));
        assert_eq!(get(&sdr, "colorspace").as_deref(), Some("bt709"));
        assert_eq!(get(&sdr, "color_primaries").as_deref(), Some("bt709"));
        assert_eq!(get(&sdr, "color_trc").as_deref(), Some("bt709"));
        assert_eq!(get(&sdr, "color_range").as_deref(), Some("tv"));
        assert_eq!(get(&sdr, "profile"), None);
        let hdr_config = EncoderConfig {
            format: VideoFormat::HDR10_420,
            ..config(Codec::Hevc)
        };
        let hdr = low_latency_options(Codec::Hevc, Backend::Nvenc, &hdr_config);
        assert_eq!(get(&hdr, "colorspace").as_deref(), Some("bt2020nc"));
        assert_eq!(get(&hdr, "color_trc").as_deref(), Some("smpte2084"));
        assert_eq!(get(&hdr, "profile").as_deref(), Some("main10"));
        let full = EncoderConfig {
            format: VideoFormat::SDR_444,
            ..config(Codec::H264)
        };
        let nvenc = low_latency_options(Codec::H264, Backend::Nvenc, &full);
        assert_eq!(get(&nvenc, "profile").as_deref(), Some("high444p"));
        assert_eq!(
            software_format(Backend::Nvenc, VideoFormat::SDR_444),
            ffi::AV_PIX_FMT_YUV444P
        );
        assert_eq!(
            software_format(Backend::VulkanVideo, VideoFormat::SDR_444),
            ffi::AV_PIX_FMT_NV24
        );
        assert_eq!(
            software_format(Backend::Software, VideoFormat::HDR10_420),
            ffi::AV_PIX_FMT_YUV420P10LE
        );
        assert_eq!(
            software_format(Backend::Qsv, VideoFormat::HDR10_420),
            ffi::AV_PIX_FMT_P010LE
        );
        let svt = low_latency_options(Codec::Av1, Backend::Software, &hdr_config);
        assert_eq!(get(&svt, "profile"), None, "SVT-AV1 lo deduce del formato");
    }

    /// Los formatos de píxel que declara cada codificador de la build respaldan la tabla de
    /// [`crate::encoder_supports`] (los de VA-API y Vulkan solo declaran su formato de GPU: ahí
    /// manda el perfil, comprobado en `profile_option`).
    #[test]
    fn encoder_pixel_formats_back_the_table() {
        let mut checked = Vec::new();
        for codec in Codec::PREFERENCE {
            for backend in Backend::ALL {
                let Some(av_codec) =
                    encoder_name(codec, backend).and_then(AVCodec::find_encoder_by_name)
                else {
                    continue;
                };
                let Some(formats) = av_codec.pix_fmts() else {
                    continue;
                };
                let hw_only = formats.iter().all(|f| {
                    [
                        ffi::AV_PIX_FMT_VAAPI,
                        ffi::AV_PIX_FMT_VULKAN,
                        ffi::AV_PIX_FMT_QSV,
                        ffi::AV_PIX_FMT_CUDA,
                    ]
                    .contains(f)
                });
                if hw_only {
                    continue;
                }
                for format in [VideoFormat::SDR_444, VideoFormat::HDR10_420] {
                    if crate::encoder_supports(codec, backend, format) {
                        let wanted = software_format(backend, format);
                        assert!(
                            formats.contains(&wanted),
                            "{} no acepta {wanted} para {format}",
                            av_codec.name().to_string_lossy()
                        );
                    }
                }
                checked.push(av_codec.name().to_string_lossy().into_owned());
            }
        }
        eprintln!("formatos comprobados en: {checked:?}");
        assert!(checked.iter().any(|name| name == "libsvtav1"));
    }

    /// SVT-AV1 a 10 bits de verdad: abre con `yuv420p10le` y BT.2020 PQ y entrega paquetes.
    #[test]
    fn svt_av1_encodes_hdr10() {
        let mut encoder = FfmpegEncoder::new(Codec::Av1, Backend::Software).unwrap();
        encoder
            .configure(&EncoderConfig {
                width: 256,
                height: 256,
                format: VideoFormat::HDR10_420,
                ..config(Codec::Av1)
            })
            .unwrap();
        assert!(
            encoder.unused_options().is_empty(),
            "{:?}",
            encoder.unused_options()
        );
        let mut out = 0;
        for id in 0..4 {
            out += encoder
                .encode(&super::super::probe::test_pattern(256, id))
                .unwrap()
                .len();
        }
        assert!(out > 0, "SVT-AV1 entregó paquetes de 10 bits");
    }

    #[test]
    fn options_follow_the_contract() {
        let nvenc = low_latency_options(Codec::Hevc, Backend::Nvenc, &config(Codec::Hevc));
        let get = |options: &[(&str, String)], key: &str| {
            options
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get(&nvenc, "preset").as_deref(), Some("p1"));
        assert_eq!(get(&nvenc, "tune").as_deref(), Some("ull"));
        assert_eq!(get(&nvenc, "forced-idr").as_deref(), Some("1"));
        assert_eq!(get(&nvenc, "bf").as_deref(), Some("0"));
        assert_eq!(get(&nvenc, "slices").as_deref(), Some("4"));
        let svt = low_latency_options(Codec::Av1, Backend::Software, &config(Codec::Av1));
        assert_eq!(get(&svt, "preset").as_deref(), Some("12"));
        assert!(get(&svt, "svtav1-params").is_some_and(|p| p.contains("pred-struct=1")));
        assert_eq!(get(&svt, "b"), get(&svt, "maxrate"), "CBR");
        let amf = low_latency_options(Codec::Av1, Backend::Amf, &config(Codec::Av1));
        assert_eq!(get(&amf, "usage").as_deref(), Some("ultralowlatency"));
        assert_eq!(get(&amf, "latency").as_deref(), Some("lowest_latency"));
        let qsv = low_latency_options(Codec::H264, Backend::Qsv, &config(Codec::H264));
        assert_eq!(get(&qsv, "low_delay_brc").as_deref(), Some("1"));
        let vaapi = low_latency_options(Codec::H264, Backend::Vaapi, &config(Codec::H264));
        assert_eq!(get(&vaapi, "rc_mode").as_deref(), Some("CBR"));
    }

    /// NVENC y QSV cambian el bitrate sin reabrir, pero en NVENC cada cambio es un IDR (FFmpeg
    /// 8.1): ningún backend dice que lo cambia en marcha y la sesión espacia los cambios igual en
    /// todos.
    #[test]
    fn no_backend_changes_bitrate_live() {
        assert!(supports_live_bitrate(Backend::Nvenc) && supports_live_bitrate(Backend::Qsv));
        for codec in Codec::PREFERENCE {
            for backend in Backend::ALL {
                // Sin la GPU del backend (la CI) no abre el dispositivo.
                if let Ok(encoder) = FfmpegEncoder::new(codec, backend) {
                    assert!(!encoder.live_bitrate(), "{codec:?}/{backend:?}");
                }
            }
        }
    }

    /// Ningún codificador repinta por su cuenta: con la imagen quieta, la franja de un
    /// *intra-refresh* que recorre la pantalla fotograma a fotograma se ve como un parpadeo. En
    /// cada codificador compilado, toda opción de *intra-refresh* que tenga se pide apagada (sin
    /// pedirla queda la del driver), el GOP es infinito y el intra sale solo al pedirlo.
    #[test]
    fn no_encoder_refreshes_on_its_own() {
        const NAMES: [&str; 4] = [
            "intra-refresh",
            "intra_refresh_mb",
            "int_ref_type",
            "intra_refresh",
        ];
        let mut checked = Vec::new();
        for codec in Codec::PREFERENCE {
            for backend in Backend::ALL {
                let Some(av_codec) =
                    encoder_name(codec, backend).and_then(AVCodec::find_encoder_by_name)
                else {
                    continue;
                };
                let name = av_codec.name().to_string_lossy().into_owned();
                let options = low_latency_options(codec, backend, &config(codec));
                let get = |key: &str| {
                    options
                        .iter()
                        .find(|(k, _)| *k == key)
                        .map(|(_, v)| v.as_str())
                };
                for option in NAMES {
                    if option_exists(&av_codec, option) {
                        assert!(
                            matches!(get(option), Some("0" | "none")),
                            "{name}: `{option}` no se pide apagado ({:?})",
                            get(option)
                        );
                    }
                }
                assert_eq!(get("g"), Some(LONG_GOP.to_string().as_str()), "{name}: GOP");
                let forced = if backend == Backend::Software {
                    // SVT-AV1 con `pred-struct=1` admite intra en cualquier fotograma.
                    "svtav1-params"
                } else {
                    "forced_idr"
                };
                if backend != Backend::Vaapi && backend != Backend::VulkanVideo {
                    assert!(
                        options.iter().any(|(key, _)| key.starts_with(&forced[..4])),
                        "{name}: sin intra forzado"
                    );
                }
                checked.push(name);
            }
        }
        eprintln!("sin intra-refresh comprobado en: {checked:?}");
        for (codec, backend) in [
            (Codec::Hevc, Backend::Nvenc),
            (Codec::H264, Backend::Amf),
            (Codec::Hevc, Backend::Qsv),
        ] {
            let options = low_latency_options(codec, backend, &config(codec));
            assert!(
                options
                    .iter()
                    .any(|(key, value)| NAMES.contains(key)
                        && matches!(value.as_str(), "0" | "none")),
                "{codec:?}/{backend:?}: intra-refresh apagado aunque esta build no lo traiga"
            );
        }
    }

    /// Las opciones genéricas del diccionario llegan al contexto: `b`, `maxrate` y `bufsize`
    /// quedan en `bit_rate`, `rc_max_rate` y `rc_buffer_size` tras `avcodec_open2`, y
    /// [`read_rate_control`] los lee de ahí (un cero sería el «fixed QP» de Vulkan Video).
    #[test]
    fn rate_control_is_read_back_from_the_open_context() {
        let Some(codec) = AVCodec::find_encoder_by_name(c"libopenh264") else {
            eprintln!("AVISO: esta build de FFmpeg no trae libopenh264; se omite");
            return;
        };
        let mut ctx = AVCodecContext::new(&codec);
        ctx.set_width(320);
        ctx.set_height(192);
        ctx.set_time_base(ffi::AVRational { num: 1, den: 30 });
        ctx.set_pix_fmt(ffi::AV_PIX_FMT_YUV420P);
        let options = vec![
            ("b", "1000000".to_owned()),
            ("maxrate", "1000000".to_owned()),
            ("bufsize", "33333".to_owned()),
            // `rc` (el modo de NVENC y AMF) no existe en libopenh264: vuelve sin usar.
            ("rc", "cbr".to_owned()),
        ];
        let unused = super::super::open_codec(&mut ctx, &options).unwrap();
        assert_eq!(unused, vec!["rc".to_owned()]);
        let rc = read_rate_control(&ctx, &options, &unused);
        assert_eq!(rc.bit_rate, 1_000_000);
        assert_eq!(rc.max_rate, 1_000_000);
        assert_eq!(rc.buffer_bits, 33_333);
        assert_eq!(
            rc.mode, None,
            "libopenh264 no tiene `rc`: cuenta como no usada"
        );
        assert_eq!(rc.to_string(), "tasa 1000 kbit/s, VBV 33 kbit");
        let vulkan_like = RateControl {
            mode: Some("cbr".into()),
            ..rc
        };
        assert_eq!(vulkan_like.to_string(), "CBR 1000 kbit/s, VBV 33 kbit");
        let none = RateControl {
            bit_rate: 0,
            max_rate: 0,
            buffer_bits: 0,
            mode: None,
        };
        assert!(none.to_string().contains("QP fijo"));
    }

    /// Slices reales: `libopenh264` de FFmpeg con `slices=4` escribe cuatro NAL de slice por
    /// fotograma; [`crate::slices::split_slices`] los separa, cada uno empieza por su NAL y,
    /// juntos, el `h264` de FFmpeg los decodifica.
    #[test]
    fn real_h264_slices_are_split_and_decode() {
        let Some(codec) = AVCodec::find_encoder_by_name(c"libopenh264") else {
            eprintln!("AVISO: esta build de FFmpeg no trae libopenh264; se omite");
            return;
        };
        let (w, h) = (320i32, 192i32);
        let mut ctx = AVCodecContext::new(&codec);
        ctx.set_width(w);
        ctx.set_height(h);
        ctx.set_time_base(ffi::AVRational { num: 1, den: 30 });
        ctx.set_pix_fmt(ffi::AV_PIX_FMT_YUV420P);
        ctx.set_bit_rate(1_000_000);
        super::super::open_codec(&mut ctx, &[("slices", "4".to_owned())]).unwrap();
        let mut decoder =
            crate::ffmpeg::FfmpegDecoder::new(Codec::H264, crate::DecoderBackend::Software)
                .unwrap();
        let mut seen = 0;
        for index in 0..3i64 {
            let mut frame = AVFrame::new();
            frame.set_format(ffi::AV_PIX_FMT_YUV420P);
            frame.set_width(w);
            frame.set_height(h);
            frame.alloc_buffer().unwrap();
            let linesize = *frame.linesize_mut();
            let data = *frame.data_mut();
            for (plane, rows) in [(0usize, h), (1, h / 2), (2, h / 2)] {
                for row in 0..rows as usize {
                    // SAFETY: plano reservado por `alloc_buffer` con `linesize × filas` bytes.
                    let line = unsafe {
                        std::slice::from_raw_parts_mut(
                            data[plane].add(row * linesize[plane] as usize),
                            linesize[plane] as usize,
                        )
                    };
                    for (x, px) in line.iter_mut().enumerate() {
                        *px = ((x + row + index as usize * 7) % 200) as u8 + 16;
                    }
                }
            }
            frame.set_pts(index);
            ctx.send_frame(Some(&frame)).unwrap();
            while let Ok(packet) = ctx.receive_packet() {
                let (_, _, data) = read_packet(&packet);
                let ranges = crate::slices::split_slices(Codec::H264, &data);
                assert_eq!(ranges.len(), 4, "cuatro slices por fotograma");
                let slices: Vec<EncodedSlice> = ranges
                    .iter()
                    .enumerate()
                    .map(|(i, range)| EncodedSlice {
                        frame_id: index as u64,
                        index: i as u8,
                        count: 4,
                        keyframe: index == 0,
                        data: data[range.clone()].to_vec(),
                    })
                    .collect();
                for slice in &slices[1..] {
                    assert!(
                        slice.data.starts_with(&[0, 0, 0, 1]) || slice.data.starts_with(&[0, 0, 1]),
                        "cada slice empieza por su código de inicio"
                    );
                }
                let image = crate::Decoder::decode(&mut decoder, &slices)
                    .unwrap()
                    .expect("imagen");
                assert_eq!((image.width, image.height), (w as u32, h as u32));
                seen += 1;
            }
        }
        assert!(seen >= 1, "libopenh264 entregó fotogramas");
    }

    #[test]
    fn software_av1_does_not_ask_for_slices() {
        let svt = low_latency_options(Codec::Av1, Backend::Software, &config(Codec::Av1));
        assert!(svt.iter().all(|(key, _)| *key != "slices"));
        let vaapi = low_latency_options(Codec::Hevc, Backend::Vaapi, &config(Codec::Hevc));
        assert!(
            vaapi
                .iter()
                .any(|(key, value)| *key == "slices" && value == "4")
        );
    }

    /// Selección de ruta sin GPU: un codificador por software no pide imágenes de GPU y, si le
    /// llega una (por ejemplo, tras renegociar el códec), lo explica en vez de copiar a ciegas.
    #[cfg(unix)]
    #[test]
    fn software_encoders_refuse_gpu_images_with_a_clear_error() {
        use crate::gpu::{DRM_FORMAT_XRGB8888, DmaBufImage, DmaBufPlane};

        let mut encoder = FfmpegEncoder::new(Codec::Av1, Backend::Software).unwrap();
        encoder
            .configure(&EncoderConfig {
                width: 256,
                height: 256,
                ..config(Codec::Av1)
            })
            .unwrap();
        assert_eq!(encoder.gpu_input(), GpuInput::None);
        let file = std::fs::File::open("/dev/null").unwrap();
        let image = GpuImage::DmaBuf(std::sync::Arc::new(DmaBufImage {
            planes: vec![DmaBufPlane {
                fd: file.into(),
                offset: 0,
                stride: 1024,
            }],
            modifier: crate::gpu::DRM_FORMAT_MOD_LINEAR,
            fourcc: DRM_FORMAT_XRGB8888,
            width: 256,
            height: 256,
            keepalive: None,
        }));
        let err = encoder
            .encode(&RawFrame::gpu(1, (256, 256), image))
            .unwrap_err();
        assert!(
            matches!(err, CodecError::GpuImport(_)),
            "la sesión pasa la captura a CPU al ver este error: {err}"
        );
        let text = err.to_string();
        assert!(text.contains("DMA-BUF"), "{text}");
        assert!(!encoder.zero_copy());
        assert!(
            encoder.gpu_note().is_some(),
            "anota por qué no hay ruta sin copias"
        );
        // Y sigue sirviendo para imágenes de CPU.
        let frame = super::super::probe::test_pattern(256, 2);
        assert!(encoder.encode(&frame).is_ok());
        assert_eq!(encoder.cpu_copies(), 1, "conversión de color en CPU");
    }

    #[test]
    fn unavailable_backends_are_reported() {
        assert_eq!(
            FfmpegEncoder::new(Codec::Hevc, Backend::Software).err(),
            Some(CodecError::BackendUnavailable(Backend::Software))
        );
    }
}
