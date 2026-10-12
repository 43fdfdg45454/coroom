//! Envío de la entrada del invitado: agrupa, reparte en los dos canales, cifra y envía.
//!
//! [`InputSender`] tiene un hilo `entrada` que recibe los eventos con su instante:
//!
//! - **Estado** (teclas, botones, rueda, mandos conectados, [`InputEvent::ReleaseAll`]): sale al
//!   momento por el stream fiable en un [`InputMessage`] (con los demás de estado que ya estén en
//!   cola). Antes se vacía el movimiento pendiente, para no adelantar un clic al movimiento que lo
//!   precede.
//! - **Continuo** (ratón y ejes): se agrupa en un [`InputPacket`] y sale como mucho cada
//!   [`BATCH_INTERVAL`] (≤ 500 datagramas/s); el primero tras una pausa sale al momento. Dentro
//!   del paquete los `MouseMove` se conservan uno a uno (el anfitrión los inyecta todos, en orden);
//!   las posiciones absolutas y los ejes se quedan con el último valor. Por encima de
//!   [`MAX_EVENTS`] eventos, los movimientos relativos se suman al último.
//!
//! Cada paquete lleva `sent_us`, la hora de su primer evento en el reloj del anfitrión
//! ([`InputLink::remote_clock_us`]), para medir la latencia de entrada en el anfitrión. Se cifra con
//! [`InputCipher`] (canal y secuencia propios de cada canal) y sale por el [`InputLink`] que da la
//! sesión (la conexión QUIC).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use coroom_crypto::input::{Channel, InputCipher};
use coroom_proto::input::{InputMessage, InputPacket, MAX_EVENTS, datagram_header, stream_frame};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use tracing::debug;

use crate::{InputError, InputEvent};

/// Intervalo mínimo entre dos datagramas de movimiento.
pub const BATCH_INTERVAL: Duration = Duration::from_millis(2);

/// Por dónde sale la entrada (lo implementa la sesión sobre su conexión).
pub trait InputLink: Send + Sync {
    /// Envía un datagrama (cabecera de entrada y sellado).
    fn send_datagram(&self, datagram: Vec<u8>) -> Result<(), String>;
    /// Escribe una trama en el stream de entrada (`longitud ‖ sellado`).
    fn send_stream(&self, frame: Vec<u8>) -> Result<(), String>;
    /// `at` en µs del reloj del anfitrión, si ya se conoce el desfase.
    fn remote_clock_us(&self, at: Instant) -> Option<u64>;
}

/// Contadores del emisor.
#[derive(Debug, Default)]
pub struct SenderStats {
    events: AtomicU64,
    datagrams: AtomicU64,
    messages: AtomicU64,
    bytes: AtomicU64,
    merged: AtomicU64,
    errors: AtomicU64,
}

/// Instantánea de [`SenderStats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SenderSnapshot {
    /// Eventos enviados.
    pub events: u64,
    /// Datagramas (movimiento).
    pub datagrams: u64,
    /// Mensajes del stream (estado).
    pub messages: u64,
    /// Bytes enviados (cabeceras y etiquetas incluidas).
    pub bytes: u64,
    /// Movimientos relativos sumados a otro por no caber en el paquete.
    pub merged: u64,
    /// Envíos fallidos.
    pub errors: u64,
}

impl SenderStats {
    /// Instantánea.
    pub fn snapshot(&self) -> SenderSnapshot {
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        SenderSnapshot {
            events: load(&self.events),
            datagrams: load(&self.datagrams),
            messages: load(&self.messages),
            bytes: load(&self.bytes),
            merged: load(&self.merged),
            errors: load(&self.errors),
        }
    }
}

/// Movimiento continuo pendiente de enviar.
#[derive(Debug, Default)]
struct Batch {
    events: Vec<InputEvent>,
    first: Option<Instant>,
}

