//! Cuánto escribir en el búfer de un dispositivo de reproducción por eventos (WASAPI en modo
//! compartido) cada vez que avisa.
//!
//! El motor de audio consume un **periodo** del búfer en cada pasada (el que dice el dispositivo
//! con `GetDevicePeriod`: 10 ms casi siempre, pero 10,16 ms con una mezcla a 44,1 kHz convertida a
//! 48 kHz, 512 tramas en algunos controladores, 20 ms en USB o Bluetooth) y avisa con un evento.
//! Escribir «una trama de 10 ms por evento» deja el búfer cada vez más corto cuando el periodo no
//! es exactamente 480 tramas, y un evento perdido (o dos fundidos en uno por un despertar tardío)
//! lo vacía: el motor reproduce silencio a trozos y el audio suena robótico.
//!
//! [`Pacing`] rellena en cada evento hasta un **nivel objetivo** de **un periodo más un margen
//! medido**: en cada evento mira cuánto se llevó el motor desde la escritura anterior (el nivel
//! que quedó entonces menos el relleno de ahora, `GetCurrentPadding`) y el margen es lo que más
//! pasó de un periodo en la ventana de [`MARGIN_WINDOW`] eventos (la conversión de frecuencia que
//! no consume siempre lo mismo, un despertar tardío, un evento perdido). Si el motor se lo llevó
//! todo (se quedó sin audio), lo consumido se estima con el tiempo que pasó de verdad. Ninguna
//! constante dice cuánto tarda el hardware: durante la primera ventana el margen es un periodo
//! (aguanta un evento perdido) más lo que ya se vio bailar; después, solo lo observado. Es una máquina de estados pura para poder probarla
//! contra un dispositivo simulado.

/// Eventos en los que se recuerda el mayor exceso sobre un periodo (unos 5 s con periodos de
/// 10 ms): un despertar tardío de vez en cuando sigue cubierto ese tiempo, y si no se repite, el
/// margen vuelve a bajar.
pub const MARGIN_WINDOW: usize = 500;

/// Tramas (por canal) a 48 kHz de una duración en unidades de 100 ns, redondeando hacia arriba.
pub fn hns_to_frames(hns: i64) -> usize {
    let hns = u64::try_from(hns).unwrap_or(0);
    (hns * u64::from(crate::SAMPLE_RATE_HZ)).div_ceil(10_000_000) as usize
}

/// Qué escribir en cada evento de un dispositivo de reproducción.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pacing {
    /// Tamaño del búfer del dispositivo, en tramas.
    pub buffer: usize,
    /// Periodo del motor (lo que consume en cada pasada), en tramas.
    pub period: usize,
    /// Nivel al que se rellena el búfer en cada evento, en tramas: un periodo más el margen.
    pub target: usize,
    /// Margen medido sobre un periodo, en tramas.
    pub margin: usize,
    /// Relleno que quedó tras la última escritura.
    level: Option<usize>,
    /// Mayor exceso de la ventana actual y de la anterior.
    window_max: usize,
    previous_max: Option<usize>,
    events: usize,
}

impl Pacing {
    /// Para un búfer de `buffer` tramas y un periodo de `period` (0 si el dispositivo no lo dice:
    /// una trama de 10 ms, la unidad del pipeline).
    pub fn new(buffer: usize, period: usize) -> Self {
        let period = if period == 0 {
            crate::SAMPLES_PER_CHANNEL
        } else {
            period
        };
        let mut pacing = Self {
            buffer,
            period,
            target: 0,
            // Sin medidas aún: que aguante un evento perdido.
            margin: period,
            level: None,
            window_max: 0,
            previous_max: None,
            events: 0,
        };
        pacing.update_target();
        pacing
    }

    fn update_target(&mut self) {
        self.target = (self.period + self.margin).min(self.buffer);
    }

    /// Tramas de silencio que se escriben antes de arrancar el flujo (un periodo): el motor tiene
    /// algo que leer en su primera pasada y el primer evento ya rellena hasta el objetivo.
    pub fn prefill(&mut self) -> usize {
        let frames = self.period.min(self.buffer);
        self.level = Some(frames);
        frames
    }

    /// Tramas que escribir ahora con `padding` tramas aún en el búfer (`GetCurrentPadding`),
    /// cuando desde la llamada anterior han pasado `elapsed` tramas de tiempo real (lo que dura
    /// ese tiempo a 48 kHz): lo que falta hasta el objetivo, nunca más del sitio libre. Anota lo
    /// que se llevó el motor para el margen y supone que se escribe todo lo que devuelve.
    pub fn frames_to_write(&mut self, padding: usize, elapsed: usize) -> usize {
        if let Some(level) = self.level {
            let consumed = if padding == 0 {
                // Se quedó sin nada: se llevó al menos lo que había, y el tiempo dice cuánto más.
                level.max(elapsed)
            } else {
                level.saturating_sub(padding)
            };
            self.observe(consumed.saturating_sub(self.period));
        }
        let frames = self
            .target
            .saturating_sub(padding)
            .min(self.buffer.saturating_sub(padding));
        self.level = Some(padding + frames);
        frames
    }

