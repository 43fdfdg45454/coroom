//! Buffer de jitter adaptativo por emisor.
//!
//! Guarda los paquetes Opus de un emisor por número de secuencia (`seq` de la cabecera de audio,
//! que cuenta los paquetes **enviados**, sin huecos aunque el emisor calle) en [`SLOTS`] huecos
//! reservados al crearlo: no asigna nada después. Cada paquete lleva una o dos **tramas** de
//! 10 ms (`frames` de la cabecera, [`JitterBuffer::push_frames`]); todo se mide y se dimensiona en
//! tramas, no en paquetes: la cola, el objetivo, el jitter (una muestra por trama: la ventana son
//! [`JITTER_WINDOW`] tramas, 2 s, tenga el paquete 10 o 20 ms) y los periodos de ajuste. La
//! reproducción pide lo siguiente con [`JitterBuffer::pop`] cuando acaba de sonar lo anterior (una
//! trama, o las dos de un paquete de 20 ms):
//!
//! - [`Pop::Frame`]: el paquete que toca.
//! - [`Pop::Plc`]: el que toca no llegó pero hay alguno posterior (o se acabó la cola en mitad de
//!   una frase): hay que ocultarlo. Lleva el paquete siguiente, si llegó, para recuperarlo con la
//!   FEC en banda o DRED ([`crate::codec::OpusDecoder::recover`]). Con `stretch`, es una trama de
//!   PLC **insertada** para alargar el buffer (no se pierde nada).
//! - [`Pop::Wait`]: aún no hay que reproducir nada (cebado inicial o el emisor calló: la trama
//!   anterior llevaba la bandera de DTX, `dtx_after`).
//!
//! **Objetivo adaptativo**: el retardo de tránsito relativo de cada paquete es
//! `llegada − captura` (en relojes distintos: solo importan sus diferencias); su jitter, lo que
//! excede al mínimo de las últimas [`JITTER_WINDOW`] tramas. El objetivo es
//! `p95 del jitter (en tramas, redondeando hacia arriba) + las tramas de un paquete`, entre
//! [`MIN_TARGET`] y [`MAX_TARGET`], más el ajuste que pida la sincronía A/V
//! ([`JitterBuffer::set_extra_delay`]). El nivel (tramas en cola, suavizado) se
//! lleva al objetivo **recortando** una trama cuando sobra o **estirando** con una de PLC cuando
//! falta (como mucho una cada [`ADJUST_PERIOD_FRAMES`] tramas).

use std::time::{Duration, Instant};

use crate::FRAME_DURATION;

/// Huecos del buffer (640 ms de tramas de 10 ms).
pub const SLOTS: usize = 64;

/// Bytes máximos por paquete (un paquete Opus de 10 ms cabe de sobra).
pub const MAX_PACKET: usize = 1500;

/// Objetivo mínimo.
pub const MIN_TARGET: Duration = Duration::from_millis(20);

/// Objetivo máximo (sin contar el ajuste de la sincronía A/V).
pub const MAX_TARGET: Duration = Duration::from_millis(60);

/// Tramas de 10 ms con las que se estima el jitter (2 s): una muestra de tránsito por trama.
pub const JITTER_WINDOW: usize = 200;

/// Tramas de 10 ms que lleva como mucho un paquete (20 ms, como la cabecera de audio).
pub const MAX_PACKET_FRAMES: u8 = 2;

/// Tramas entre dos recortes o estiramientos.
pub const ADJUST_PERIOD_FRAMES: u32 = 10;

/// Tramas (1 s) en las que la cola no puede bajar del objetivo para recortar: las llegadas que
/// caen a un lado u otro de la reproducción tienen que verse en la ventana (si no, se recorta,
/// falta la siguiente y se vuelve a llenar), y un vacío de hace más no frena el recorte.
pub const TRIM_WINDOW_FRAMES: u32 = 100;

/// Tramas ocultadas seguidas tras vaciarse la cola (sin DTX) antes de volver a cebar (200 ms):
/// un parón corto de la red se rellena con PLC y el audio sigue en cuanto llega lo siguiente
/// (con más retardo, que después se recorta).
const UNDERRUN_CONCEAL: u32 = 20;

/// Ajuste máximo que puede pedir la sincronía A/V, en tramas.
const MAX_EXTRA_FRAMES: i32 = 20;

const FRAME_US: i64 = FRAME_DURATION.as_micros() as i64;

/// `a` va después de `b` en aritmética circular de 32 bits.
const fn seq_after(a: u32, b: u32) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000_0000
}

#[derive(Clone)]
struct Slot {
    used: bool,
    seq: u32,
    capture_us: u32,
    flags: u8,
    /// Tramas de 10 ms del paquete.
    frames: u8,
    len: u16,
    arrived: Instant,
    data: [u8; MAX_PACKET],
}

/// Paquete que toca reproducir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRef<'a> {
    /// Secuencia.
    pub seq: u32,
    /// Marca de captura del emisor (µs, reloj del anfitrión, 32 bits).
    pub capture_us: u32,
    /// Banderas de la cabecera de audio.
    pub flags: u8,
    /// Tramas de 10 ms que lleva (1 o 2).
    pub frames: u8,
    /// Cuándo llegó.
    pub arrived: Instant,
    /// Paquete Opus.
    pub data: &'a [u8],
}

