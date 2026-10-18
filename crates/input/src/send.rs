//! Envío de la entrada del invitado: agrupa, cifra y envía, con el estado completo en cada paquete.
//!
//! [`InputSender`] tiene un hilo `entrada` que recibe los eventos con su instante y lleva el
//! [`InputState`] del invitado (lo que hay pulsado y lo acumulado). Todo sale por **datagramas**
//! en [`InputPacket`](coroom_proto::input::InputPacket), con los eventos en orden y el estado tal
//! como queda tras ellos:
//!
//! - Un **cambio de estado** (tecla, botón, rueda, mando que se conecta, [`InputEvent::ReleaseAll`])
//!   sale al momento, con el movimiento que tuviera delante en el mismo paquete (un clic nunca
//!   adelanta al movimiento que lo precede).
//! - El **texto** sin tecla física ([`InputEvent::Text`]) no está en el estado: también sale al
//!   momento, como un cambio, y además va **repetido** en los datagramas siguientes (delante de
//!   lo nuevo, con su número de orden en `text_seq`) hasta que salió en tantos como el estado
//!   tras un cambio (el suyo y los de [`REPEATS`]) y pasó el último plazo de [`REPEATS`]. El
//!   anfitrión teclea cada número una vez y en orden. Si un datagrama lleno no deja sitio, los
//!   textos más antiguos dejan de repetirse antes (como mucho [`MAX_TEXT_REPEATS`] se reservan).
//! - El **movimiento** (ratón y ejes) se agrupa como mucho [`BATCH_INTERVAL`] (≤ 1000 datagramas/s);
//!   el primero tras una pausa sale al momento. Dentro del paquete los `MouseMove` se conservan uno
//!   a uno (el anfitrión los inyecta todos, en orden); las posiciones absolutas y los ejes se
//!   quedan con el último valor. Por encima de [`MAX_EVENTS`] eventos, los movimientos relativos se
//!   suman al último.
//! - Tras un cambio de estado, si no sale ningún otro paquete, el estado se **repite** a los
//!   plazos de [`REPEATS`]: perder el datagrama de una tecla no la deja pegada ni sin pulsar, el
//!   anfitrión lo arregla con el estado del siguiente ([`InputState::diff`]). Después, mientras
//!   no salga nada, el estado se sigue repitiendo cada [`KEEPALIVE_PRESSED`] si hay algo pulsado
//!   (una ráfaga de pérdidas que se lleve una liberación y sus repeticiones no deja nada pegado
//!   más que eso) y cada [`KEEPALIVE_IDLE`] si no (señal de vida: el anfitrión suelta lo de un
//!   invitado del que no sabe nada en `STALE_AFTER`).
//! - **Mandos tal cual** ([`PadData`]): la declaración del mando ([`PadData::Device`]) va por el
//!   stream fiable (su descriptor no cabe en un datagrama); los informes HID ([`PadData::Report`])
//!   van con el movimiento, todos en orden (un informe puede ser la única pulsación de un botón),
//!   hasta [`MAX_PADS`] o [`MAX_PAD_DATAGRAM_BYTES`] por datagrama.
//!
//! Cada paquete lleva `sent_us`, la hora de su primer evento en el reloj del anfitrión
//! ([`InputLink::remote_clock_us`]), y `queued_us`, lo que ese evento esperó aquí hasta salir: el
//! anfitrión mide así cada etapa. Se cifra con [`InputCipher`] (canal y secuencia propios de cada
//! canal) y sale por el [`InputLink`] que da la sesión (la conexión QUIC).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use coroom_crypto::input::{Channel, InputCipher};
use coroom_proto::input::{
    InputMessage, InputPacketRef, InputState, KEEPALIVE_IDLE, KEEPALIVE_PRESSED, MAX_EVENTS,
    MAX_PACKET_BYTES, MAX_PADS, PadData, datagram_header, stream_frame,
};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use tracing::debug;

use crate::{InputError, InputEvent};

/// Intervalo mínimo entre dos datagramas de movimiento.
pub const BATCH_INTERVAL: Duration = Duration::from_millis(1);

/// Plazos, desde que salió el último cambio de estado, a los que se repite el estado si no salió
/// otro paquete: cubren una pérdida aislada y una ráfaga corta sin esperar al siguiente
/// movimiento.
pub const REPEATS: [Duration; 3] = [
    Duration::from_millis(3),
    Duration::from_millis(8),
    Duration::from_millis(20),
];

/// Textos repetidos para los que un datagrama reserva sitio, como mucho (la mitad de
/// [`MAX_EVENTS`]: los eventos nuevos siempre caben).
pub const MAX_TEXT_REPEATS: usize = MAX_EVENTS / 2;

/// Bytes de informes de mando por datagrama, como mucho (con el estado y lo demás, por debajo del
/// MTU).
pub const MAX_PAD_DATAGRAM_BYTES: usize = 800;

/// Lo que llega al hilo `entrada`.
#[derive(Debug)]
enum Item {
    Event(InputEvent),
    Pad(PadData),
}

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
    repeats: AtomicU64,
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
    /// Datagramas (con eventos, informes o solo el estado).
    pub datagrams: u64,
    /// Datagramas que solo repiten el estado.
    pub repeats: u64,
    /// Mensajes del stream (declaraciones de mandos).
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
            repeats: load(&self.repeats),
            messages: load(&self.messages),
            bytes: load(&self.bytes),
            merged: load(&self.merged),
            errors: load(&self.errors),
        }
    }
}

