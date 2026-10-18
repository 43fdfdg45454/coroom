//! Mezcla de N flujos con ganancia por emisor y limitador de picos.
//!
//! Cada trama de salida se construye con [`Mixer::begin`], un [`Mixer::add`] por flujo (mono o
//! estéreo; el mono se reparte en los dos canales) y [`Mixer::finish`]. El acumulador va en
//! unidades de muestra `i16` en coma flotante (exacto para enteros: un flujo a ganancia 1 vuelve
//! idéntico) y vive en el mezclador: no asigna en caliente.
//!
//! **Transparente mientras quepa**: si la suma no pasa del fondo de escala, [`Mixer::finish`] la
//! escribe tal cual, muestra a muestra. Un solo flujo a ganancia 1 (el audio del juego en el
//! anfitrión o en el invitado, sin voces) sale bit a bit igual que entró, venga de PipeWire o de
//! WASAPI: el mezclador no cambia el nivel ni la dinámica de nada que ya fuera válido.
//!
//! **Limitador solo en los excesos**: si la suma se pasa del fondo de escala (varias fuentes
//! fuertes a la vez, una ganancia mayor que 1), una ganancia común a todos los canales (no mueve
//! la imagen estéreo) baja justo lo que hace falta para que el pico quepa. Baja con una rampa de
//! [`ATTACK_MS`] que empieza antes del pico dentro de la propia trama (la trama está entera en el
//! acumulador antes de escribirla: ver por delante no añade retardo) y vuelve a 1 con una rampa
//! de [`RELEASE_MS`]. Sin recorte duro: ninguna muestra se queda plana en el tope, la forma de
//! onda solo se escala.

use crate::{MAX_FRAME_SAMPLES, SAMPLE_RATE_HZ, SAMPLES_PER_CHANNEL};

/// Lo que tarda el limitador en bajar la ganancia de 1 a 0 (una reducción de 6 dB, la mitad).
pub const ATTACK_MS: u32 = 1;

/// Lo que tarda el limitador en volver de 0 a 1 (de una reducción de 6 dB, la mitad).
pub const RELEASE_MS: u32 = 100;

/// Emisores con ganancia propia como mucho.
pub const MAX_SOURCES: usize = 32;

/// Muestras por delante que mira el limitador: lo que dura su ataque.
const LOOKAHEAD: usize = (SAMPLE_RATE_HZ * ATTACK_MS / 1_000) as usize;

/// Cambio máximo de la ganancia del limitador por muestra al bajar.
const ATTACK_STEP: f32 = 1_000.0 / (SAMPLE_RATE_HZ as f32 * ATTACK_MS as f32);

/// Cambio máximo de la ganancia del limitador por muestra al volver.
const RELEASE_STEP: f32 = 1_000.0 / (SAMPLE_RATE_HZ as f32 * RELEASE_MS as f32);

/// Mezclador de tramas de 10 ms.
#[derive(Debug, Clone)]
pub struct Mixer {
    channels: usize,
    acc: [f32; MAX_FRAME_SAMPLES],
    gains: [(u32, f32); MAX_SOURCES],
    gains_len: usize,
    added: usize,
    master: f32,
    /// Ganancia del limitador al final de la trama anterior (1: no actúa).
    limiter: f32,
    /// Ganancia del limitador para cada instante de la trama (se reutiliza).
    envelope: [f32; SAMPLES_PER_CHANNEL],
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
            limiter: 1.0,
            envelope: [1.0; SAMPLES_PER_CHANNEL],
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

