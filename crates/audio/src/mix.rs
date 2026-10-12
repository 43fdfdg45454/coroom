//! Mezcla de N flujos con ganancia por emisor y limitador suave.
//!
//! Cada trama de salida se construye con [`Mixer::begin`], un [`Mixer::add`] por flujo (mono o
//! estéreo; el mono se reparte en los dos canales) y [`Mixer::finish`], que pasa la suma por un
//! limitador suave: lineal hasta [`KNEE`] del fondo de escala y, por encima, una curva `tanh` que
//! se acerca al fondo de escala sin llegar nunca (sin *clipping* duro aunque se sumen varias voces
//! fuertes). El acumulador es de coma flotante y vive en el mezclador: no asigna en caliente.

use crate::{MAX_FRAME_SAMPLES, SAMPLES_PER_CHANNEL};

/// Proporción del fondo de escala a partir de la que actúa el limitador.
pub const KNEE: f32 = 0.7;

/// Emisores con ganancia propia como mucho.
pub const MAX_SOURCES: usize = 32;

/// Mezclador de tramas de 10 ms.
#[derive(Debug, Clone)]
pub struct Mixer {
    channels: usize,
    acc: [f32; MAX_FRAME_SAMPLES],
    gains: [(u32, f32); MAX_SOURCES],
    gains_len: usize,
    added: usize,
    master: f32,
}

impl Mixer {
    /// Mezclador con salida de `channels` canales (1 o 2).
    pub fn new(channels: u16) -> Self {
        Self {
            channels: usize::from(channels.clamp(1, 2)),
            acc: [0.0; MAX_FRAME_SAMPLES],
            gains: [(0, 1.0); MAX_SOURCES],
            gains_len: 0,
            added: 0,
            master: 1.0,
        }
    }

    /// Canales de salida.
    pub fn channels(&self) -> u16 {
        self.channels as u16
    }

    /// Ganancia lineal del emisor `source` (1 por defecto; 0 lo silencia).
    pub fn set_gain(&mut self, source: u32, gain: f32) {
        let gain = gain.max(0.0);
        if let Some(entry) = self.gains[..self.gains_len]
            .iter_mut()
            .find(|(id, _)| *id == source)
        {
            entry.1 = gain;
        } else if self.gains_len < MAX_SOURCES {
            self.gains[self.gains_len] = (source, gain);
            self.gains_len += 1;
        }
    }

    /// Ganancia del emisor `source`.
    pub fn gain(&self, source: u32) -> f32 {
        self.gains[..self.gains_len]
            .iter()
            .find(|(id, _)| *id == source)
            .map_or(1.0, |(_, gain)| *gain)
    }

    /// Ganancia general (después de la suma, antes del limitador).
    pub fn set_master(&mut self, gain: f32) {
        self.master = gain.max(0.0);
    }

    fn frame_len(&self) -> usize {
        SAMPLES_PER_CHANNEL * self.channels
    }

    /// Empieza una trama nueva.
    pub fn begin(&mut self) {
        let len = self.frame_len();
        self.acc[..len].fill(0.0);
        self.added = 0;
    }

    /// Flujos sumados en esta trama.
    pub fn added(&self) -> usize {
        self.added
    }

    /// Suma una trama de `source` con `channels` canales.
    pub fn add(&mut self, source: u32, samples: &[i16], channels: u16) {
        let gain = self.gain(source) / 32_768.0;
        if gain == 0.0 {
            return;
        }
        let out_channels = self.channels;
        let len = self.frame_len();
        match (channels, out_channels) {
            (1, 2) => {
                for (frame, sample) in self.acc[..len]
                    .as_chunks_mut::<2>()
                    .0
                    .iter_mut()
                    .zip(samples.iter().take(SAMPLES_PER_CHANNEL))
                {
                    let v = f32::from(*sample) * gain;
                    frame[0] += v;
                    frame[1] += v;
                }
            }
            (2, 1) => {
                for (out, frame) in self.acc[..len]
                    .iter_mut()
                    .zip(samples.as_chunks::<2>().0.iter().take(SAMPLES_PER_CHANNEL))
                {
                    *out += (f32::from(frame[0]) + f32::from(frame[1])) * 0.5 * gain;
                }
            }
            _ => {
                for (out, sample) in self.acc[..len].iter_mut().zip(samples) {
                    *out += f32::from(*sample) * gain;
                }
            }
        }
        self.added += 1;
    }

