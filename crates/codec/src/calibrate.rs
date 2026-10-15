//! Calibración **medida** de los codificadores para el modo automático.
//!
//! En vez de una preferencia fija por fabricante, con varios backends disponibles para un códec
//! se abre cada uno, se codifican unos fotogramas sintéticos y se mide por fotograma la latencia
//! de entrega a paquete ([`calibrate`]); gana el de menor p95 (a igual p95, menor p50:
//! [`pick`]). El resultado se guarda ([`CalibrationCache`], JSON) con la clave de la GPU y su
//! driver, el códec, la clase de resolución y el formato de píxel ([`CalibrationKey`]), y se
//! vuelve a medir solo si cambia la clave o el backend guardado ya no abre.
//!
//! Lo orquesta [`crate::EncoderSet::open_calibrated`]; los ajustes explícitos de backend
//! (`only_backend`) dejan un solo candidato y no calibran nada.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{Backend, Capabilities, Encoder, EncoderConfig, PixelFormat, RawFrame};

/// Fotogramas sintéticos que se codifican por candidato.
pub const FRAMES: u32 = 30;

/// Primeros fotogramas que no cuentan (arranque del codificador, intra, pools de la GPU).
pub const WARMUP: u32 = 5;

/// Tiempo máximo que puede tardar un candidato en abrir; más, y se descarta (la apertura va en un
/// hilo aparte y no se espera más que esto; el hilo termina por su cuenta y suelta el codificador).
pub const OPEN_LIMIT: Duration = Duration::from_secs(1);

/// Presupuesto de codificación por candidato: pasado, se deja de medir con lo que haya (al menos
/// [`MIN_SAMPLES`] muestras, si no, el candidato no cuenta).
pub const ENCODE_BUDGET: Duration = Duration::from_millis(400);

/// Muestras mínimas para que una medida valga.
pub const MIN_SAMPLES: usize = 8;

/// Lo que se midió de un candidato.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalibrationResult {
    /// Backend medido.
    pub backend: Backend,
    /// Si abrió (y a tiempo) y entregó al menos [`MIN_SAMPLES`] fotogramas.
    pub opened: bool,
    /// Mediana de la latencia entrega → paquete.
    pub p50: Duration,
    /// Percentil 95 de esa latencia.
    pub p95: Duration,
    /// Bitrate real de lo que salió, en kbit/s (con la cadencia de la configuración).
    pub kbit_s: u32,
    /// Fotogramas medidos.
    pub samples: u32,
    /// Por qué no cuenta, si no abrió o no entregó.
    pub note: Option<String>,
}

impl fmt::Display for CalibrationResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.opened {
            write!(
                f,
                "{}: p50 {:.1} ms, p95 {:.1} ms, {} kbit/s ({} fotogramas)",
                self.backend,
                self.p50.as_secs_f64() * 1000.0,
                self.p95.as_secs_f64() * 1000.0,
                self.kbit_s,
                self.samples
            )
        } else {
            write!(
                f,
                "{}: descartado ({})",
                self.backend,
                self.note.as_deref().unwrap_or("no abrió")
            )
        }
    }
}

/// Fotograma BGRA sintético (degradado con movimiento según `frame_id`), del tamaño pedido. BGRA
/// es lo que entrega la captura en CPU y lo que los codificadores de FFmpeg convierten a todos sus
/// formatos de entrada (I420/NV12, I444/NV24 para 4:4:4 y P010 para HDR10, ver `fill`), así que
/// sirve para medir cualquier `config.format`.
pub fn synthetic_frame(width: u32, height: u32, frame_id: u64) -> RawFrame {
    let (w, h) = (width as usize, height as usize);
    let stride = width * 4;
    let mut data = vec![0u8; w * h * 4];
    let shift = frame_id as usize * 3;
    for (y, row) in data.chunks_exact_mut(w * 4).enumerate() {
        for (x, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            *px = [
                ((x + shift) * 255 / w.max(1)) as u8,
                (y * 255 / h.max(1)) as u8,
                (((x + y) / 2 + shift) % 256) as u8,
                255,
            ];
        }
    }
    RawFrame::cpu(frame_id, (width, height), PixelFormat::Bgra, stride, data)
}

