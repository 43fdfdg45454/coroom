//! Capa fina sobre MediaCodec del NDK (crate `ndk` 0.9, nivel de API 28): implementa
//! [`CodecOps`] y nada más; la lógica está en `super::decoder`, probada fuera de Android.
//!
//! El códec funciona en **modo asíncrono** (`AMediaCodec_setAsyncNotifyCallback`): la API segura
//! de `ndk` 0.9 no da el índice de los búferes que devuelven `dequeueInputBuffer` y
//! `dequeueOutputBuffer` (sin él no se puede soltar un búfer de salida desde otro hilo), y las
//! retrollamadas sí. Las retrollamadas llegan en un hilo interno del NDK y solo apuntan el suceso
//! en una cola con su `Condvar`: `dequeue_*` espera en ella y vuelve en cuanto llega algo (sin
//! sondear).
//!
//! Sin superficie de la vista, el códec dibuja en una de reserva: un `AImageReader` fuera de
//! pantalla (`AImageReader_newWithUsage`, nivel 26) que suelta en el acto lo que reciba. Hace
//! falta porque MediaCodec no admite quedarse sin superficie si se configuró con una, y seguir
//! con la de una `SurfaceView` destruida lo deja en error al pedirle búferes.

// Backend nativo de MediaCodec (ver `CLAUDE.md`: `unsafe` solo en backends nativos). El único
// `unsafe` es el `Send`/`Sync` de `NdkCodec`, con su `// SAFETY:`.
#![allow(unsafe_code)]

use std::collections::VecDeque;
use std::mem::MaybeUninit;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ndk::hardware_buffer::HardwareBufferUsage;
use ndk::media::image_reader::{AcquireResult, ImageFormat, ImageReader};
use ndk::media::media_codec::{AsyncNotifyCallback, MediaCodec, MediaCodecDirection};
use ndk::media::media_format::MediaFormat;
use ndk::native_window::NativeWindow;

use super::decoder::{CodecOps, Factory, MediaCodecDecoder, OutputEvent, OutputFormat, QueueError};
use super::info::{
    AndroidDecoderInfo, FormatParams, KEY_CROP_BOTTOM, KEY_CROP_LEFT, KEY_CROP_RIGHT, KEY_CROP_TOP,
    KEY_HEIGHT, KEY_WIDTH, ParamValue,
};
use super::surface::{OutputSurface, Release, SurfaceSlot};
use crate::{Codec, CodecError, Decoder, StreamHint};

/// Imágenes del `AImageReader` de reserva: las suelta en cuanto llegan, así que con dos el
/// productor siempre tiene dónde escribir mientras se suelta la anterior.
const PLACEHOLDER_IMAGES: i32 = 2;

/// Abre el decodificador de `info` para `codec`.
pub(super) fn open(
    codec: Codec,
    info: AndroidDecoderInfo,
    hint: Option<StreamHint>,
    slot: &SurfaceSlot,
) -> Result<Box<dyn Decoder>, CodecError> {
    let name = info.name.clone();
    let factory: Factory<NdkCodec> =
        Box::new(move |params, surface| NdkCodec::create(&name, params, surface));
    Ok(Box::new(MediaCodecDecoder::open(
        codec, info, hint, slot, factory,
    )?))
}

/// Lo que apuntan las retrollamadas.
#[derive(Default)]
struct Events {
    inputs: VecDeque<usize>,
    outputs: VecDeque<OutputEvent>,
    format: OutputFormat,
    /// Error del códec (`onAsyncError`): lo devuelve el siguiente `dequeue_*`.
    error: Option<String>,
}

