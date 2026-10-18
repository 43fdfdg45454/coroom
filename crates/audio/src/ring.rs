//! Anillo de muestras entre el hilo de un backend nativo (el callback de PipeWire, el bucle de
//! eventos de WASAPI) y el hilo de audio de la sesión. Se reserva al abrir el flujo: ni el
//! callback ni el lector asignan después. También [`Pull`], la reproducción a demanda de esos
//! mismos hilos.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::{AudioError, AudioFormat, Captured, DeviceCounters, Played, Render, SAMPLE_RATE_HZ};

/// Capacidad del anillo: 500 ms.
const RING_MS: usize = 500;

/// Espera máxima de la reproducción antes de dar el flujo por perdido.
pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(2);

/// Sin audio en este tiempo, la captura entrega una trama de silencio (un *loopback* no da nada
/// mientras no suena nada; un dispositivo suspendido, tampoco).
pub(crate) const SILENCE_AFTER: Duration = Duration::from_millis(40);

/// Si la captura acumula más de esto, se descarta lo más antiguo (la latencia no crece): como
/// mucho dos tramas esperando detrás de la que se lee.
const MAX_CAPTURE_FRAMES: usize = 3;

#[derive(Debug)]
pub(crate) struct Ring {
    pub(crate) samples: VecDeque<i16>,
    pub(crate) capacity: usize,
    /// Captura: instante (reloj monótono local) que sigue a la muestra más reciente, es decir,
    /// cuándo se capturó el final de lo último que llegó, con la marca del dispositivo si la dio
    /// o, si no, la llegada. Reproducción: cuándo se encoló lo último.
    pub(crate) last_push: Option<Instant>,
    /// Captura: muestras intercaladas de lo último que llegó de una vez (el cuanto del flujo).
    pub(crate) last_chunk: usize,
    pub(crate) failure: Option<AudioError>,
    pub(crate) overruns: u64,
    pub(crate) underruns: u64,
}

#[derive(Debug)]
pub(crate) struct Shared {
    ring: Mutex<Ring>,
    pub(crate) cond: Condvar,
    /// Retardo del dispositivo hasta el altavoz, en µs (reproducción; 0 si no se conoce).
    pub(crate) delay_us: AtomicU64,
    /// Captura: a quién más avisar de cada llegada (la mezcla por aplicación espera a la primera
    /// de varias capturas).
    arrivals: OnceLock<Arc<Arrivals>>,
}

/// Aviso de llegada compartido por varias capturas: quien espera a cualquiera de ellas (la mezcla
/// por aplicación, [`crate::apps::AppMixSource`]) duerme en él en vez de en un reloj propio.
#[derive(Debug, Default)]
pub(crate) struct Arrivals {
    generation: Mutex<u64>,
    cond: Condvar,
}

impl Arrivals {
    /// Llegadas hasta ahora: se lee antes de mirar los anillos y se pasa a [`Arrivals::wait`], así
    /// no se pierde una que llegue entre medias.
    pub(crate) fn generation(&self) -> u64 {
        *self
            .generation
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn notify(&self) {
        let mut generation = self
            .generation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *generation = generation.wrapping_add(1);
        drop(generation);
        self.cond.notify_all();
    }

    /// Espera a que llegue algo después de `seen` o, como mucho, hasta `deadline`.
    pub(crate) fn wait(&self, seen: u64, deadline: Instant) {
        let mut generation = self
            .generation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while *generation == seen {
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            generation = self
                .cond
                .wait_timeout(generation, deadline - now)
                .map(|(guard, _)| guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner().0);
        }
    }
}

impl Shared {
    pub(crate) fn new(format: AudioFormat) -> Arc<Self> {
        let capacity = format.samples_per_frame() * RING_MS / crate::FRAME_DURATION_MS as usize;
        Arc::new(Self {
            ring: Mutex::new(Ring {
                samples: VecDeque::with_capacity(capacity),
                capacity,
                last_push: None,
                last_chunk: 0,
                failure: None,
                overruns: 0,
                underruns: 0,
            }),
            cond: Condvar::new(),
            delay_us: AtomicU64::new(0),
            arrivals: OnceLock::new(),
        })
    }

    /// Captura: avisa también a `arrivals` de cada llegada (una vez; las siguientes no cambian
    /// nada).
    pub(crate) fn notify_arrivals_to(&self, arrivals: Arc<Arrivals>) {
        let _ = self.arrivals.set(arrivals);
    }

