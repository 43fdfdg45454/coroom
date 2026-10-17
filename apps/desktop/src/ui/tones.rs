//! Los tonos propios de los avisos, sintetizados (sin ficheros con derechos de nadie).
//!
//! Una familia coherente: notas cortas de timbre de campana o de marimba (unos pocos parciales
//! con su caída exponencial), en la tesitura de sol₄ a sol₆, que se oye bien en altavoces de
//! portátil y no molesta con auriculares. Cada tono dice algo por su forma: dos notas que suben
//! (entra alguien), dos que bajan (sale alguien), tres campanas (te invitan), un toc-toc (piden
//! entrar), un acorde cálido (eres el anfitrión)…
//!
//! Todos se igualan en **sonoridad** ([`TARGET_LOUDNESS`], la sonoridad momentánea de
//! ITU-R BS.1770 con ventanas de 100 ms: lo que el oído compara) y ninguno pasa de [`PEAK`]; el
//! volumen de los avisos de Ajustes se aplica después. Empiezan y acaban en cero, con rampas de
//! unos milisegundos, para que no haya chasquidos.

use std::f64::consts::PI;

/// Frecuencia de muestreo de los tonos (la de toda la sala).
pub const RATE: u32 = 48_000;

/// Duración máxima de un tono.
pub const MAX_SECONDS: f64 = 0.6;

/// Sonoridad a la que se igualan (LUFS momentáneos, ventana de 100 ms).
pub const TARGET_LOUDNESS: f64 = -17.0;

/// Pico máximo (sobre 1.0, el fondo de escala): −6 dBFS, con margen para el volumen.
pub const PEAK: f64 = 0.5;

/// Rampa de entrada y de salida de todo el tono.
const EDGE_FADE: f64 = 0.004;

/// Tonos propios.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Builtin {
    /// Dos notas cortas que suben una cuarta (el tono del chat de siempre).
    Chime,
    /// Dos notas que suben una quinta.
    Rise,
    /// Dos notas que bajan una quinta.
    Fall,
    /// Tres campanas que suben.
    Bells,
    /// Dos golpes suaves, como llamar a la puerta.
    Knock,
    /// Cuatro notas rápidas que suben (un arpegio).
    Arpeggio,
    /// Un acorde mayor rasgueado, de ataque suave.
    Chord,
    /// Dos burbujas que suben de tono.
    Bubble,
    /// Una sola campana aguda.
    Ping,
}

impl Builtin {
    /// Todos, en el orden de los selectores.
    pub const ALL: [Builtin; 9] = [
        Builtin::Chime,
        Builtin::Rise,
        Builtin::Fall,
        Builtin::Bells,
        Builtin::Knock,
        Builtin::Arpeggio,
        Builtin::Chord,
        Builtin::Bubble,
        Builtin::Ping,
    ];

