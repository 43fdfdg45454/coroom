//! Cuánto escribir en el búfer de un dispositivo de reproducción por eventos (WASAPI en modo
//! compartido) cada vez que avisa.
//!
//! El motor de audio consume un **periodo** del búfer en cada pasada (10 ms casi siempre, pero
//! 10,16 ms con una mezcla a 44,1 kHz convertida a 48 kHz, 512 tramas en algunos controladores, 20 ms
//! en USB o Bluetooth) y avisa con un evento. Escribir «una trama de 10 ms por evento» deja el búfer
//! cada vez más corto cuando el periodo no es exactamente 480 tramas, y un evento perdido (o dos
//! fundidos en uno por un despertar tardío) lo vacía: el motor reproduce silencio a trozos y el audio
//! suena robótico. [`Pacing`] rellena en cada evento hasta un **nivel objetivo** que aguanta un
//! evento perdido: `max(2 periodos, 20 ms)` más un margen de [`MARGIN_FRAMES`], sin pasar del
//! búfer. Es una función pura para poder probarla contra un dispositivo simulado.

use crate::SAMPLE_RATE_HZ;

/// Nivel mínimo del búfer (20 ms a 48 kHz), aunque el periodo sea corto.
pub const MIN_FILL_FRAMES: usize = (SAMPLE_RATE_HZ / 50) as usize;

/// Margen sobre el nivel objetivo (2 ms): absorbe las tramas de más o de menos de cada pasada (la
/// conversión de frecuencia no consume siempre lo mismo) cuando además se pierde un evento.
pub const MARGIN_FRAMES: usize = (SAMPLE_RATE_HZ / 500) as usize;

/// Tramas (por canal) a 48 kHz de una duración en unidades de 100 ns, redondeando hacia arriba.
pub fn hns_to_frames(hns: i64) -> usize {
    let hns = u64::try_from(hns).unwrap_or(0);
    (hns * u64::from(SAMPLE_RATE_HZ)).div_ceil(10_000_000) as usize
}

/// Qué escribir en cada evento de un dispositivo de reproducción.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pacing {
    /// Tamaño del búfer del dispositivo, en tramas.
    pub buffer: usize,
    /// Periodo del motor (lo que consume en cada pasada), en tramas.
    pub period: usize,
    /// Nivel al que se rellena el búfer en cada evento, en tramas.
    pub target: usize,
}

impl Pacing {
    /// Para un búfer de `buffer` tramas y un periodo de `period` (0 si no se conoce: 10 ms).
    pub fn new(buffer: usize, period: usize) -> Self {
        let period = if period == 0 {
            crate::SAMPLES_PER_CHANNEL
        } else {
            period
        };
        let target = (2 * period).max(MIN_FILL_FRAMES) + MARGIN_FRAMES;
        Self {
            buffer,
            period,
            target: target.min(buffer),
        }
    }

    /// Tramas de silencio que se escriben antes de arrancar el flujo (un periodo): el motor tiene
    /// algo que leer en su primera pasada y el primer evento ya rellena hasta el objetivo.
    pub fn prefill(&self) -> usize {
        self.period.min(self.buffer)
    }

