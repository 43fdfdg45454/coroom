//! Codificadores y decodificadores de FFmpeg 8 con `rsmpeg`.
//!
//! - [`FfmpegEncoder`]: SVT-AV1 por software y los codificadores por hardware de FFmpeg (Vulkan
//!   Video, NVENC, AMF, QSV, VA-API) con opciones de baja latencia por backend
//!   ([`encoder::low_latency_options`]). La imagen se convierte en CPU (BGRA → I420 o NV12 con la
//!   crate `yuv`; si la captura es más grande que el tope de resolución, libswscale la reduce y
//!   convierte en un solo paso, [`scale`]) y, en los backends con *hw frames*, se sube a la GPU con
//!   `av_hwframe_transfer_data`; la importación sin copias está en [`import`], donde la reducción
//!   va en el mismo filtro de la GPU que la conversión de formato.
//! - [`FfmpegDecoder`]: dav1d, `h264` y `hevc` de FFmpeg por software, y los mismos `h264`, `hevc`
//!   y `av1` con `hwaccel` (Vulkan, NVDEC, D3D12VA, D3D11VA, VA-API). La salida de la GPU se baja a
//!   CPU (normalmente NV12, que `coroom-render` dibuja sin convertir).
//! - El sondeo de capacidades vive en [`probe`] y lo expone [`crate::Capabilities::probe`].
//!
//! Cero copias: [`import`] lleva los DMA-BUF y las texturas D3D11 de la captura a los
//! *hw frames* del codificador; [`vulkan`] comparte el `VkDevice` del presentador con el
//! decodificador y sincroniza sus imágenes; [`filter`] monta los grafos de conversión en la GPU;
//! [`hw`] declara las estructuras de `hwcontext_*.h` que no genera `rusty_ffmpeg`.
//!
//! `rsmpeg` cubre casi todo con envoltorios seguros; el `unsafe` de este módulo se limita a leer y
//! escribir los planos de un `AVFrame`, los datos de un `AVPacket`, los contextos de dispositivo y
//! de fotogramas de GPU (`hwctx`), los grafos de filtros y las llamadas a Vulkan y D3D11 de la ruta
//! sin copias, siempre con su `// SAFETY:`.

// Este módulo es el backend nativo de FFmpeg (ver `CLAUDE.md`: `unsafe` solo en backends nativos).
#![allow(unsafe_code)]

pub mod decoder;
pub mod encoder;
pub(crate) mod filter;
pub mod fingerprint;
pub(crate) mod hw;
pub(crate) mod import;
pub(crate) mod probe;
pub mod probe_child;
pub(crate) mod scale;
pub(crate) mod vulkan;

use std::ffi::{CStr, CString};
use std::ptr::NonNull;
use std::sync::Once;

use rsmpeg::avcodec::AVCodecContext;
use rsmpeg::avutil::{AVDictionary, AVHWDeviceContext};
use rsmpeg::error::RsmpegError;
use rsmpeg::ffi;

pub use decoder::FfmpegDecoder;
pub use encoder::FfmpegEncoder;

use crate::{Backend, Codec, CodecError, DecoderBackend};

/// Versión de FFmpeg con la que se ejecuta (la de `libavutil` cargada, por ejemplo `8.1.2`).
pub fn version() -> String {
    rsmpeg::avutil::version_info()
        .to_string_lossy()
        .into_owned()
}

/// Directorio de las bibliotecas de FFmpeg: en Linux, el de la `libavcodec` cargada de verdad (la
/// que aparece en `/proc/self/maps`: en el Flatpak, la de la extensión `codecs-extra` si está); si
/// no se puede saber, el de compilación (`FFMPEG_LIBS_DIR`).
pub fn library_dir() -> String {
    loaded_library_dir().unwrap_or_else(|| env!("COROOM_FFMPEG_LIB_DIR").to_owned())
}

/// Directorio de la `libavcodec` mapeada en este proceso.
fn loaded_library_dir() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    maps.lines()
        .filter_map(|line| line.split_whitespace().nth(5))
        .find(|path| path.contains("/libavcodec.so"))
        .and_then(|path| std::path::Path::new(path).parent())
        .map(|dir| dir.display().to_string())
}