    /// Clave en `settings.toml`.
    pub const fn key(self) -> &'static str {
        match self {
            Builtin::Chime => "chime",
            Builtin::Rise => "rise",
            Builtin::Fall => "fall",
            Builtin::Bells => "bells",
            Builtin::Knock => "knock",
            Builtin::Arpeggio => "arpeggio",
            Builtin::Chord => "chord",
            Builtin::Bubble => "bubble",
            Builtin::Ping => "ping",
        }
    }

    /// El de clave `key`.
    pub fn from_key(key: &str) -> Option<Builtin> {
        Builtin::ALL.into_iter().find(|tone| tone.key() == key)
    }

    /// Las muestras (mono, [`RATE`], en `[-PEAK, PEAK]`), ya igualadas en sonoridad.
    pub fn samples(self) -> Vec<f32> {
        let raw = render(&self.voices());
        normalize(raw)
    }

    /// El tono como fichero WAV (PCM de 16 bits, mono, [`RATE`]).
    pub fn wav(self) -> Vec<u8> {
        wav_pcm16(RATE, 1, &to_i16(&self.samples(), 1.0))
    }

    /// Las notas del tono.
    fn voices(self) -> Vec<Voice> {
        match self {
            // Sol₅ y do₆, 90 ms cada una: la campanilla del chat de siempre.
            Builtin::Chime => vec![
                Voice::new(0.0, 783.99, 0.09, &CHIME)
                    .decay(0.045)
                    .attack(0.004),
                Voice::new(0.09, 1046.50, 0.09, &CHIME)
                    .decay(0.045)
                    .attack(0.004),
            ],
            // Mi₅ → si₅: alguien llega.
            Builtin::Rise => vec![
                Voice::new(0.0, 659.25, 0.30, &BELL).decay(0.12),
                Voice::new(0.12, 987.77, 0.30, &BELL).decay(0.16),
            ],
            // La₅ → re₅: alguien se va (la segunda, más larga y algo más suave).
            Builtin::Fall => vec![
                Voice::new(0.0, 880.00, 0.30, &BELL).decay(0.11),
                Voice::new(0.13, 587.33, 0.32, &BELL).decay(0.17).gain(0.85),
            ],
            // La₅, do♯₆, mi₆: tres campanas que suben, como un timbre.
            Builtin::Bells => vec![
                Voice::new(0.0, 880.00, 0.34, &BELL).decay(0.14),
                Voice::new(0.11, 1108.73, 0.34, &BELL).decay(0.14),
                Voice::new(0.22, 1318.51, 0.36, &BELL).decay(0.17),
            ],
            // Toc-toc: dos golpes de marimba en mi♭₅ con un poco de caída de tono, y el segundo
            // un pelo más fuerte (como quien llama).
            Builtin::Knock => vec![
                Voice::new(0.0, 622.25, 0.14, &MARIMBA)
                    .decay(0.05)
                    .attack(0.002)
                    .glide(0.96, 0.03),
                Voice::new(0.15, 622.25, 0.17, &MARIMBA)
                    .decay(0.06)
                    .attack(0.002)
                    .glide(0.96, 0.03)
                    .gain(1.15),
            ],
            // Do₅, mi₅, sol₅, do₆ cada 65 ms.
            Builtin::Arpeggio => vec![
                Voice::new(0.0, 523.25, 0.16, &MARIMBA).decay(0.07),
                Voice::new(0.065, 659.25, 0.16, &MARIMBA).decay(0.07),
                Voice::new(0.13, 783.99, 0.16, &MARIMBA).decay(0.07),
                Voice::new(0.195, 1046.50, 0.30, &MARIMBA).decay(0.12),
            ],
            // Sol mayor (sol₄, si₄, re₅, sol₅) rasgueado cada 25 ms, de ataque lento.
            Builtin::Chord => vec![
                Voice::new(0.0, 392.00, 0.52, &WARM)
                    .decay(0.28)
                    .attack(0.025),
                Voice::new(0.025, 493.88, 0.50, &WARM)
                    .decay(0.28)
                    .attack(0.025),
                Voice::new(0.05, 587.33, 0.48, &WARM)
                    .decay(0.28)
                    .attack(0.025),
                Voice::new(0.075, 783.99, 0.46, &WARM)
                    .decay(0.26)
                    .attack(0.025)
                    .gain(0.8),
            ],
            // Dos barridos de 500 a 1000 Hz.
            Builtin::Bubble => vec![
                Voice::new(0.0, 500.0, 0.09, &PURE)
                    .decay(0.05)
                    .attack(0.006)
                    .glide(2.0, 0.06),
                Voice::new(0.11, 600.0, 0.11, &PURE)
                    .decay(0.06)
                    .attack(0.006)
                    .glide(2.0, 0.07),
            ],
            // Mi₆, una vez.
            Builtin::Ping => vec![Voice::new(0.0, 1318.51, 0.32, &BELL).decay(0.13)],
        }
    }
}