    /// Captura: si hay una trama entera de `format` esperando.
    pub(crate) fn has_frame(&self, format: AudioFormat) -> bool {
        let ring = self.lock();
        ring.failure.is_none() && ring.samples.len() >= format.samples_per_frame()
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Ring> {
        self.ring.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Veces que faltó o sobró audio.
    pub(crate) fn counters(&self) -> DeviceCounters {
        let ring = self.lock();
        DeviceCounters {
            underruns: ring.underruns,
            overruns: ring.overruns,
        }
    }

    /// El dispositivo se quedó sin audio (lo detecta el backend, a demanda o con la cola).
    #[cfg_attr(not(all(target_os = "windows", feature = "wasapi")), allow(dead_code))]
    pub(crate) fn note_underrun(&self) {
        self.lock().underruns += 1;
    }

    /// El error del flujo, si se detuvo.
    pub(crate) fn failure(&self) -> Option<AudioError> {
        self.lock().failure.clone()
    }

    pub(crate) fn fail(&self, err: AudioError) {
        let mut ring = self.lock();
        if ring.failure.is_none() {
            ring.failure = Some(err);
        }
        drop(ring);
        self.cond.notify_all();
    }

    /// Captura: añade muestras llegadas ahora (si no caben, se pierden las más antiguas).
    /// `captured_at` es cuándo se capturó la **primera** de ellas según el dispositivo (la marca
    /// de PipeWire o de WASAPI para ese búfer); sin ella (`None`), se toma la llegada como el
    /// final de lo capturado.
    pub(crate) fn push(
        &self,
        format: AudioFormat,
        samples: impl ExactSizeIterator<Item = i16>,
        captured_at: Option<Instant>,
    ) {
        let len = samples.len();
        let end = match captured_at {
            Some(first) => first + format.duration_of(len),
            None => Instant::now(),
        };
        let mut ring = self.lock();
        for sample in samples {
            if ring.samples.len() >= ring.capacity {
                ring.samples.pop_front();
                ring.overruns += 1;
            }
            ring.samples.push_back(sample);
        }
        ring.last_push = Some(end);
        ring.last_chunk = len;
        drop(ring);
        self.cond.notify_all();
        if let Some(arrivals) = self.arrivals.get() {
            arrivals.notify();
        }
    }

    /// Captura: el dispositivo dice que se perdieron `frames` tramas (por canal) justo antes de lo
    /// siguiente que llegue (WASAPI: `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`, con el hueco medido
    /// por la posición del dispositivo de cada paquete). Se rellena con silencio, como mucho la
    /// capacidad del anillo, para que lo que hay en cola siga siendo seguido: sin él, la hora de
    /// captura de lo anterior al hueco ([`Shared::head_captured_at`]) saldría corrida lo que dura
    /// el hueco. No cambia la hora de la última llegada: la pone el paquete que viene detrás.
    #[cfg_attr(not(all(target_os = "windows", feature = "wasapi")), allow(dead_code))]
    pub(crate) fn push_gap(&self, format: AudioFormat, frames: u64) {
        let mut ring = self.lock();
        let samples = usize::try_from(frames)
            .unwrap_or(usize::MAX)
            .saturating_mul(usize::from(format.channels.max(1)))
            .min(ring.capacity);
        for _ in 0..samples {
            if ring.samples.len() >= ring.capacity {
                ring.samples.pop_front();
                ring.overruns += 1;
            }
            ring.samples.push_back(0);
        }
    }

    /// Cuándo se capturó la muestra más antigua de la cola: el final de lo último que llegó
    /// menos lo que hay en cola (las muestras de un flujo de captura son seguidas).
    fn head_captured_at(ring: &Ring, format: AudioFormat) -> Option<Instant> {
        let last = ring.last_push?;
        Some(
            last.checked_sub(format.duration_of(ring.samples.len()))
                .unwrap_or(last),
        )
    }

    /// Reproducción: saca `out.len()` muestras (silencio si faltan) para el dispositivo.
    pub(crate) fn pop_into(&self, out: &mut [i16]) {
        let mut ring = self.lock();
        if ring.samples.len() < out.len() && ring.last_push.is_some() {
            ring.underruns += 1;
        }
        for sample in out.iter_mut() {
            *sample = ring.samples.pop_front().unwrap_or(0);
        }
        drop(ring);
        self.cond.notify_all();
    }

    fn wait<'a>(
        &self,
        ring: MutexGuard<'a, Ring>,
        deadline: Instant,
    ) -> Result<MutexGuard<'a, Ring>, AudioError> {
        let now = Instant::now();
        if now >= deadline {
            return Err(AudioError::DeviceLost);
        }
        Ok(self
            .cond
            .wait_timeout(ring, deadline - now)
            .map(|(guard, _)| guard)
            .unwrap_or_else(|poisoned| poisoned.into_inner().0))
    }