// Constantes de enumeraciones de C como `i32`: `bindgen` las da como `u32` en Linux y como `i32` en
// Windows (allí el `as` sobra y `clippy` lo señala).
/// `AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX`.
#[allow(clippy::unnecessary_cast)]
pub(crate) const HW_CONFIG_DEVICE_CTX: i32 = ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32;
/// `AV_HWFRAME_MAP_READ` (solo lo usa la importación de DMA-BUF).
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)]
pub(crate) const MAP_READ: i32 = ffi::AV_HWFRAME_MAP_READ as i32;
/// `AV_HWFRAME_MAP_READ | AV_HWFRAME_MAP_DIRECT`.
#[allow(clippy::unnecessary_cast)]
pub(crate) const MAP_READ_DIRECT: i32 =
    (ffi::AV_HWFRAME_MAP_READ | ffi::AV_HWFRAME_MAP_DIRECT) as i32;
/// `AV_BUFFERSRC_FLAG_KEEP_REF | AV_BUFFERSRC_FLAG_PUSH`.
#[allow(clippy::unnecessary_cast)]
pub(crate) const BUFFERSRC_KEEP_PUSH: i32 =
    (ffi::AV_BUFFERSRC_FLAG_KEEP_REF | ffi::AV_BUFFERSRC_FLAG_PUSH) as i32;

static LOG_LEVEL: Once = Once::new();

/// Nivel de registro de FFmpeg de este proceso: `COROOM_FFMPEG_LOG` (`quiet`, `fatal`, `error`,
/// `warning`, `info`, `verbose`, `debug`) o, por defecto, `fatal`. FFmpeg escribe en la salida de
/// error; los errores que importan ya vuelven como [`CodecError`] con el texto de `av_strerror`, y
/// los esperados (probar CUDA sin NVIDIA, VA-API sin `/dev/dri`…) no deben ensuciar la consola.
pub(crate) fn default_log_level() -> i32 {
    match std::env::var("COROOM_FFMPEG_LOG").as_deref() {
        Ok("quiet") => ffi::AV_LOG_QUIET,
        Ok("error") => ffi::AV_LOG_ERROR as i32,
        Ok("warning") => ffi::AV_LOG_WARNING as i32,
        Ok("info") => ffi::AV_LOG_INFO as i32,
        Ok("verbose") => ffi::AV_LOG_VERBOSE as i32,
        Ok("debug") => ffi::AV_LOG_DEBUG as i32,
        _ => ffi::AV_LOG_FATAL as i32,
    }
}

/// Aviso por el registro de FFmpeg (se ve con `COROOM_FFMPEG_LOG=warning` o más): por qué no se
/// usa la ruta sin copias.
pub(crate) fn log_warning(message: &str) {
    let Ok(text) = CString::new(message.replace('\0', " ")) else {
        return;
    };
    // SAFETY: el formato es una cadena C estática con un solo `%s`, que recibe `text`, una cadena C
    // válida durante toda la llamada; un contexto nulo es válido para `av_log`.
    unsafe {
        ffi::av_log(
            std::ptr::null_mut(),
            ffi::AV_LOG_WARNING as i32,
            c"coroom: %s\n".as_ptr(),
            text.as_ptr(),
        );
    }
}

/// Aplica [`default_log_level`] la primera vez.
pub(crate) fn init_logging() {
    LOG_LEVEL.call_once(|| set_log_level(default_log_level()));
}

pub(crate) fn set_log_level(level: i32) {
    // SAFETY: `av_log_set_level` solo escribe un entero global de libavutil; no hay invariantes.
    unsafe { ffi::av_log_set_level(level) }
}

