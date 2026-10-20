//! Audio **por aplicación**: qué aplicaciones suenan ([`playing_apps`]) y una fuente que mezcla
//! las elegidas ([`AppMixSource`]), igual en todas las plataformas.
//!
//! Cada backend dice qué flujos suenan, cada uno con la clave de su aplicación, y abre una
//! captura por flujo (PipeWire: el nodo de reproducción de la aplicación; WASAPI: el loopback de
//! su proceso). La mezcla:
//!
//! - **Va al ritmo de una captura**: la que marca el ritmo es la primera que entregó una trama
//!   entera; la mezcla espera a su siguiente trama (un aviso común a todas, [`Arrivals`]) y
//!   entonces toma lo que haya de las demás sin esperarlas (una aplicación en silencio no entrega
//!   nada) y suma con el [`Mixer`], que deja pasar intacto un flujo solo. Cada captura tiene su
//!   propia fase (WASAPI despierta cada loopback por separado): mezclar en cuanto llegaba
//!   cualquiera sacaba una trama por llegada, el doble de tramas con dos aplicaciones. Si la que
//!   marca el ritmo deja de entregar (su aplicación calló), el ritmo pasa a otra en cuanto esta
//!   tiene la cola llena, antes de perder audio. El reloj es el del dispositivo (el grafo de
//!   PipeWire, el motor de WASAPI), no uno propio: con uno propio, cualquier deriva o fase entre
//!   los dos relojes metía tramas de silencio o tiraba audio. Mientras llega audio espera a la siguiente trama hasta
//!   [`SILENCE_AFTER`] después de la última, como una captura sola ([`Shared::read_frame`]);
//!   sin audio, entrega silencio cada 10 ms con su propio reloj. Detrás de lo que toma no deja
//!   más de una trama (10 ms) en cola por captura, y la trama lleva la hora de captura real de
//!   lo que mezcla (la del dispositivo para ese búfer).
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
use crate::ring::{Arrivals, SILENCE_AFTER, Shared};
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
    /// Aviso de llegada común a todas las capturas.
    arrivals: Arc<Arrivals>,
    /// Flujo de la captura que marca el ritmo de la mezcla.
    lead: Option<String>,
    /// Cuándo se mezcló audio por última vez (no silencio).
    last_audio: Option<Instant>,
    /// Siguiente trama de silencio con el reloj propio (sin audio).
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
            arrivals: Arc::default(),
            lead: None,
            last_audio: None,
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

/// Abre lo que falta (con el aviso de llegada `arrivals`) y cierra lo que ya no suena o ya no se
/// envía.
fn sync_taps(
    backend: &dyn AppBackend,
    filter: &AppFilter,
    format: AudioFormat,
    taps: &Taps,
    arrivals: &Arc<Arrivals>,
) {
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
                tap.shared.notify_arrivals_to(Arc::clone(arrivals));
                taps.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(stream, tap);
            }
            Err(err) => tracing::debug!(%err, stream, "audio por aplicación: no se pudo abrir"),
        }
    }
}

