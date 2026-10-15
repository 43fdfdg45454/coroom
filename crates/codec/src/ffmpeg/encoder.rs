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
//! - [`intra_refresh`] dice cómo se refresca cada backend sin IDR grandes.

use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, Instant};

use rsmpeg::avcodec::{AVCodec, AVCodecContext, AVCodecRef, AVPacket};
use rsmpeg::avutil::{AVFrame, AVHWDeviceContext, AVHWFramesContext};
use rsmpeg::error::RsmpegError;
use rsmpeg::ffi;

use super::import::{GpuImporter, ImportMode, Prepared};
use super::{backend_error, create_device, encoder_device, encoder_name, hw_pixel_format};
use crate::color::{Chroma, VideoFormat};
use crate::convert::{
    TenBitPlanes, bgra_to_i420_planes, bgra_to_i444_planes, bgra_to_nv12_planes,
    bgra_to_nv24_planes, i420_to_nv12_planes, rgba16f_to_bgra, to_hdr10_planes,
};
use crate::gpu::GpuImage;
use crate::{
    Backend, Codec, CodecError, EncodedFrame, EncodedSlice, Encoder, EncoderConfig, GpuInput,
    PixelFormat, RawFrame,
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
///   `forced-idr`, `intra-refresh` (con él, `g` es el periodo de la ola de refresco: un segundo) y
///   `no-scenecut`.
/// - **AMF**: `usage=ultralowlatency`, `rc=cbr`, `latency` (en AV1, `lowest_latency`),
///   `preencode=0`, `async_depth=1`, `forced_idr` e `intra_refresh_mb` en H.264 (refresco en un
///   segundo).
/// - **QSV**: `preset=veryfast`, `async_depth=1`, `low_delay_brc`, `forced_idr` y, en H.264 y
///   HEVC, `scenario=remotegaming` e *intra-refresh* vertical (`int_ref_type`,
///   `int_ref_cycle_size` de un segundo).
/// - **VA-API**: `rc_mode=CBR`, `async_depth=1`. FFmpeg 8.1 no expone *intra-refresh* en VA-API.
/// - **Vulkan Video**: `rc_mode=cbr`, `tune=ull`, `usage=stream`, `content=rendered`,
///   `async_depth=1`. FFmpeg 8.1 no expone *intra-refresh* en Vulkan.
pub fn low_latency_options(
    codec: Codec,
    backend: Backend,
    config: &EncoderConfig,
) -> Vec<(&'static str, String)> {
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
                "pred-struct=1:rc=2:rtc=1:lookahead=0:scd=0".into(),
            );
        }
        Backend::Nvenc => {
            set("preset", "p1".into());
            set("tune", "ull".into());
            set("rc", "cbr".into());
            set("zerolatency", "1".into());
            set("delay", "0".into());
            set("rc-lookahead", "0".into());
            set("forced-idr", "1".into());
            set("intra-refresh", "1".into());
            set("no-scenecut", "1".into());
            set("g", fps.to_string());
        }
        Backend::Amf => {
            set("usage", "ultralowlatency".into());
            set("rc", "cbr".into());
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
                let mbs = config.width.div_ceil(16) * config.height.div_ceil(16);
                set("intra_refresh_mb", mbs.div_ceil(fps).max(1).to_string());
            }
        }
        Backend::Qsv => {
            set("preset", "veryfast".into());
            set("async_depth", "1".into());
            set("low_delay_brc", "1".into());
            set("forced_idr", "1".into());
            if codec != Codec::Av1 {
                set("scenario", "remotegaming".into());
                set("int_ref_type", "vertical".into());
                set("int_ref_cycle_size", fps.to_string());
            }
        }
        Backend::Vaapi => {
            set("rc_mode", "CBR".into());
            set("async_depth", "1".into());
        }
        Backend::VulkanVideo => {
            set("rc_mode", "cbr".into());
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
pub fn supports_live_bitrate(backend: Backend) -> bool {
    matches!(backend, Backend::Nvenc | Backend::Qsv)
}

/// Cómo se evita el IDR grande en estado estable con cada backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntraRefresh {
    /// *Intra-refresh* del codificador con esta opción de FFmpeg 8.1 (`intra-refresh` en NVENC,
    /// `intra_refresh_mb` en AMF H.264, `int_ref_type` en QSV H.264/HEVC).
    Native(&'static str),
    /// Sin *intra-refresh* en FFmpeg 8.1 (VA-API, Vulkan Video, SVT-AV1, AMF HEVC/AV1, QSV AV1):
    /// GOP infinito e IDR solo cuando el invitado lo pide (`KeyframeRequest`) o al empezar.
    ForcedIdr,
}

/// *Intra-refresh* de `codec` con `backend` (lo mismo que pone [`low_latency_options`]).
pub fn intra_refresh(codec: Codec, backend: Backend) -> IntraRefresh {
    match (backend, codec) {
        (Backend::Nvenc, _) => IntraRefresh::Native("intra-refresh"),
        (Backend::Amf, Codec::H264) => IntraRefresh::Native("intra_refresh_mb"),
        (Backend::Qsv, Codec::H264 | Codec::Hevc) => IntraRefresh::Native("int_ref_type"),
        _ => IntraRefresh::ForcedIdr,
    }
}

/// Codificador abierto con un tamaño concreto.
struct Session {
    ctx: AVCodecContext,
    frames: Option<AVHWFramesContext>,
    sw: AVFrame,
    sw_format: ffi::AVPixelFormat,
    width: u32,
    height: u32,
    /// Con *hw frames* de la importación: identidad de su contexto.
    imported: Option<usize>,
    /// Control de tasa leído del contexto ya abierto ([`RateControl`]).
    rate_control: RateControl,
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

    /// Abre el codificador; con `imported`, sobre los *hw frames* de la importación.
    fn open_with(
        &mut self,
        config: &EncoderConfig,
        imported: Option<AVHWFramesContext>,
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

        let options = low_latency_options(self.codec, self.backend, config);
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
        if rate_control.bit_rate <= 0 {
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
        self.in_flight.clear();
        self.next_pts = 0;
        Ok(Session {
            ctx,
            frames,
            sw,
            sw_format,
            width,
            height,
            imported: imported_key,
            rate_control,
        })
    }

    /// Control de tasa con el que quedó abierto el codificador ([`RateControl`]).
    pub fn rate_control_info(&self) -> Option<&RateControl> {
        self.session.as_ref().map(|session| &session.rate_control)
    }

    /// Reabre con `config` conservando el modo de entrada (CPU o *hw frames* importados).
    fn reopen(&mut self, config: EncoderConfig) -> Result<(), CodecError> {
        let imported = match (&self.session, &self.importer) {
            (Some(session), Some(importer)) if session.imported.is_some() => {
                importer.encoder_frames()
            }
            _ => None,
        };
        self.reopen_as(config, imported)
    }

    /// Reabre con `config`, sobre los *hw frames* `imported` o con entrada en CPU.
    fn reopen_as(
        &mut self,
        config: EncoderConfig,
        imported: Option<AVHWFramesContext>,
    ) -> Result<(), CodecError> {
        self.session = None;
        self.last_hw = None;
        let session = self.open_with(&config, imported)?;
        self.session = Some(session);
        self.config = Some(config);
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

/// Copia un fotograma (BGRA, RGBA de coma flotante, I420, NV12, I444, NV24 o P010) en el
/// `AVFrame` de CPU del codificador, convirtiendo al formato de la sesión.
fn fill(session: &mut Session, frame: &RawFrame) -> Result<(), CodecError> {
    session
        .sw
        .make_writable()
        .map_err(|err| backend_error("fotograma escribible", &err))?;
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (cw, ch) = RawFrame::chroma_size(frame.width, frame.height);
    let (cw, ch) = (cw as usize, ch as usize);
    let short = || CodecError::Unsupported("búfer del fotograma más corto que su tamaño".into());
    let mismatch = || {
        CodecError::Unsupported(format!(
            "el codificador no convierte {:?} a {}",
            frame.format,
            rsmpeg::avutil::get_pix_fmt_name(session.sw_format).map_or_else(
                || "?".to_owned(),
                |name| name.to_string_lossy().into_owned()
            )
        ))
    };
    let sw_format = session.sw_format;
    if frame.format == PixelFormat::Gpu {
        return Err(gpu_in_cpu_path());
    }
    if sw_format == ffi::AV_PIX_FMT_YUV420P {
        let mut planes = frame_planes(&mut session.sw, [h, ch, ch]);
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
        let mut planes = frame_planes(&mut session.sw, [h, ch, 0]);
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
        let mut planes = frame_planes(&mut session.sw, [h, h, h]);
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
        let mut planes = frame_planes(&mut session.sw, [h, h, 0]);
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
        let mut planes = frame_planes(&mut session.sw, [h, ch, 0]);
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
        let mut planes = frame_planes(&mut session.sw, [h, ch, ch]);
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
        Ok(())
    }

    /// Cambia el bitrate en caliente donde el backend lo admite ([`supports_live_bitrate`]);
    /// donde no, reabre el codificador y el siguiente fotograma sale intra.
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

    fn live_bitrate(&self) -> bool {
        supports_live_bitrate(self.backend)
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
        match self.submit(frame)? {
            Some(submitted) => self.finish_frame(submitted),
            None => Ok(Vec::new()),
        }
    }

    fn encode_into(
        &mut self,
        frame: &RawFrame,
        out: &mut Vec<u8>,
    ) -> Result<Option<EncodedFrame>, CodecError> {
        match self.submit(frame)? {
            Some(submitted) => self.finish_frame_into(submitted, out),
            None => {
                out.clear();
                Ok(None)
            }
        }
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
        // La captura de pantalla puede traer otro tamaño que el configurado: se reabre a ese.
        let config = EncoderConfig {
            width: frame.width,
            height: frame.height,
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

    /// Copia `frame` en el `AVFrame` de CPU de la sesión, lo sube a la GPU si hace falta y lo
    /// entrega. Devuelve las copias a CPU que hizo.
    fn send_cpu(&mut self, frame: &RawFrame, pts: i64, keyframe: bool) -> Result<u32, CodecError> {
        let Some(session) = self.session.as_mut() else {
            return Err(CodecError::Backend("sin sesión del codificador".into()));
        };
        fill(session, frame)?;
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
                (session.ctx.send_frame(Some(&hw)), 2)
            }
            None => (session.ctx.send_frame(Some(&session.sw)), 1),
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
        if self
            .importer
            .as_ref()
            .is_none_or(|importer| !importer.matches(image, size))
        {
            self.importer = None;
            let importer = self.build_importer(image, size)?;
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
                    let importer = self.build_importer(image, size)?;
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
                CodecError::Unsupported(format!(
                    "no se pudo importar la imagen {} en {}: {err} (COROOM_ZERO_COPY=0 vuelve a \
                     la captura en memoria de CPU)",
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

    /// Importador para `image`: sin copias si no falló antes; si no se puede, el de respaldo.
    fn build_importer(
        &mut self,
        image: &GpuImage,
        size: (u32, u32),
    ) -> Result<GpuImporter, CodecError> {
        if !self.zero_copy_failed {
            match GpuImporter::new(
                self.backend,
                self.device.as_ref(),
                image,
                size,
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
            ImportMode::Download,
        )
        .map_err(|err| {
            CodecError::Unsupported(format!(
                "{} no puede leer imágenes {} de la captura: {err} (COROOM_ZERO_COPY=0 vuelve a \
                 la captura en memoria de CPU)",
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
                for (key, _) in low_latency_options(codec, backend, &config(codec)) {
                    assert!(
                        option_exists(&av_codec, key),
                        "{} no tiene la opción `{key}`",
                        av_codec.name().to_string_lossy()
                    );
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
        assert_eq!(get(&nvenc, "intra-refresh").as_deref(), Some("1"));
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

    /// *Intra-refresh* verificado por opción en cada codificador compilado: donde
    /// [`intra_refresh`] dice que existe, la opción está en el codificador y en
    /// [`low_latency_options`]; donde no, el codificador no tiene ninguna opción de *intra-refresh*
    /// (FFmpeg 8.1: VA-API y Vulkan) y se fuerza un IDR al pedirlo.
    #[test]
    fn intra_refresh_is_verified_per_backend() {
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
                match intra_refresh(codec, backend) {
                    IntraRefresh::Native(option) => {
                        assert!(option_exists(&av_codec, option), "{name}: falta `{option}`");
                        assert!(
                            options.iter().any(|(key, _)| *key == option),
                            "{name}: `{option}` no se pide"
                        );
                    }
                    IntraRefresh::ForcedIdr => {
                        if matches!(backend, Backend::Vaapi | Backend::VulkanVideo) {
                            for option in NAMES {
                                assert!(
                                    !option_exists(&av_codec, option),
                                    "{name} tiene `{option}`: actualizar `intra_refresh`"
                                );
                            }
                        }
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
                    }
                }
                checked.push(name);
            }
        }
        eprintln!("intra-refresh comprobado en: {checked:?}");
        assert_eq!(
            intra_refresh(Codec::H264, Backend::Vaapi),
            IntraRefresh::ForcedIdr
        );
        assert_eq!(
            intra_refresh(Codec::Av1, Backend::Nvenc),
            IntraRefresh::Native("intra-refresh")
        );
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
        let text = err.to_string();
        assert!(text.contains("COROOM_ZERO_COPY=0"), "{text}");
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