/// Mide `candidates` (backends de `config.codec`) con [`FRAMES`] fotogramas sintéticos de la
/// configuración dada. Abre cada uno con [`crate::select::create_encoder`] y lo descarta si tarda
/// más de [`OPEN_LIMIT`] en abrir o no entrega [`MIN_SAMPLES`] fotogramas en [`ENCODE_BUDGET`].
/// La latencia de cada fotograma va de la entrega al codificador hasta que sale su paquete (con un
/// codificador que retrasa un fotograma, desde la entrega del más antiguo pendiente).
pub fn calibrate(candidates: &[Backend], config: &EncoderConfig) -> Vec<CalibrationResult> {
    candidates
        .iter()
        .map(|&backend| {
            let owned = *config;
            let open = move || -> Result<Box<dyn Encoder>, String> {
                let mut encoder = crate::select::create_encoder(owned.codec, backend)
                    .map_err(|err| err.to_string())?;
                encoder.configure(&owned).map_err(|err| err.to_string())?;
                Ok(encoder)
            };
            measure(backend, config, open)
        })
        .collect()
}

/// Mide un codificador que abre `open` (separado de [`calibrate`] para probarlo con falsos).
pub fn measure(
    backend: Backend,
    config: &EncoderConfig,
    open: impl FnOnce() -> Result<Box<dyn Encoder>, String> + Send + 'static,
) -> CalibrationResult {
    let dropped = |note: String| CalibrationResult {
        backend,
        opened: false,
        p50: Duration::ZERO,
        p95: Duration::ZERO,
        kbit_s: 0,
        samples: 0,
        note: Some(note),
    };
    let mut encoder = match open_within(OPEN_LIMIT, open) {
        Ok(encoder) => encoder,
        Err(err) => return dropped(err),
    };
    let mut pending: std::collections::VecDeque<Instant> = std::collections::VecDeque::new();
    let mut latencies: Vec<Duration> = Vec::with_capacity(FRAMES as usize);
    let mut bytes = 0usize;
    let mut out = Vec::new();
    let budget_start = Instant::now();
    for frame_id in 0..u64::from(FRAMES) {
        if budget_start.elapsed() > ENCODE_BUDGET && latencies.len() >= MIN_SAMPLES {
            break;
        }
        let frame = synthetic_frame(config.width, config.height, frame_id);
        let submitted = Instant::now();
        pending.push_back(submitted);
        match encoder.encode_into(&frame, &mut out) {
            Ok(Some(_)) => {
                let done = Instant::now();
                let since = pending.pop_front().unwrap_or(submitted);
                if frame_id >= u64::from(WARMUP) {
                    latencies.push(done.saturating_duration_since(since));
                    bytes += out.len();
                }
            }
            Ok(None) => {}
            Err(err) => return dropped(format!("falló al codificar: {err}")),
        }
    }
    if latencies.len() < MIN_SAMPLES {
        return dropped(format!(
            "entregó {} fotogramas medibles (mínimo {MIN_SAMPLES})",
            latencies.len()
        ));
    }
    latencies.sort_unstable();
    let p50 = percentile(&latencies, 50);
    let p95 = percentile(&latencies, 95);
    let kbit_s = (bytes as u64 * 8 * u64::from(config.fps.max(1)) / latencies.len() as u64 / 1000)
        .min(u64::from(u32::MAX)) as u32;
    CalibrationResult {
        backend,
        opened: true,
        p50,
        p95,
        kbit_s,
        samples: latencies.len() as u32,
        note: None,
    }
}

/// Abre con `open` en un hilo aparte y espera como mucho `limit`: pasado, el candidato queda
/// descartado sin esperar a que el driver termine (el hilo acaba solo y suelta lo que abrió).
fn open_within(
    limit: Duration,
    open: impl FnOnce() -> Result<Box<dyn Encoder>, String> + Send + 'static,
) -> Result<Box<dyn Encoder>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("calibracion".into())
        .spawn(move || {
            let _ = tx.send(open());
        });
    if spawned.is_err() {
        // Sin hilo (raro), se abre aquí y se aplica el límite a lo que tardó.
        return Err("no se pudo abrir en un hilo aparte".to_owned());
    }
    match rx.recv_timeout(limit) {
        Ok(Ok(encoder)) => Ok(encoder),
        Ok(Err(err)) => Err(format!("no abre: {err}")),
        Err(_) => Err(format!(
            "tardó más de {:.0} s en abrir (se abandona)",
            limit.as_secs_f64()
        )),
    }
}

/// Percentil `pct` (0–100) de `sorted`, ya ordenado (el más cercano por arriba).
fn percentile(sorted: &[Duration], pct: usize) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let index = (sorted.len() * pct).div_ceil(100).clamp(1, sorted.len()) - 1;
    sorted[index]
}