    /// Ganancia que aplica ahora el limitador (1 si no actúa).
    pub fn limiter_gain(&self) -> f32 {
        self.limiter
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
        let gain = self.gain(source);
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

    /// Escribe la mezcla en `out` (una trama de [`Mixer::channels`] canales): tal cual si cabe en
    /// el fondo de escala; si no, con el limitador.
    pub fn finish(&mut self, out: &mut [i16]) {
        let channels = self.channels;
        let len = self.frame_len();
        let master = self.master;
        // Lo que tendría que valer la ganancia en cada instante para que ningún canal se pase.
        let mut over = false;
        for (need, frame) in self
            .envelope
            .iter_mut()
            .zip(self.acc[..len].chunks_exact(channels))
        {
            *need = frame
                .iter()
                .map(|value| headroom(value * master))
                .fold(1.0, f32::min);
            over |= *need < 1.0;
        }
        if !over && self.limiter >= 1.0 {
            for (out, value) in out.iter_mut().zip(&self.acc[..len]) {
                *out = to_i16(value * master);
            }
            return;
        }
        // Por delante: la ganancia de cada instante es la menor de las de [`LOOKAHEAD`] muestras
        // por delante (dentro de la trama; lo de después aún no se conoce). Sin esto, delante de
        // cada pico la ganancia seguiría a la forma de onda muestra a muestra y la dejaría plana
        // en el tope: un recorte con otro nombre.
        for n in 0..SAMPLES_PER_CHANNEL {
            let end = (n + LOOKAHEAD).min(SAMPLES_PER_CHANNEL);
            self.envelope[n] = self.envelope[n..end].iter().copied().fold(1.0, f32::min);
        }
        // Hacia atrás: la ganancia empieza a bajar antes de cada exceso, a [`ATTACK_STEP`] por
        // muestra como mucho.
        let mut ahead = 1.0f32;
        for need in self.envelope.iter_mut().rev() {
            ahead = need.min(ahead + ATTACK_STEP);
            *need = ahead;
        }
        // Hacia delante: vuelve a 1 a [`RELEASE_STEP`] por muestra, sin pasar nunca de lo que
        // cabe. Si la trama empieza ya por encima de lo que cabe, baja de golpe en su primera
        // muestra (no hay nada antes que rampear): sigue sin haber recorte.
        let mut gain = self.limiter;
        for (need, (outs, values)) in self.envelope.iter().zip(
            out.chunks_mut(channels)
                .zip(self.acc[..len].chunks_exact(channels)),
        ) {
            gain = (gain + RELEASE_STEP).min(*need);
            for (out, value) in outs.iter_mut().zip(values) {
                *out = to_i16(value * master * gain);
            }
        }
        self.limiter = gain;
    }
}

/// Ganancia como mucho para que `value` (en unidades de `i16`) quepa: 1 si ya cabe.
fn headroom(value: f32) -> f32 {
    if value > f32::from(i16::MAX) {
        f32::from(i16::MAX) / value
    } else if value < f32::from(i16::MIN) {
        f32::from(i16::MIN) / value
    } else {
        1.0
    }
}

/// Redondea a `i16` (la saturación solo cubre el error de redondeo de la ganancia).
fn to_i16(value: f32) -> i16 {
    value
        .round()
        .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f64, amplitude: f64, start: usize) -> Vec<i16> {
        (start..start + SAMPLES_PER_CHANNEL)
            .map(|n| {
                ((2.0 * std::f64::consts::PI * freq * n as f64 / 48_000.0).sin()
                    * amplitude
                    * 32_767.0)
                    .round() as i16
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

    /// Muestras seguidas más largas pegadas al tope (la firma del recorte duro).
    fn longest_run_at_full_scale(samples: &[i16]) -> usize {
        let (mut longest, mut run) = (0, 0);
        for s in samples {
            if s.unsigned_abs() >= 32_767 {
                run += 1;
                longest = longest.max(run);
            } else {
                run = 0;
            }
        }
        longest
    }

    #[test]
    fn a_single_source_passes_bit_exact_even_at_full_scale() {
        // Mono: una senoidal a fondo de escala con los dos extremos de `i16`.
        let mut mixer = Mixer::new(1);
        let mut out = [0i16; SAMPLES_PER_CHANNEL];
        for frame in 0..50 {
            let mut input = tone(997.0, 1.0, frame * SAMPLES_PER_CHANNEL);
            input[0] = i16::MIN;
            input[1] = i16::MAX;
            mixer.begin();
            mixer.add(1, &input, 1);
            mixer.finish(&mut out);
            assert_eq!(out.as_slice(), input.as_slice(), "trama {frame}");
        }
        assert_eq!(mixer.limiter_gain(), 1.0, "el limitador no actuó");

        // Estéreo: ruido de todo el rango (generador congruencial: reproducible).
        let mut mixer = Mixer::new(2);
        let mut out = [0i16; MAX_FRAME_SAMPLES];
        let mut state = 0x2545_f491_u32;
        for frame in 0..50 {
            let input: Vec<i16> = (0..MAX_FRAME_SAMPLES)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 16) as u16 as i16
                })
                .collect();
            mixer.begin();
            mixer.add(1, &input, 2);
            mixer.finish(&mut out);
            assert_eq!(out.as_slice(), input.as_slice(), "trama {frame}");
        }
    }

