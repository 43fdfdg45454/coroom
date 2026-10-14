//! Elección de fotogramas para una cadencia fija a partir de una fuente más rápida.
//!
//! La fuente (WGC al ritmo del monitor) entrega cada refresco con su marca de tiempo; el selector
//! se queda con el primero que llega en cada hueco de la cadencia pedida. Un intervalo mínimo entre
//! entregas (`MinUpdateInterval`) no sirve para esto: redondea hacia arriba al múltiplo del refresco
//! (60 fps pedidos con un monitor de 144 Hz daban 48, un fotograma cada tres refrescos). Con la
//! marca de tiempo y una cadencia que avanza sola (no desde el último fotograma), los huecos de dos
//! y tres refrescos se alternan y la media queda en la cadencia pedida.

use std::time::Duration;

/// Selector de fotogramas para `fps` fotogramas por segundo.
#[derive(Debug, Clone)]
pub(crate) struct FramePacer {
    period: Duration,
    /// Momento ideal del siguiente fotograma, en la escala de las marcas de tiempo.
    next: Option<Duration>,
}

impl FramePacer {
    /// Selector para `fps` (0 o 1 o más: se toma al menos 1).
    pub(crate) fn new(fps: u32) -> Self {
        Self {
            period: Duration::from_secs(1) / fps.max(1),
            next: None,
        }
    }

    /// Margen con el que se acepta un fotograma que llega antes de su momento: el jitter de una
    /// fuente a la misma frecuencia que la cadencia (que no debe perder fotogramas) sin tomar
    /// el refresco anterior de una más rápida (hasta 4 veces: 240 Hz para 60 fps).
    fn tolerance(&self) -> Duration {
        self.period / 8
    }

    /// Si se entrega el fotograma con marca de tiempo `at` (creciente, en cualquier origen).
    pub(crate) fn accept(&mut self, at: Duration) -> bool {
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
    }

    #[test]
    fn a_source_at_the_same_rate_with_jitter_loses_nothing() {
        let got = accepted(60, 60.0, 10.0, Duration::from_micros(800));
        assert_eq!(got, 600);
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