/// El candidato que gana: el de menor p95 entre los que abrieron (a igual p95, menor p50).
pub fn pick(results: &[CalibrationResult]) -> Option<&CalibrationResult> {
    results
        .iter()
        .filter(|result| result.opened)
        .min_by_key(|result| (result.p95, result.p50))
}

/// Clase de resolución de la caché: lo que cambia el coste de codificar sin que cada tamaño de
/// ventana sea una clave distinta.
pub fn resolution_class(width: u32, height: u32) -> &'static str {
    match u64::from(width) * u64::from(height) {
        0..=921_600 => "720p",
        921_601..=2_073_600 => "1080p",
        2_073_601..=3_686_400 => "1440p",
        3_686_401..=8_294_400 => "2160p",
        _ => "mayor",
    }
}

/// Clave de una medida guardada.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibrationKey {
    /// GPU y driver ([`adapter_identity`]).
    pub adapter: String,
    /// [`crate::Codec::id`].
    pub codec: String,
    /// [`resolution_class`].
    pub resolution_class: String,
    /// [`crate::VideoFormat`] en texto.
    pub pixel_format: String,
}

impl CalibrationKey {
    /// La clave de `config` en esta máquina.
    pub fn of(config: &EncoderConfig) -> Self {
        Self::with_adapter(adapter_identity(), config)
    }

    /// La clave de `config` con una identidad de GPU dada.
    pub fn with_adapter(adapter: impl Into<String>, config: &EncoderConfig) -> Self {
        Self {
            adapter: adapter.into(),
            codec: config.codec.id().to_owned(),
            resolution_class: resolution_class(config.width, config.height).to_owned(),
            pixel_format: config.format.to_string(),
        }
    }
}

/// Lo que se guarda por clave: el backend elegido y sus números.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedChoice {
    /// Clave de la medida.
    pub key: CalibrationKey,
    /// [`Backend::id`] del elegido.
    pub backend: String,
    /// Mediana de la latencia, en µs.
    pub p50_us: u64,
    /// Percentil 95 de la latencia, en µs.
    pub p95_us: u64,
    /// Bitrate real medido en kbit/s.
    pub kbit_s: u32,
    /// Segundos desde la época Unix.
    pub timestamp: u64,
}

impl CachedChoice {
    /// El backend guardado, si sigue existiendo en esta versión.
    pub fn backend(&self) -> Option<Backend> {
        Backend::from_id(&self.backend)
    }

    /// Percentil 95 guardado.
    pub fn p95(&self) -> Duration {
        Duration::from_micros(self.p95_us)
    }

    /// Mediana guardada.
    pub fn p50(&self) -> Duration {
        Duration::from_micros(self.p50_us)
    }
}

/// Caché de calibraciones: en memoria y, con ruta, en un fichero JSON (una lista de
/// [`CachedChoice`]). Los errores de lectura o escritura no detienen nada: sin fichero legible se
/// calibra y se intenta guardar.
#[derive(Debug)]
pub struct CalibrationCache {
    path: Option<PathBuf>,
    entries: Vec<CachedChoice>,
}

impl CalibrationCache {
    /// Carga la caché de `path` (o una vacía en memoria sin ruta).
    pub fn load(path: Option<&Path>) -> Self {
        let entries = path
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<Vec<CachedChoice>>(&bytes).ok())
            .unwrap_or_default();
        Self {
            path: path.map(Path::to_path_buf),
            entries,
        }
    }

    /// Lo guardado para `key`.
    pub fn lookup(&self, key: &CalibrationKey) -> Option<&CachedChoice> {
        self.entries.iter().find(|entry| entry.key == *key)
    }

    /// Guarda (o sustituye) la elección de su clave y escribe el fichero si hay ruta.
    pub fn store(&mut self, choice: CachedChoice) -> std::io::Result<()> {
        self.entries.retain(|entry| entry.key != choice.key);
        self.entries.push(choice);
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(&self.entries)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        std::fs::write(path, json)
    }

    /// Quita lo guardado para `key` (el backend ya no abre).
    pub fn forget(&mut self, key: &CalibrationKey) {
        self.entries.retain(|entry| entry.key != *key);
    }

    /// Todo lo guardado.
    pub fn entries(&self) -> &[CachedChoice] {
        &self.entries
    }
}

/// [`CachedChoice`] a partir de una medida.
pub fn choice_from(key: CalibrationKey, result: &CalibrationResult) -> CachedChoice {
    CachedChoice {
        key,
        backend: result.backend.id().to_owned(),
        p50_us: u64::try_from(result.p50.as_micros()).unwrap_or(u64::MAX),
        p95_us: u64::try_from(result.p95.as_micros()).unwrap_or(u64::MAX),
        kbit_s: result.kbit_s,
        timestamp: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    }
}