    /// Captura: espera una trama de `format` y la deja en `out` (silencio si no llega nada en
    /// [`SILENCE_AFTER`]).
    pub(crate) fn read_frame(
        &self,
        format: AudioFormat,
        out: &mut Vec<i16>,
    ) -> Result<Captured, AudioError> {
        let needed = format.samples_per_frame();
        let deadline = Instant::now() + SILENCE_AFTER;
        let mut ring = self.lock();
        loop {
            if let Some(err) = &ring.failure {
                return Err(err.clone());
            }
            if ring.samples.len() >= needed {
                break;
            }
            match self.wait(ring, deadline) {
                Ok(guard) => ring = guard,
                Err(_) => {
                    let mut ring = self.lock();
                    ring.underruns += 1;
                    drop(ring);
                    out.clear();
                    out.resize(needed, 0);
                    // No se capturó nada: el silencio lleva la hora en que se decide.
                    return Ok(Captured {
                        samples: needed,
                        captured_at: Instant::now(),
                        channels: format.channels,
                    });
                }
            }
        }
        let excess = ring
            .samples
            .len()
            .saturating_sub(needed * MAX_CAPTURE_FRAMES);
        if excess > 0 {
            let drop_to = excess + needed * (MAX_CAPTURE_FRAMES - 2);
            let keep = ring.samples.len() - needed;
            ring.samples.drain(..drop_to.min(keep));
            ring.overruns += 1;
        }
        let captured_at = Self::head_captured_at(&ring, format).unwrap_or_else(Instant::now);
        out.clear();
        out.extend(ring.samples.drain(..needed));
        Ok(Captured {
            samples: needed,
            captured_at,
            channels: format.channels,
        })
    }

    /// Captura sin esperar: si hay una trama de `format`, la deja en `out` y devuelve cuándo se
    /// capturó su primera muestra. Lo que quede en cola detrás no pasa de una trama (10 ms) o,
    /// si el flujo entrega cuantos mayores, de lo que trajo la última llegada: lo más antiguo se
    /// descarta. Así una mezcla con su propio reloj ([`crate::apps::AppMixSource`]) no acumula
    /// cola oculta.
    pub(crate) fn try_take(&self, format: AudioFormat, out: &mut Vec<i16>) -> Option<Instant> {
        let needed = format.samples_per_frame();
        let mut ring = self.lock();
        if ring.failure.is_some() || ring.samples.len() < needed {
            return None;
        }
        let keep = needed + needed.max(ring.last_chunk);
        let excess = ring.samples.len().saturating_sub(keep);
        if excess > 0 {
            ring.samples.drain(..excess);
            ring.overruns += 1;
        }
        let captured_at = Self::head_captured_at(&ring, format);
        out.clear();
        out.extend(ring.samples.drain(..needed));
        captured_at
    }

    /// Reproducción: espera a que en cola quede como mucho una trama (de las dos que se admiten).
    pub(crate) fn wait_ready(&self, format: AudioFormat) -> Result<(), AudioError> {
        let frame = format.samples_per_frame();
        let deadline = Instant::now() + IO_TIMEOUT;
        let mut ring = self.lock();
        loop {
            if let Some(err) = &ring.failure {
                return Err(err.clone());
            }
            if ring.samples.len() <= frame {
                return Ok(());
            }
            ring = self.wait(ring, deadline)?;
        }
    }

