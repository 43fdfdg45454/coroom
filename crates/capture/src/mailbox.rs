//! Buzón de un solo hueco entre el hilo de un backend y `Capturer::next_frame`: si llega un
//! fotograma antes de que se recoja el anterior, gana el último.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::{CaptureError, CapturedFrame};

/// Buzón de un solo hueco.
#[derive(Default)]
pub(crate) struct Mailbox {
    slot: Mutex<MailboxState>,
    changed: Condvar,
    /// Cadencia nueva para la captura en marcha (0: ninguna pendiente); el hilo del backend la
    /// recoge con [`Mailbox::take_max_fps`] y rehace su selector de fotogramas.
    max_fps: AtomicU32,
}

#[derive(Default)]
struct MailboxState {
    frame: Option<CapturedFrame>,
    error: Option<CaptureError>,
}

impl Mailbox {
    /// Deja una cadencia nueva para el hilo del backend (`Capturer::set_options` en marcha).
    pub(crate) fn set_max_fps(&self, max_fps: u32) {
        self.max_fps.store(max_fps.max(1), Ordering::Release);
    }

    /// Recoge la cadencia nueva, si la hay.
    pub(crate) fn take_max_fps(&self) -> Option<u32> {
        match self.max_fps.swap(0, Ordering::AcqRel) {
            0 => None,
            fps => Some(fps),
        }
    }

    pub(crate) fn put(&self, frame: CapturedFrame) {
        let mut state = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(evicted) = state.frame.replace(frame) {
            // Gana el último: el búfer del que nadie recogió vuelve al pool.
            crate::pool::recycle_frame(evicted);
        }
        self.changed.notify_one();
    }

    pub(crate) fn fail(&self, error: CaptureError) {
        let mut state = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        state.error.get_or_insert(error);
        self.changed.notify_all();
    }

    pub(crate) fn take(
        &self,
        timeout: Option<Duration>,
    ) -> Result<Option<CapturedFrame>, CaptureError> {
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        let mut state = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(frame) = state.frame.take() {
                return Ok(Some(frame));
            }
            if let Some(error) = state.error.clone() {
                return Err(error);
            }
            state = match deadline {
                None => self
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner),
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Ok(None);
                    }
                    self.changed
                        .wait_timeout(state, deadline - now)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FramePixels;

    #[test]
    fn a_new_cadence_is_taken_once() {
        let mailbox = Mailbox::default();
        assert_eq!(mailbox.take_max_fps(), None);
        mailbox.set_max_fps(0);
        assert_eq!(mailbox.take_max_fps(), Some(1), "al menos 1");
        mailbox.set_max_fps(30);
        mailbox.set_max_fps(60);
        assert_eq!(mailbox.take_max_fps(), Some(60), "gana la última");
        assert_eq!(mailbox.take_max_fps(), None);
    }

    fn frame(frame_id: u64) -> CapturedFrame {
        CapturedFrame {
            frame_id,
            width: 1,
            height: 1,
            captured_at: Instant::now(),
            cursor: None,
            cursor_shape: None,
            pixels: FramePixels::Bgra {
                data: vec![0; 4],
                stride: 4,
            },
            cpu_copies: 0,
        }
    }

    #[test]
    fn the_newest_frame_wins_and_errors_stick() {
        let mailbox = Mailbox::default();
        assert_eq!(mailbox.take(Some(Duration::from_millis(5))), Ok(None));
        mailbox.put(frame(1));
        mailbox.put(frame(2));
        let taken = mailbox.take(Some(Duration::ZERO)).unwrap().unwrap();
        assert_eq!(taken.frame_id, 2);
        mailbox.fail(CaptureError::TargetLost);
        assert_eq!(mailbox.take(None), Err(CaptureError::TargetLost));
    }
}
