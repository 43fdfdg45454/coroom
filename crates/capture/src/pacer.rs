//! Elección de fotogramas para una cadencia fija a partir de una fuente más rápida.
//!
//! La fuente (WGC al ritmo del monitor) entrega cada refresco con su marca de tiempo; el selector
//! se queda con el primero que llega en cada hueco de la cadencia pedida. Un intervalo mínimo entre
//! entregas (`MinUpdateInterval`) no sirve para esto: redondea hacia arriba al múltiplo del refresco
//! (60 fps pedidos con un monitor de 144 Hz daban 48, un fotograma cada tres refrescos). Con la
//! marca de tiempo y una cadencia que avanza sola (no desde el último fotograma), los huecos de dos
//! y tres refrescos se alternan y la media queda en la cadencia pedida.
//!
//! Un fotograma se acepta aunque llegue antes de su hueco si el adelanto es menor que la mitad del
//! intervalo típico de la fuente (medido), con tope de medio periodo: así nunca entra el refresco
//! anterior de una fuente más rápida (llega un intervalo entero antes), y una fuente al mismo ritmo
//! cuyas marcas bailan varios milisegundos (KWin con la captura por memoria compartida) no pierde
//! ninguno. Con un margen fijo de 1/8 de periodo (2,08 ms a 60 fps) se tiraban hasta 7 de cada 60.

use std::time::Duration;

/// Selector de fotogramas para `fps` fotogramas por segundo.
#[derive(Debug, Clone)]
pub(crate) struct FramePacer {
    period: Duration,
    /// Momento ideal del siguiente fotograma, en la escala de las marcas de tiempo.
    next: Option<Duration>,
    /// Marca del último fotograma ofrecido (aceptado o no).
    last: Option<Duration>,
    /// Intervalo típico entre fotogramas de la fuente (media móvil), si ya se midió.
    interval: Option<Duration>,
}

impl FramePacer {
    /// Selector para `fps` (0 o 1 o más: se toma al menos 1).
    pub(crate) fn new(fps: u32) -> Self {
        Self {
            period: Duration::from_secs(1) / fps.max(1),
            next: None,
            last: None,
            interval: None,
        }
    }

    /// Margen con el que se acepta un fotograma que llega antes de su momento: medio intervalo
    /// típico de la fuente (el refresco anterior de una fuente más rápida llega un intervalo
    /// entero antes y no entra; el baile de las marcas de una al mismo ritmo, sí), entre 1/8 de
    /// periodo (sin medida aún: lo seguro con 240 Hz para 60 fps) y medio periodo.
    fn tolerance(&self) -> Duration {
        let floor = self.period / 8;
        self.interval.map_or(floor, |interval| {
            (interval / 2).clamp(floor, self.period / 2)
        })
    }

    /// Anota el intervalo desde el fotograma anterior en la media móvil (1/8 por muestra). Las
    /// pausas (más de 4 periodos: pantalla quieta) no cuentan.
    fn measure(&mut self, at: Duration) {
        if let Some(last) = self.last
            && let Some(gap) = at.checked_sub(last)
            && !gap.is_zero()
            && gap <= self.period * 4
        {
            self.interval = Some(match self.interval {
                Some(interval) => (interval * 7 + gap) / 8,
                None => gap,
            });
        }
        self.last = Some(at);
    }

