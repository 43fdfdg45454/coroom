//! Audio **por aplicación**: qué aplicaciones suenan ([`playing_apps`]) y una fuente que mezcla
//! las elegidas ([`AppMixSource`]), igual en todas las plataformas.
//!
//! Cada backend dice qué flujos suenan, cada uno con la clave de su aplicación, y abre una
//! captura por flujo (PipeWire: el nodo de reproducción de la aplicación; WASAPI: el loopback de
//! su proceso). La mezcla:
//!
//! - **Lleva su propio reloj** de 10 ms: en cada trama toma lo que haya de cada captura sin
//!   esperar (una aplicación en silencio no entrega nada) y suma con el [`Mixer`].
//! - **Sigue a las aplicaciones**: un hilo (`audio-apps`) vuelve a mirar qué suena cada
//!   [`RESCAN`] y abre o cierra capturas según el [`AppFilter`]. Con [`AppFilter::AllExcept`],
//!   una aplicación que empieza a sonar después entra sola.
//! - **Nunca captura coroom**: el backend descarta los flujos de este proceso, así la voz de la
//!   sala no vuelve como eco.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::mix::Mixer;
use crate::ring::Shared;
use crate::{AudioConfig, AudioError, AudioFormat, AudioSource, Captured, FRAME_DURATION};

/// Cada cuánto se vuelve a mirar qué aplicaciones suenan.
pub const RESCAN: Duration = Duration::from_secs(1);

/// Una aplicación que reproduce audio ahora.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayingApp {
    /// Clave estable entre sesiones (en minúsculas): el nombre del ejecutable en Windows
    /// (`juego.exe`), `application.name` (o el binario) en PipeWire. Es la que se guarda para
    /// recordar una aplicación quitada.
    pub key: String,
    /// Nombre para mostrar.
    pub name: String,
    /// Flujos que suenan de esta aplicación (identificadores del backend).
    pub(crate) streams: Vec<String>,
}

impl PlayingApp {
    /// Aplicación con la clave normalizada a minúsculas.
    pub fn new(key: &str, name: &str, streams: Vec<String>) -> Self {
        Self {
            key: normalize(key),
            name: if name.trim().is_empty() {
                key.to_owned()
            } else {
                name.trim().to_owned()
            },
            streams,
        }
    }
}

/// Clave normalizada (minúsculas, sin espacios alrededor).
pub fn normalize(key: &str) -> String {
    key.trim().to_lowercase()
}

/// Qué aplicaciones se envían.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppFilter {
    /// Todas las que suenen, ahora o después, salvo estas claves.
    AllExcept(Vec<String>),
    /// Solo estas claves.
    Only(Vec<String>),
}

impl AppFilter {
    /// Si la aplicación de clave `key` se envía.
    pub fn includes(&self, key: &str) -> bool {
        let key = normalize(key);
        match self {
            AppFilter::AllExcept(excluded) => !excluded.iter().any(|k| normalize(k) == key),
            AppFilter::Only(included) => included.iter().any(|k| normalize(k) == key),
        }
    }
}

/// Junta flujos por aplicación: una entrada por clave, con todos sus flujos, sin duplicados y
/// ordenada por nombre.
#[cfg_attr(
    not(any(
        all(target_os = "linux", feature = "pipewire"),
        all(target_os = "windows", feature = "wasapi")
    )),
    allow(dead_code)
)]
pub(crate) fn group(
    streams: impl IntoIterator<Item = (String, String, String)>,
) -> Vec<PlayingApp> {
    let mut apps: Vec<PlayingApp> = Vec::new();
    for (key, name, stream) in streams {
        let app = PlayingApp::new(&key, &name, Vec::new());
        if app.key.is_empty() {
            continue;
        }
        match apps.iter_mut().find(|known| known.key == app.key) {
            Some(known) => {
                if !known.streams.contains(&stream) {
                    known.streams.push(stream);
                }
            }
            None => apps.push(PlayingApp {
                streams: vec![stream],
                ..app
            }),
        }
    }
    apps.sort_by_key(|app| app.name.to_lowercase());
    apps
}