/// Trama que hay que ocultar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lost<'a> {
    /// Secuencia de la trama que falta.
    pub seq: u32,
    /// Captura estimada (la de la anterior + 10 ms), si se conoce.
    pub capture_us: Option<u32>,
    /// El paquete siguiente (`seq + 1`), si llegó, con sus banderas: sirve para la FEC o DRED.
    pub next: Option<(&'a [u8], u8)>,
    /// Es una trama insertada para alargar el buffer, no una pérdida.
    pub stretch: bool,
    /// Tramas de 10 ms que hay que ocultar (las del paquete perdido; una al estirar).
    pub frames: u8,
}

/// Lo que devuelve [`JitterBuffer::pop`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pop<'a> {
    /// El paquete que toca.
    Frame(FrameRef<'a>),
    /// Una trama que ocultar (PLC, o FEC/DRED con el paquete siguiente).
    Plc(Lost<'a>),
    /// Nada que reproducir aún.
    Wait,
}

/// Contadores del buffer.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct JitterStats {
    /// Paquetes aceptados.
    pub received: u64,
    /// Tramas reproducidas desde su paquete.
    pub played: u64,
    /// Tramas perdidas que hubo que ocultar (PLC, FEC o DRED).
    pub concealed: u64,
    /// De ellas, recuperadas con FEC en banda.
    pub recovered_fec: u64,
    /// De ellas, recuperadas con DRED.
    pub recovered_dred: u64,
    /// Paquetes llegados después de su turno.
    pub late: u64,
    /// Paquetes repetidos.
    pub duplicates: u64,
    /// Veces que la cola se vació en mitad de una frase.
    pub underruns: u64,
    /// Tramas recortadas para bajar el nivel.
    pub trimmed: u64,
    /// Tramas de PLC insertadas para subirlo.
    pub stretched: u64,
    /// Mayor racha de tramas seguidas sin paquete (ocultadas o en espera) una vez empezado.
    pub max_gap_frames: u32,
    /// Objetivo actual en ms (con el ajuste de la sincronía).
    pub target_ms: f64,
    /// p95 del jitter medido, en ms.
    pub jitter_p95_ms: f64,
    /// Nivel suavizado (tramas en cola).
    pub level_frames: f64,
}

/// Buffer de jitter de un emisor.
pub struct JitterBuffer {
    slots: Box<[Slot]>,
    origin: Instant,
    next: Option<u32>,
    highest: Option<u32>,
    primed: bool,
    started: bool,
    prime_from: Option<Instant>,
    last_played: Option<(u32, u8)>,
    last_capture: Option<(u32, u32)>,
    /// Tramas por paquete del emisor (las del último que llegó).
    packet_frames: u8,
    /// Tramas que duró lo último que devolvió [`JitterBuffer::pop`]: lo que avanzó el reloj de
    /// la reproducción hasta esta llamada.
    elapsed: u8,
    target_frames: i32,
    extra_frames: i32,
    level: f64,
    /// Menor cola vista en la ventana actual ([`TRIM_WINDOW_FRAMES`] tramas o desde el último
    /// ajuste).
    min_queued: i32,
    /// Tramas de la ventana actual de `min_queued`.
    window: u32,
    /// Menor cola vista en las últimas [`ADJUST_PERIOD_FRAMES`] tramas.
    min_recent: i32,
    /// Tramas de la ventana actual de `min_recent`.
    recent: u32,
    since_adjust: u32,
    run: u32,
    underrun_run: u32,
    transit: [i64; JITTER_WINDOW],
    transit_len: usize,
    transit_pos: usize,
    scratch: [i64; JITTER_WINDOW],
    since_target: u32,
    stats: JitterStats,
}