impl Batch {
    fn push(&mut self, event: InputEvent, at: Instant, stats: &SenderStats) {
        self.first.get_or_insert(at);
        match event {
            InputEvent::MouseMoveAbsolute { .. } => {
                if let Some(last) = self
                    .events
                    .iter_mut()
                    .rev()
                    .find(|e| matches!(e, InputEvent::MouseMoveAbsolute { .. }))
                {
                    *last = event;
                    return;
                }
            }
            InputEvent::GamepadAxis { slot, axis, .. } => {
                if let Some(last) = self.events.iter_mut().rev().find(|e| {
                    matches!(e, InputEvent::GamepadAxis { slot: s, axis: a, .. } if *s == slot && *a == axis)
                }) {
                    *last = event;
                    return;
                }
            }
            InputEvent::MouseMove { dx, dy } if self.events.len() >= MAX_EVENTS => {
                if let Some(InputEvent::MouseMove { dx: ldx, dy: ldy }) = self
                    .events
                    .iter_mut()
                    .rev()
                    .find(|e| matches!(e, InputEvent::MouseMove { .. }))
                {
                    *ldx = ldx.saturating_add(dx);
                    *ldy = ldy.saturating_add(dy);
                    stats.merged.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
            _ => {}
        }
        if self.events.len() < MAX_EVENTS {
            self.events.push(event);
        }
    }
}

struct Worker {
    link: Arc<dyn InputLink>,
    cipher: Arc<Mutex<InputCipher>>,
    stats: Arc<SenderStats>,
    sealed: Vec<u8>,
}

impl Worker {
    fn sent_us(&self, at: Option<Instant>) -> u32 {
        at.and_then(|at| self.link.remote_clock_us(at))
            // 0 significa "sin hora": un 0 de verdad se manda como 1 µs.
            .map_or(0, |us| (us as u32).max(1))
    }

    fn send_batch(&mut self, batch: &mut Batch) {
        if batch.events.is_empty() {
            return;
        }
        let events = std::mem::take(&mut batch.events);
        let sent_us = self.sent_us(batch.first.take());
        let count = events.len() as u64;
        let mut cipher = self.cipher.lock().unwrap_or_else(PoisonError::into_inner);
        let seq = cipher.next_seq(Channel::Datagram);
        let Ok(seq32) = u32::try_from(seq) else {
            self.stats.errors.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let packet = InputPacket {
            seq: seq32,
            sent_us,
            events,
        };
        let result = packet
            .to_bytes()
            .map_err(|err| err.to_string())
            .and_then(|plain| {
                self.sealed.clear();
                self.sealed
                    .extend_from_slice(&datagram_header(cipher.key_id()));
                cipher
                    .seal(Channel::Datagram, seq, &plain, &mut self.sealed)
                    .map_err(|err| err.to_string())
            });
        drop(cipher);
        match result.and_then(|()| {
            let bytes = self.sealed.len() as u64;
            self.link.send_datagram(self.sealed.clone()).map(|()| bytes)
        }) {
            Ok(bytes) => {
                self.stats.datagrams.fetch_add(1, Ordering::Relaxed);
                self.stats.events.fetch_add(count, Ordering::Relaxed);
                self.stats.bytes.fetch_add(bytes, Ordering::Relaxed);
            }
            Err(err) => {
                debug!(error = %err, "no se pudo enviar un datagrama de entrada");
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn send_message(&mut self, events: Vec<InputEvent>, at: Instant) {
        let sent_us = self.sent_us(Some(at));
        let count = events.len() as u64;
        let mut cipher = self.cipher.lock().unwrap_or_else(PoisonError::into_inner);
        let seq = cipher.next_seq(Channel::Stream);
        let Ok(seq32) = u32::try_from(seq) else {
            self.stats.errors.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let message = InputMessage {
            seq: seq32,
            sent_us,
            events,
        };
        let result = message
            .to_bytes()
            .map_err(|err| err.to_string())
            .and_then(|plain| {
                self.sealed.clear();
                cipher
                    .seal(Channel::Stream, seq, &plain, &mut self.sealed)
                    .map_err(|err| err.to_string())
            })
            .and_then(|()| stream_frame(&self.sealed).map_err(|err| err.to_string()));
        drop(cipher);
        match result.and_then(|frame| {
            let bytes = frame.len() as u64;
            self.link.send_stream(frame).map(|()| bytes)
        }) {
            Ok(bytes) => {
                self.stats.messages.fetch_add(1, Ordering::Relaxed);
                self.stats.events.fetch_add(count, Ordering::Relaxed);
                self.stats.bytes.fetch_add(bytes, Ordering::Relaxed);
            }
            Err(err) => {
                debug!(error = %err, "no se pudo enviar un mensaje de entrada");
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn run(mut self, rx: &Receiver<(Instant, InputEvent)>) {
        let mut batch = Batch::default();
        // Hace tiempo que no sale nada: el primer movimiento sale al momento.
        let mut last_batch: Option<Instant> = None;
        loop {
            let due = last_batch.map(|at| at + BATCH_INTERVAL);
            let wait = if batch.events.is_empty() {
                Duration::from_millis(250)
            } else {
                due.map_or(Duration::ZERO, |due| {
                    due.saturating_duration_since(Instant::now())
                })
            };
            match rx.recv_timeout(wait) {
                Ok((at, event)) if event.is_continuous() => batch.push(event, at, &self.stats),
                Ok((at, event)) => {
                    // Lo de estado sale ya, con el movimiento anterior por delante.
                    if !batch.events.is_empty() {
                        last_batch = Some(Instant::now());
                        self.send_batch(&mut batch);
                    }
                    let mut events = vec![event];
                    while events.len() < MAX_EVENTS {
                        match rx.try_recv() {
                            Ok((at, event)) if event.is_continuous() => {
                                batch.push(event, at, &self.stats);
                            }
                            Ok((_, event)) => events.push(event),
                            Err(_) => break,
                        }
                    }
                    self.send_message(events, at);
                    if !batch.events.is_empty() {
                        last_batch = Some(Instant::now());
                        self.send_batch(&mut batch);
                    }
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.send_batch(&mut batch);
                    return;
                }
            }
            let now = Instant::now();
            if !batch.events.is_empty() && due.is_none_or(|due| now >= due) {
                last_batch = Some(now);
                self.send_batch(&mut batch);
            }
        }
    }
}

/// Emisor de la entrada de un invitado.
pub struct InputSender {
    tx: Option<Sender<(Instant, InputEvent)>>,
    stats: Arc<SenderStats>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for InputSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputSender")
            .field("stats", &self.stats.snapshot())
            .finish_non_exhaustive()
    }
}

impl InputSender {
    /// Arranca el hilo `entrada` sobre `link` con las claves de `cipher` (del lado del invitado).
    pub fn spawn(
        link: Arc<dyn InputLink>,
        cipher: Arc<Mutex<InputCipher>>,
    ) -> Result<InputSender, InputError> {
        let (tx, rx) = unbounded();
        let stats = Arc::new(SenderStats::default());
        let worker = Worker {
            link,
            cipher,
            stats: Arc::clone(&stats),
            sealed: Vec::with_capacity(256),
        };
        let thread = thread::Builder::new()
            .name("entrada".into())
            .spawn(move || worker.run(&rx))
            .map_err(|err| InputError::Backend(format!("hilo de entrada: {err}")))?;
        Ok(InputSender {
            tx: Some(tx),
            stats,
            thread: Some(thread),
        })
    }

    /// Envía un evento que ocurrió ahora.
    pub fn send(&self, event: InputEvent) {
        self.send_at(event, Instant::now());
    }

    /// Envía un evento que ocurrió en `at`.
    pub fn send_at(&self, event: InputEvent, at: Instant) {
        if let Some(tx) = &self.tx {
            let _ = tx.send((at, event));
        }
    }

    /// Contadores (para el overlay).
    pub fn stats(&self) -> SenderSnapshot {
        self.stats.snapshot()
    }

    /// Envía lo pendiente y para el hilo.
    pub fn stop(&mut self) {
        self.tx.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for InputSender {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coroom_crypto::hpke::PairwiseSecret;
    use coroom_crypto::input::Role;
    use coroom_proto::input::parse_datagram;

    #[derive(Default)]
    struct Captured {
        datagrams: Mutex<Vec<(Instant, Vec<u8>)>>,
        frames: Mutex<Vec<Vec<u8>>>,
    }

    impl InputLink for Captured {
        fn send_datagram(&self, datagram: Vec<u8>) -> Result<(), String> {
            self.datagrams
                .lock()
                .map_err(|e| e.to_string())?
                .push((Instant::now(), datagram));
            Ok(())
        }

        fn send_stream(&self, frame: Vec<u8>) -> Result<(), String> {
            self.frames.lock().map_err(|e| e.to_string())?.push(frame);
            Ok(())
        }

        fn remote_clock_us(&self, _at: Instant) -> Option<u64> {
            Some(1234)
        }
    }

    fn secret() -> PairwiseSecret {
        PairwiseSecret::new([3u8; 32])
    }

    #[test]
    fn moves_are_batched_and_state_goes_on_the_stream() {
        let link = Arc::new(Captured::default());
        let guest = InputCipher::from_pairwise(&secret(), Role::Guest).unwrap();
        let mut host = InputCipher::from_pairwise(&secret(), Role::Host).unwrap();
        let mut sender = InputSender::spawn(
            Arc::clone(&link) as Arc<dyn InputLink>,
            Arc::new(Mutex::new(guest)),
        )
        .unwrap();
        let start = Instant::now();
        for i in 0..200 {
            sender.send(InputEvent::MouseMove { dx: i + 1, dy: 0 });
            if i == 100 {
                sender.send(InputEvent::Key {
                    code: 30,
                    pressed: true,
                });
            }
            // 1000 Hz.
            let next = start + Duration::from_millis(u64::try_from(i).unwrap() + 1);
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
        }
        sender.stop();
        let stats = sender.stats();
        assert_eq!(stats.events, 201);
        assert_eq!(stats.messages, 1);
        assert_eq!(stats.merged, 0);

        let datagrams = link.datagrams.lock().unwrap();
        // ≤ 500 paquetes/s: con 200 ms de movimiento, unos 100 (más alguno por el mensaje).
        assert!(
            datagrams.len() <= 110 && datagrams.len() >= 60,
            "{} datagramas",
            datagrams.len()
        );
        let gaps: Vec<Duration> = datagrams
            .windows(2)
            .map(|pair| pair[1].0 - pair[0].0)
            .collect();
        let fast = gaps
            .iter()
            .filter(|gap| **gap < Duration::from_micros(1500))
            .count();
        assert!(
            fast <= 3,
            "{fast} datagramas a menos de 1,5 ms del anterior"
        );

        let mut moves = Vec::new();
        let mut out = Vec::new();
        let mut last_seq = None;
        for (_, datagram) in datagrams.iter() {
            let (key_id, sealed) = parse_datagram(datagram).unwrap();
            assert_eq!(key_id, host.key_id());
            let opened = host.open(sealed, &mut out).unwrap();
            assert_eq!(opened.channel, Channel::Datagram);
            let packet = InputPacket::from_bytes(opened.plaintext).unwrap();
            assert_eq!(u64::from(packet.seq), opened.seq);
            assert!(last_seq.is_none_or(|last| packet.seq == last + 1));
            last_seq = Some(packet.seq);
            assert_eq!(packet.sent_us, 1234);
            for event in packet.events {
                if let InputEvent::MouseMove { dx, .. } = event {
                    moves.push(dx);
                }
            }
        }
        assert_eq!(moves, (1..=200).collect::<Vec<_>>(), "todos y en orden");

        let frames = link.frames.lock().unwrap();
        assert_eq!(frames.len(), 1);
        let mut buffer = coroom_proto::input::StreamFrames::new();
        buffer.extend(&frames[0]);
        let sealed = buffer.next_frame().unwrap().unwrap();
        let opened = host.open(&sealed, &mut out).unwrap();
        assert_eq!(opened.channel, Channel::Stream);
        let message = InputMessage::from_bytes(opened.plaintext).unwrap();
        assert_eq!(
            message.events,
            vec![InputEvent::Key {
                code: 30,
                pressed: true
            }]
        );
    }

    #[test]
    fn absolute_positions_and_axes_keep_the_last_value() {
        let stats = SenderStats::default();
        let mut batch = Batch::default();
        let now = Instant::now();
        batch.push(InputEvent::MouseMoveAbsolute { x: 1, y: 1 }, now, &stats);
        batch.push(InputEvent::MouseMoveAbsolute { x: 2, y: 2 }, now, &stats);
        batch.push(
            InputEvent::GamepadAxis {
                slot: 1,
                axis: crate::GamepadAxis::LeftX,
                value: 5,
            },
            now,
            &stats,
        );
        batch.push(
            InputEvent::GamepadAxis {
                slot: 1,
                axis: crate::GamepadAxis::LeftX,
                value: 6,
            },
            now,
            &stats,
        );
        assert_eq!(
            batch.events,
            vec![
                InputEvent::MouseMoveAbsolute { x: 2, y: 2 },
                InputEvent::GamepadAxis {
                    slot: 1,
                    axis: crate::GamepadAxis::LeftX,
                    value: 6
                }
            ]
        );
        let mut full = Batch::default();
        for _ in 0..MAX_EVENTS + 5 {
            full.push(InputEvent::MouseMove { dx: 1, dy: 2 }, now, &stats);
        }
        assert_eq!(full.events.len(), MAX_EVENTS);
        assert_eq!(
            full.events.last(),
            Some(&InputEvent::MouseMove { dx: 6, dy: 12 })
        );
        assert_eq!(stats.snapshot().merged, 5);
    }
}