/// Lo pendiente de salir en el siguiente datagrama.
#[derive(Debug, Default)]
struct Batch {
    events: Vec<InputEvent>,
    pads: Vec<PadData>,
    pad_bytes: usize,
    first: Option<Instant>,
    /// Hay un cambio de estado: sale ya.
    urgent: bool,
    /// Sitios que se guardan para los textos que se repiten.
    reserved: usize,
}

impl Batch {
    fn is_empty(&self) -> bool {
        self.events.is_empty() && self.pads.is_empty()
    }

    /// Vacía el lote conservando la memoria de sus vectores (el siguiente no reserva).
    fn clear(&mut self) {
        self.events.clear();
        self.pads.clear();
        self.pad_bytes = 0;
        self.first = None;
        self.urgent = false;
    }

    /// Si un informe más de `len` bytes ya no cabe en este datagrama.
    fn pads_full(&self, len: usize) -> bool {
        self.pads.len() >= MAX_PADS || self.pad_bytes + len > MAX_PAD_DATAGRAM_BYTES
    }

    /// Si un evento más que no se funde con otro ya no cabe.
    fn events_full(&self) -> bool {
        self.events.len() + self.reserved >= MAX_EVENTS
    }

    fn push_pad(&mut self, pad: PadData, at: Instant) {
        self.first.get_or_insert(at);
        if let PadData::Report { data, .. } = &pad {
            self.pad_bytes += data.len();
        }
        self.pads.push(pad);
    }

    /// Añade `event` (que ya se aplicó al estado). Lo continuo se funde con el último de su clase;
    /// lo demás marca el paquete como urgente. Devuelve `false` si no cabía (hay que enviar antes).
    fn push(&mut self, event: InputEvent, at: Instant, stats: &SenderStats) -> bool {
        match event {
            InputEvent::MouseMoveAbsolute { .. } => {
                if let Some(last) = self
                    .events
                    .iter_mut()
                    .rev()
                    .find(|e| matches!(e, InputEvent::MouseMoveAbsolute { .. }))
                {
                    self.first.get_or_insert(at);
                    *last = event;
                    return true;
                }
            }
            InputEvent::GamepadAxis { slot, axis, .. } => {
                if let Some(last) = self.events.iter_mut().rev().find(|e| {
                    matches!(e, InputEvent::GamepadAxis { slot: s, axis: a, .. } if *s == slot && *a == axis)
                }) {
                    self.first.get_or_insert(at);
                    *last = event;
                    return true;
                }
            }
            InputEvent::MouseMove { dx, dy } if self.events_full() => {
                if let Some(InputEvent::MouseMove { dx: ldx, dy: ldy }) = self
                    .events
                    .iter_mut()
                    .rev()
                    .find(|e| matches!(e, InputEvent::MouseMove { .. }))
                {
                    *ldx = ldx.saturating_add(dx);
                    *ldy = ldy.saturating_add(dy);
                    stats.merged.fetch_add(1, Ordering::Relaxed);
                    return true;
                }
            }
            _ => {}
        }
        if self.events_full() {
            return false;
        }
        self.first.get_or_insert(at);
        self.urgent |= !event.is_continuous();
        self.events.push(event);
        true
    }
}

struct Worker {
    link: Arc<dyn InputLink>,
    cipher: Arc<Mutex<InputCipher>>,
    stats: Arc<SenderStats>,
    /// El paquete en claro, serializado aquí sin reservar memoria.
    plain: Box<[u8; MAX_PACKET_BYTES]>,
    /// Cabecera y sellado del datagrama; se entrega al enlace tal cual (una sola reserva por
    /// datagrama) y se vuelve a reservar con la misma capacidad.
    sealed: Vec<u8>,
    state: InputState,
    /// Último cambio de estado y cuántos paquetes han salido desde entonces (cada uno lleva el
    /// estado, así que cuenta como repetición).
    changed_at: Option<Instant>,
    repeated: usize,
    /// El cambio aún no salió: su propio paquete no cuenta como repetición.
    carrying_change: bool,
    /// Cuándo salió el último datagrama (para el *keepalive*).
    last_sent: Option<Instant>,
    /// Textos que aún se repiten, el más antiguo delante: carácter, en cuántos datagramas salió
    /// y cuándo salió el primero. Los que aún no salieron son los del lote, en su orden.
    texts: VecDeque<PendingText>,
    /// Número de orden del primero de `texts`.
    text_first: u16,
    /// Los eventos del datagrama: los textos repetidos y los del lote (reutilizado).
    outgoing: Vec<InputEvent>,
}

/// Un texto que se repite ([`Worker::texts`]).
#[derive(Debug, Clone, Copy)]
struct PendingText {
    ch: char,
    sent: usize,
    first_sent: Option<Instant>,
}

impl PendingText {
    /// Ya salió en tantos datagramas como el estado tras un cambio y pasó el último plazo de
    /// [`REPEATS`].
    fn done(&self, now: Instant) -> bool {
        let last = REPEATS[REPEATS.len() - 1];
        self.sent > REPEATS.len()
            && self
                .first_sent
                .is_some_and(|first| now.saturating_duration_since(first) >= last)
    }
}