/// Un parcial del timbre: múltiplo de la fundamental, amplitud relativa y cuánto dura su caída
/// respecto a la de la nota (los agudos se apagan antes, como en una campana real).
#[derive(Debug, Clone, Copy)]
struct Partial {
    ratio: f64,
    amplitude: f64,
    decay: f64,
}

const fn partial(ratio: f64, amplitude: f64, decay: f64) -> Partial {
    Partial {
        ratio,
        amplitude,
        decay,
    }
}

/// Campanilla del chat: fundamental y un segundo armónico suave.
const CHIME: [Partial; 2] = [partial(1.0, 1.0, 1.0), partial(2.0, 0.25, 1.0)];

/// Campana: armónicos y un parcial inarmónico (2,76) que la hace sonar a metal.
const BELL: [Partial; 4] = [
    partial(1.0, 1.0, 1.0),
    partial(2.0, 0.30, 0.6),
    partial(2.76, 0.18, 0.45),
    partial(5.40, 0.05, 0.25),
];

/// Marimba: fundamental y el cuarto armónico, que se apaga enseguida (el golpe).
const MARIMBA: [Partial; 3] = [
    partial(1.0, 1.0, 1.0),
    partial(3.93, 0.28, 0.25),
    partial(9.80, 0.05, 0.12),
];

/// Cálido: los tres primeros armónicos, suaves.
const WARM: [Partial; 3] = [
    partial(1.0, 1.0, 1.0),
    partial(2.0, 0.22, 0.7),
    partial(3.0, 0.08, 0.5),
];

/// Casi un seno.
const PURE: [Partial; 2] = [partial(1.0, 1.0, 1.0), partial(2.0, 0.08, 1.0)];

/// Una nota: cuándo empieza, su fundamental, cuánto dura y cómo suena.
#[derive(Debug, Clone)]
struct Voice {
    start: f64,
    frequency: f64,
    length: f64,
    timbre: &'static [Partial],
    attack: f64,
    decay: f64,
    gain: f64,
    /// Barrido de tono: la fundamental llega a `frequency * ratio` en `seconds`.
    glide: Option<(f64, f64)>,
}

impl Voice {
    fn new(start: f64, frequency: f64, length: f64, timbre: &'static [Partial]) -> Self {
        Self {
            start,
            frequency,
            length,
            timbre,
            attack: 0.005,
            decay: 0.1,
            gain: 1.0,
            glide: None,
        }
    }

    fn attack(mut self, seconds: f64) -> Self {
        self.attack = seconds;
        self
    }

    fn decay(mut self, seconds: f64) -> Self {
        self.decay = seconds;
        self
    }

    fn gain(mut self, gain: f64) -> Self {
        self.gain = gain;
        self
    }

    fn glide(mut self, ratio: f64, seconds: f64) -> Self {
        self.glide = Some((ratio, seconds));
        self
    }

    /// Suma la nota en `out` (mono, [`RATE`]).
    fn render_into(&self, out: &mut [f64]) {
        let rate = f64::from(RATE);
        let first = (self.start * rate).round() as usize;
        let count = (self.length * rate).round() as usize;
        // La cola de la nota baja a cero en los últimos 20 ms (coseno), sin cortar en seco.
        let release = 0.02f64.min(self.length / 3.0);
        let mut phase = 0.0f64;
        for index in 0..count {
            let Some(slot) = out.get_mut(first + index) else {
                break;
            };
            let t = index as f64 / rate;
            let frequency = match self.glide {
                Some((ratio, seconds)) => self.frequency * ratio.powf((t / seconds).min(1.0)),
                None => self.frequency,
            };
            let attack = if t < self.attack {
                0.5 - 0.5 * (PI * t / self.attack).cos()
            } else {
                1.0
            };
            let left = self.length - t;
            let tail = if left < release {
                0.5 - 0.5 * (PI * left / release).cos()
            } else {
                1.0
            };
            let after = (t - self.attack).max(0.0);
            let mut sample = 0.0;
            for part in self.timbre {
                if frequency * part.ratio >= rate * 0.45 {
                    continue;
                }
                let envelope = (-after / (self.decay * part.decay)).exp();
                sample += part.amplitude * envelope * (2.0 * PI * part.ratio * phase).sin();
            }
            *slot += self.gain * attack * tail * sample;
            phase += frequency / rate;
        }
    }
}