    /// Un exceso de consumo sobre un periodo entre dos escrituras.
    fn observe(&mut self, excess: usize) {
        self.window_max = self.window_max.max(excess);
        self.events += 1;
        if self.events >= MARGIN_WINDOW {
            self.previous_max = Some(self.window_max);
            self.window_max = 0;
            self.events = 0;
        }
        self.margin = match self.previous_max {
            Some(previous) => previous.max(self.window_max),
            // Primera ventana: un periodo (un evento perdido) más lo que ya se vio bailar.
            None => self.period + self.window_max,
        };
        self.update_target();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dispositivo simulado: cada pasada consume `rate` tramas (con parte fraccionaria acumulada,
    /// como la conversión 44,1 → 48 kHz) ±1 trama de baile; tras cada pasada avisa, salvo en las
    /// pasadas de `missed`, cuyo evento se pierde (el siguiente despertar ve dos pasadas). `decide`
    /// dice cuánto escribir con el relleno actual y el tiempo pasado desde la escritura anterior.
    /// Devuelve las pasadas en que faltó audio tras el calentamiento, el mayor relleno visto y lo
    /// escrito y consumido en total.
    fn simulate(
        rate: f64,
        buffer: usize,
        prefill: usize,
        passes: usize,
        missed: &[usize],
        mut decide: impl FnMut(usize, usize) -> usize,
    ) -> (Vec<usize>, usize, u64, u64) {
        let mut padding = prefill;
        let mut carry = 0.0;
        let (mut underflows, mut max_padding) = (Vec::new(), padding);
        let (mut written, mut consumed) = (prefill as u64, 0u64);
        let mut elapsed = 0;
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
            elapsed += want;
            if padding < want && pass >= 2 {
                underflows.push(pass);
            }
            let took = want.min(padding);
            padding -= took;
            consumed += took as u64;
            if missed.contains(&pass) {
                continue;
            }
            let frames = decide(padding, elapsed);
            elapsed = 0;
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
    fn a_steady_device_settles_at_one_period_and_what_it_wobbles() {
        for rate in RATES {
            let period = rate.ceil() as usize;
            let buffer = buffer_for(period);
            let mut pacing = Pacing::new(buffer, period);
            let prefill = pacing.prefill();
            assert_eq!(
                pacing.target,
                (2 * period).min(buffer),
                "sin medidas, aguanta un evento perdido"
            );
            let (underflows, max_padding, written, consumed) =
                simulate(rate, buffer, prefill, 3_000, &[], |padding, elapsed| {
                    pacing.frames_to_write(padding, elapsed)
                });
            assert!(underflows.is_empty(), "periodo {rate}: {underflows:?}");
            // Pasada la primera ventana, el objetivo es un periodo y el baile medido (±1 trama y
            // la parte fraccionaria de la conversión), no dos periodos.
            assert!(pacing.target <= period + 3, "periodo {rate}: {pacing:?}");
            assert!(
                max_padding <= 2 * period + 3,
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
    fn a_lost_event_is_covered_and_then_measured() {
        for rate in RATES {
            let period = rate.ceil() as usize;
            let buffer = buffer_for(period);
            let mut pacing = Pacing::new(buffer, period);
            let prefill = pacing.prefill();
            // Un evento perdido en la primera ventana (lo cubre el margen inicial), otro cuando el
            // margen ya bajó a lo medido (falta audio una vez) y un tercero poco después, que el
            // margen medido ya cubre.
            let late = MARGIN_WINDOW * 3;
            let missed = [100, late, late + 50];
            let (underflows, ..) = simulate(rate, buffer, prefill, late + 200, &missed, |p, e| {
                pacing.frames_to_write(p, e)
            });
            assert!(
                underflows.len() <= 1 && underflows.iter().all(|&pass| pass > late),
                "periodo {rate}: {underflows:?}"
            );
            // Lo medido: casi un periodo (lo que se llevó la pasada sin aviso, con su baile).
            assert!(pacing.margin + 2 >= period, "periodo {rate}: {pacing:?}");
        }
    }

    #[test]
    fn one_block_per_event_starves_any_period_but_480() {
        // La regla anterior: una trama de 10 ms por evento como mucho, en un búfer de 10 ms y sin
        // relleno inicial. Con periodos que no son 480 tramas, el dispositivo se queda sin audio en
        // casi todas las pasadas.
        let buffer: usize = 480;
        for rate in [448.0 * 48_000.0 / 44_100.0, 512.0] {
            let (underflows, ..) = simulate(rate, 1_100, 0, 1_000, &[], |padding, _| {
                buffer
                    .saturating_sub(padding)
                    .min(crate::SAMPLES_PER_CHANNEL)
            });
            assert!(
                underflows.len() > 900,
                "periodo {rate}: {}",
                underflows.len()
            );
        }
    }

    #[test]
    fn the_target_respects_the_buffer_and_unknown_periods() {
        let mut pacing = Pacing::new(4_800, 0);
        assert_eq!(pacing.period, 480);
        assert_eq!(pacing.target, 960);
        assert_eq!(pacing.prefill(), 480);
        assert_eq!(pacing.frames_to_write(480, 0), 480);
        assert_eq!(pacing.frames_to_write(2_000, 0), 0, "nunca negativo");
        let mut small = Pacing::new(1_000, 960);
        assert_eq!(small.target, 1_000, "no más que el búfer");
        assert_eq!(small.frames_to_write(100, 0), 900);
        assert_eq!(hns_to_frames(100_000), 480);
        assert_eq!(hns_to_frames(101_587), 488);
        assert_eq!(hns_to_frames(-5), 0);
    }

    #[test]
    fn an_empty_buffer_counts_the_real_time_that_passed() {
        let mut pacing = Pacing::new(4_800, 480);
        pacing.prefill();
        for _ in 0..MARGIN_WINDOW * 2 {
            let padding = pacing.target - 480;
            pacing.frames_to_write(padding, 480);
        }
        assert_eq!(pacing.margin, 0, "dispositivo exacto: {pacing:?}");
        // El hilo despertó 25 ms tarde y el búfer estaba vacío: el margen sube a lo que pasó.
        let frames = pacing.frames_to_write(0, 1_200);
        assert_eq!(pacing.margin, 1_200 - 480);
        assert_eq!(frames, 480 + 720);
    }
}