/// Una captura abierta de un flujo: el anillo que llena el backend y lo que la mantiene viva
/// (al soltarlo, el backend la cierra).
pub(crate) struct Tap {
    pub(crate) shared: Arc<Shared>,
    pub(crate) _guard: Box<dyn Send>,
}

/// Lo que cada backend aporta a la mezcla.
pub(crate) trait AppBackend: Send + Sync + 'static {
    /// Aplicaciones que suenan ahora (sin coroom).
    fn playing(&self) -> Result<Vec<PlayingApp>, AudioError>;

    /// Abre la captura del flujo `stream` en `format`.
    fn open(&self, stream: &str, format: AudioFormat) -> Result<Tap, AudioError>;

    /// Nombre del backend para [`AudioSource::describe`].
    fn label(&self) -> &'static str;

    /// De los flujos que se quieren capturar, los que hay que abrir: en Windows cada captura
    /// incluye a los procesos hijos, así que se quita el que ya cubre un ancestro (si no, sonaría
    /// dos veces). Por defecto, todos.
    fn covering(&self, streams: Vec<String>) -> Vec<String> {
        streams
    }
}

/// Capturas abiertas, por flujo.
type Taps = Arc<Mutex<HashMap<String, Tap>>>;

/// Hilo que mantiene las capturas al día.
struct Scanner {
    quit: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Scanner {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

/// Mezcla del audio de las aplicaciones que elige un [`AppFilter`].
pub struct AppMixSource {
    backend: Arc<dyn AppBackend>,
    /// Compartido con el hilo que abre y cierra los flujos: se puede cambiar en marcha.
    filter: Arc<Mutex<AppFilter>>,
    format: AudioFormat,
    taps: Taps,
    scanner: Option<Scanner>,
    mixer: Mixer,
    next_tick: Option<Instant>,
    scratch: Vec<i16>,
}

impl AppMixSource {
    pub(crate) fn new(backend: Arc<dyn AppBackend>, filter: AppFilter) -> Self {
        Self {
            backend,
            filter: Arc::new(Mutex::new(filter)),
            format: AudioFormat::GAME,
            taps: Arc::default(),
            scanner: None,
            mixer: Mixer::new(AudioFormat::GAME.channels),
            next_tick: None,
            scratch: Vec::new(),
        }
    }
}

impl AppMixSource {
    fn filter(&self) -> AppFilter {
        self.filter
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Abre lo que falta y cierra lo que ya no suena o ya no se envía.
fn sync_taps(backend: &dyn AppBackend, filter: &AppFilter, format: AudioFormat, taps: &Taps) {
    let wanted: Vec<String> = match backend.playing() {
        Ok(apps) => apps
            .into_iter()
            .filter(|app| filter.includes(&app.key))
            .flat_map(|app| app.streams)
            .collect(),
        Err(err) => {
            tracing::debug!(%err, "audio por aplicación: no se pudo listar");
            return;
        }
    };
    let wanted = backend.covering(wanted);
    let stale: Vec<String> = {
        let open = taps.lock().unwrap_or_else(PoisonError::into_inner);
        open.keys()
            .filter(|stream| !wanted.contains(stream))
            .cloned()
            .collect()
    };
    for stream in stale {
        // Se suelta fuera del cerrojo: cerrar un flujo espera a su hilo.
        let tap = taps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&stream);
        drop(tap);
    }
    for stream in wanted {
        let open = taps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&stream);
        if open {
            continue;
        }
        match backend.open(&stream, format) {
            Ok(tap) => {
                taps.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(stream, tap);
            }
            Err(err) => tracing::debug!(%err, stream, "audio por aplicación: no se pudo abrir"),
        }
    }
}

impl AudioSource for AppMixSource {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn start(&mut self, config: &AudioConfig) -> Result<(), AudioError> {
        self.stop();
        self.format = config.format;
        self.mixer = Mixer::new(config.format.channels);
        // La primera pasada, aquí: si el backend no responde, se dice al abrir.
        self.backend.playing()?;
        let filter = self.filter();
        sync_taps(self.backend.as_ref(), &filter, self.format, &self.taps);
        let quit = Arc::new(AtomicBool::new(false));
        let thread = {
            let quit = Arc::clone(&quit);
            let backend = Arc::clone(&self.backend);
            let filter = Arc::clone(&self.filter);
            let format = self.format;
            let taps = Arc::clone(&self.taps);
            thread::Builder::new()
                .name("audio-apps".into())
                .spawn(move || {
                    while !quit.load(Ordering::Relaxed) {
                        thread::park_timeout(RESCAN);
                        if quit.load(Ordering::Relaxed) {
                            break;
                        }
                        let filter = filter
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .clone();
                        sync_taps(backend.as_ref(), &filter, format, &taps);
                    }
                })
                .map_err(|err| AudioError::Backend(format!("no se pudo crear el hilo: {err}")))?
        };
        self.scanner = Some(Scanner {
            quit,
            thread: Some(thread),
        });
        self.next_tick = None;
        Ok(())
    }

    fn read(&mut self, out: &mut Vec<i16>) -> Result<Captured, AudioError> {
        if self.scanner.is_none() {
            return Err(AudioError::Stopped);
        }
        let now = Instant::now();
        let tick = match self.next_tick {
            // Muy atrasado (el hilo se paró): se vuelve a empezar desde ahora.
            Some(tick) if now.saturating_duration_since(tick) < FRAME_DURATION * 5 => tick,
            _ => now,
        };
        if tick > now {
            thread::sleep(tick - now);
        }
        self.next_tick = Some(tick + FRAME_DURATION);
        let needed = self.format.samples_per_frame();
        self.mixer.begin();
        {
            let taps = self.taps.lock().unwrap_or_else(PoisonError::into_inner);
            for (index, tap) in taps.values().enumerate() {
                if tap.shared.try_take(self.format, &mut self.scratch) {
                    self.mixer.add(
                        u32::try_from(index).unwrap_or(u32::MAX),
                        &self.scratch,
                        self.format.channels,
                    );
                }
            }
        }
        out.clear();
        out.resize(needed, 0);
        if self.mixer.added() > 0 {
            self.mixer.finish(out);
        }
        Ok(Captured {
            samples: needed,
            captured_at: Instant::now()
                .checked_sub(FRAME_DURATION)
                .unwrap_or_else(Instant::now),
            channels: self.format.channels,
        })
    }

    fn stop(&mut self) {
        self.scanner = None;
        let taps: Vec<Tap> = self
            .taps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
            .map(|(_, tap)| tap)
            .collect();
        drop(taps);
    }

    /// Cambia qué aplicaciones se envían; el hilo de los flujos lo aplica enseguida (abre las
    /// nuevas y cierra las que ya no van) sin parar la mezcla.
    fn set_app_filter(&mut self, filter: &AppFilter) -> bool {
        *self.filter.lock().unwrap_or_else(PoisonError::into_inner) = filter.clone();
        if let Some(thread) = self.scanner.as_ref().and_then(|s| s.thread.as_ref()) {
            thread.thread().unpark();
        }
        true
    }

    fn describe(&self) -> String {
        match &self.filter() {
            AppFilter::AllExcept(excluded) if excluded.is_empty() => {
                format!("todas las aplicaciones ({})", self.backend.label())
            }
            AppFilter::AllExcept(excluded) => format!(
                "todas las aplicaciones salvo {} ({})",
                excluded.join(", "),
                self.backend.label()
            ),
            AppFilter::Only(included) => format!(
                "solo {} ({})",
                if included.is_empty() {
                    "ninguna aplicación".to_owned()
                } else {
                    included.join(", ")
                },
                self.backend.label()
            ),
        }
    }
}

impl Drop for AppMixSource {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_match_keys_without_case() {
        let all = AppFilter::AllExcept(vec!["Discord".into()]);
        assert!(all.includes("juego.exe"));
        assert!(!all.includes("discord"));
        let only = AppFilter::Only(vec!["juego.exe".into()]);
        assert!(only.includes("JUEGO.EXE"));
        assert!(!only.includes("firefox"));
    }

    #[test]
    fn streams_group_by_app() {
        let apps = group([
            ("Firefox".into(), "Firefox".into(), "1".into()),
            ("firefox".into(), "Firefox".into(), "2".into()),
            ("Juego".into(), "Juego".into(), "3".into()),
            ("firefox".into(), "Firefox".into(), "2".into()),
            (String::new(), "sin clave".into(), "4".into()),
        ]);
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].key, "firefox");
        assert_eq!(apps[0].streams, vec!["1".to_owned(), "2".to_owned()]);
        assert_eq!(apps[1].name, "Juego");
    }