/// Suma las notas en un búfer del largo justo, con las rampas de entrada y salida.
fn render(voices: &[Voice]) -> Vec<f64> {
    let rate = f64::from(RATE);
    let seconds = voices
        .iter()
        .map(|voice| voice.start + voice.length)
        .fold(0.0, f64::max)
        .min(MAX_SECONDS);
    let mut out = vec![0.0; (seconds * rate).round() as usize];
    for voice in voices {
        voice.render_into(&mut out);
    }
    let fade = (EDGE_FADE * rate) as usize;
    let len = out.len();
    for index in 0..fade.min(len) {
        let ramp = 0.5 - 0.5 * (PI * index as f64 / fade as f64).cos();
        out[index] *= ramp;
        out[len - 1 - index] *= ramp;
    }
    if let Some(last) = out.last_mut() {
        *last = 0.0;
    }
    out
}

/// Iguala la sonoridad a [`TARGET_LOUDNESS`] sin pasar de [`PEAK`].
fn normalize(samples: Vec<f64>) -> Vec<f32> {
    let loudness = max_momentary_loudness(&samples);
    let mut gain = if loudness.is_finite() {
        10f64.powf((TARGET_LOUDNESS - loudness) / 20.0)
    } else {
        0.0
    };
    let peak = samples.iter().fold(0.0f64, |peak, s| peak.max(s.abs()));
    if peak * gain > PEAK {
        gain = PEAK / peak;
    }
    samples.iter().map(|s| (s * gain) as f32).collect()
}

/// Sonoridad momentánea máxima (LUFS) de una señal mono a [`RATE`]: el filtro K de
/// ITU-R BS.1770 y la energía media en ventanas de 100 ms, cada 10 ms. `-inf` si es silencio.
pub fn max_momentary_loudness(samples: &[f64]) -> f64 {
    let weighted = k_weight(samples);
    let window = (f64::from(RATE) * 0.1) as usize;
    let hop = (f64::from(RATE) * 0.01) as usize;
    let squares: Vec<f64> = weighted.iter().map(|s| s * s).collect();
    let mut best = 0.0f64;
    let mut start = 0;
    loop {
        let end = (start + window).min(squares.len());
        if end <= start {
            break;
        }
        let energy: f64 = squares[start..end].iter().sum::<f64>() / window as f64;
        best = best.max(energy);
        if end == squares.len() {
            break;
        }
        start += hop;
    }
    -0.691 + 10.0 * best.log10()
}

/// El filtro K (estante de agudos y paso alto) de ITU-R BS.1770 a 48 kHz.
fn k_weight(samples: &[f64]) -> Vec<f64> {
    let shelf = Biquad {
        b: [
            1.535_124_859_586_97,
            -2.691_696_189_406_38,
            1.198_392_810_852_85,
        ],
        a: [-1.690_659_293_182_41, 0.732_480_774_215_85],
    };
    let high_pass = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [-1.990_047_454_833_98, 0.990_072_250_366_21],
    };
    high_pass.run(&shelf.run(samples))
}

struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
}

impl Biquad {
    fn run(&self, input: &[f64]) -> Vec<f64> {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        input
            .iter()
            .map(|&x| {
                let y = self.b[0] * x + self.b[1] * x1 + self.b[2] * x2
                    - self.a[0] * y1
                    - self.a[1] * y2;
                (x2, x1, y2, y1) = (x1, x, y1, y);
                y
            })
            .collect()
    }
}

