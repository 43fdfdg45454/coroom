//! Fuente que no comparte nada: la de una sala solo de voz y chat, o la que queda tras «Dejar de
//! compartir». No entrega fotogramas; la sesión la reconoce ([`Capturer::shares_nothing`]),
//! avisa a los invitados de que nadie comparte y olvida el último fotograma, para no volver a
//! enviarlo (un intra pedido no repite la pantalla que se dejó de compartir).

use std::time::Duration;

use crate::{CaptureError, CaptureTarget, CapturedFrame, Capturer};

/// Fuente sin fotogramas ([`crate::idle`]).
#[derive(Debug, Default)]
pub struct IdleCapturer;

impl IdleCapturer {
    /// Una fuente que no comparte nada.
    pub fn new() -> Self {
        Self
    }
}

impl Capturer for IdleCapturer {
    fn targets(&self) -> Result<Vec<CaptureTarget>, CaptureError> {
        Ok(vec![CaptureTarget::Monitor {
            id: 0,
            name: "nada".to_owned(),
        }])
    }

    fn start(&mut self, _target: &CaptureTarget) -> Result<(), CaptureError> {
        Ok(())
    }

    fn next_frame(&mut self) -> Result<CapturedFrame, CaptureError> {
        Err(CaptureError::Unsupported(
            "no se comparte nada: no hay fotogramas".into(),
        ))
    }

    fn next_frame_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<CapturedFrame>, CaptureError> {
        std::thread::sleep(timeout);
        Ok(None)
    }

    fn stop(&mut self) {}

    fn shares_nothing(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_idle_source_waits_and_gives_nothing() {
        let mut idle = IdleCapturer::new();
        assert!(idle.shares_nothing());
        let target = idle.targets().expect("objetivo").remove(0);
        idle.start(&target).expect("arranca");
        let start = std::time::Instant::now();
        let frame = idle
            .next_frame_timeout(Duration::from_millis(20))
            .expect("sin error");
        assert!(frame.is_none());
        assert!(
            start.elapsed() >= Duration::from_millis(20),
            "espera, no gira"
        );
        assert!(idle.next_frame().is_err());
    }
}