    /// Si se entrega el fotograma con marca de tiempo `at` (creciente, en cualquier origen).
    pub(crate) fn accept(&mut self, at: Duration) -> bool {
        self.measure(at);
        let Some(next) = self.next else {
            self.next = Some(at + self.period);
            return true;
        };
        if at + self.tolerance() < next {
            return false;
        }
        // La cadencia avanza desde el momento ideal; si la fuente estuvo parada (pantalla quieta)
        // o se retrasó más de un periodo, vuelve a empezar desde este fotograma.
        let ideal = next + self.period;
        self.next = Some(if at >= next + self.period {
            at + self.period
        } else {
            ideal
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fotogramas que deja pasar en `seconds` de una fuente a `hz` con `jitter` alterno.
    fn accepted(fps: u32, hz: f64, seconds: f64, jitter: Duration) -> usize {
        let mut pacer = FramePacer::new(fps);
        let frames = (hz * seconds) as usize;
        (0..frames)
            .filter(|&i| {
                let base = Duration::from_secs_f64(i as f64 / hz);
                let at = if i % 2 == 0 {
                    base + jitter
                } else {
                    base.saturating_sub(jitter)
                };
                pacer.accept(at)
            })
            .count()
    }

    #[test]
    fn a_faster_monitor_gives_the_requested_cadence() {
        // 144 Hz, 120 Hz, 165 Hz y 240 Hz para 60 fps: 60 por segundo, no 48 ni 40.
        for hz in [144.0, 120.0, 165.0, 240.0] {
            let got = accepted(60, hz, 10.0, Duration::ZERO);
            assert!((598..=601).contains(&got), "{hz} Hz: {got} en 10 s");
        }
        assert!((1198..=1201).contains(&accepted(120, 144.0, 10.0, Duration::ZERO)));
        // 30, 60 y 120 con monitores de 144, 165, 170 y 240 Hz: siempre la cadencia pedida.
        for hz in [144.0, 165.0, 170.0, 240.0] {
            for fps in [30u32, 60, 120] {
                let want = fps as usize * 10;
                let got = accepted(fps, hz, 10.0, Duration::from_micros(300));
                assert!(
                    got.abs_diff(want) <= 2,
                    "{fps} fps con {hz} Hz: {got} en 10 s"
                );
            }
        }
    }

    #[test]
    fn a_source_at_the_same_rate_with_jitter_loses_nothing() {
        let got = accepted(60, 60.0, 10.0, Duration::from_micros(800));
        assert_eq!(got, 600);
    }

    /// Fotogramas que deja pasar de una fuente a `hz` cuyas marcas bailan ±`jitter` al azar
    /// (determinista), como las de KWin con la captura por memoria compartida.
    fn accepted_random(fps: u32, hz: f64, seconds: f64, jitter: Duration) -> usize {
        let mut pacer = FramePacer::new(fps);
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let frames = (hz * seconds) as usize;
        let spread = jitter.as_secs_f64();
        (0..frames)
            .filter(|&i| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let unit = (seed >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0;
                let at = (i as f64 / hz + spread * unit).max(0.0);
                pacer.accept(Duration::from_secs_f64(at))
            })
            .count()
    }

    #[test]
    fn a_source_at_the_same_rate_with_wide_jitter_loses_nothing() {
        // Lo que dio un anfitrión real: la fuente a 59-60 Hz y 5-7 fotogramas por segundo
        // «fuera de cadencia» (el margen era 1/8 de periodo, 2,08 ms).
        for jitter_ms in [2, 3, 4] {
            let got = accepted_random(60, 60.0, 10.0, Duration::from_millis(jitter_ms));
            assert!(
                got >= 598,
                "60 Hz con ±{jitter_ms} ms: {got} de 600 en 10 s"
            );
        }
        let got = accepted_random(60, 59.94, 10.0, Duration::from_millis(3));
        assert!(got >= 597, "59,94 Hz con ±3 ms: {got} en 10 s");
    }

    #[test]
    fn a_slightly_faster_source_gives_the_requested_cadence() {
        // También del mismo anfitrión: la fuente a 64-65 Hz daba 49-50 fps en vez de 60.
        for hz in [61.0, 64.0, 65.0, 75.0] {
            let got = accepted(60, hz, 10.0, Duration::ZERO);
            assert!((598..=601).contains(&got), "{hz} Hz: {got} en 10 s");
            let got = accepted_random(60, hz, 10.0, Duration::from_millis(2));
            assert!(
                (595..=601).contains(&got),
                "{hz} Hz con ±2 ms: {got} en 10 s"
            );
        }
    }

    #[test]
    fn a_slower_source_passes_through() {
        assert_eq!(accepted(60, 30.0, 10.0, Duration::ZERO), 300);
    }

    #[test]
    fn after_a_pause_the_cadence_restarts() {
        let mut pacer = FramePacer::new(60);
        assert!(pacer.accept(Duration::ZERO));
        // Pantalla quieta un segundo: el siguiente pasa y la cadencia sigue desde él.
        assert!(pacer.accept(Duration::from_millis(1000)));
        assert!(!pacer.accept(Duration::from_millis(1007)));
        assert!(pacer.accept(Duration::from_millis(1017)));
    }
}