/// Muestras en coma flotante a PCM de 16 bits con `gain` (lo que pase del fondo de escala, al tope).
pub fn to_i16(samples: &[f32], gain: f32) -> Vec<i16> {
    samples
        .iter()
        .map(|s| (s * gain * 32_767.0).round().clamp(-32_768.0, 32_767.0) as i16)
        .collect()
}

/// Un WAV (RIFF) de PCM de 16 bits con las muestras dadas (intercaladas si hay varios canales).
pub fn wav_pcm16(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let data_len = u32::try_from(samples.len() * 2).unwrap_or(u32::MAX);
    let block = channels * 2;
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * u32::from(block)).to_le_bytes()); // bytes por segundo
    out.extend_from_slice(&block.to_le_bytes()); // bytes por muestra (todos los canales)
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le_u32(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
    }

    fn le_u16(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([bytes[at], bytes[at + 1]])
    }

    fn as_f64(samples: &[f32]) -> Vec<f64> {
        samples.iter().map(|s| f64::from(*s)).collect()
    }

    #[test]
    fn keys_round_trip() {
        for tone in Builtin::ALL {
            assert_eq!(Builtin::from_key(tone.key()), Some(tone));
        }
        assert_eq!(Builtin::from_key("gong"), None);
    }

    /// Cortos, se oyen sin saturar, sin continua y sin chasquidos al empezar ni al acabar.
    #[test]
    fn every_tone_is_short_soft_and_clean() {
        let rate = f64::from(RATE);
        let edge = (rate * 0.001) as usize;
        for tone in Builtin::ALL {
            let samples = tone.samples();
            let seconds = samples.len() as f64 / rate;
            assert!(
                (0.1..=MAX_SECONDS).contains(&seconds),
                "{tone:?}: {seconds} s"
            );
            assert!(samples.iter().all(|s| s.is_finite()), "{tone:?}");
            let peak = samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
            assert!(peak <= PEAK as f32 + 1e-6, "{tone:?} satura: {peak}");
            assert!(peak > 0.25, "{tone:?} apenas se oye: {peak}");
            let mean = samples.iter().map(|s| f64::from(*s)).sum::<f64>() / samples.len() as f64;
            assert!(mean.abs() < 1e-4, "{tone:?} con continua: {mean}");
            assert!(
                samples[0].abs() < 1e-6,
                "{tone:?} empieza en {}",
                samples[0]
            );
            assert_eq!(
                *samples.last().expect("muestras"),
                0.0,
                "{tone:?} acaba en cero"
            );
            // El primer y el último milisegundo, en rampa: nada de saltos.
            let head = samples[..edge].iter().fold(0.0f32, |m, s| m.max(s.abs()));
            let tail = samples[samples.len() - edge..]
                .iter()
                .fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(head < 0.02 && tail < 0.02, "{tone:?}: {head} {tail}");
            // Ningún salto entre muestras seguidas más allá de lo que da un seno de su tesitura.
            let jump = samples
                .windows(2)
                .map(|pair| (pair[1] - pair[0]).abs())
                .fold(0.0f32, f32::max);
            assert!(jump < 0.25, "{tone:?}: salto de {jump}");
        }
    }

    /// Todos igual de fuertes al oído (sonoridad BS.1770), dentro de medio LU.
    #[test]
    fn tones_are_equally_loud() {
        for tone in Builtin::ALL {
            let loudness = max_momentary_loudness(&as_f64(&tone.samples()));
            assert!(
                (loudness - TARGET_LOUDNESS).abs() < 0.5,
                "{tone:?}: {loudness:.2} LUFS"
            );
        }
        // La medida responde a la señal: la mitad de amplitud son 6 dB menos.
        let tone = as_f64(&Builtin::Rise.samples());
        let half: Vec<f64> = tone.iter().map(|s| s / 2.0).collect();
        let drop = max_momentary_loudness(&tone) - max_momentary_loudness(&half);
        assert!((drop - 6.02).abs() < 0.05, "{drop}");
        assert_eq!(max_momentary_loudness(&[0.0; 4800]), f64::NEG_INFINITY);
    }

    /// El espectro de cada tono a lo largo del tiempo (24 bandas de 300 Hz a 6 kHz cada 30 ms).
    fn spectrogram(samples: &[f32]) -> Vec<f64> {
        let frame = (f64::from(RATE) * 0.03) as usize;
        let frames = (MAX_SECONDS / 0.03).ceil() as usize;
        let bands: Vec<f64> = (0..24)
            .map(|band| 300.0 * 20f64.powf(f64::from(band) / 23.0))
            .collect();
        let mut out = Vec::with_capacity(frames * bands.len());
        for index in 0..frames {
            let start = index * frame;
            for frequency in &bands {
                let (mut re, mut im) = (0.0, 0.0);
                for n in 0..frame {
                    let x = samples.get(start + n).map_or(0.0, |s| f64::from(*s));
                    let window = 0.5 - 0.5 * (2.0 * PI * n as f64 / frame as f64).cos();
                    let phase = 2.0 * PI * frequency * n as f64 / f64::from(RATE);
                    re += x * window * phase.cos();
                    im -= x * window * phase.sin();
                }
                out.push((re * re + im * im).sqrt());
            }
        }
        out
    }

    /// Cada tono se distingue de los demás: sus espectrogramas se parecen poco (coseno < 0,8;
    /// los más parecidos andan por 0,5).
    #[test]
    fn tones_are_clearly_distinct() {
        let features: Vec<(Builtin, Vec<f64>)> = Builtin::ALL
            .iter()
            .map(|tone| (*tone, spectrogram(&tone.samples())))
            .collect();
        let norm = |v: &[f64]| v.iter().map(|x| x * x).sum::<f64>().sqrt();
        for (i, (a, fa)) in features.iter().enumerate() {
            for (b, fb) in &features[i + 1..] {
                let dot: f64 = fa.iter().zip(fb).map(|(x, y)| x * y).sum();
                let similarity = dot / (norm(fa) * norm(fb));
                assert!(
                    similarity < 0.8,
                    "{a:?} y {b:?} se parecen: {similarity:.3}"
                );
            }
        }
    }

    #[test]
    fn tones_are_valid_wavs() {
        for tone in Builtin::ALL {
            let wav = tone.wav();
            assert_eq!(&wav[0..4], b"RIFF");
            assert_eq!(le_u32(&wav, 4) as usize, wav.len() - 8);
            assert_eq!(&wav[8..16], b"WAVEfmt ");
            assert_eq!(le_u32(&wav, 16), 16, "fmt de PCM");
            assert_eq!(le_u16(&wav, 20), 1, "PCM");
            assert_eq!(le_u16(&wav, 22), 1, "mono");
            assert_eq!(le_u32(&wav, 24), RATE);
            assert_eq!(le_u32(&wav, 28), RATE * 2);
            assert_eq!(le_u16(&wav, 32), 2);
            assert_eq!(le_u16(&wav, 34), 16);
            assert_eq!(&wav[36..40], b"data");
            assert_eq!(le_u32(&wav, 40) as usize, tone.samples().len() * 2);
        }
        let stereo = wav_pcm16(RATE, 2, &[1, -1, 2, -2]);
        assert_eq!(le_u16(&stereo, 22), 2);
        assert_eq!(le_u32(&stereo, 28), RATE * 4);
        assert_eq!(le_u16(&stereo, 32), 4);
    }

    /// Con `COROOM_TONES_DIR`, deja cada tono en `<clave>.wav` para oírlos.
    #[test]
    fn tones_can_be_exported_for_listening() {
        let Some(dir) = std::env::var_os("COROOM_TONES_DIR") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).expect("directorio");
        for tone in Builtin::ALL {
            std::fs::write(dir.join(format!("{}.wav", tone.key())), tone.wav()).expect("escribir");
        }
    }
}