/// Si toca mezclar: cuando la captura que marca el ritmo (`lead`) tiene una trama. Sin ella (aún
/// ninguna, o se cerró) marca el ritmo la primera que tenga una trama; si la que lo marca no
/// entrega, otra con la cola llena, que si no empezaría a perder audio.
fn pace(taps: &HashMap<String, Tap>, lead: &mut Option<String>, format: AudioFormat) -> bool {
    let current = lead.as_ref().and_then(|stream| taps.get(stream));
    if current.is_some_and(|tap| tap.shared.has_frame(format)) {
        return true;
    }
    let next = taps
        .iter()
        .find(|(_, tap)| match current {
            Some(_) => tap.shared.backlog_full(format),
            None => tap.shared.has_frame(format),
        })
        .map(|(stream, _)| stream.clone());
    match next {
        Some(stream) => {
            *lead = Some(stream);
            true
        }
        None => false,
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
        sync_taps(
            self.backend.as_ref(),
            &filter,
            self.format,
            &self.taps,
            &self.arrivals,
        );
        let quit = Arc::new(AtomicBool::new(false));
        let thread = {
            let quit = Arc::clone(&quit);
            let backend = Arc::clone(&self.backend);
            let filter = Arc::clone(&self.filter);
            let format = self.format;
            let taps = Arc::clone(&self.taps);
            let arrivals = Arc::clone(&self.arrivals);
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
                        sync_taps(backend.as_ref(), &filter, format, &taps, &arrivals);
                    }
                })
                .map_err(|err| AudioError::Backend(format!("no se pudo crear el hilo: {err}")))?
        };
        self.scanner = Some(Scanner {
            quit,
            thread: Some(thread),
        });
        self.lead = None;
        self.last_audio = None;
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
        // Mientras llega audio, se espera a la siguiente trama de las capturas hasta
        // `SILENCE_AFTER` después de la última; sin audio, hasta la trama de silencio del reloj
        // propio.
        let deadline = match self.last_audio {
            Some(at) if at + SILENCE_AFTER > tick => at + SILENCE_AFTER,
            _ => tick,
        };
        let format = self.format;
        loop {
            let seen = self.arrivals.generation();
            let ready = pace(
                &self.taps.lock().unwrap_or_else(PoisonError::into_inner),
                &mut self.lead,
                format,
            );
            if ready || Instant::now() >= deadline {
                break;
            }
            self.arrivals.wait(seen, deadline);
        }
        let needed = format.samples_per_frame();
        self.mixer.begin();
        // La hora de la trama es la de captura de lo que se mezcla (la más antigua, si hay varias
        // aplicaciones), que da el dispositivo para cada búfer; nunca «ahora menos algo».
        let mut captured_at: Option<Instant> = None;
        {
            let taps = self.taps.lock().unwrap_or_else(PoisonError::into_inner);
            for (index, tap) in taps.values().enumerate() {
                if let Some(at) = tap.shared.try_take(format, &mut self.scratch) {
                    captured_at = Some(captured_at.map_or(at, |earliest| earliest.min(at)));
                    self.mixer.add(
                        u32::try_from(index).unwrap_or(u32::MAX),
                        &self.scratch,
                        format.channels,
                    );
                }
            }
        }
        out.clear();
        out.resize(needed, 0);
        if self.mixer.added() > 0 {
            self.mixer.finish(out);
            let mixed = Instant::now();
            self.last_audio = Some(mixed);
            self.next_tick = Some(mixed + FRAME_DURATION);
        } else {
            self.next_tick = Some(deadline + FRAME_DURATION);
        }
        Ok(Captured {
            samples: needed,
            // Sin ninguna aplicación sonando, el silencio lleva la hora en que se decidió.
            captured_at: captured_at.unwrap_or(deadline),
            channels: format.channels,
        })
    }

    fn stop(&mut self) {
        self.scanner = None;
        self.lead = None;
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

    /// Backend de prueba: dos flujos, `a` (juego) y `b` (voz), cuyos anillos llena cada prueba.
    #[derive(Default)]
    struct Fake {
        opened: Mutex<HashMap<String, Arc<Shared>>>,
    }

    impl Fake {
        fn stream(&self, stream: &str) -> Arc<Shared> {
            let opened = self.opened.lock().unwrap();
            Arc::clone(opened.get(stream).expect("flujo abierto"))
        }
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
            self.opened
                .lock()
                .unwrap()
                .insert(stream.to_owned(), Arc::clone(&shared));
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
        let fake = Arc::new(Fake::default());
        let mut source = AppMixSource::new(
            Arc::clone(&fake) as Arc<dyn AppBackend>,
            AppFilter::AllExcept(vec!["voz".into()]),
        );
        source.start(&AudioConfig::game()).expect("start");
        assert_eq!(source.taps.lock().unwrap().len(), 1, "voz no se abre");
        let loud = fake.stream("a");
        // El dispositivo fecha el búfer hace 30 ms: es la hora que lleva la trama mezclada.
        let device = Instant::now() - Duration::from_millis(30);
        loud.push(
            AudioFormat::GAME,
            std::iter::repeat_n(1000, AudioFormat::GAME.samples_per_frame()),
            Some(device),
        );
        let mut frame = Vec::new();
        let started = Instant::now();
        let first = source.read(&mut frame).expect("trama");
        assert_eq!(first.samples, AudioFormat::GAME.samples_per_frame());
        assert_eq!(first.captured_at, device, "la hora real de captura");
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
    fn the_mix_follows_the_capture_clock_instead_of_inserting_silence() {
        let fake = Arc::new(Fake::default());
        let mut source = AppMixSource::new(
            Arc::clone(&fake) as Arc<dyn AppBackend>,
            AppFilter::Only(vec!["juego".into()]),
        );
        source.start(&AudioConfig::game()).expect("start");
        let loud = fake.stream("a");
        let frame_of =
            |value: i16| std::iter::repeat_n(value, AudioFormat::GAME.samples_per_frame());
        // Un flujo solo pasa intacto por la mezcla: ni limitador ni redondeos, aunque vaya al
        // fondo de escala.
        loud.push(AudioFormat::GAME, frame_of(i16::MAX), None);
        let mut frame = Vec::new();
        let first_started = Instant::now();
        source.read(&mut frame).expect("trama");
        assert!(frame.iter().all(|&s| s == i16::MAX), "{:?}", &frame[..4]);
        // La captura entrega la siguiente trama tarde (un reloj de dispositivo más lento que el
        // del sistema, un despertar tardío): la mezcla la espera en vez de meter silencio a los
        // 10 ms de su propio reloj.
        let pusher = {
            let loud = Arc::clone(&loud);
            thread::spawn(move || {
                thread::park_timeout(FRAME_DURATION * 2);
                loud.push(
                    AudioFormat::GAME,
                    std::iter::repeat_n(-1234, AudioFormat::GAME.samples_per_frame()),
                    None,
                );
            })
        };
        source.read(&mut frame).expect("trama");
        let returned = Instant::now();
        pusher.join().expect("hilo");
        let silent = frame.iter().all(|&s| s == 0);
        if silent {
            // Solo si de verdad no llegó nada en `SILENCE_AFTER` (una máquina muy cargada).
            assert!(
                returned.duration_since(first_started) >= SILENCE_AFTER,
                "silencio a los {:?}: no esperó a la captura",
                returned.duration_since(first_started)
            );
        } else {
            assert!(frame.iter().all(|&s| s == -1234), "la trama llega entera");
        }
        coroom_testkit::timing_assert!(
            !silent,
            "la captura tardó más de {SILENCE_AFTER:?} en entregar la trama"
        );
    }

    /// Dos aplicaciones con su propia fase, como dos loopback de WASAPI: cada 10 ms llega una
    /// trama de cada una, a 5 ms de la otra. La mezcla saca una trama por cada 10 ms con las dos
    /// sumadas, no una por llegada (el doble, que el receptor tenía que recortar).
    #[test]
    fn two_captures_out_of_phase_mix_at_one_frame_per_period() {
        const FRAMES: u32 = 40;
        let fake = Arc::new(Fake::default());
        let mut source = AppMixSource::new(
            Arc::clone(&fake) as Arc<dyn AppBackend>,
            AppFilter::AllExcept(Vec::new()),
        );
        source.start(&AudioConfig::game()).expect("start");
        let started = Instant::now();
        let pushers: Vec<_> = [
            ("a", 1000_i16, Duration::ZERO),
            ("b", 100, FRAME_DURATION / 2),
        ]
        .into_iter()
        .map(|(stream, value, phase)| {
            let shared = fake.stream(stream);
            thread::spawn(move || {
                for frame in 0..FRAMES {
                    let at = started + phase + FRAME_DURATION * frame;
                    // El ritmo del dispositivo simulado, no una espera para sincronizar.
                    thread::sleep(at.saturating_duration_since(Instant::now()));
                    shared.push(
                        AudioFormat::GAME,
                        std::iter::repeat_n(value, AudioFormat::GAME.samples_per_frame()),
                        None,
                    );
                }
            })
        })
        .collect();
        let mut frame = Vec::new();
        let (mut audio, mut both) = (0_u32, 0_u32);
        loop {
            source.read(&mut frame).expect("trama");
            let value = frame[0];
            assert!(
                frame.iter().all(|&s| s == value),
                "trama partida: {:?}",
                &frame[..4]
            );
            assert!(
                matches!(value, 0 | 100 | 1000 | 1100),
                "mezcla con valores ajenos: {value}"
            );
            if value == 0 {
                if pushers.iter().all(|pusher| pusher.is_finished()) {
                    break;
                }
                continue;
            }
            audio += 1;
            both += u32::from(value == 1100);
        }
        for pusher in pushers {
            pusher.join().expect("hilo");
        }
        // Con una trama por llegada salían 2 × FRAMES; seguir a una captura no pasa de una trama
        // por cada una de las suyas, más algún relevo si los hilos de prueba se atrasan (lo
        // determinista lo prueba `the_lead_capture_sets_the_pace`).
        assert!(audio > 0, "no salió audio");
        coroom_testkit::timing_assert!(
            audio <= FRAMES + 2 && both + 4 >= FRAMES,
            "{audio} tramas con audio ({both} con las dos aplicaciones) de {FRAMES} por aplicación"
        );
    }

    /// El ritmo, sin hilos ni relojes: llegan tramas de dos capturas desfasadas y se mezcla cada
    /// vez que toca. Sale una mezcla por trama de la que marca el ritmo, con las dos sumadas; si
    /// esa calla, la otra toma el ritmo cuando se le llena la cola, sin perder audio.
    #[test]
    fn the_lead_capture_sets_the_pace() {
        const FRAMES: u32 = 40;
        let format = AudioFormat::GAME;
        let taps: HashMap<String, Tap> = ["a", "b"]
            .into_iter()
            .map(|stream| {
                let tap = Tap {
                    shared: Shared::new(format),
                    _guard: Box::new(()),
                };
                (stream.to_owned(), tap)
            })
            .collect();
        let push = |stream: &str| {
            taps[stream].shared.push(
                format,
                std::iter::repeat_n(1, format.samples_per_frame()),
                None,
            );
        };
        let mut lead = None;
        let mut scratch = Vec::new();
        // Mezcla mientras toque y cuenta cuántas veces y cuántas con las dos capturas.
        let mut mix = |lead: &mut Option<String>, mixes: &mut u32, both: &mut u32| {
            while pace(&taps, lead, format) {
                let taken = taps
                    .values()
                    .filter(|tap| tap.shared.try_take(format, &mut scratch).is_some())
                    .count();
                *mixes += 1;
                *both += u32::from(taken == 2);
            }
        };
        let (mut mixes, mut both) = (0, 0);
        for _ in 0..FRAMES {
            push("a");
            mix(&mut lead, &mut mixes, &mut both);
            push("b");
            mix(&mut lead, &mut mixes, &mut both);
        }
        assert_eq!(lead.as_deref(), Some("a"), "marca el ritmo la primera");
        assert_eq!(mixes, FRAMES, "una mezcla por trama, no una por llegada");
        assert_eq!(both, FRAMES - 1, "todas con las dos salvo la primera");
        // `a` calla: `b` tiene en cola la trama que llegó tras la última mezcla y se espera a
        // `a`; con la siguiente, la cola de `b` está llena y toma el ritmo.
        let (mut mixes, mut both) = (0, 0);
        mix(&mut lead, &mut mixes, &mut both);
        assert_eq!(mixes, 0, "una trama de retraso se espera");
        push("b");
        mix(&mut lead, &mut mixes, &mut both);
        assert_eq!(
            lead.as_deref(),
            Some("b"),
            "con la cola llena, el ritmo pasa a la otra"
        );
        assert_eq!(mixes, 2, "lo que esperaba sale entero");
        assert!(!taps["b"].shared.has_frame(format));
    }

    #[test]
    fn the_filter_changes_while_mixing() {
        let fake = Arc::new(Fake::default());
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
                started.elapsed() < Duration::from_secs(10),
                "el filtro nuevo no se aplica: {:?}",
                open(&source)
            );
            thread::sleep(Duration::from_millis(5));
        }
        let elapsed = started.elapsed();
        coroom_testkit::timing_assert!(
            elapsed < RESCAN / 2,
            "el filtro tardó {elapsed:?} en aplicarse: parece que esperó a la siguiente pasada"
        );
        assert!(source.describe().contains("solo voz"));
        let mut frame = Vec::new();
        source.read(&mut frame).expect("la mezcla sigue");
    }
}
