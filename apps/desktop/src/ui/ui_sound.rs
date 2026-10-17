//! El reproductor de los sonidos de la interfaz ([`coroom_ui::sound`]).
//!
//! - **Sin esperas en la interfaz**: un aviso de un componente solo mira el estado (atómicos) y
//!   encola el sonido ya sintetizado; un hilo propio lo saca por la salida elegida.
//! - **Una salida abierta mientras se usa**: abrir el dispositivo para cada «tic» llegaría tarde.
//!   El hilo la abre con el primer sonido, mezcla los que se pisan y la cierra cuando lleva
//!   [`IDLE_CLOSE`] sin sonar nada (para no tener el dispositivo despierto).
//! - **Cuándo no suena**: apagados en Notificaciones, con volumen cero, con el teclado y el ratón
//!   tomados o con el vídeo a pantalla completa ([`set_muted`]). Pasar el ratón suena como mucho
//!   una vez cada [`HOVER_GAP`].
//! - Como los avisos, va por el reproductor de la sala, que el audio del juego que se envía deja
//!   fuera: nunca llega a la sala.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use coroom_ui::sound::Cue;

use super::tones;

/// Lo menos que pasa entre dos «tic» de pasar el ratón.
pub const HOVER_GAP: Duration = Duration::from_millis(50);

/// Sin sonar nada este tiempo, la salida se cierra.
const IDLE_CLOSE: Duration = Duration::from_secs(2);

/// Estado compartido entre la interfaz (que lo cambia) y los avisos de los componentes.
struct Shared {
    /// Ganancia lineal (bits de un `f32`); cero: apagados.
    gain: AtomicU32,
    /// Teclado y ratón tomados o vídeo a pantalla completa.
    muted: AtomicBool,
    /// Salida elegida (`None`: la del sistema).
    device: Mutex<Option<String>>,
    /// Cuándo sonó el último «tic» de pasar.
    last_hover: Mutex<Option<Instant>>,
    /// Los sonidos ya sintetizados (el de pasar, por su altura redondeada).
    cache: Mutex<HashMap<CueKey, Arc<[f32]>>>,
    queue: Mutex<Sender<Request>>,
}

/// Un sonido para el hilo: las muestras (mono, 48 kHz, ya con la ganancia) y por dónde.
struct Request {
    samples: Vec<f32>,
    device: Option<String>,
}

/// Clave de la caché: cada sonido, y el de pasar en 8 alturas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum CueKey {
    Hover(u8),
    Other(u8),
}

impl CueKey {
    fn of(cue: Cue) -> (Self, Cue) {
        match cue {
            Cue::Hover { x } => {
                let step = (x.clamp(0.0, 1.0) * 7.0).round() as u8;
                (
                    CueKey::Hover(step),
                    Cue::Hover {
                        x: f32::from(step) / 7.0,
                    },
                )
            }
            Cue::Press => (CueKey::Other(0), cue),
            Cue::Confirm => (CueKey::Other(1), cue),
            Cue::Toggle(true) => (CueKey::Other(2), cue),
            Cue::Toggle(false) => (CueKey::Other(3), cue),
            Cue::Open => (CueKey::Other(4), cue),
            Cue::Close => (CueKey::Other(5), cue),
            Cue::Navigate => (CueKey::Other(6), cue),
            Cue::Copied => (CueKey::Other(7), cue),
            Cue::Error => (CueKey::Other(8), cue),
        }
    }
}

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

/// Arranca el reproductor y lo engancha a los componentes (una vez por proceso).
pub fn install() {
    let shared = SHARED.get_or_init(|| {
        let (sender, receiver) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("coroom-ui-sound".into())
            .spawn(move || run(receiver));
        if let Err(err) = spawned {
            tracing::debug!(error = %err, "sin hilo para los sonidos de la interfaz");
        }
        Arc::new(Shared {
            gain: AtomicU32::new(0f32.to_bits()),
            muted: AtomicBool::new(false),
            device: Mutex::new(None),
            last_hover: Mutex::new(None),
            cache: Mutex::new(HashMap::new()),
            queue: Mutex::new(sender),
        })
    });
    let shared = Arc::clone(shared);
    coroom_ui::sound::set_player(Some(Arc::new(move |cue| play(&shared, cue))));
}