    /// Reproducción: encola `samples` cuando quedan como mucho dos tramas en cola y estima
    /// cuándo sonarán (`latency` si el dispositivo no dice su retardo).
    pub(crate) fn write_frame(
        &self,
        format: AudioFormat,
        samples: &[i16],
        latency: Duration,
    ) -> Result<Played, AudioError> {
        let limit = (format.samples_per_frame() * 2).max(samples.len());
        let deadline = Instant::now() + IO_TIMEOUT;
        let mut ring = self.lock();
        loop {
            if let Some(err) = &ring.failure {
                return Err(err.clone());
            }
            if ring.samples.len() + samples.len() <= limit {
                break;
            }
            ring = self.wait(ring, deadline)?;
        }
        let queued = format.duration_of(ring.samples.len());
        let room = ring.capacity.saturating_sub(ring.samples.len());
        if samples.len() > room {
            ring.overruns += 1;
        }
        ring.samples.extend(samples.iter().take(room));
        ring.last_push = Some(Instant::now());
        drop(ring);
        let device = Duration::from_micros(self.delay_us.load(Ordering::Relaxed));
        let delay = if device.is_zero() { latency } else { device };
        Ok(Played {
            played_at_estimate: Instant::now() + queued + delay,
        })
    }
}

/// Reproducción a demanda: lo que pide cada trama y lo que sobró de la última, si el dispositivo
/// pide cuantos que no son de 10 ms.
pub(crate) struct Pull {
    render: Render,
    frame: [i16; crate::MAX_FRAME_SAMPLES],
    len: usize,
    pos: usize,
}

impl Pull {
    pub(crate) fn new(render: Render) -> Self {
        Self {
            render,
            frame: [0; crate::MAX_FRAME_SAMPLES],
            len: 0,
            pos: 0,
        }
    }

    /// Llena `out` (intercalado, `channels` canales) pidiendo tramas nuevas cuando se acaba la
    /// anterior. La primera muestra de `out` sonará dentro de `delay`.
    pub(crate) fn fill(&mut self, out: &mut [i16], channels: usize, delay: Duration) {
        let frame_len = crate::SAMPLES_PER_CHANNEL * channels;
        let now = Instant::now();
        let mut written = 0;
        while written < out.len() {
            if self.pos == self.len {
                let ahead = Duration::from_micros(
                    (written / channels) as u64 * 1_000_000 / u64::from(SAMPLE_RATE_HZ),
                );
                let frame = &mut self.frame[..frame_len];
                frame.fill(0);
                (self.render)(frame, now + delay + ahead);
                (self.len, self.pos) = (frame_len, 0);
            }
            let n = (self.len - self.pos).min(out.len() - written);
            out[written..written + n].copy_from_slice(&self.frame[self.pos..self.pos + n]);
            self.pos += n;
            written += n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_gap_keeps_the_capture_times_in_line() {
        let format = AudioFormat::VOICE;
        let shared = Shared::new(format);
        let t0 = Instant::now();
        shared.push(format, std::iter::repeat_n(5, 480), Some(t0));
        // Se perdieron 10 ms en el dispositivo: el siguiente paquete es de t0 + 20 ms.
        shared.push_gap(format, 480);
        shared.push(
            format,
            std::iter::repeat_n(9, 480),
            Some(t0 + Duration::from_millis(20)),
        );
        let mut out = Vec::new();
        let first = shared.read_frame(format, &mut out).expect("trama");
        assert_eq!(
            first.captured_at, t0,
            "lo de antes del hueco conserva su hora"
        );
        assert!(out.iter().all(|s| *s == 5));
        let gap = shared.read_frame(format, &mut out).expect("hueco");
        assert_eq!(gap.captured_at, t0 + Duration::from_millis(10));
        assert!(out.iter().all(|s| *s == 0), "el hueco es silencio");
        let after = shared.read_frame(format, &mut out).expect("trama");
        assert_eq!(after.captured_at, t0 + Duration::from_millis(20));
        assert!(out.iter().all(|s| *s == 9));
        // Un hueco enorme no pasa de la capacidad del anillo.
        shared.push_gap(format, u64::MAX);
        let ring = shared.lock();
        assert_eq!(ring.samples.len(), ring.capacity);
    }

    #[test]
    fn arrivals_wake_whoever_waits_for_any_capture() {
        let format = AudioFormat::GAME;
        let arrivals = Arc::new(Arrivals::default());
        let shared = Shared::new(format);
        shared.notify_arrivals_to(Arc::clone(&arrivals));
        let seen = arrivals.generation();
        assert!(!shared.has_frame(format));
        let pusher = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                shared.push(
                    format,
                    std::iter::repeat_n(1, format.samples_per_frame()),
                    None,
                );
            })
        };
        // Funcional: despierta por la llegada (el plazo es holgado), no por el plazo.
        arrivals.wait(seen, Instant::now() + Duration::from_secs(10));
        pusher.join().expect("hilo");
        assert_ne!(arrivals.generation(), seen);
        assert!(shared.has_frame(format));
        // Sin llegadas, vuelve en el plazo.
        let seen = arrivals.generation();
        let deadline = Instant::now() + Duration::from_millis(5);
        arrivals.wait(seen, deadline);
        assert!(Instant::now() >= deadline);
    }