impl std::fmt::Debug for JitterBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JitterBuffer")
            .field("next", &self.next)
            .field("primed", &self.primed)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl Default for JitterBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl JitterBuffer {
    /// Buffer vacío con el objetivo mínimo.
    pub fn new() -> Self {
        let empty = Slot {
            used: false,
            seq: 0,
            capture_us: 0,
            flags: 0,
            frames: 1,
            len: 0,
            arrived: Instant::now(),
            data: [0; MAX_PACKET],
        };
        let mut buffer = Self {
            slots: vec![empty; SLOTS].into_boxed_slice(),
            origin: Instant::now(),
            next: None,
            highest: None,
            primed: false,
            started: false,
            prime_from: None,
            last_played: None,
            last_capture: None,
            packet_frames: 1,
            elapsed: 1,
            target_frames: frames_for(MIN_TARGET),
            extra_frames: 0,
            level: 0.0,
            min_queued: i32::MAX,
            window: 0,
            min_recent: i32::MAX,
            recent: 0,
            since_adjust: 0,
            run: 0,
            underrun_run: 0,
            transit: [0; JITTER_WINDOW],
            transit_len: 0,
            transit_pos: 0,
            scratch: [0; JITTER_WINDOW],
            since_target: 0,
            stats: JitterStats::default(),
        };
        buffer.stats.target_ms = buffer.target().as_secs_f64() * 1000.0;
        buffer
    }

    /// Contadores.
    pub fn stats(&self) -> JitterStats {
        self.stats
    }

    /// Objetivo actual (con el ajuste de la sincronía).
    pub fn target(&self) -> Duration {
        FRAME_DURATION * self.desired_frames() as u32
    }

    fn desired_frames(&self) -> i32 {
        // El ajuste de la sincronía puede quitar el margen, pero no bajar del p95 del jitter medido
        // ni del mínimo de 20 ms: con una sola trama en cola, cualquier llegada un poco tarde se
        // oculta con PLC y una racha de ellas suena robótica.
        (self.target_frames + self.extra_frames).max(self.floor_frames())
    }

    /// Retardo extra (o menos, si es negativo) que pide la sincronía A/V, en ms.
    pub fn set_extra_delay(&mut self, extra_ms: i32) {
        let frames = extra_ms.div_euclid(crate::FRAME_DURATION_MS as i32);
        self.extra_frames = frames.clamp(-MAX_EXTRA_FRAMES, MAX_EXTRA_FRAMES);
        self.stats.target_ms = self.target().as_secs_f64() * 1000.0;
    }

    /// Retardo extra actual en ms.
    pub fn extra_delay_ms(&self) -> i32 {
        self.extra_frames * crate::FRAME_DURATION_MS as i32
    }

    /// Ajuste que se puede pedir ahora con [`JitterBuffer::set_extra_delay`], en ms: de lo que
    /// falta para el mínimo (el p95 del jitter, o [`MIN_TARGET`]) a lo que sobra hasta el máximo.
    pub fn extra_delay_range_ms(&self) -> (i32, i32) {
        let frame = crate::FRAME_DURATION_MS as i32;
        let floor = self.floor_frames();
        (
            (floor - self.target_frames) * frame,
            MAX_EXTRA_FRAMES * frame,
        )
    }

    fn floor_frames(&self) -> i32 {
        ((self.stats.jitter_p95_ms * 1000.0) as u64)
            .div_ceil(FRAME_US as u64)
            .clamp(frames_for(MIN_TARGET) as u64, frames_for(MAX_TARGET) as u64) as i32
    }

    fn slot(&self, seq: u32) -> Option<&Slot> {
        let slot = &self.slots[seq as usize % SLOTS];
        (slot.used && slot.seq == seq).then_some(slot)
    }

    fn take(&mut self, seq: u32) {
        let slot = &mut self.slots[seq as usize % SLOTS];
        if slot.seq == seq {
            slot.used = false;
        }
    }

    fn reset(&mut self) {
        for slot in self.slots.iter_mut() {
            slot.used = false;
        }
        self.next = None;
        self.highest = None;
        self.primed = false;
        self.prime_from = None;
        self.last_played = None;
        self.last_capture = None;
        self.level = 0.0;
    }

    /// Guarda el paquete `seq` de una trama de 10 ms (con su marca de captura en µs del reloj del
    /// emisor y sus banderas) que llegó en `arrived`. Devuelve si se aceptó.
    pub fn push(
        &mut self,
        seq: u32,
        capture_us: u32,
        flags: u8,
        packet: &[u8],
        arrived: Instant,
    ) -> bool {
        self.push_frames(seq, capture_us, flags, 1, packet, arrived)
    }

    /// Como [`JitterBuffer::push`], con las tramas de 10 ms que lleva el paquete (`frames` de la
    /// cabecera; entre 1 y [`MAX_PACKET_FRAMES`]).
    pub fn push_frames(
        &mut self,
        seq: u32,
        capture_us: u32,
        flags: u8,
        frames: u8,
        packet: &[u8],
        arrived: Instant,
    ) -> bool {
        let frames = frames.clamp(1, MAX_PACKET_FRAMES);
        if packet.is_empty() || packet.len() > MAX_PACKET {
            return false;
        }
        if let Some(next) = self.next {
            if seq_after(next, seq) {
                self.stats.late += 1;
                return false;
            }
            if seq.wrapping_sub(next) as usize >= SLOTS {
                // Demasiado por delante (el emisor se reinició o hubo un corte largo).
                self.reset();
            }
        }
        if self.slot(seq).is_some() {
            self.stats.duplicates += 1;
            return false;
        }
        let slot = &mut self.slots[seq as usize % SLOTS];
        slot.used = true;
        slot.seq = seq;
        slot.capture_us = capture_us;
        slot.flags = flags;
        slot.frames = frames;
        slot.len = packet.len() as u16;
        slot.arrived = arrived;
        slot.data[..packet.len()].copy_from_slice(packet);
        self.stats.received += 1;
        self.packet_frames = frames;
        if self.next.is_none() {
            self.next = Some(seq);
        }
        if self.highest.is_none_or(|highest| seq_after(seq, highest)) {
            self.highest = Some(seq);
        }
        if !self.primed && self.prime_from.is_none() {
            self.prime_from = Some(arrived);
        }
        // Tránsito relativo: llegada (reloj local) − captura (reloj del emisor), en 32 bits. Una
        // muestra por trama: la ventana cubre el mismo tiempo de audio con paquetes de 10 o 20 ms.
        let arrived_us = arrived.saturating_duration_since(self.origin).as_micros() as u32;
        let transit = i64::from(arrived_us.wrapping_sub(capture_us) as i32);
        for _ in 0..frames {
            self.transit[self.transit_pos] = transit;
            self.transit_pos = (self.transit_pos + 1) % JITTER_WINDOW;
        }
        self.transit_len = (self.transit_len + usize::from(frames)).min(JITTER_WINDOW);
        true
    }

    /// Recalcula el objetivo con el p95 del jitter de la ventana.
    fn update_target(&mut self) {
        let n = self.transit_len;
        if n < 10 {
            return;
        }
        let window = &mut self.scratch[..n];
        window.copy_from_slice(&self.transit[..n]);
        let min = window.iter().copied().min().unwrap_or(0);
        for value in window.iter_mut() {
            *value -= min;
        }
        window.sort_unstable();
        let rank = ((0.95 * n as f64).ceil() as usize).clamp(1, n) - 1;
        let p95 = window[rank].max(0);
        self.stats.jitter_p95_ms = p95 as f64 / 1000.0;
        // El jitter en tramas (hacia arriba) más el paquete que suena.
        let jitter_frames = (p95 as u64).div_ceil(FRAME_US as u64) as i32;
        self.target_frames = (jitter_frames + i32::from(self.packet_frames))
            .clamp(frames_for(MIN_TARGET), frames_for(MAX_TARGET));
        self.stats.target_ms = self.target().as_secs_f64() * 1000.0;
    }

    /// Tramas de 10 ms en cola desde la que toca hasta el paquete más alto recibido: las de cada
    /// paquete que está y, por cada hueco, las de un paquete del emisor.
    fn queued(&self) -> i32 {
        match (self.next, self.highest) {
            (Some(next), Some(highest)) if !seq_after(next, highest) => {
                let packets = highest.wrapping_sub(next) as usize + 1;
                (0..packets as u32)
                    .map(|i| {
                        let seq = next.wrapping_add(i);
                        i32::from(
                            self.slot(seq)
                                .map_or(self.packet_frames, |slot| slot.frames),
                        )
                    })
                    .sum()
            }
            _ => 0,
        }
    }

    fn note_gap(&mut self, frames: u8) {
        if self.started {
            self.run += u32::from(frames);
            self.stats.max_gap_frames = self.stats.max_gap_frames.max(self.run);
        }
    }

    /// Anota cómo se recuperó la última trama perdida (para las estadísticas).
    pub fn note_recovery(&mut self, recovery: crate::codec::Recovery) {
        match recovery {
            crate::codec::Recovery::Fec => self.stats.recovered_fec += 1,
            crate::codec::Recovery::Dred => self.stats.recovered_dred += 1,
            crate::codec::Recovery::Plc => {}
        }
    }

    /// Lo que toca reproducir ahora (una trama de 10 ms, o las dos de un paquete de 20 ms): se
    /// llama cuando acaba de sonar lo que devolvió la llamada anterior.
    pub fn pop(&mut self, now: Instant) -> Pop<'_> {
        // Lo que sonó desde la llamada anterior, en tramas: el reloj de todos los periodos.
        let elapsed = u32::from(self.elapsed.max(1));
        self.elapsed = 1;
        self.since_target += elapsed;
        if self.since_target >= 25 {
            self.since_target = 0;
            self.update_target();
        }
        let Some(next) = self.next else {
            if self.started && self.last_played.is_none_or(|(_, flags)| flags & DTX == 0) {
                self.note_gap(1);
            }
            return Pop::Wait;
        };
        let queued = self.queued();
        if !self.primed {
            let ready = self
                .prime_from
                .is_some_and(|from| now >= from + self.target())
                || queued >= self.desired_frames();
            if !ready {
                if self.started && self.last_played.is_none_or(|(_, flags)| flags & DTX == 0) {
                    self.note_gap(1);
                }
                return Pop::Wait;
            }
            self.primed = true;
            self.level = f64::from(queued);
            self.min_queued = i32::MAX;
            self.window = 0;
            self.min_recent = i32::MAX;
            self.recent = 0;
            self.since_adjust = 0;
        }
        for _ in 0..elapsed {
            self.level = self.level * 0.9 + f64::from(queued) * 0.1;
        }
        self.stats.level_frames = self.level;
        // La ventana es la de las últimas tramas: un vacío de hace segundos no frena el recorte.
        if self.window >= TRIM_WINDOW_FRAMES {
            self.window = 0;
            self.min_queued = i32::MAX;
        }
        self.window += elapsed;
        self.min_queued = self.min_queued.min(queued);
        if self.recent >= ADJUST_PERIOD_FRAMES {
            self.recent = 0;
            self.min_recent = i32::MAX;
        }
        self.recent += elapsed;
        self.min_recent = self.min_recent.min(queued);
        self.since_adjust = self.since_adjust.saturating_add(elapsed);
        let desired = f64::from(self.desired_frames());
        let adjust = self.since_adjust >= ADJUST_PERIOD_FRAMES;

        // Sobra cola: se recorta el paquete que toca si está (y hay más detrás). Sobran dos o más
        // tramas si en las últimas [`ADJUST_PERIOD_FRAMES`] la cola no bajó de objetivo + el
        // paquete + 1 (una ráfaga tras un parón): se recorta uno cada periodo hasta quedar en
        // objetivo + 1. Sobra un paquete si en todo un [`TRIM_WINDOW_FRAMES`] la cola no bajó
        // nunca de objetivo + el paquete: así no se recorta el que hace falta cuando las llegadas
        // caen a un lado u otro de la reproducción, y la cola no se queda una trama por encima.
        let desired_frames = self.desired_frames();
        let head = self.slot(next).map(|slot| i32::from(slot.frames));
        if let Some(head) = head {
            let burst =
                self.recent >= ADJUST_PERIOD_FRAMES && self.min_recent > desired_frames + head;
            let one_extra =
                self.window >= TRIM_WINDOW_FRAMES && self.min_queued >= desired_frames + head;
            if adjust && (burst || one_extra) && queued > head {
                self.take(next);
                self.next = Some(next.wrapping_add(1));
                self.stats.trimmed += 1;
                self.level -= f64::from(head);
                self.min_queued = i32::MAX;
                self.window = 0;
                self.min_recent = i32::MAX;
                self.recent = 0;
                self.since_adjust = 0;
                return self.pop_inner();
            }
        }
        // Falta cola: una trama de PLC sin avanzar.
        if adjust && self.level < desired - 1.5 && queued > 0 {
            self.stats.stretched += 1;
            self.level += 1.0;
            self.min_queued = i32::MAX;
            self.window = 0;
            self.min_recent = i32::MAX;
            self.recent = 0;
            self.since_adjust = 0;
            return Pop::Plc(Lost {
                seq: next,
                capture_us: None,
                next: None,
                stretch: true,
                frames: 1,
            });
        }
        self.pop_inner()
    }

    fn pop_inner(&mut self) -> Pop<'_> {
        let Some(next) = self.next else {
            return Pop::Wait;
        };
        let queued = self.queued();
        if self.slot(next).is_some() {
            self.take(next);
            self.next = Some(next.wrapping_add(1));
            self.started = true;
            self.run = 0;
            self.underrun_run = 0;
            self.stats.played += 1;
            let slot = &self.slots[next as usize % SLOTS];
            self.last_played = Some((next, slot.flags));
            self.last_capture = Some((next, slot.capture_us));
            self.elapsed = slot.frames;
            return Pop::Frame(FrameRef {
                seq: next,
                capture_us: slot.capture_us,
                flags: slot.flags,
                frames: slot.frames,
                arrived: slot.arrived,
                data: &slot.data[..usize::from(slot.len)],
            });
        }
        let packet_us = u32::from(self.packet_frames) * FRAME_US as u32;
        let estimate = self.last_capture.map(|(seq, capture)| {
            capture.wrapping_add(next.wrapping_sub(seq).wrapping_mul(packet_us))
        });
        if queued > 0 {
            // Hueco con algo detrás: pérdida (las tramas de un paquete del emisor).
            let frames = self.packet_frames;
            self.next = Some(next.wrapping_add(1));
            self.stats.concealed += 1;
            self.underrun_run = 0;
            self.note_gap(frames);
            self.elapsed = frames;
            let following = next.wrapping_add(1);
            let slot = &self.slots[following as usize % SLOTS];
            let next_packet = (slot.used && slot.seq == following)
                .then(|| (&slot.data[..usize::from(slot.len)], slot.flags));
            return Pop::Plc(Lost {
                seq: next,
                capture_us: estimate,
                next: next_packet,
                stretch: false,
                frames,
            });
        }
        // Cola vacía.
        let spoke_last = self.last_played.is_some_and(|(_, flags)| flags & DTX == 0);
        if self.started && spoke_last && self.underrun_run < UNDERRUN_CONCEAL {
            if self.underrun_run == 0 {
                self.stats.underruns += 1;
            }
            self.underrun_run += 1;
            self.note_gap(1);
            return Pop::Plc(Lost {
                seq: next,
                capture_us: estimate,
                next: None,
                stretch: true,
                frames: 1,
            });
        }
        if spoke_last {
            self.note_gap(1);
        }
        // El emisor calló (DTX) o la cola lleva vacía demasiado: se vuelve a cebar.
        self.primed = false;
        self.prime_from = None;
        Pop::Wait
    }
}