/// Nombre del codificador de FFmpeg de cada combinación (`None`: no existe en FFmpeg).
pub(crate) fn encoder_name(codec: Codec, backend: Backend) -> Option<&'static CStr> {
    Some(match (codec, backend) {
        (Codec::Av1, Backend::VulkanVideo) => c"av1_vulkan",
        (Codec::Av1, Backend::Nvenc) => c"av1_nvenc",
        (Codec::Av1, Backend::Amf) => c"av1_amf",
        (Codec::Av1, Backend::Qsv) => c"av1_qsv",
        (Codec::Av1, Backend::Vaapi) => c"av1_vaapi",
        (Codec::Av1, Backend::Software) => c"libsvtav1",
        (Codec::Hevc, Backend::VulkanVideo) => c"hevc_vulkan",
        (Codec::Hevc, Backend::Nvenc) => c"hevc_nvenc",
        (Codec::Hevc, Backend::Amf) => c"hevc_amf",
        (Codec::Hevc, Backend::Qsv) => c"hevc_qsv",
        (Codec::Hevc, Backend::Vaapi) => c"hevc_vaapi",
        (Codec::H264, Backend::VulkanVideo) => c"h264_vulkan",
        (Codec::H264, Backend::Nvenc) => c"h264_nvenc",
        (Codec::H264, Backend::Amf) => c"h264_amf",
        (Codec::H264, Backend::Qsv) => c"h264_qsv",
        (Codec::H264, Backend::Vaapi) => c"h264_vaapi",
        // HEVC no tiene codificador software LGPL; H.264 por software es OpenH264 (fuera de FFmpeg).
        (Codec::Hevc | Codec::H264, Backend::Software) => return None,
    })
}

/// Nombre del decodificador de FFmpeg de cada combinación: dav1d para AV1 por software y los
/// nativos de FFmpeg (con `hwaccel` si el backend es de hardware) para lo demás.
pub(crate) fn decoder_name(codec: Codec, backend: DecoderBackend) -> &'static CStr {
    match (codec, backend) {
        (Codec::Av1, DecoderBackend::Software) => c"libdav1d",
        (Codec::Av1, _) => c"av1",
        (Codec::Hevc, _) => c"hevc",
        (Codec::H264, _) => c"h264",
    }
}

/// Tipo de dispositivo de FFmpeg que necesita cada codificador (`None`: sin dispositivo).
pub(crate) fn encoder_device(backend: Backend) -> Option<ffi::AVHWDeviceType> {
    match backend {
        Backend::VulkanVideo => Some(ffi::AV_HWDEVICE_TYPE_VULKAN),
        Backend::Nvenc => Some(ffi::AV_HWDEVICE_TYPE_CUDA),
        Backend::Qsv => Some(ffi::AV_HWDEVICE_TYPE_QSV),
        Backend::Vaapi => Some(ffi::AV_HWDEVICE_TYPE_VAAPI),
        // AMF abre su propio dispositivo (D3D11 en Windows, Vulkan en Linux) con entrada NV12.
        Backend::Amf | Backend::Software => None,
    }
}

/// Tipo de dispositivo de FFmpeg de cada decodificador (`None`: por software).
pub(crate) fn decoder_device(backend: DecoderBackend) -> Option<ffi::AVHWDeviceType> {
    match backend {
        DecoderBackend::Vulkan => Some(ffi::AV_HWDEVICE_TYPE_VULKAN),
        DecoderBackend::Nvdec => Some(ffi::AV_HWDEVICE_TYPE_CUDA),
        DecoderBackend::D3D12va => Some(ffi::AV_HWDEVICE_TYPE_D3D12VA),
        DecoderBackend::D3D11va => Some(ffi::AV_HWDEVICE_TYPE_D3D11VA),
        DecoderBackend::Vaapi => Some(ffi::AV_HWDEVICE_TYPE_VAAPI),
        DecoderBackend::Software => None,
        // No es de FFmpeg: `FfmpegDecoder` lo rechaza antes de llegar aquí.
        DecoderBackend::MediaCodec => None,
    }
}