    #[test]
    fn frames_flow_through_the_ring() {
        let shared = Shared::new(AudioFormat::VOICE);
        shared.push(AudioFormat::VOICE, (0..1000).map(|i| i as i16), None);
        let mut out = Vec::new();
        let captured = shared
            .read_frame(AudioFormat::VOICE, &mut out)
            .expect("trama");
        assert_eq!(captured.samples, 480);
        assert_eq!(out[0], 0);
        assert_eq!(out[479], 479);
        let mut device = [0i16; 600];
        shared.pop_into(&mut device);
        assert_eq!((device[0], device[519], device[520]), (480, 999, 0));
        let before = Instant::now();
        let played = shared
            .write_frame(AudioFormat::VOICE, &[7; 480], Duration::from_millis(10))
            .expect("encolar");
        // Funcional: suena como pronto una latencia después de escribirla.
        assert!(played.played_at_estimate >= before + Duration::from_millis(10));
        coroom_testkit::timing_assert!(
            played.played_at_estimate > Instant::now(),
            "la hora estimada ya pasó al volver de escribir: el hilo se quedó parado más de 10 ms"
        );
        shared.pop_into(&mut device[..480]);
        assert!(device[..480].iter().all(|s| *s == 7));
        // Sin nada que leer: silencio en 40 ms.
        let silent = shared
            .read_frame(AudioFormat::VOICE, &mut out)
            .expect("silencio");
        assert_eq!(silent.samples, 480);
        assert!(out.iter().all(|s| *s == 0));
        shared.fail(AudioError::DeviceLost);
        assert_eq!(
            shared.read_frame(AudioFormat::VOICE, &mut out),
            Err(AudioError::DeviceLost)
        );
        // El silencio por falta de captura y el hueco del dispositivo cuentan como faltas.
        assert_eq!(
            shared.counters(),
            DeviceCounters {
                underruns: 2,
                overruns: 0
            }
        );
        assert_eq!(shared.failure(), Some(AudioError::DeviceLost));
    }

    #[test]
    fn overruns_and_underruns_are_counted() {
        let shared = Shared::new(AudioFormat::VOICE);
        // 500 ms de capacidad: lo que no cabe desplaza a lo más antiguo.
        shared.push(
            AudioFormat::VOICE,
            std::iter::repeat_n(1, 24_000 + 10),
            None,
        );
        assert_eq!(shared.counters().overruns, 10);
        shared.note_underrun();
        assert_eq!(shared.counters().underruns, 1);
    }

    #[test]
    fn capture_times_come_from_the_device_not_the_arrival() {
        let shared = Shared::new(AudioFormat::VOICE);
        // El dispositivo dice que el primer búfer (5 ms) se capturó hace 50 ms; llega otro
        // seguido (los flujos de captura son continuos).
        let device = Instant::now() - Duration::from_millis(50);
        shared.push(
            AudioFormat::VOICE,
            std::iter::repeat_n(1, 240),
            Some(device),
        );
        shared.push(
            AudioFormat::VOICE,
            std::iter::repeat_n(2, 240),
            Some(device + Duration::from_millis(5)),
        );
        let mut out = Vec::new();
        let frame = shared
            .read_frame(AudioFormat::VOICE, &mut out)
            .expect("trama");
        assert_eq!(
            frame.captured_at, device,
            "la marca del dispositivo, no la llegada"
        );
        // Sin marca: el final de lo que llega es la llegada.
        let before = Instant::now();
        shared.push(AudioFormat::VOICE, std::iter::repeat_n(3, 480), None);
        let frame = shared
            .read_frame(AudioFormat::VOICE, &mut out)
            .expect("trama");
        assert!(frame.captured_at + Duration::from_millis(10) >= before);
        assert!(frame.captured_at + Duration::from_millis(10) <= Instant::now());
    }