/// Bandera de la cabecera de audio: el emisor deja de enviar después de esta trama (silencio o
/// DTX); que la cola se vacíe entonces no es una pérdida. (Los mismos valores que
/// `coroom_proto::audio::AudioHeader`.)
pub const DTX: u8 = 0b0000_0001;

/// Bandera de la cabecera de audio: el paquete lleva FEC en banda de la trama anterior.
pub const FEC: u8 = 0b0000_0010;

/// Bandera de la cabecera de audio: el paquete lleva DRED.
pub const DRED: u8 = 0b0000_0100;

fn frames_for(target: Duration) -> i32 {
    target.as_micros().div_ceil(FRAME_DURATION.as_micros()) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn packet(seq: u32) -> [u8; 4] {
        seq.to_be_bytes()
    }

    /// Simula `frames` tramas enviadas cada 10 ms que llegan con el retardo que diga `delay` (o se
    /// pierden con `None`); devuelve lo que sale de la reproducción, también cada 10 ms.
    fn run(frames: u32, delay: impl Fn(u32) -> Option<Duration>) -> (Vec<String>, JitterStats) {
        let mut buffer = JitterBuffer::new();
        let base = Instant::now();
        let mut arrivals: Vec<(Instant, u32)> = (0..frames)
            .filter_map(|seq| delay(seq).map(|d| (base + FRAME_DURATION * seq + d, seq)))
            .collect();
        arrivals.sort();
        let mut out = Vec::new();
        let mut arrivals = arrivals.into_iter().peekable();
        // Hasta que sale la última trama (sin contar el final del flujo como un hueco).
        let last = frames - 1;
        for tick in 0..frames + 20 {
            if out
                .last()
                .is_some_and(|s: &String| *s == format!("F{last}") || *s == format!("L{last}"))
            {
                break;
            }
            let now = base + FRAME_DURATION * tick + Duration::from_micros(500);
            while let Some((at, seq)) = arrivals.peek().copied() {
                if at > now {
                    break;
                }
                arrivals.next();
                let capture = (seq * 10_000).wrapping_add(7);
                buffer.push(seq, capture, 0, &packet(seq), at);
            }
            out.push(match buffer.pop(now) {
                Pop::Frame(frame) => {
                    assert_eq!(frame.data, packet(frame.seq));
                    format!("F{}", frame.seq)
                }
                Pop::Plc(lost) if lost.stretch => "S".to_owned(),
                Pop::Plc(lost) => format!("L{}", lost.seq),
                Pop::Wait => "W".to_owned(),
            });
        }
        (out, buffer.stats())
    }

    #[test]
    fn steady_arrivals_play_every_frame_after_the_target() {
        let (out, stats) = run(200, |_| Some(2 * MS));
        let first = out
            .iter()
            .position(|s| s.starts_with('F'))
            .expect("algo suena");
        // El cebado espera el objetivo mínimo (20 ms) desde la primera llegada.
        assert!((2..=4).contains(&first), "{first}: {out:?}");
        assert_eq!(stats.played, 200, "{out:?}");
        assert_eq!(stats.concealed, 0);
        assert_eq!(stats.max_gap_frames, 0, "{out:?}");
        assert!((stats.target_ms - 20.0).abs() < 1e-6, "{stats:?}");
    }

    #[test]
    fn reordering_and_losses_are_concealed_without_long_gaps() {
        // 3 % de pérdidas (nunca dos seguidas) y reordenación de hasta 15 ms.
        let lost = |seq: u32| seq % 33 == 5;
        let (out, stats) = run(590, |seq| {
            if lost(seq) {
                None
            } else {
                Some(MS * (2 + (seq * 7919 % 16)))
            }
        });
        println!("{stats:?}");
        let expected_lost = (0..590).filter(|seq| lost(*seq)).count() as u64;
        assert!(stats.concealed >= expected_lost, "{stats:?}");
        assert!(stats.max_gap_frames <= 1, "{stats:?}\n{out:?}");
        assert_eq!(
            stats.late, 0,
            "el objetivo cubre la reordenación: {stats:?}"
        );
        assert!(
            stats.target_ms >= 20.0 && stats.target_ms <= 60.0,
            "{stats:?}"
        );
        // Las tramas que sonaron salieron en orden.
        let played: Vec<u32> = out
            .iter()
            .filter_map(|s| s.strip_prefix('F').and_then(|n| n.parse().ok()))
            .collect();
        assert!(played.windows(2).all(|w| w[1] > w[0]), "{played:?}");
    }

    #[test]
    fn the_target_grows_with_jitter_and_stays_bounded() {
        let (_, calm) = run(400, |_| Some(MS));
        let (_, jittery) = run(400, |seq| Some(MS * (seq * 31 % 40)));
        let (_, wild) = run(400, |seq| Some(MS * (seq * 31 % 150)));
        println!("calma {calm:?}\njitter {jittery:?}\nmucho {wild:?}");
        assert!(jittery.target_ms > calm.target_ms);
        assert!(wild.target_ms <= 60.0);
        assert!(jittery.jitter_p95_ms > 30.0);
    }

    #[test]
    fn late_duplicate_and_far_packets() {
        let mut buffer = JitterBuffer::new();
        let now = Instant::now();
        assert!(buffer.push(10, 0, 0, b"a", now));
        assert!(!buffer.push(10, 0, 0, b"a", now), "repetido");
        assert!(!buffer.push(11, 0, 0, &[], now), "vacío");
        assert!(
            !buffer.push(12, 0, 0, &[0; MAX_PACKET + 1], now),
            "demasiado grande"
        );
        assert!(matches!(
            buffer.pop(now + 30 * MS),
            Pop::Frame(FrameRef { seq: 10, .. })
        ));
        assert!(!buffer.push(9, 0, 0, b"b", now), "tarde");
        assert_eq!(buffer.stats().late, 1);
        assert_eq!(buffer.stats().duplicates, 1);
        // Muy por delante: se reinicia en ese paquete.
        assert!(buffer.push(10_000, 0, 0, b"c", now));
        assert!(matches!(
            buffer.pop(now + 60 * MS),
            Pop::Frame(FrameRef { seq: 10_000, .. })
        ));
    }

    #[test]
    fn a_silent_sender_is_not_a_loss() {
        let mut buffer = JitterBuffer::new();
        let base = Instant::now();
        for seq in 0..5u32 {
            let flags = if seq == 4 { DTX } else { 0 };
            buffer.push(seq, seq * 10_000, flags, b"v", base + FRAME_DURATION * seq);
        }
        let mut gaps = 0;
        for tick in 0..40u32 {
            if let Pop::Plc(_) = buffer.pop(base + 30 * MS + FRAME_DURATION * tick) {
                gaps += 1;
            }
        }
        assert_eq!(gaps, 0);
        assert_eq!(buffer.stats().max_gap_frames, 0);
        // La frase siguiente vuelve a cebarse y suena.
        let later = base + Duration::from_secs(1);
        buffer.push(5, 900_000, 0, b"w", later);
        assert_eq!(buffer.pop(later), Pop::Wait);
        assert!(matches!(
            buffer.pop(later + 25 * MS),
            Pop::Frame(FrameRef { seq: 5, .. })
        ));
    }

    #[test]
    fn extra_delay_moves_the_target() {
        let mut buffer = JitterBuffer::new();
        buffer.set_extra_delay(30);
        assert_eq!(buffer.target(), Duration::from_millis(50));
        assert_eq!(buffer.extra_delay_ms(), 30);
        buffer.set_extra_delay(-500);
        assert_eq!(buffer.target(), MIN_TARGET, "nunca menos del mínimo");
        // Ni del p95 del jitter medido.
        buffer.stats.jitter_p95_ms = 25.0;
        assert_eq!(buffer.target(), Duration::from_millis(30));
        assert_eq!(buffer.extra_delay_range_ms(), (10, 200));
    }

    #[test]
    fn av_sync_never_takes_the_target_below_the_minimum() {
        // La sincronía pide todo lo que puede quitar: el objetivo se queda en 20 ms (dos tramas);
        // con una sola, cada llegada un poco tarde era una trama de PLC (audio robótico).
        let mut buffer = JitterBuffer::new();
        let (min, _) = buffer.extra_delay_range_ms();
        assert_eq!(min, 0, "con el objetivo mínimo no hay nada que quitar");
        buffer.set_extra_delay(-200);
        assert_eq!(buffer.desired_frames(), frames_for(MIN_TARGET));
        assert_eq!(buffer.target(), MIN_TARGET);
        // Con un objetivo mayor por el jitter, sí se puede bajar, pero solo hasta el p95.
        buffer.stats.jitter_p95_ms = 8.0;
        buffer.target_frames = 4;
        assert_eq!(buffer.extra_delay_range_ms(), (-20, 200));
        assert_eq!(buffer.target(), MIN_TARGET);
        // Llegadas que bailan 1-9 ms con la sincronía pidiendo −50 ms: nada se oculta.
        let wobble = |seq: u32| MS * (seq * 7 % 9);
        let (_, stats) = hold(MS, wobble, -50);
        assert!(stats.target_ms >= 20.0, "{stats:?}");
        assert_eq!(stats.concealed + stats.underruns, 0, "{stats:?}");
    }

    /// Tramas cada 10 ms que llegan `delay` después de capturarse (más `wobble(seq)`), con la
    /// reproducción a mitad de cada trama; `extra` es el ajuste de la sincronía que se pide desde
    /// la trama 50. Devuelve la espera media captura → reproducción del último 1,5 s y los
    /// contadores.
    fn hold(delay: Duration, wobble: impl Fn(u32) -> Duration, extra: i32) -> (f64, JitterStats) {
        let mut buffer = JitterBuffer::new();
        let base = Instant::now();
        let mut held = Vec::new();
        let mut seq = 0;
        for tick in 0..600u32 {
            let now = base + FRAME_DURATION * tick + 5 * MS;
            loop {
                let at = base + FRAME_DURATION * seq + delay + wobble(seq);
                if at > now {
                    break;
                }
                buffer.push(seq, seq * 10_000, 0, &packet(seq), at);
                seq += 1;
            }
            if tick >= 50 && tick % 10 == 0 {
                let (min, _) = buffer.extra_delay_range_ms();
                buffer.set_extra_delay(extra.max(min));
            }
            if let Pop::Frame(frame) = buffer.pop(now)
                && tick >= 450
            {
                held.push((now - (base + FRAME_DURATION * frame.seq)).as_secs_f64() * 1000.0);
            }
        }
        (held.iter().sum::<f64>() / held.len() as f64, buffer.stats())
    }

    #[test]
    fn the_queue_settles_at_the_target_not_one_frame_above() {
        // Llegan 3 ms después de capturarse y se reproducen a los 5 ms de cada trama: con el
        // objetivo de 2 tramas la espera es 15 ms (antes la cola se quedaba en 3 y eran 25).
        let (wait, stats) = hold(3 * MS, |_| Duration::ZERO, 0);
        assert!((wait - 15.0).abs() < 0.5, "{wait} ms {stats:?}");
        assert_eq!((stats.trimmed, stats.stretched), (0, 0), "{stats:?}");
        // Con la sincronía pidiendo una trama menos, la cola no baja del mínimo (dos tramas).
        let (wait, stats) = hold(3 * MS, |_| Duration::ZERO, -10);
        assert!((wait - 15.0).abs() < 0.5, "{wait} ms {stats:?}");
        assert_eq!((stats.trimmed, stats.stretched), (0, 0), "{stats:?}");
    }

    #[test]
    fn a_burst_after_a_stall_drains_in_well_under_a_second() {
        // Un parón de 60 ms: las tramas 100 a 106 llegan juntas con la 106. La cola crece 6
        // tramas y tiene que volver al objetivo enseguida (con una por segundo tardaba 6 s, y el
        // audio sonaba 60 ms tarde todo ese tiempo).
        let stall = |seq: u32| {
            if (100..106).contains(&seq) {
                FRAME_DURATION * (106 - seq)
            } else {
                Duration::ZERO
            }
        };
        let (wait, stats) = hold(3 * MS, stall, 0);
        assert!((wait - 15.0).abs() < 0.5, "{wait} ms {stats:?}");
        assert!(stats.trimmed >= 5, "{stats:?}");
        // El propio parón vacía la cola una vez; los recortes no añaden ninguna.
        assert!(stats.underruns <= 1, "{stats:?}");
    }

    #[test]
    fn an_old_empty_tick_does_not_block_trimming() {
        // La trama 60 llega 25 ms tarde: la cola se vacía una vez y luego se queda con una de
        // más. Pasado ese momento se recorta igual (antes el vacío frenaba el recorte para siempre).
        let late = |seq: u32| if seq == 60 { 25 * MS } else { Duration::ZERO };
        let (wait, stats) = hold(3 * MS, late, 0);
        assert!((wait - 15.0).abs() < 0.5, "{wait} ms {stats:?}");
        assert!(stats.trimmed >= 1, "{stats:?}");
    }

    #[test]
    fn twenty_millisecond_packets_are_measured_in_frames() {
        // Paquetes de 20 ms (dos tramas) que llegan 3 ms después de completarse; la reproducción
        // pide cada 10 ms salvo cuando aún suena la segunda trama del paquete anterior (como
        // `Stream::tick`). La cola y el objetivo cuentan tramas: un paquete en cola son 20 ms y
        // no hace falta estirar ni recortar (contando paquetes, la cola parecía la mitad).
        let mut buffer = JitterBuffer::new();
        let base = Instant::now();
        let (mut seq, mut pending, mut played) = (0u32, false, Vec::new());
        for tick in 0..400u32 {
            let now = base + FRAME_DURATION * tick + 5 * MS;
            loop {
                let at = base + FRAME_DURATION * (2 * seq + 2) + 3 * MS;
                if at > now {
                    break;
                }
                buffer.push_frames(seq, seq * 20_000, 0, 2, &packet(seq), at);
                seq += 1;
            }
            if pending {
                pending = false;
                continue;
            }
            match buffer.pop(now) {
                Pop::Frame(frame) => {
                    assert_eq!(frame.frames, 2);
                    pending = true;
                    played.push(frame.seq);
                }
                Pop::Plc(lost) => pending = lost.frames == 2,
                Pop::Wait => {}
            }
        }
        let stats = buffer.stats();
        assert_eq!((stats.trimmed, stats.stretched), (0, 0), "{stats:?}");
        assert_eq!(stats.concealed + stats.underruns, 0, "{stats:?}");
        assert!(played.windows(2).all(|w| w[1] == w[0] + 1), "{played:?}");
        assert!(played.len() >= 190, "{}", played.len());
        assert!((stats.target_ms - 20.0).abs() < 1e-6, "{stats:?}");
        assert!(
            stats.level_frames >= 2.0,
            "un paquete son dos tramas: {stats:?}"
        );
        // Un paquete de 20 ms perdido se oculta con sus dos tramas.
        let mut buffer = JitterBuffer::new();
        for seq in [0u32, 1, 3] {
            buffer.push_frames(seq, seq * 20_000, 0, 2, &packet(seq), base);
        }
        let later = base + Duration::from_secs(1);
        assert!(matches!(
            buffer.pop(later),
            Pop::Frame(FrameRef {
                seq: 0,
                frames: 2,
                ..
            })
        ));
        assert!(matches!(
            buffer.pop(later),
            Pop::Frame(FrameRef { seq: 1, .. })
        ));
        match buffer.pop(later) {
            Pop::Plc(lost) => assert_eq!((lost.seq, lost.frames, lost.stretch), (2, 2, false)),
            other => panic!("{other:?}"),
        }
        assert_eq!(buffer.stats().max_gap_frames, 2);
    }

    #[test]
    fn arrivals_around_the_tick_keep_the_frame_they_need() {
        // Llegan entre 4 y 6 ms, a un lado u otro de la reproducción (5 ms): una trama de cola a
        // veces sobra y a veces no. No se recorta (cada recorte quita 10 ms de audio) ni falta.
        let wobble = |seq: u32| {
            Duration::from_micros(u64::from(seq.wrapping_mul(2_654_435_761) >> 22) % 2000)
        };
        let (wait, stats) = hold(4 * MS, wobble, -10);
        assert_eq!((stats.trimmed, stats.stretched), (0, 0), "{stats:?}");
        assert_eq!(stats.concealed + stats.underruns, 0, "{stats:?}");
        assert!(wait < 16.0, "{wait} ms");
    }
}