    /// Tramas que escribir ahora con `padding` tramas aún en el búfer (`GetCurrentPadding`): lo
    /// que falta hasta el objetivo, nunca más del sitio libre.
    pub fn frames_to_write(&self, padding: usize) -> usize {
        self.target
            .saturating_sub(padding)
            .min(self.buffer.saturating_sub(padding))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dispositivo simulado: cada pasada consume `rate` tramas (con parte fraccionaria acumulada,
    /// como la conversión 44,1 → 48 kHz) ±1 trama de baile; tras cada pasada avisa, salvo en la
    /// pasada `missed`, cuyo evento se pierde (el siguiente despertar ve dos pasadas). `decide`
    /// dice cuánto escribir con el relleno actual. Devuelve las pasadas en que faltó audio tras el
    /// calentamiento, el mayor relleno visto y lo escrito y consumido en total.
    fn simulate(
        rate: f64,
        buffer: usize,
        prefill: usize,
        passes: usize,
        missed: Option<usize>,
        mut decide: impl FnMut(usize) -> usize,
    ) -> (usize, usize, u64, u64) {
        let mut padding = prefill;
        let mut carry = 0.0;
        let (mut underflows, mut max_padding) = (0, padding);
        let (mut written, mut consumed) = (prefill as u64, 0u64);
        for pass in 0..passes {
            carry += rate;
            let whole = carry.floor();
            carry -= whole;
            // Baile de ±1 trama, pseudoaleatorio y determinista.
            let wobble = match (pass * 7919) % 3 {
                0 => -1,
                1 => 0,
                _ => 1,
            };
            let want = (whole as i64 + wobble).max(0) as usize;
            if padding < want && pass >= 2 {
                underflows += 1;
            }
            let took = want.min(padding);
            padding -= took;
            consumed += took as u64;
            if Some(pass) == missed {
                continue;
            }
            let frames = decide(padding);
            assert!(padding + frames <= buffer, "no cabe: {padding} + {frames}");
            padding += frames;
            written += frames as u64;
            max_padding = max_padding.max(padding);
        }
        (underflows, max_padding, written, consumed)
    }

    /// Periodos en tramas a 48 kHz: 448 tramas a 44,1 kHz (≈ 487,6), 480, 512 y 960 (20 ms).
    const RATES: [f64; 4] = [448.0 * 48_000.0 / 44_100.0, 480.0, 512.0, 960.0];

    /// Búfer típico del modo compartido: lo pedido (50 ms) o, si es menor, dos periodos y algo.
    fn buffer_for(period: usize) -> usize {
        hns_to_frames(500_000).max(2 * period + period / 2)
    }

    #[test]
    fn filling_to_the_target_never_starves_the_device() {
        for rate in RATES {
            let period = rate.ceil() as usize;
            let buffer = buffer_for(period);
            let pacing = Pacing::new(buffer, period);
            let (underflows, max_padding, written, consumed) = simulate(
                rate,
                buffer,
                pacing.prefill(),
                3_000,
                Some(1_500),
                |padding| pacing.frames_to_write(padding),
            );
            assert_eq!(underflows, 0, "periodo {rate}: {pacing:?}");
            assert!(
                max_padding <= pacing.target,
                "periodo {rate}: {max_padding}"
            );
            // Se escribe al ritmo del dispositivo: la cola no crece ni se vacía.
            let level = written - consumed;
            assert!(
                level <= pacing.target as u64,
                "periodo {rate}: {written} escritas, {consumed} consumidas"
            );
        }
    }

    #[test]
    fn one_block_per_event_starves_any_period_but_480() {
        // La regla anterior: una trama de 10 ms por evento como mucho, en un búfer de 10 ms y sin
        // relleno inicial. Con periodos que no son 480 tramas, el dispositivo se queda sin audio en
        // casi todas las pasadas.
        let buffer: usize = 480;
        for rate in [448.0 * 48_000.0 / 44_100.0, 512.0] {
            let (underflows, ..) = simulate(rate, 1_100, 0, 1_000, None, |padding| {
                buffer
                    .saturating_sub(padding)
                    .min(crate::SAMPLES_PER_CHANNEL)
            });
            assert!(underflows > 900, "periodo {rate}: {underflows}");
        }
    }

    #[test]
    fn the_target_respects_the_buffer_and_unknown_periods() {
        let pacing = Pacing::new(4_800, 0);
        assert_eq!(pacing.period, 480);
        assert_eq!(pacing.target, 960 + MARGIN_FRAMES);
        assert_eq!(pacing.prefill(), 480);
        assert_eq!(pacing.frames_to_write(0), 960 + MARGIN_FRAMES);
        assert_eq!(pacing.frames_to_write(2_000), 0, "nunca negativo");
        let small = Pacing::new(1_000, 960);
        assert_eq!(small.target, 1_000, "no más que el búfer");
        assert_eq!(small.frames_to_write(100), 900);
        assert_eq!(hns_to_frames(100_000), 480);
        assert_eq!(hns_to_frames(101_587), 488);
        assert_eq!(hns_to_frames(-5), 0);
    }
}
