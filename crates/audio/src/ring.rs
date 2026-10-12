//! Anillo de muestras entre el hilo de un backend nativo (el callback de PipeWire, el bucle de
//! eventos de WASAPI) y el hilo de audio de la sesión. Se reserva al abrir el flujo: ni el
//! callback ni el lector asignan después.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::{AudioError, AudioFormat, Captured, Played};

/// Capacidad del anillo: 500 ms.
const RING_MS: usize = 500;

/// Espera máxima de la reproducción antes de dar el flujo por perdido.
pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(2);

/// Sin audio en este tiempo, la captura entrega una trama de silencio (un *loopback* no da nada
/// mientras no suena nada; un dispositivo suspendido, tampoco).
pub(crate) const SILENCE_AFTER: Duration = Duration::from_millis(40);

/// Si la captura acumula más de esto, se descarta lo más antiguo (la latencia no crece).
const MAX_CAPTURE_FRAMES: usize = 6;

#[derive(Debug)]
pub(crate) struct Ring {
    pub(crate) samples: VecDeque<i16>,
    pub(crate) capacity: usize,
    /// Cuándo llegó (captura) o se encoló (reproducción) la muestra más reciente.
    pub(crate) last_push: Option<Instant>,
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
}

impl Shared {
    pub(crate) fn new(format: AudioFormat) -> Arc<Self> {
        let capacity = format.samples_per_frame() * RING_MS / crate::FRAME_DURATION_MS as usize;
        Arc::new(Self {
            ring: Mutex::new(Ring {
                samples: VecDeque::with_capacity(capacity),
                capacity,
                last_push: None,
                failure: None,
                overruns: 0,
                underruns: 0,
            }),
            cond: Condvar::new(),
            delay_us: AtomicU64::new(0),
        })
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Ring> {
        self.ring.lock().unwrap_or_else(PoisonError::into_inner)
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
    pub(crate) fn push(&self, samples: impl Iterator<Item = i16>) {
        let now = Instant::now();
        let mut ring = self.lock();
        for sample in samples {
            if ring.samples.len() >= ring.capacity {
                ring.samples.pop_front();
                ring.overruns += 1;
            }
            ring.samples.push_back(sample);
        }
        ring.last_push = Some(now);
        drop(ring);
        self.cond.notify_all();
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
                    return Ok(Captured {
                        samples: needed,
                        captured_at: Instant::now()
                            .checked_sub(crate::FRAME_DURATION)
                            .unwrap_or_else(Instant::now),
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
        // La muestra más antigua se capturó «llegada de la última − lo que hay en cola».
        let queued = format.duration_of(ring.samples.len());
        let captured_at = ring.last_push.map_or_else(Instant::now, |last| {
            last.checked_sub(queued).unwrap_or(last)
        });
        out.clear();
        out.extend(ring.samples.drain(..needed));
        Ok(Captured {
            samples: needed,
            captured_at,
            channels: format.channels,
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_flow_through_the_ring() {
        let shared = Shared::new(AudioFormat::VOICE);
        shared.push((0..1000).map(|i| i as i16));
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
        let played = shared
            .write_frame(AudioFormat::VOICE, &[7; 480], Duration::from_millis(10))
            .expect("encolar");
        assert!(played.played_at_estimate > Instant::now());
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
    }
}