    #[test]
    fn taking_without_waiting_leaves_at_most_one_frame_behind() {
        let format = AudioFormat::VOICE;
        let shared = Shared::new(format);
        let start = Instant::now() - Duration::from_millis(100);
        // Cinco tramas de 10 ms seguidas, de una en una (el cuanto del flujo es una trama).
        for frame in 0..5u32 {
            shared.push(
                format,
                std::iter::repeat_n(frame as i16, 480),
                Some(start + FRAME * frame),
            );
        }
        let mut out = Vec::new();
        let captured = shared.try_take(format, &mut out).expect("trama");
        // Se descartan las tres más antiguas: suena la cuarta y queda solo la quinta (10 ms).
        assert_eq!(out[0], 3);
        assert_eq!(captured, start + FRAME * 3, "con su hora real de captura");
        assert_eq!(shared.lock().samples.len(), 480);
        assert_eq!(shared.counters().overruns, 1);
        assert!(shared.try_take(format, &mut out).is_some());
        assert!(
            shared.try_take(format, &mut out).is_none(),
            "sin nada, no espera"
        );
        // Con cuantos de 20 ms la cola que se deja es la de un cuanto, no menos (no se tira
        // audio que el flujo entrega de dos en dos).
        shared.push(format, std::iter::repeat_n(7, 960), Some(start));
        shared.push(format, std::iter::repeat_n(8, 960), Some(start + FRAME * 2));
        assert!(shared.try_take(format, &mut out).is_some());
        assert_eq!(out[0], 7);
        assert_eq!(shared.lock().samples.len(), 960);
    }

    const FRAME: Duration = crate::FRAME_DURATION;

    #[test]
    fn on_demand_playback_splits_frames_across_small_quanta() {
        // El grafo pide cuantos de 256 muestras por canal (5,3 ms): cada trama de 480 se pide una
        // vez, entera, y lo que sobra va al cuanto siguiente sin perder ni repetir muestras.
        let calls: Arc<Mutex<Vec<Instant>>> = Arc::default();
        let seen = Arc::clone(&calls);
        let mut next = 0i16;
        let mut pull = Pull::new(Box::new(move |out: &mut [i16], played_at: Instant| {
            seen.lock().unwrap().push(played_at);
            for sample in out.as_chunks_mut::<2>().0 {
                sample.fill(next);
                next = next.wrapping_add(1);
            }
        }));
        let delay = Duration::from_millis(10);
        let before = Instant::now();
        let mut heard = Vec::new();
        for _ in 0..15 {
            let mut quantum = [0i16; 256 * 2];
            pull.fill(&mut quantum, 2, delay);
            heard.extend(quantum.as_chunks::<2>().0.iter().map(|s| s[0]));
        }
        let expected: Vec<i16> = (0..15 * 256).map(|i| i as i16).collect();
        assert_eq!(heard, expected);
        let calls = calls.lock().unwrap();
        // 15 × 256 = 3840 muestras = 8 tramas exactas.
        assert_eq!(calls.len(), 8);
        // Una trama pedida a mitad de cuanto suena después de lo que ya se escribió en él.
        let first = calls[0];
        assert!(first >= before + delay, "{calls:?}");
        assert!(calls[1] > first, "{calls:?}");
    }

    #[test]
    fn pulling_odd_quanta_asks_for_whole_frames_in_order() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&calls);
        let mut next = 0i16;
        let mut pull = Pull::new(Box::new(move |frame: &mut [i16], at: Instant| {
            frame.fill(next);
            next += 1;
            seen.lock().expect("lista").push(at);
        }));
        let mut out = [0i16; 512];
        pull.fill(&mut out, 1, Duration::from_millis(5));
        assert_eq!((out[479], out[480], out[511]), (0, 1, 1));
        pull.fill(&mut out[..448], 1, Duration::ZERO);
        assert_eq!((out[0], out[447]), (1, 1), "sigue con lo que sobró");
        pull.fill(&mut out[..1], 1, Duration::ZERO);
        assert_eq!(out[0], 2);
        let calls = calls.lock().expect("lista");
        assert_eq!(calls.len(), 3);
        // La segunda trama de la primera llamada suena 10 ms después que la primera.
        assert_eq!(calls[1] - calls[0], Duration::from_millis(10));
    }
}