/// Los ajustes de ahora: la ganancia (cero: apagados) y la salida.
pub fn configure(gain: f32, device: Option<&str>) {
    let Some(shared) = SHARED.get() else {
        return;
    };
    shared.gain.store(gain.to_bits(), Ordering::Relaxed);
    if let Ok(mut current) = shared.device.lock()
        && current.as_deref() != device
    {
        *current = device.map(str::to_owned);
    }
}

/// Con el teclado y el ratón tomados o el vídeo a pantalla completa no suena nada.
pub fn set_muted(muted: bool) {
    if let Some(shared) = SHARED.get() {
        shared.muted.store(muted, Ordering::Relaxed);
    }
}

/// Hace sonar `cue` aunque no toque (la vista previa de Notificaciones), con `gain`.
pub fn preview(cue: Cue, gain: f32, device: Option<&str>) {
    if let Some(shared) = SHARED.get() {
        enqueue(shared, cue, gain, device.map(str::to_owned));
    }
}

fn play(shared: &Shared, cue: Cue) {
    let gain = f32::from_bits(shared.gain.load(Ordering::Relaxed));
    if gain <= 0.0 || shared.muted.load(Ordering::Relaxed) {
        return;
    }
    if let Cue::Hover { .. } = cue {
        let Ok(mut last) = shared.last_hover.lock() else {
            return;
        };
        let now = Instant::now();
        if last.is_some_and(|at| now.saturating_duration_since(at) < HOVER_GAP) {
            return;
        }
        *last = Some(now);
    }
    let device = shared.device.lock().ok().and_then(|device| device.clone());
    enqueue(shared, cue, gain, device);
}

fn enqueue(shared: &Shared, cue: Cue, gain: f32, device: Option<String>) {
    let (key, cue) = CueKey::of(cue);
    let samples = {
        let Ok(mut cache) = shared.cache.lock() else {
            return;
        };
        Arc::clone(
            cache
                .entry(key)
                .or_insert_with(|| tones::cue_samples(cue).into()),
        )
    };
    let request = Request {
        samples: samples.iter().map(|sample| sample * gain).collect(),
        device,
    };
    if let Ok(queue) = shared.queue.lock() {
        let _ = queue.send(request);
    }
}