    #[test]
    fn two_loud_sources_sum_without_hard_clipping() {
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
        let run = longest_run_at_full_scale(&all);
        println!("pico {peak}, racha más larga en el tope {run}");
        // La suma llega a 1,6 del fondo de escala: el limitador la mete justo hasta el tope (no
        // la aplasta por debajo) y sin mesetas en él.
        assert!(peak > 30_000, "la mezcla usa el fondo de escala: {peak}");
        assert!(run <= 2, "recorte duro: {run} muestras seguidas en el tope");
        assert!(mixer.limiter_gain() < 1.0, "el limitador actúa");
        // Los dos tonos están en la salida con el mismo peso.
        let (ma, mb) = (magnitude_at(&all, 400.0), magnitude_at(&all, 1000.0));
        assert!(ma > 5_000.0 && mb > 5_000.0, "{ma} {mb}");
        assert!((ma / mb - 1.0).abs() < 0.05, "{ma} {mb}");
    }

    #[test]
    fn the_limiter_lets_go_and_the_mix_is_exact_again() {
        let mut mixer = Mixer::new(2);
        let mut out = [0i16; MAX_FRAME_SAMPLES];
        let loud = [30_000i16; MAX_FRAME_SAMPLES];
        for _ in 0..5 {
            mixer.begin();
            mixer.add(1, &loud, 2);
            mixer.add(2, &loud, 2);
            mixer.finish(&mut out);
            // 60 000 de suma constante: el limitador la deja justo por debajo del tope.
            assert!(out.iter().all(|s| *s > 30_000), "{:?}", &out[..4]);
        }
        assert!(mixer.limiter_gain() < 0.6, "{}", mixer.limiter_gain());
        // Vuelve a 1 en [`RELEASE_MS`] como mucho; después, un solo flujo sale exacto.
        let quiet: Vec<i16> = tone(440.0, 0.5, 0).iter().flat_map(|s| [*s, -*s]).collect();
        let frames = RELEASE_MS / crate::FRAME_DURATION_MS + 1;
        let mut last = Vec::new();
        for _ in 0..frames {
            mixer.begin();
            mixer.add(1, &quiet, 2);
            mixer.finish(&mut out);
            // Durante la vuelta, nunca más alto que la entrada.
            assert!(
                out.iter()
                    .zip(&quiet)
                    .all(|(o, q)| o.unsigned_abs() <= q.unsigned_abs()),
                "la vuelta no sube por encima de la entrada"
            );
            last = out.to_vec();
        }
        assert_eq!(mixer.limiter_gain(), 1.0);
        assert_eq!(last, quiet, "otra vez transparente");
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
        assert!(out.iter().all(|s| *s == 500), "{:?}", &out[..4]);
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
        assert!(out.iter().all(|s| *s == 2000));
    }

    #[test]
    fn a_gain_above_one_is_limited_not_clipped() {
        let mut mixer = Mixer::new(1);
        mixer.set_gain(1, 2.0);
        let mut out = [0i16; SAMPLES_PER_CHANNEL];
        let mut all = Vec::new();
        for frame in 0..10 {
            mixer.begin();
            mixer.add(1, &tone(300.0, 0.9, frame * SAMPLES_PER_CHANNEL), 1);
            mixer.finish(&mut out);
            all.extend_from_slice(&out);
        }
        assert!(longest_run_at_full_scale(&all) <= 2);
        let peak = all.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        assert!(peak > 30_000, "{peak}");
    }
}