impl Worker {
    fn remote_us(&self, at: Instant) -> u32 {
        self.link
            .remote_clock_us(at)
            // 0 significa "sin hora": un 0 de verdad se manda como 1 µs.
            .map_or(0, |us| (us as u32).max(1))
    }

    /// Envía lo pendiente (o solo el estado, si no hay nada) como un datagrama.
    fn send_batch(&mut self, batch: &mut Batch, now: Instant) {
        let first = batch.first;
        let sent_us = self.remote_us(first.unwrap_or(now));
        let queued_us = first.map_or(0, |first| {
            u16::try_from(now.saturating_duration_since(first).as_micros()).unwrap_or(u16::MAX)
        });
        let repeat = batch.is_empty();
        let count = (batch.events.len() + batch.pads.len()) as u64;
        // Los textos que ya salieron van delante, repetidos; si no caben todos, se dejan de
        // repetir los más antiguos.
        let resent = self.texts.iter().take_while(|text| text.sent > 0).count();
        let dropped = resent.saturating_sub(MAX_EVENTS - batch.events.len());
        for _ in 0..dropped {
            self.texts.pop_front();
            self.text_first = self.text_first.wrapping_add(1);
        }
        self.outgoing.clear();
        self.outgoing.extend(
            self.texts
                .iter()
                .take(resent - dropped)
                .map(|text| InputEvent::Text { ch: text.ch }),
        );
        self.outgoing.extend_from_slice(&batch.events);
        let mut cipher = self.cipher.lock().unwrap_or_else(PoisonError::into_inner);
        let seq = cipher.next_seq(Channel::Datagram);
        let Ok(seq32) = u32::try_from(seq) else {
            self.stats.errors.fetch_add(1, Ordering::Relaxed);
            batch.clear();
            return;
        };
        let packet = InputPacketRef {
            seq: seq32,
            sent_us,
            queued_us,
            events: &self.outgoing,
            text_seq: self.text_first,
            state: &self.state,
            pads: &batch.pads,
        };
        // Sin reservar: el paquete se serializa en `plain` y se sella en `sealed`.
        let sealed = match packet.write_to(&mut self.plain[..]) {
            Ok(plain) => {
                self.sealed.clear();
                self.sealed
                    .extend_from_slice(&datagram_header(cipher.key_id()));
                cipher
                    .seal(Channel::Datagram, seq, plain, &mut self.sealed)
                    .map_err(|err| err.to_string())
            }
            Err(err) => Err(err.to_string()),
        };
        drop(cipher);
        batch.clear();
        // Un datagrama más para cada texto (también si no se pudo enviar: cuenta como perdido).
        for text in &mut self.texts {
            text.sent += 1;
            text.first_sent.get_or_insert(now);
        }
        while self.texts.front().is_some_and(|text| text.done(now)) {
            self.texts.pop_front();
            self.text_first = self.text_first.wrapping_add(1);
        }
        batch.reserved = self.texts.len().min(MAX_TEXT_REPEATS);
        let result = sealed.and_then(|()| {
            let bytes = self.sealed.len() as u64;
            // El enlace se queda con el vector (quinn lo envuelve en `Bytes` sin copiar); aquí
            // se reserva otro igual para el siguiente.
            let capacity = self.sealed.capacity();
            let datagram = std::mem::replace(&mut self.sealed, Vec::with_capacity(capacity));
            self.link.send_datagram(datagram).map(|()| bytes)
        });
        match result {
            Ok(bytes) => {
                self.stats.datagrams.fetch_add(1, Ordering::Relaxed);
                self.stats.events.fetch_add(count, Ordering::Relaxed);
                self.stats.bytes.fetch_add(bytes, Ordering::Relaxed);
                if repeat {
                    self.stats.repeats.fetch_add(1, Ordering::Relaxed);
                }
            }
            Err(err) => {
                debug!(error = %err, "no se pudo enviar un datagrama de entrada");
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.last_sent = Some(now);
        if self.carrying_change {
            // Las repeticiones se cuentan desde que el cambio salió, no desde que ocurrió: si
            // este hilo llegó tarde, no salen todas seguidas.
            self.carrying_change = false;
            self.changed_at = Some(now);
        } else if self.changed_at.is_some() {
            self.repeated += 1;
            if self.repeated >= REPEATS.len() {
                self.changed_at = None;
            }
        }
    }

    /// Declara un mando por el stream.
    fn send_message(&mut self, pads: Vec<PadData>) {
        let count = pads.len() as u64;
        let mut cipher = self.cipher.lock().unwrap_or_else(PoisonError::into_inner);
        let seq = cipher.next_seq(Channel::Stream);
        let Ok(seq32) = u32::try_from(seq) else {
            self.stats.errors.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let message = InputMessage { seq: seq32, pads };
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

    /// Mete `item` en `batch`, enviando antes si no cabe. Un evento que cambia el estado reinicia
    /// las repeticiones.
    fn accept(&mut self, batch: &mut Batch, at: Instant, item: Item) {
        match item {
            Item::Event(event) => {
                self.state.apply(&event);
                if !event.is_continuous() {
                    self.changed_at = Some(at);
                    self.repeated = 0;
                    self.carrying_change = true;
                }
                if !batch.push(event, at, &self.stats) {
                    self.send_batch(batch, Instant::now());
                    batch.push(event, at, &self.stats);
                }
                if let InputEvent::Text { ch } = event {
                    self.texts.push_back(PendingText {
                        ch,
                        sent: 0,
                        first_sent: None,
                    });
                }
            }
            Item::Pad(pad @ PadData::Report { .. }) => {
                let len = match &pad {
                    PadData::Report { data, .. } => data.len(),
                    PadData::Device { .. } => 0,
                };
                if batch.pads_full(len) {
                    self.send_batch(batch, Instant::now());
                }
                batch.push_pad(pad, at);
            }
            Item::Pad(pad @ PadData::Device { .. }) => self.send_message(vec![pad]),
        }
    }

    /// Cuándo toca repetir el estado, si queda alguna repetición.
    fn next_repeat(&self) -> Option<Instant> {
        let changed = self.changed_at?;
        REPEATS.get(self.repeated).map(|delay| changed + *delay)
    }

    /// Cuándo toca el *keepalive*: [`KEEPALIVE_PRESSED`] desde el último datagrama si hay algo
    /// pulsado, [`KEEPALIVE_IDLE`] si no. Antes del primer datagrama, nunca.
    fn next_keepalive(&self) -> Option<Instant> {
        let period = if self.state.any_pressed() {
            KEEPALIVE_PRESSED
        } else {
            KEEPALIVE_IDLE
        };
        self.last_sent.map(|at| at + period)
    }

    /// Lo primero que toca: una repetición o el *keepalive*.
    fn next_resend(&self) -> Option<Instant> {
        match (self.next_repeat(), self.next_keepalive()) {
            (Some(repeat), Some(keepalive)) => Some(repeat.min(keepalive)),
            (repeat, keepalive) => repeat.or(keepalive),
        }
    }

    fn run(mut self, rx: &Receiver<(Instant, Item)>) {
        let mut batch = Batch::default();
        // Hace tiempo que no sale nada: el primer movimiento sale al momento.
        let mut last_batch: Option<Instant> = None;
        loop {
            let now = Instant::now();
            let due = last_batch.map(|at| at + BATCH_INTERVAL);
            let mut wake = self.next_resend();
            if !batch.is_empty() {
                let batch_due = if batch.urgent {
                    now
                } else {
                    due.unwrap_or(now)
                };
                wake = Some(wake.map_or(batch_due, |wake| wake.min(batch_due)));
            }
            let wait = wake.map_or(Duration::from_millis(250), |wake| {
                wake.saturating_duration_since(now)
            });
            match rx.recv_timeout(wait) {
                Ok((at, item)) => {
                    self.accept(&mut batch, at, item);
                    // Lo que ya esté en cola va en el mismo paquete.
                    while let Ok((at, item)) = rx.try_recv() {
                        self.accept(&mut batch, at, item);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    if !batch.is_empty() {
                        self.send_batch(&mut batch, Instant::now());
                    }
                    return;
                }
            }
            let now = Instant::now();
            if !batch.is_empty() {
                if batch.urgent || due.is_none_or(|due| now >= due) {
                    last_batch = Some(now);
                    self.send_batch(&mut batch, now);
                }
            } else if self.next_resend().is_some_and(|at| at <= now) {
                last_batch = Some(now);
                self.send_batch(&mut batch, now);
            }
        }
    }
}

/// Emisor de la entrada de un invitado.
pub struct InputSender {
    tx: Option<Sender<(Instant, Item)>>,
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

/// Pide al sistema temporizadores de 1 ms, una vez por proceso. Los plazos de [`REPEATS`] son de
/// 3, 8 y 20 ms y en Windows el reloj del planificador va por defecto a 15,6 ms: sin esto, la
/// espera del hilo de entrada (`recv_timeout`) se alargaba hasta ahí y la repetición de 8 ms
/// salía a los 23. Windows 11 ignora la petición si el proceso no tiene ventana (o está oculta),
/// salvo que se exima de esa regulación con `ProcessPowerThrottling`. El transporte pide lo
/// mismo al abrir su socket; aquí se pide igual para no depender de que se haya abierto antes (y
/// para que los tests del crate midan lo real).
// Únicas llamadas `unsafe` de este módulo: la API de Windows, con su invariante en el `SAFETY`.
#[allow(unsafe_code)]
fn fine_timers() {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::System::Threading::{
            GetCurrentProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION, PROCESS_POWER_THROTTLING_STATE,
            ProcessPowerThrottling, SetProcessInformation,
        };

        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let state = PROCESS_POWER_THROTTLING_STATE {
                Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
                ControlMask: PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
                StateMask: 0,
            };
            // SAFETY: `GetCurrentProcess` devuelve un pseudo-identificador que no hay que cerrar;
            // `state` es un `PROCESS_POWER_THROTTLING_STATE` válido que vive durante la llamada y
            // su tamaño es el que se pasa. Los Windows anteriores a 11 rechazan el bit con un
            // error, que solo se anota.
            let throttling = unsafe {
                SetProcessInformation(
                    GetCurrentProcess(),
                    ProcessPowerThrottling,
                    std::ptr::from_ref(&state).cast(),
                    std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
                )
            };
            if let Err(err) = throttling {
                debug!(error = %err, "sin exención de la regulación de temporizadores");
            }
            // SAFETY: `timeBeginPeriod` solo recibe el periodo en milisegundos y no toca memoria
            // nuestra; el periodo queda pedido hasta que el proceso termina.
            let result = unsafe { windows::Win32::Media::timeBeginPeriod(1) };
            if result != 0 {
                tracing::warn!(
                    result,
                    "no se pudo pedir resolución de 1 ms a los temporizadores"
                );
            }
        });
    }
}

impl InputSender {
    /// Arranca el hilo `entrada` sobre `link` con las claves de `cipher` (del lado del invitado).
    pub fn spawn(
        link: Arc<dyn InputLink>,
        cipher: Arc<Mutex<InputCipher>>,
    ) -> Result<InputSender, InputError> {
        fine_timers();
        let (tx, rx) = unbounded();
        let stats = Arc::new(SenderStats::default());
        let worker = Worker {
            link,
            cipher,
            stats: Arc::clone(&stats),
            plain: Box::new([0u8; MAX_PACKET_BYTES]),
            sealed: Vec::with_capacity(256),
            state: InputState::default(),
            changed_at: None,
            repeated: 0,
            carrying_change: false,
            last_sent: None,
            texts: VecDeque::new(),
            text_first: 0,
            outgoing: Vec::with_capacity(MAX_EVENTS),
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
            let _ = tx.send((at, Item::Event(event)));
        }
    }

    /// Envía datos de un mando tal cual (la declaración por el stream, los informes por
    /// datagrama).
    pub fn send_pad(&self, pad: PadData) {
        if let Some(tx) = &self.tx {
            let _ = tx.send((Instant::now(), Item::Pad(pad)));
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
    use coroom_proto::input::{InputPacket, parse_datagram};
    use coroom_testkit::timing_assert;
    use std::collections::BTreeMap;

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

    fn pair() -> (Arc<Captured>, InputSender, InputCipher) {
        let link = Arc::new(Captured::default());
        let guest = InputCipher::from_pairwise(&secret(), Role::Guest).unwrap();
        let host = InputCipher::from_pairwise(&secret(), Role::Host).unwrap();
        let sender = InputSender::spawn(
            Arc::clone(&link) as Arc<dyn InputLink>,
            Arc::new(Mutex::new(guest)),
        )
        .unwrap();
        (link, sender, host)
    }

    /// Abre los datagramas capturados, comprobando la clave, el canal y la secuencia seguida.
    fn open_all(link: &Captured, host: &mut InputCipher) -> Vec<(Instant, InputPacket)> {
        let datagrams = link.datagrams.lock().unwrap();
        let mut out = Vec::new();
        let mut last_seq = None;
        datagrams
            .iter()
            .map(|(at, datagram)| {
                let (key_id, sealed) = parse_datagram(datagram).unwrap();
                assert_eq!(key_id, host.key_id());
                let opened = host.open(sealed, &mut out).unwrap();
                assert_eq!(opened.channel, Channel::Datagram);
                let packet = InputPacket::from_bytes(opened.plaintext).unwrap();
                assert_eq!(u64::from(packet.seq), opened.seq);
                assert!(last_seq.is_none_or(|last| packet.seq == last + 1));
                last_seq = Some(packet.seq);
                (*at, packet)
            })
            .collect()
    }

    /// Si los movimientos `1, 2, 3…` salieron en orden aunque se sumaran algunos (el emisor solo
    /// funde uno con el último pendiente): la suma tras cada uno recibido es `n(n+1)/2`, con `n`
    /// creciente.
    fn assert_triangular_prefixes(moves: &[i32]) {
        let mut sum = 0i64;
        let mut last_n = 0i64;
        for (index, dx) in moves.iter().enumerate() {
            sum += i64::from(*dx);
            // n(n+1)/2 = sum → n = (√(8·sum+1) − 1)/2.
            let n = ((((8 * sum + 1) as f64).sqrt() - 1.0) / 2.0).round() as i64;
            assert!(
                n * (n + 1) / 2 == sum && n > last_n,
                "movimiento {index}: suma {sum} no sigue a la anterior (n = {last_n}): {moves:?}"
            );
            last_n = n;
        }
    }

    #[test]
    fn moves_are_batched_and_a_key_goes_at_once_with_the_state() {
        let (link, mut sender, mut host) = pair();
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
        assert_eq!(stats.messages, 0, "nada va por el stream");
        // Si el hilo del emisor se queda parado, los movimientos se suman en un paquete: no se
        // pierde nada (lo comprueba la suma de abajo), pero es cosa del planificador.
        timing_assert!(
            stats.merged == 0,
            "{} movimientos sumados a otro",
            stats.merged
        );

        let packets = open_all(&link, &mut host);
        // ≤ 1000 paquetes/s: con 200 ms de movimiento, unos 200 (más alguna repetición). Más que
        // eventos y repeticiones no puede haber; menos, si el emisor juntó movimientos.
        assert!(
            (1..=215).contains(&packets.len()),
            "{} datagramas",
            packets.len()
        );
        timing_assert!(
            packets.len() >= 100,
            "solo {} datagramas en 200 ms de movimiento a 1000 Hz",
            packets.len()
        );
        let gaps: Vec<Duration> = packets
            .windows(2)
            .map(|pair| pair[1].0 - pair[0].0)
            .collect();
        let fast = gaps
            .iter()
            .filter(|gap| **gap < Duration::from_micros(700))
            .count();
        timing_assert!(
            fast <= 3,
            "{fast} datagramas a menos de 0,7 ms del anterior"
        );

        let mut moves = Vec::new();
        let mut key_seen = false;
        for (_, packet) in &packets {
            assert_eq!(packet.sent_us, 1234);
            for event in &packet.events {
                match event {
                    InputEvent::MouseMove { dx, .. } => moves.push(*dx),
                    InputEvent::Key { code: 30, pressed } => {
                        assert!(*pressed);
                        key_seen = true;
                    }
                    other => panic!("evento inesperado {other:?}"),
                }
            }
            assert_eq!(packet.state.keys, if key_seen { vec![30] } else { vec![] });
            assert_eq!(
                packet.state.motion.0,
                moves.iter().sum::<i32>(),
                "el acumulado sigue a los eventos"
            );
        }
        // Funcional: no se pierde ni sobra movimiento, y la tecla salió.
        assert_eq!(moves.iter().sum::<i32>(), 20_100, "suma de los movimientos");
        assert_eq!(
            packets.last().map(|(_, packet)| packet.state.motion.0),
            Some(20_100),
            "el acumulado final"
        );
        assert!(key_seen);
        // En orden y sin perder ninguno aunque el emisor parado sume alguno; que salgan uno a uno
        // depende del planificador.
        assert_triangular_prefixes(&moves);
        timing_assert!(
            moves == (1..=200).collect::<Vec<_>>(),
            "los movimientos no salieron uno a uno: {} paquetes de movimiento",
            moves.len()
        );
        assert!(link.frames.lock().unwrap().is_empty());
    }

    #[test]
    fn a_state_change_is_repeated_when_nothing_else_goes_out() {
        let (link, mut sender, mut host) = pair();
        // Antes de mandar la tecla: el emisor cuenta los plazos desde que sale su paquete, que es
        // después de esto (y antes de que el enlace lo apunte).
        let pressed_at = Instant::now();
        sender.send(InputEvent::Key {
            code: 30,
            pressed: true,
        });
        std::thread::sleep(Duration::from_millis(60));
        // Al menos una repetición antes del movimiento, aunque el emisor se haya quedado parado.
        let deadline = Instant::now() + Duration::from_secs(20);
        while sender.stats().repeats == 0 {
            assert!(Instant::now() < deadline, "el estado no se repite");
            std::thread::sleep(Duration::from_millis(1));
        }
        sender.send(InputEvent::MouseMove { dx: 1, dy: 0 });
        std::thread::sleep(Duration::from_millis(60));
        sender.stop();
        let packets = open_all(&link, &mut host);
        assert_eq!(
            packets[0].1.events,
            vec![InputEvent::Key {
                code: 30,
                pressed: true
            }]
        );
        let moved = packets
            .iter()
            .position(|(_, packet)| {
                packet
                    .events
                    .iter()
                    .any(|event| matches!(event, InputEvent::MouseMove { .. }))
            })
            .expect("sale el movimiento");
        assert_eq!(
            packets[moved].1.events,
            vec![InputEvent::MouseMove { dx: 1, dy: 0 }]
        );
        assert_eq!(packets[moved].1.state.motion, (1, 0));
        assert!(
            moved >= 2,
            "ninguna repetición antes del movimiento: {packets:?}"
        );
        // Lo demás son repeticiones del estado: sin eventos y con la tecla pulsada.
        for (index, (_, packet)) in packets.iter().enumerate().skip(1) {
            if index != moved {
                assert!(packet.events.is_empty(), "repetición {index}: {packet:?}");
            }
            assert_eq!(packet.state.keys, vec![30]);
        }
        for (index, (at, _)) in packets
            .iter()
            .enumerate()
            .take(moved.min(REPEATS.len() + 1))
            .skip(1)
        {
            let since = *at - packets[0].0;
            let expected = REPEATS[index - 1];
            // Desde `pressed_at` es una cota segura: un parón al sellar el paquete de la tecla
            // retrasa `packets[0].0`, no el plazo, y acortaría `since`.
            let since_pressed = *at - pressed_at;
            assert!(
                since_pressed >= expected,
                "repetición {index} antes de tiempo, a los {since_pressed:?} de la tecla (se \
                 esperaba {expected:?})"
            );
            timing_assert!(
                since < expected + Duration::from_millis(15),
                "repetición {index} a los {since:?} (se esperaba {expected:?})"
            );
        }
        // La tecla, sus tres repeticiones y el movimiento (que no es cambio de estado: sin
        // repeticiones).
        timing_assert!(
            packets.len() == 1 + REPEATS.len() + 1 && moved == REPEATS.len() + 1,
            "{} datagramas, el movimiento en el {moved}: {packets:?}",
            packets.len()
        );
        let repeats = sender.stats().repeats;
        timing_assert!(
            repeats == REPEATS.len() as u64,
            "{repeats} repeticiones, se esperaban {}",
            REPEATS.len()
        );
    }

    /// Un texto sale al momento y se repite en los datagramas siguientes con su número de orden,
    /// como mínimo en tantos como el estado tras un cambio; el siguiente texto lleva el número
    /// siguiente. Así el anfitrión lo teclea aunque se pierda el datagrama original.
    #[test]
    fn texts_are_repeated_with_their_numbers() {
        let (link, mut sender, mut host) = pair();
        sender.send(InputEvent::Text { ch: 'ñ' });
        // Hasta que salgan las repeticiones del estado tras el texto.
        let deadline = Instant::now() + Duration::from_secs(20);
        while sender.stats().repeats < REPEATS.len() as u64 {
            assert!(Instant::now() < deadline, "el texto no se repite");
            std::thread::sleep(Duration::from_millis(1));
        }
        sender.send(InputEvent::Text { ch: 'a' });
        sender.send(InputEvent::Key {
            code: 30,
            pressed: true,
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        while sender.stats().repeats < 2 * REPEATS.len() as u64 {
            assert!(Instant::now() < deadline, "el segundo texto no se repite");
            std::thread::sleep(Duration::from_millis(1));
        }
        sender.stop();
        let packets = open_all(&link, &mut host);
        // Cada número de orden es siempre el mismo carácter, y los textos de un paquete son
        // consecutivos.
        let mut by_seq = BTreeMap::new();
        let mut copies = BTreeMap::new();
        for (_, packet) in &packets {
            let texts = packet.events.iter().filter_map(|event| match event {
                InputEvent::Text { ch } => Some(*ch),
                _ => None,
            });
            for (index, ch) in texts.enumerate() {
                let seq = packet.text_seq.wrapping_add(index as u16);
                assert_eq!(*by_seq.entry(seq).or_insert(ch), ch, "{packets:?}");
                *copies.entry(seq).or_insert(0usize) += 1;
            }
        }
        assert_eq!(
            by_seq.into_iter().collect::<Vec<_>>(),
            vec![(0, 'ñ'), (1, 'a')]
        );
        for (seq, count) in copies {
            assert!(
                count > REPEATS.len(),
                "el texto {seq} salió en {count} datagramas: {packets:?}"
            );
        }
        // El primero sale solo; la «a», delante de la tecla que se pulsó después.
        assert_eq!(packets[0].1.events, vec![InputEvent::Text { ch: 'ñ' }]);
        let with_key = packets
            .iter()
            .find(|(_, packet)| {
                packet
                    .events
                    .iter()
                    .any(|e| matches!(e, InputEvent::Key { .. }))
            })
            .expect("sale la tecla");
        let a = with_key
            .1
            .events
            .iter()
            .position(|e| *e == InputEvent::Text { ch: 'a' })
            .expect("la «a» con la tecla");
        let key = with_key
            .1
            .events
            .iter()
            .position(|e| matches!(e, InputEvent::Key { .. }))
            .expect("tecla");
        assert!(a < key, "{with_key:?}");
    }

    /// Con una tecla pulsada, pasadas las repeticiones cortas el estado sigue saliendo cada
    /// [`KEEPALIVE_PRESSED`]; al soltarla, nada hasta [`KEEPALIVE_IDLE`].
    #[test]
    fn the_state_is_kept_alive_while_something_is_pressed() {
        let (link, mut sender, mut host) = pair();
        let start = Instant::now();
        sender.send(InputEvent::Key {
            code: 30,
            pressed: true,
        });
        // Tres repeticiones cortas (hasta 20 ms) y luego keepalives a los ~120 y ~220 ms; se
        // suelta cuando ha salido al menos un keepalive (con un emisor parado, más tarde).
        let sent = || link.datagrams.lock().unwrap().len();
        std::thread::sleep(Duration::from_millis(250));
        let deadline = Instant::now() + Duration::from_secs(20);
        while sent() < 2 + REPEATS.len() {
            assert!(
                Instant::now() < deadline,
                "no sale ningún keepalive: {}",
                sent()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        // Antes de mandarla: todo paquete sin la tecla sale después.
        let released = Instant::now();
        sender.send(InputEvent::Key {
            code: 30,
            pressed: false,
        });
        // La liberación y sus tres repeticiones (hasta 20 ms); el margen, para ver que no sale
        // nada más antes del keepalive en reposo (500 ms).
        std::thread::sleep(Duration::from_millis(300));
        // Paquetes sin la tecla desde `released` (con un descifrador nuevo en cada vuelta: el de
        // la prueba no puede abrir dos veces el mismo número).
        let released_packets = || {
            let mut host = InputCipher::from_pairwise(&secret(), Role::Host).unwrap();
            open_all(&link, &mut host)
                .into_iter()
                .filter(|(at, packet)| *at >= released && packet.state.keys.is_empty())
                .collect::<Vec<_>>()
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        while released_packets().len() < 1 + REPEATS.len() {
            assert!(
                Instant::now() < deadline,
                "la liberación no se repite: {:?}",
                released_packets()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        sender.stop();
        let packets = open_all(&link, &mut host);
        let pressed: Vec<&(Instant, InputPacket)> = packets
            .iter()
            .filter(|(_, packet)| packet.state.keys == vec![30])
            .collect();
        assert!(
            pressed.len() > REPEATS.len(),
            "{} paquetes con la tecla pulsada",
            pressed.len()
        );
        timing_assert!(
            (5..=7).contains(&pressed.len()),
            "{} paquetes con la tecla pulsada",
            pressed.len()
        );
        let keepalives = pressed
            .iter()
            .filter(|(at, _)| *at - start > REPEATS[2] + Duration::from_millis(30))
            .count();
        assert!(keepalives >= 1, "ningún keepalive con la tecla pulsada");
        timing_assert!((1..=3).contains(&keepalives), "{keepalives} keepalives");
        for (index, pair) in pressed.windows(2).enumerate() {
            let gap = pair[1].0 - pair[0].0;
            timing_assert!(
                gap <= KEEPALIVE_PRESSED + Duration::from_millis(40),
                "hueco {index} de {gap:?} con la tecla pulsada"
            );
        }
        // Tras soltarla: el paquete de la liberación, sus tres repeticiones y nada más antes del
        // keepalive en reposo. Las repeticiones no se saltan aunque el emisor se pare (salen
        // tarde, una tras otra) y el keepalive se cuenta desde la última, que salió después de
        // `released`: sale como pronto a `released + KEEPALIVE_IDLE`, pase lo que pase.
        let empty: Vec<&(Instant, InputPacket)> = packets
            .iter()
            .filter(|(at, packet)| *at >= released && packet.state.keys.is_empty())
            .collect();
        assert!(empty.len() > REPEATS.len(), "{empty:?}");
        assert_eq!(
            empty[0].1.events,
            vec![InputEvent::Key {
                code: 30,
                pressed: false
            }],
            "el primero sin la tecla es el de la liberación: {empty:?}"
        );
        assert!(
            empty[1..=REPEATS.len()]
                .iter()
                .all(|(_, packet)| packet.events.is_empty()),
            "luego, sus repeticiones: {empty:?}"
        );
        assert!(
            empty[1 + REPEATS.len()..]
                .iter()
                .all(|(at, _)| *at - released >= KEEPALIVE_IDLE),
            "sale algo más antes del keepalive en reposo: {empty:?}"
        );
        timing_assert!(
            empty
                .iter()
                .filter(|(at, _)| *at - released < KEEPALIVE_IDLE / 2)
                .count()
                == 1 + REPEATS.len(),
            "la liberación y sus repeticiones no salieron en {:?}: {empty:?}",
            KEEPALIVE_IDLE / 2
        );
        // Un keepalive con la tecla que saliera a la vez que se soltaba puede caer detrás de
        // `released`; el último paquete ya va sin ella.
        let after: Vec<&(Instant, InputPacket)> =
            packets.iter().filter(|(at, _)| *at >= released).collect();
        assert!(after.len() > REPEATS.len(), "{after:?}");
        assert!(
            after.last().unwrap().1.state.keys.is_empty(),
            "la tecla sigue pulsada: {after:?}"
        );
        timing_assert!(
            after.len() == 1 + REPEATS.len()
                && after.iter().all(|(_, packet)| packet.state.keys.is_empty()),
            "tras soltar: {after:?}"
        );
        let last = after.last().unwrap().0 - released;
        timing_assert!(
            last < Duration::from_millis(100),
            "la última repetición de la liberación a los {last:?}"
        );
    }

    #[test]
    fn a_pad_declaration_goes_on_the_stream_and_reports_by_datagram() {
        let (link, mut sender, mut host) = pair();
        let device = coroom_proto::input::GamepadDevice {
            name: "Mando".to_owned(),
            vendor: 1,
            product: 2,
            version: 3,
            bus: 3,
            transport: coroom_proto::input::GamepadTransport::XInput,
        };
        sender.send_pad(PadData::Device { slot: 1, device });
        sender.send_pad(PadData::Report {
            slot: 1,
            data: vec![1, 2, 3],
        });
        std::thread::sleep(Duration::from_millis(20));
        sender.stop();
        let frames = link.frames.lock().unwrap();
        assert_eq!(frames.len(), 1);
        let mut buffer = coroom_proto::input::StreamFrames::new();
        buffer.extend(&frames[0]);
        let sealed = buffer.next_frame().unwrap().unwrap();
        let mut out = Vec::new();
        let opened = host.open(&sealed, &mut out).unwrap();
        assert_eq!(opened.channel, Channel::Stream);
        let message = InputMessage::from_bytes(opened.plaintext).unwrap();
        assert!(matches!(
            message.pads[..],
            [PadData::Device { slot: 1, .. }]
        ));
        drop(frames);
        let packets = open_all(&link, &mut host);
        assert_eq!(packets.len(), 1);
        assert_eq!(
            packets[0].1.pads,
            vec![PadData::Report {
                slot: 1,
                data: vec![1, 2, 3]
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
        assert!(!batch.urgent);
        let mut full = Batch::default();
        for _ in 0..MAX_EVENTS + 5 {
            assert!(full.push(InputEvent::MouseMove { dx: 1, dy: 2 }, now, &stats));
        }
        assert_eq!(full.events.len(), MAX_EVENTS);
        assert_eq!(
            full.events.last(),
            Some(&InputEvent::MouseMove { dx: 6, dy: 12 })
        );
        assert_eq!(stats.snapshot().merged, 5);
        // Una tecla no se funde: con el paquete lleno hay que enviar antes.
        assert!(!full.push(
            InputEvent::Key {
                code: 1,
                pressed: true
            },
            now,
            &stats
        ));
    }
}
