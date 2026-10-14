//! Recuento de lo que entrega la fuente de captura, antes de la cadencia: para saber, con el
//! registro de una sesión real, si los fotogramas que faltan no llegaron del compositor, los
//! descartó la cadencia pedida o llegaron sin imagen nueva.
//!
//! Son contadores del proceso (acumulados y atómicos, como los de `coroom_session::stats`): los
//! tocan los callbacks de los backends y la línea de estadísticas del anfitrión resta la
//! instantánea anterior.

use std::sync::atomic::{AtomicU64, Ordering};

static ARRIVED: AtomicU64 = AtomicU64::new(0);
static EMPTY: AtomicU64 = AtomicU64::new(0);
static PACED_OUT: AtomicU64 = AtomicU64::new(0);
static REPEATED: AtomicU64 = AtomicU64::new(0);

/// Instantánea de los contadores acumulados de la captura.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceCounters {
    /// Fotogramas con imagen que dio la fuente (antes de la cadencia).
    pub arrived: u64,
    /// Búferes sin imagen: solo el cursor (PipeWire) o dañados.
    pub empty: u64,
    /// Fotogramas que sobraban para la cadencia pedida.
    pub paced_out: u64,
    /// Fotogramas iguales al anterior (WGC por CPU: la ventana no se redibujó, se movió el
    /// cursor). Con imagen de GPU no se miran los píxeles y no se cuentan.
    pub repeated: u64,
}

impl SourceCounters {
    /// Lo que va de `before` a `self`.
    pub fn since(self, before: Self) -> Self {
        Self {
            arrived: self.arrived.saturating_sub(before.arrived),
            empty: self.empty.saturating_sub(before.empty),
            paced_out: self.paced_out.saturating_sub(before.paced_out),
            repeated: self.repeated.saturating_sub(before.repeated),
        }
    }
}

/// Contadores acumulados de la captura de este proceso.
pub fn source_counters() -> SourceCounters {
    SourceCounters {
        arrived: ARRIVED.load(Ordering::Relaxed),
        empty: EMPTY.load(Ordering::Relaxed),
        paced_out: PACED_OUT.load(Ordering::Relaxed),
        repeated: REPEATED.load(Ordering::Relaxed),
    }
}

#[allow(dead_code)] // Cada backend usa los suyos.
pub(crate) fn record_arrived() {
    ARRIVED.fetch_add(1, Ordering::Relaxed);
}

#[allow(dead_code)]
pub(crate) fn record_empty() {
    EMPTY.fetch_add(1, Ordering::Relaxed);
}

#[allow(dead_code)]
pub(crate) fn record_paced_out() {
    PACED_OUT.fetch_add(1, Ordering::Relaxed);
}

#[allow(dead_code)]
pub(crate) fn record_repeated() {
    REPEATED.fetch_add(1, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_subtracts_and_never_underflows() {
        let before = SourceCounters {
            arrived: 10,
            empty: 3,
            paced_out: 4,
            repeated: 1,
        };
        let now = SourceCounters {
            arrived: 70,
            empty: 3,
            paced_out: 34,
            repeated: 0,
        };
        assert_eq!(
            now.since(before),
            SourceCounters {
                arrived: 60,
                empty: 0,
                paced_out: 30,
                repeated: 0,
            }
        );
    }
}