static ADAPTER: OnceLock<String> = OnceLock::new();

/// Identidad de la GPU de esta máquina para la clave de la caché: nombre del dispositivo Vulkan y
/// versión de su driver (`NVIDIA GeForce RTX 5070 Ti / driver 580.76.5 / vulkan 1.4.312`). Solo
/// se crea un dispositivo Vulkan si el sondeo ([`Capabilities::probe`]) dio por bueno algún códec
/// Vulkan: si el sondeo lo descartó (el cargador viejo que mata el proceso al abrir), la identidad
/// es `sin-vulkan` y no se toca Vulkan en el proceso del anfitrión. Se consulta una vez por
/// proceso.
pub fn adapter_identity() -> String {
    ADAPTER
        .get_or_init(|| {
            let caps = Capabilities::probe();
            let vulkan_usable = caps
                .encoders
                .iter()
                .any(|(_, backend)| *backend == Backend::VulkanVideo)
                || caps
                    .decoders
                    .iter()
                    .any(|(_, backend)| *backend == crate::DecoderBackend::Vulkan);
            if !vulkan_usable {
                return "sin-vulkan".to_owned();
            }
            #[cfg(feature = "ffmpeg")]
            {
                crate::ffmpeg::vulkan::adapter_identity().unwrap_or_else(|| "sin-vulkan".to_owned())
            }
            #[cfg(not(feature = "ffmpeg"))]
            {
                "sin-vulkan".to_owned()
            }
        })
        .clone()
}

/// Caché en memoria del proceso (la de fichero se lee y escribe además, si hay ruta): una
/// renegociación con otro invitado no vuelve a medir.
pub(crate) fn process_cache() -> &'static Mutex<Vec<CachedChoice>> {
    static CACHE: OnceLock<Mutex<Vec<CachedChoice>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

pub(crate) fn remember(choice: &CachedChoice) {
    let mut cache = process_cache()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    cache.retain(|entry| entry.key != choice.key);
    cache.push(choice.clone());
}

pub(crate) fn remembered(key: &CalibrationKey) -> Option<CachedChoice> {
    process_cache()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|entry| entry.key == *key)
        .cloned()
}