/// El hilo: espera un sonido, abre la salida, mezcla lo que llegue mientras suena y la cierra al
/// quedarse sin nada.
fn run(receiver: Receiver<Request>) {
    use coroom_session::audio::{AudioConfig, AudioFormat, DEVICE_LATENCY, playback_sink};

    let format = AudioFormat::GAME;
    let frame = format.samples_per_frame();
    let channels = usize::from(format.channels.max(1));
    while let Ok(first) = receiver.recv() {
        let device = first.device.clone();
        let config = AudioConfig {
            format,
            device: device.clone(),
            latency: DEVICE_LATENCY,
        };
        let mut sink = match playback_sink(device.as_deref(), format)
            .and_then(|mut sink| sink.start(&config).map(|()| sink))
        {
            Ok(sink) => sink,
            Err(err) => {
                tracing::debug!(error = %err, "no sonó un sonido de la interfaz");
                continue;
            }
        };
        // Lo que falta por sonar (mono), con lo nuevo mezclado desde el principio.
        let mut pending: Vec<f32> = first.samples;
        let mut idle_since: Option<Instant> = None;
        let mut out = Vec::with_capacity(frame);
        loop {
            // Mientras está abierta, todo suena por la misma salida (si se elige otra, la usa la
            // apertura siguiente).
            while let Ok(request) = receiver.try_recv() {
                mix(&mut pending, &request.samples);
                idle_since = None;
            }
            if pending.is_empty() {
                let since = *idle_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= IDLE_CLOSE {
                    break;
                }
                match receiver.recv_timeout(format.duration_of(frame)) {
                    Ok(request) => {
                        mix(&mut pending, &request.samples);
                        idle_since = None;
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                continue;
            }
            let take = (frame / channels).min(pending.len());
            out.clear();
            out.extend(
                tones::to_i16(&pending[..take], 1.0)
                    .into_iter()
                    .flat_map(|sample| std::iter::repeat_n(sample, channels)),
            );
            out.resize(frame, 0);
            pending.drain(..take);
            if let Err(err) = sink.write(&out) {
                tracing::debug!(error = %err, "se cortó un sonido de la interfaz");
                break;
            }
        }
        sink.stop();
    }
}

/// Suma `samples` a `pending` desde el principio (alargándolo si hace falta).
fn mix(pending: &mut Vec<f32>, samples: &[f32]) {
    if pending.len() < samples.len() {
        pending.resize(samples.len(), 0.0);
    }
    for (slot, sample) in pending.iter_mut().zip(samples) {
        *slot += sample;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared(gain: f32, muted: bool) -> (Shared, Receiver<Request>) {
        let (sender, receiver) = mpsc::channel();
        let shared = Shared {
            gain: AtomicU32::new(gain.to_bits()),
            muted: AtomicBool::new(muted),
            device: Mutex::new(None),
            last_hover: Mutex::new(None),
            cache: Mutex::new(HashMap::new()),
            queue: Mutex::new(sender),
        };
        (shared, receiver)
    }

    #[test]
    fn nothing_sounds_when_off_or_muted() {
        let (off, queue) = shared(0.0, false);
        play(&off, Cue::Press);
        assert!(queue.try_recv().is_err(), "apagados");
        let (muted, queue) = shared(1.0, true);
        play(&muted, Cue::Confirm);
        assert!(queue.try_recv().is_err(), "con la entrada tomada");
        let (on, queue) = shared(0.25, false);
        play(&on, Cue::Press);
        let request = queue.try_recv().expect("suena");
        let full = tones::cue_samples(Cue::Press);
        assert_eq!(request.samples.len(), full.len());
        assert!((request.samples[full.len() / 3] - full[full.len() / 3] * 0.25).abs() < 1e-6);
    }

    #[test]
    fn hovering_sounds_at_most_once_per_gap() {
        let (on, queue) = shared(1.0, false);
        play(&on, Cue::Hover { x: 0.2 });
        play(&on, Cue::Hover { x: 0.8 });
        assert_eq!(
            queue.try_iter().count(),
            1,
            "el segundo, dentro de {HOVER_GAP:?}"
        );
        // Pulsar no espera al de pasar.
        play(&on, Cue::Press);
        assert_eq!(queue.try_iter().count(), 1);
    }

    #[test]
    fn overlapping_sounds_mix_from_the_start() {
        let mut pending = vec![0.25, 0.25];
        mix(&mut pending, &[0.5, 0.5, 0.5]);
        assert_eq!(pending, vec![0.75, 0.75, 0.5]);
    }

    #[test]
    fn every_cue_is_short_quiet_and_silent_at_the_edges() {
        let cues = [
            Cue::Hover { x: 0.0 },
            Cue::Hover { x: 1.0 },
            Cue::Press,
            Cue::Confirm,
            Cue::Toggle(true),
            Cue::Toggle(false),
            Cue::Open,
            Cue::Close,
            Cue::Navigate,
            Cue::Copied,
            Cue::Error,
        ];
        for cue in cues {
            let samples = tones::cue_samples(cue);
            let seconds = samples.len() as f64 / f64::from(tones::RATE);
            assert!(seconds > 0.0 && seconds <= 0.2, "{cue:?} dura {seconds} s");
            let peak = samples.iter().fold(0f32, |peak, s| peak.max(s.abs()));
            assert!(
                peak > 0.0 && peak < tones::PEAK as f32,
                "{cue:?}: pico {peak}"
            );
            assert_eq!(
                samples.first().copied(),
                Some(0.0),
                "{cue:?} empieza en cero"
            );
            assert_eq!(samples.last().copied(), Some(0.0), "{cue:?} acaba en cero");
        }
        // El «tic» de pasar sube de izquierda a derecha y es el más bajo.
        let loudness = |cue| {
            let samples: Vec<f64> = tones::cue_samples(cue)
                .iter()
                .map(|s| f64::from(*s))
                .collect();
            tones::max_momentary_loudness(&samples)
        };
        assert!(loudness(Cue::Hover { x: 0.5 }) < loudness(Cue::Press));
        assert_ne!(
            tones::cue_samples(Cue::Hover { x: 0.0 }),
            tones::cue_samples(Cue::Hover { x: 1.0 })
        );
    }
}