/// Formato de píxel de los fotogramas en la GPU de cada tipo de dispositivo.
pub(crate) fn hw_pixel_format(device: ffi::AVHWDeviceType) -> ffi::AVPixelFormat {
    match device {
        ffi::AV_HWDEVICE_TYPE_VULKAN => ffi::AV_PIX_FMT_VULKAN,
        ffi::AV_HWDEVICE_TYPE_CUDA => ffi::AV_PIX_FMT_CUDA,
        ffi::AV_HWDEVICE_TYPE_QSV => ffi::AV_PIX_FMT_QSV,
        ffi::AV_HWDEVICE_TYPE_VAAPI => ffi::AV_PIX_FMT_VAAPI,
        ffi::AV_HWDEVICE_TYPE_D3D11VA => ffi::AV_PIX_FMT_D3D11,
        ffi::AV_HWDEVICE_TYPE_D3D12VA => ffi::AV_PIX_FMT_D3D12,
        _ => ffi::AV_PIX_FMT_NONE,
    }
}

fn device_label(device: ffi::AVHWDeviceType) -> String {
    rsmpeg::avutil::hwdevice_get_type_name(device).map_or_else(
        || format!("{device}"),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Crea el dispositivo de FFmpeg (`AVHWDeviceContext`) del tipo pedido en el adaptador por defecto.
pub(crate) fn create_device(device: ffi::AVHWDeviceType) -> Result<AVHWDeviceContext, String> {
    init_logging();
    if device == ffi::AV_HWDEVICE_TYPE_VAAPI {
        libva_check()?;
    }
    AVHWDeviceContext::create(device, None, None, 0).map_err(|err| {
        format!(
            "no se pudo crear el dispositivo {}: {}",
            device_label(device),
            describe(&err)
        )
    })
}

/// Versión mínima de libva que enlaza esta FFmpeg: 2.21 trae `vaMapBuffer2`, que FFmpeg 8.1
/// llama por su *implib* (en una libva más vieja la llamada no resuelve y el proceso **aborta**,
/// no falla). El síntoma: SIGABRT al abrir cualquier codificador o decodificador VA-API.
pub const LIBVA_MIN_VERSION: &str = "2.21";

/// Comprueba, sin abrir ningún dispositivo, que la libva del sistema sirve para FFmpeg 8.1:
/// carga `libva.so.2` y busca `vaMapBuffer2` (libva ≥ 2.21). `Err` con el motivo si falta la
/// biblioteca o el símbolo; se llama antes de crear el dispositivo VA-API (sondeo y sesión) y lo
/// enseña `coroom doctor`.
#[cfg(target_os = "linux")]
pub fn libva_check() -> Result<(), String> {
    use std::sync::OnceLock;
    static RESULT: OnceLock<Result<(), String>> = OnceLock::new();
    RESULT
        .get_or_init(|| {
            // SAFETY: `dlopen` con una ruta C estática solo carga (o cuenta otra referencia de)
            // libva; `dlsym` busca un símbolo sin llamarlo; `dlclose` suelta la referencia. No se
            // ejecuta nada de la biblioteca.
            unsafe {
                let handle =
                    libc::dlopen(c"libva.so.2".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
                if handle.is_null() {
                    return Err(format!(
                        "no se encuentra libva.so.2 (hace falta libva ≥ {LIBVA_MIN_VERSION})"
                    ));
                }
                let symbol = libc::dlsym(handle, c"vaMapBuffer2".as_ptr());
                let found = !symbol.is_null();
                libc::dlclose(handle);
                if found {
                    Ok(())
                } else {
                    Err(format!(
                        "la libva del sistema es anterior a {LIBVA_MIN_VERSION} (sin \
                         `vaMapBuffer2`): FFmpeg 8.1 abortaría al abrir VA-API; actualiza libva o \
                         usa Vulkan Video"
                    ))
                }
            }
        })
        .clone()
}

/// En otros sistemas no hay VA-API: siempre bien.
#[cfg(not(target_os = "linux"))]
pub fn libva_check() -> Result<(), String> {
    Ok(())
}

/// Texto de un error de `rsmpeg` con el mensaje de FFmpeg (`av_strerror`).
pub(crate) fn describe(err: &RsmpegError) -> String {
    let code = match err {
        RsmpegError::AVError(code)
        | RsmpegError::SendPacketError(code)
        | RsmpegError::ReceiveFrameError(code)
        | RsmpegError::SendFrameError(code)
        | RsmpegError::ReceivePacketError(code) => Some(*code),
        _ => None,
    };
    match code.and_then(rsmpeg::avutil::err2str) {
        Some(text) => format!("{err} ({text})"),
        None => err.to_string(),
    }
}

/// Abre `ctx` con `options` y devuelve las opciones que el códec no reconoció.
///
/// No usa `AVCodecContext::open` de `rsmpeg` 0.18: si `avcodec_open2` falla después de sustituir
/// el diccionario, `rsmpeg` libera el antiguo otra vez (doble liberación, visto al sondear
/// `av1_vulkan` con lavapipe). Aquí el diccionario es siempre el que deja `avcodec_open2`.
pub(crate) fn open_codec(
    ctx: &mut AVCodecContext,
    options: &[(&str, String)],
) -> Result<Vec<String>, RsmpegError> {
    let mut dict: Option<AVDictionary> = None;
    for (key, value) in options {
        let (Ok(key), Ok(value)) = (CString::new(*key), CString::new(value.as_str())) else {
            continue;
        };
        dict = Some(match dict {
            Some(dict) => dict.set(&key, &value, 0),
            None => AVDictionary::new(&key, &value, 0),
        });
    }
    let mut raw: *mut ffi::AVDictionary =
        dict.map_or(std::ptr::null_mut(), |dict| dict.into_raw().as_ptr());
    // SAFETY: `ctx` es un contexto sin abrir que poseemos; `raw` es nulo o un diccionario de
    // `av_dict_set` cuya propiedad hemos tomado con `into_raw`. `avcodec_open2` lo consume y deja
    // en `raw` uno nuevo (o nulo) con lo que no usó, tanto si acierta como si falla.
    let ret = unsafe { ffi::avcodec_open2(ctx.as_mut_ptr(), std::ptr::null(), &mut raw) };
    let rest = NonNull::new(raw).map(|raw| {
        // SAFETY: `raw` es el diccionario que devolvió `avcodec_open2`, ahora nuestro; el
        // `AVDictionary` lo libera al soltarse.
        unsafe { AVDictionary::from_raw(raw) }
    });
    let unused = rest
        .map(|rest| {
            rest.iter()
                .map(|entry| entry.key().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    if ret < 0 {
        Err(RsmpegError::AVError(ret))
    } else {
        Ok(unused)
    }
}

/// Texto de un código de error de FFmpeg (`av_strerror`).
pub(crate) fn describe_code(code: i32) -> String {
    match rsmpeg::avutil::err2str(code) {
        Some(text) => format!("{text} ({code})"),
        None => format!("error {code}"),
    }
}

pub(crate) fn backend_error(what: &str, err: &RsmpegError) -> CodecError {
    CodecError::Backend(format!("FFmpeg, {what}: {}", describe(err)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_ffmpeg_8_or_newer() {
        let version = version();
        let major: u32 = version
            .trim_start_matches('n')
            .split('.')
            .next()
            .and_then(|major| major.parse().ok())
            .unwrap_or(0);
        assert!(major >= 8, "FFmpeg {version}");
        assert!(!library_dir().is_empty());
    }

    #[test]
    fn every_combination_has_a_name() {
        for codec in Codec::PREFERENCE {
            for backend in Backend::ALL {
                let name = encoder_name(codec, backend);
                assert_eq!(
                    name.is_none(),
                    backend == Backend::Software && codec != Codec::Av1,
                    "{codec} {backend}"
                );
            }
            for backend in DecoderBackend::ALL {
                assert!(!decoder_name(codec, backend).is_empty());
            }
        }
    }

    #[test]
    fn missing_devices_fail_without_panicking() {
        // Sin GPU no hay CUDA: el error se explica en español.
        if let Err(err) = create_device(ffi::AV_HWDEVICE_TYPE_CUDA) {
            assert!(
                err.starts_with("no se pudo crear el dispositivo cuda"),
                "{err}"
            );
        }
    }
}