/// Todas las calibraciones recordadas en este proceso.
pub(crate) fn remembered_all() -> Vec<CachedChoice> {
    process_cache()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

pub(crate) fn forget_remembered(key: &CalibrationKey) {
    process_cache()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|entry| entry.key != *key);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Codec, VideoFormat};

    fn result(backend: Backend, p50_ms: u64, p95_ms: u64) -> CalibrationResult {
        CalibrationResult {
            backend,
            opened: true,
            p50: Duration::from_millis(p50_ms),
            p95: Duration::from_millis(p95_ms),
            kbit_s: 9_000,
            samples: 25,
            note: None,
        }
    }

    fn config() -> EncoderConfig {
        EncoderConfig {
            codec: Codec::Hevc,
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_kbps: 10_000,
            slices: 1,
            format: VideoFormat::SDR_420,
        }
    }

    #[test]
    fn the_lowest_p95_wins_and_unopened_candidates_are_dropped() {
        let mut slow = result(Backend::VulkanVideo, 2, 10);
        let fast = result(Backend::Nvenc, 3, 4);
        let broken = CalibrationResult {
            opened: false,
            note: Some("no abre".into()),
            ..result(Backend::Amf, 0, 0)
        };
        assert_eq!(
            pick(&[slow.clone(), fast.clone(), broken.clone()]).map(|r| r.backend),
            Some(Backend::Nvenc)
        );
        // A igual p95, menor p50.
        slow.p95 = fast.p95;
        assert_eq!(
            pick(&[fast.clone(), slow.clone()]).map(|r| r.backend),
            Some(Backend::VulkanVideo)
        );
        assert_eq!(pick(&[broken]), None);
        assert!(fast.to_string().contains("p95 4.0 ms"));
    }

    #[test]
    fn percentiles_and_resolution_classes() {
        let ms: Vec<Duration> = (1..=20).map(Duration::from_millis).collect();
        assert_eq!(percentile(&ms, 50), Duration::from_millis(10));
        assert_eq!(percentile(&ms, 95), Duration::from_millis(19));
        assert_eq!(percentile(&ms[..1], 95), Duration::from_millis(1));
        assert_eq!(resolution_class(1280, 720), "720p");
        assert_eq!(resolution_class(1920, 1080), "1080p");
        assert_eq!(resolution_class(2560, 1440), "1440p");
        assert_eq!(resolution_class(3840, 2160), "2160p");
        assert_eq!(resolution_class(7680, 4320), "mayor");
    }

    #[test]
    fn the_cache_hits_on_the_same_key_and_misses_when_it_changes() {
        let dir = std::env::temp_dir().join(format!(
            "coroom-calibracion-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let path = dir.join("calibracion.json");
        let key = CalibrationKey::with_adapter("GPU de prueba / 1.0", &config());
        let mut cache = CalibrationCache::load(Some(&path));
        assert!(cache.lookup(&key).is_none(), "sin fichero, vacía");
        cache
            .store(choice_from(key.clone(), &result(Backend::Nvenc, 3, 4)))
            .unwrap();
        let reloaded = CalibrationCache::load(Some(&path));
        let hit = reloaded.lookup(&key).expect("acierto");
        assert_eq!(hit.backend(), Some(Backend::Nvenc));
        assert_eq!(hit.p95(), Duration::from_millis(4));
        // Otra GPU, otra resolución u otro formato: fallo de caché.
        let other_gpu = CalibrationKey::with_adapter("otra GPU / 2.0", &config());
        assert!(reloaded.lookup(&other_gpu).is_none());
        let bigger = CalibrationKey::with_adapter(
            "GPU de prueba / 1.0",
            &EncoderConfig {
                width: 3840,
                height: 2160,
                ..config()
            },
        );
        assert!(reloaded.lookup(&bigger).is_none());
        let hdr = CalibrationKey::with_adapter(
            "GPU de prueba / 1.0",
            &EncoderConfig {
                format: VideoFormat::HDR10_420,
                ..config()
            },
        );
        assert!(reloaded.lookup(&hdr).is_none());
        // Sustituir y olvidar.
        let mut cache = reloaded;
        cache
            .store(choice_from(
                key.clone(),
                &result(Backend::VulkanVideo, 1, 2),
            ))
            .unwrap();
        assert_eq!(cache.entries().len(), 1);
        assert_eq!(
            cache.lookup(&key).and_then(CachedChoice::backend),
            Some(Backend::VulkanVideo)
        );
        cache.forget(&key);
        assert!(cache.lookup(&key).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn measuring_a_fake_encoder_counts_only_after_warmup() {
        struct Fake {
            delay: Duration,
        }
        impl Encoder for Fake {
            fn codec(&self) -> Codec {
                Codec::Hevc
            }
            fn backend(&self) -> Backend {
                Backend::Software
            }
            fn configure(&mut self, _: &EncoderConfig) -> Result<(), crate::CodecError> {
                Ok(())
            }
            fn set_bitrate(&mut self, _: u32) -> Result<(), crate::CodecError> {
                Ok(())
            }
            fn request_keyframe(&mut self) {}
            fn encode(
                &mut self,
                frame: &RawFrame,
            ) -> Result<Vec<crate::EncodedSlice>, crate::CodecError> {
                std::thread::sleep(self.delay);
                Ok(vec![crate::EncodedSlice {
                    frame_id: frame.frame_id,
                    index: 0,
                    count: 1,
                    keyframe: frame.frame_id == 0,
                    data: vec![0u8; 2_000],
                }])
            }
        }
        let config = EncoderConfig {
            width: 64,
            height: 64,
            ..config()
        };
        let measured = measure(Backend::Software, &config, || {
            Ok(Box::new(Fake {
                delay: Duration::from_millis(1),
            }) as Box<dyn Encoder>)
        });
        assert!(measured.opened, "{measured}");
        assert_eq!(measured.samples, FRAMES - WARMUP);
        assert!(measured.p50 >= Duration::from_millis(1));
        assert!(measured.p95 >= measured.p50);
        // 2000 bytes × 8 × 60 fps = 960 kbit/s.
        assert_eq!(measured.kbit_s, 960);
        let failed = measure(Backend::Nvenc, &config, || Err("sin GPU".to_owned()));
        assert!(!failed.opened);
        assert!(failed.note.as_deref().unwrap_or("").contains("sin GPU"));
        // Un driver que se cuelga al abrir no detiene la calibración: se descarta al pasar el
        // límite y el hilo que lo abre termina por su cuenta.
        let stuck = open_within(Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_millis(300));
            Err("tarde".to_owned())
        });
        let reason = match stuck {
            Ok(_) => panic!("no debía abrir"),
            Err(reason) => reason,
        };
        assert!(reason.contains("se abandona"), "{reason}");
        assert!(pick(&[failed, measured.clone()]).is_some_and(|r| r.backend == Backend::Software));
    }
}