#[derive(Default)]
struct Shared {
    events: Mutex<Events>,
    ready: Condvar,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Events> {
        self.events.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Espera como mucho `timeout` a que `take` saque algo de los sucesos.
    fn wait_for<T>(
        &self,
        timeout: Duration,
        mut take: impl FnMut(&mut Events) -> Option<T>,
    ) -> Result<Option<T>, String> {
        let deadline = Instant::now() + timeout;
        let mut events = self.lock();
        loop {
            if let Some(err) = events.error.clone() {
                return Err(err);
            }
            if let Some(value) = take(&mut events) {
                return Ok(Some(value));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            events = self
                .ready
                .wait_timeout(events, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Olvida lo apuntado (tras vaciar o reconfigurar: los índices ya no valen).
    fn reset(&self) {
        let mut events = self.lock();
        events.inputs.clear();
        events.outputs.clear();
        events.error = None;
    }
}

/// Formato de salida de `format`.
fn output_format(format: &MediaFormat) -> OutputFormat {
    let crop = (|| {
        Some((
            format.i32(KEY_CROP_LEFT)?,
            format.i32(KEY_CROP_TOP)?,
            format.i32(KEY_CROP_RIGHT)?,
            format.i32(KEY_CROP_BOTTOM)?,
        ))
    })();
    OutputFormat {
        width: format.i32(KEY_WIDTH),
        height: format.i32(KEY_HEIGHT),
        crop,
    }
}

/// Las retrollamadas del modo asíncrono, que apuntan en `shared`.
fn callbacks(shared: &Arc<Shared>) -> AsyncNotifyCallback {
    let inputs = Arc::clone(shared);
    let outputs = Arc::clone(shared);
    let formats = Arc::clone(shared);
    let errors = Arc::clone(shared);
    AsyncNotifyCallback {
        on_input_available: Some(Box::new(move |index| {
            inputs.lock().inputs.push_back(index);
            inputs.ready.notify_all();
        })),
        on_output_available: Some(Box::new(move |index, info| {
            outputs.lock().outputs.push_back(OutputEvent::Frame {
                index,
                // Se encolan marcas de `next_pts`, nunca negativas.
                pts_us: u64::try_from(info.presentation_time_us()).unwrap_or(0),
                flags: info.flags(),
            });
            outputs.ready.notify_all();
        })),
        on_format_changed: Some(Box::new(move |format| {
            let mut events = formats.lock();
            events.format = output_format(format);
            events.outputs.push_back(OutputEvent::FormatChanged);
            drop(events);
            formats.ready.notify_all();
        })),
        on_error: Some(Box::new(move |error, action, detail| {
            let mut events = errors.lock();
            events.error = Some(format!(
                "MediaCodec: {error} ({}; recuperable: {}, transitorio: {})",
                detail.to_string_lossy(),
                action.is_recoverable(),
                action.is_transient()
            ));
            drop(events);
            errors.ready.notify_all();
        })),
    }
}

/// `MediaFormat` con `params`.
fn media_format(params: &FormatParams) -> MediaFormat {
    let mut format = MediaFormat::new();
    for (key, value) in params.iter() {
        match value {
            ParamValue::I32(value) => format.set_i32(key, *value),
            ParamValue::F32(value) => format.set_f32(key, *value),
            ParamValue::Str(value) => format.set_str(key, value),
        }
    }
    format
}

/// Un `AMediaCodec` en modo asíncrono y su superficie de reserva.
pub(super) struct NdkCodec {
    codec: MediaCodec,
    shared: Arc<Shared>,
    placeholder: Mutex<Option<ImageReader>>,
    /// Tamaño configurado (para la superficie de reserva).
    size: (i32, i32),
}

// SAFETY: `MediaCodec` y `ImageReader` de `ndk` no son `Send` ni `Sync` solo porque guardan
// punteros (`NonNull`). `AMediaCodec` envuelve el `MediaCodec` de libstagefright, cuyos métodos
// públicos mandan un mensaje a su propio `ALooper` y esperan la respuesta
// (`PostAndAwaitResponse`): las llamadas desde varios hilos quedan serializadas por el propio
// códec, y MediaCodec documenta el uso habitual de soltar búferes de salida desde un hilo distinto
// del que los recibe. Desde aquí, por `&self` solo se llaman métodos de `&self` de `ndk`
// (`queue_input_buffer_by_index`, `input_buffer`, `release_output_buffer*_by_index`,
// `set_output_surface`) desde los hilos de decodificación, presentación e interfaz; los de
// `&mut self` (`set_async_notify_callback`, en `configure`) solo con acceso exclusivo (el cerrojo
// de escritura de `CodecCell`, ningún otro hilo dentro). `input_buffer` devuelve `&mut` a la
// memoria del búfer de entrada `index`, que solo toca el hilo de decodificación entre recibir el
// índice y encolarlo. El `ImageReader` solo se toca con su `Mutex`; sus retrollamadas son `Send`
// y solo usan el puntero que les pasa el NDK. Las retrollamadas del códec solo tocan `Shared`
// (`Mutex` y `Condvar`).
unsafe impl Send for NdkCodec {}
// SAFETY: ver el `Send` de arriba.
unsafe impl Sync for NdkCodec {}

impl NdkCodec {
    /// Crea el componente `name`, lo configura con `params` en `surface` (o en la de reserva) y
    /// lo arranca.
    fn create(
        name: &str,
        params: &FormatParams,
        surface: Option<&OutputSurface>,
    ) -> Result<Self, String> {
        let codec = MediaCodec::from_codec_name(name)
            .ok_or_else(|| format!("MediaCodec no crea el componente {name}"))?;
        let mut this = Self {
            codec,
            shared: Arc::default(),
            placeholder: Mutex::new(None),
            size: (0, 0),
        };
        this.configure(params, surface)?;
        this.codec.start().map_err(|err| format!("start: {err}"))?;
        Ok(this)
    }

    /// Retrollamadas (antes de `configure`, como pide el modo asíncrono) y `AMediaCodec_configure`.
    fn configure(
        &mut self,
        params: &FormatParams,
        surface: Option<&OutputSurface>,
    ) -> Result<(), String> {
        self.shared.reset();
        self.codec
            .set_async_notify_callback(Some(callbacks(&self.shared)))
            .map_err(|err| format!("setAsyncNotifyCallback: {err}"))?;
        let (width, height) = params.size().unwrap_or((0, 0));
        self.size = (
            i32::try_from(width).unwrap_or(i32::MAX),
            i32::try_from(height).unwrap_or(i32::MAX),
        );
        let format = media_format(params);
        let window = match surface {
            Some(surface) => surface.window().clone(),
            None => self.placeholder_window()?,
        };
        self.codec
            .configure(&format, Some(&window), MediaCodecDirection::Decoder)
            .map_err(|err| format!("configure ({format}): {err}"))
    }

    /// La ventana de la superficie de reserva (la crea la primera vez).
    fn placeholder_window(&self) -> Result<NativeWindow, String> {
        let mut placeholder = self
            .placeholder
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if placeholder.is_none() {
            let (width, height) = (self.size.0.max(2), self.size.1.max(2));
            let mut reader = ImageReader::new_with_usage(
                width,
                height,
                ImageFormat::PRIVATE,
                HardwareBufferUsage::GPU_SAMPLED_IMAGE,
                PLACEHOLDER_IMAGES,
            )
            .map_err(|err| format!("AImageReader de reserva: {err}"))?;
            reader
                .set_image_listener(Box::new(|reader| {
                    // Se suelta todo lo que llegue: nunca se llena.
                    while let Ok(AcquireResult::Image(image)) = reader.acquire_next_image() {
                        drop(image);
                    }
                }))
                .map_err(|err| format!("AImageReader de reserva: {err}"))?;
            *placeholder = Some(reader);
        }
        placeholder
            .as_ref()
            .map(ImageReader::window)
            .transpose()
            .map_err(|err| format!("ventana de reserva: {err}"))?
            .ok_or_else(|| "sin ventana de reserva".to_owned())
    }
}

impl CodecOps for NdkCodec {
    fn dequeue_input(&self, timeout: Duration) -> Result<Option<usize>, String> {
        self.shared
            .wait_for(timeout, |events| events.inputs.pop_front())
    }

    fn queue_input(&self, index: usize, data: &[&[u8]], pts_us: u64) -> Result<(), QueueError> {
        let total: usize = data.iter().map(|part| part.len()).sum();
        let Some(buffer) = self.codec.input_buffer(index) else {
            return Err(QueueError::Failed(format!(
                "AMediaCodec_getInputBuffer({index}) sin búfer"
            )));
        };
        if total > buffer.len() {
            // El búfer sigue siendo nuestro: vuelve a la cola para la próxima entrada.
            let capacity = buffer.len();
            self.shared.lock().inputs.push_front(index);
            return Err(QueueError::TooLarge { capacity });
        }
        let mut written = 0;
        for part in data {
            for (slot, byte) in buffer[written..written + part.len()].iter_mut().zip(*part) {
                *slot = MaybeUninit::new(*byte);
            }
            written += part.len();
        }
        self.codec
            .queue_input_buffer_by_index(index, 0, total, pts_us, 0)
            .map_err(|err| QueueError::Failed(format!("queueInputBuffer: {err}")))
    }

    fn dequeue_output(&self, timeout: Duration) -> Result<OutputEvent, String> {
        Ok(self
            .shared
            .wait_for(timeout, |events| events.outputs.pop_front())?
            .unwrap_or(OutputEvent::TryAgain))
    }

    fn output_format(&self) -> OutputFormat {
        self.shared.lock().format
    }

    fn release_output(&self, index: usize, how: Release) -> Result<(), String> {
        match how {
            Release::Discard => self.codec.release_output_buffer_by_index(index, false),
            Release::Render => self.codec.release_output_buffer_by_index(index, true),
            Release::RenderAt(ns) => self.codec.release_output_buffer_at_time_by_index(index, ns),
        }
        .map_err(|err| format!("releaseOutputBuffer: {err}"))
    }

    fn set_output(&self, surface: Option<&OutputSurface>) -> Result<(), String> {
        let window = match surface {
            Some(surface) => surface.window().clone(),
            None => self.placeholder_window()?,
        };
        self.codec
            .set_output_surface(&window)
            .map_err(|err| format!("setOutputSurface: {err}"))
    }

    fn flush(&mut self) -> Result<(), String> {
        self.codec.flush().map_err(|err| format!("flush: {err}"))?;
        // Tras vaciar, ningún índice anterior vale; en modo asíncrono hay que volver a arrancar
        // para que lleguen búferes de entrada.
        self.shared.reset();
        self.codec.start().map_err(|err| format!("start: {err}"))
    }

    fn reconfigure(
        &mut self,
        params: &FormatParams,
        surface: Option<&OutputSurface>,
    ) -> Result<(), String> {
        // `stop` puede fallar en un códec en error; `configure` dirá si sirve.
        let _ = self.codec.stop();
        self.configure(params, surface)?;
        self.codec.start().map_err(|err| format!("start: {err}"))
    }
}

impl Drop for NdkCodec {
    fn drop(&mut self) {
        // Suelta la superficie antes de que otro códec quiera conectarse a ella;
        // `AMediaCodec_delete` (el `Drop` de `MediaCodec`) libera el resto.
        let _ = self.codec.stop();
    }
}