    /// Backend de prueba: dos flujos, uno que suena (llena su anillo) y otro mudo.
    struct Fake {
        loud: Mutex<Option<Arc<Shared>>>,
    }

    impl AppBackend for Fake {
        fn playing(&self) -> Result<Vec<PlayingApp>, AudioError> {
            Ok(group([
                ("juego".into(), "Juego".into(), "a".into()),
                ("voz".into(), "Voz".into(), "b".into()),
            ]))
        }

        fn open(&self, stream: &str, format: AudioFormat) -> Result<Tap, AudioError> {
            let shared = Shared::new(format);
            if stream == "a" {
                *self.loud.lock().unwrap() = Some(Arc::clone(&shared));
            }
            Ok(Tap {
                shared,
                _guard: Box::new(()),
            })
        }

        fn label(&self) -> &'static str {
            "prueba"
        }
    }

    #[test]
    fn the_mix_paces_itself_and_skips_excluded_apps() {
        let fake = Arc::new(Fake {
            loud: Mutex::new(None),
        });
        let mut source = AppMixSource::new(
            Arc::clone(&fake) as Arc<dyn AppBackend>,
            AppFilter::AllExcept(vec!["voz".into()]),
        );
        source.start(&AudioConfig::game()).expect("start");
        assert_eq!(source.taps.lock().unwrap().len(), 1, "voz no se abre");
        let loud = fake.loud.lock().unwrap().clone().expect("juego abierto");
        loud.push(std::iter::repeat_n(
            1000,
            AudioFormat::GAME.samples_per_frame(),
        ));
        let mut frame = Vec::new();
        let started = Instant::now();
        let first = source.read(&mut frame).expect("trama");
        assert_eq!(first.samples, AudioFormat::GAME.samples_per_frame());
        assert!(frame.iter().all(|&s| s > 0), "suena el juego");
        // Sin nada en el anillo: silencio a su ritmo, sin esperar al backend.
        source.read(&mut frame).expect("trama");
        source.read(&mut frame).expect("trama");
        assert!(frame.iter().all(|&s| s == 0));
        let elapsed = started.elapsed();
        assert!(
            elapsed >= FRAME_DURATION * 2 - Duration::from_millis(2),
            "{elapsed:?}"
        );
        assert!(source.describe().contains("salvo voz"));
        source.stop();
        assert_eq!(source.read(&mut frame), Err(AudioError::Stopped));
    }

    #[test]
    fn the_filter_changes_while_mixing() {
        let fake = Arc::new(Fake {
            loud: Mutex::new(None),
        });
        let mut source = AppMixSource::new(
            Arc::clone(&fake) as Arc<dyn AppBackend>,
            AppFilter::Only(vec!["juego".into()]),
        );
        source.start(&AudioConfig::game()).expect("start");
        let open = |source: &AppMixSource| {
            let mut streams: Vec<String> = source.taps.lock().unwrap().keys().cloned().collect();
            streams.sort();
            streams
        };
        assert_eq!(open(&source), ["a"]);
        assert!(source.set_app_filter(&AppFilter::Only(vec!["voz".into()])));
        let started = Instant::now();
        while open(&source) != ["b"] {
            assert!(
                started.elapsed() < RESCAN / 2,
                "sin esperar a la siguiente pasada: {:?}",
                open(&source)
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert!(source.describe().contains("solo voz"));
        let mut frame = Vec::new();
        source.read(&mut frame).expect("la mezcla sigue");
    }
}