    /// Escribe la mezcla limitada en `out` (una trama de [`Mixer::channels`] canales).
    pub fn finish(&self, out: &mut [i16]) {
        let len = self.frame_len();
        for (out, value) in out.iter_mut().zip(&self.acc[..len]) {
            *out = (soft_limit(value * self.master) * 32_767.0).round() as i16;
        }
    }
}

/// Limitador suave: identidad hasta [`KNEE`], después `tanh` hacia ±1 (nunca lo alcanza).
pub fn soft_limit(x: f32) -> f32 {
    let magnitude = x.abs();
    if magnitude <= KNEE {
        return x;
    }
    let headroom = 1.0 - KNEE;
    let limited = KNEE + headroom * ((magnitude - KNEE) / headroom).tanh();
    limited.copysign(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f64, amplitude: f64, start: usize) -> Vec<i16> {
        (start..start + SAMPLES_PER_CHANNEL)
            .map(|n| {
                ((2.0 * std::f64::consts::PI * freq * n as f64 / 48_000.0).sin()
                    * amplitude
                    * 32_767.0) as i16
            })
            .collect()
    }

    fn magnitude_at(samples: &[i16], freq: f64) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        for (n, s) in samples.iter().enumerate() {
            let phase = 2.0 * std::f64::consts::PI * freq * n as f64 / 48_000.0;
            re += f64::from(*s) * phase.cos();
            im += f64::from(*s) * phase.sin();
        }
        (re * re + im * im).sqrt() / samples.len() as f64
    }

    #[test]
    fn two_loud_tones_mix_without_clipping_or_silence() {
        let mut mixer = Mixer::new(1);
        let mut out = [0i16; SAMPLES_PER_CHANNEL];
        let mut all = Vec::new();
        for frame in 0..20 {
            let a = tone(400.0, 0.8, frame * SAMPLES_PER_CHANNEL);
            let b = tone(1000.0, 0.8, frame * SAMPLES_PER_CHANNEL);
            mixer.begin();
            mixer.add(1, &a, 1);
            mixer.add(2, &b, 1);
            assert_eq!(mixer.added(), 2);
            mixer.finish(&mut out);
            all.extend_from_slice(&out);
        }
        let peak = all.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        let clipped = all.iter().filter(|s| s.unsigned_abs() >= 32_767).count();
        println!("pico {peak}, {clipped} muestras saturadas");
        assert!(peak < 32_767, "pico {peak}");
        assert!(peak > 20_000, "la mezcla no es silencio");
        // Los dos tonos están en la salida con un peso parecido.
        let (ma, mb) = (magnitude_at(&all, 400.0), magnitude_at(&all, 1000.0));
        assert!(ma > 3_000.0 && mb > 3_000.0, "{ma} {mb}");
        assert!((ma / mb - 1.0).abs() < 0.2, "{ma} {mb}");
    }

    #[test]
    fn gains_upmix_and_downmix() {
        let mut mixer = Mixer::new(2);
        let mono = [1000i16; SAMPLES_PER_CHANNEL];
        mixer.set_gain(7, 0.5);
        mixer.begin();
        mixer.add(7, &mono, 1);
        let mut out = [0i16; MAX_FRAME_SAMPLES];
        mixer.finish(&mut out);
        assert!(
            out.iter().all(|s| (i32::from(*s) - 500).abs() <= 1),
            "{:?}",
            &out[..4]
        );
        mixer.set_gain(7, 0.0);
        mixer.begin();
        mixer.add(7, &mono, 1);
        assert_eq!(mixer.added(), 0, "silenciado");
        assert_eq!(mixer.gain(9), 1.0);

        let mut mono_out = Mixer::new(1);
        let stereo = [2000i16; MAX_FRAME_SAMPLES];
        mono_out.begin();
        mono_out.add(1, &stereo, 2);
        let mut out = [0i16; SAMPLES_PER_CHANNEL];
        mono_out.finish(&mut out);
        assert!(out.iter().all(|s| (i32::from(*s) - 2000).abs() <= 1));
    }

    #[test]
    fn the_limiter_is_linear_below_the_knee_and_bounded_above() {
        assert_eq!(soft_limit(0.5), 0.5);
        assert_eq!(soft_limit(-0.7), -0.7);
        assert!(soft_limit(5.0) <= 1.0 && soft_limit(5.0) > 0.99);
        assert!(soft_limit(-1.2) > -1.0);
        // Monótono.
        let mut last = -2.0;
        for i in -300..=300 {
            let y = soft_limit(i as f32 / 100.0);
            assert!(y >= last);
            last = y;
        }
    }
}
