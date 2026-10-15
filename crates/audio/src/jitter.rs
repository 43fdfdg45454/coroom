//! Buffer de jitter adaptativo por emisor.
//!
//! Guarda los paquetes Opus de un emisor por número de secuencia (`seq` de la cabecera de audio,
//! que cuenta los paquetes **enviados**, sin huecos aunque el emisor calle) en [`SLOTS`] huecos
//! reservados al crearlo: no asigna nada después. Cada paquete lleva una o dos **tramas** de
//! 10 ms (`frames` de la cabecera, [`JitterBuffer::push_frames`]); todo se mide y se dimensiona en
//! tramas, no en paquetes: la cola, el jitter (una muestra por trama: la ventana son
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
//! **Objetivo adaptativo, medido en tiempo y no en tramas.** El retardo de tránsito relativo de
//! cada paquete es `llegada − captura` (en relojes distintos: solo importan sus diferencias) y su
//! jitter, lo que excede al mínimo de las últimas [`JITTER_WINDOW`] tramas; también cuentan los
//! paquetes que llegan después de su turno, que son justo los que el buffer tiene que aprender a
//! esperar. El **margen** es el **p99 del jitter**, más lo que pida la sincronía A/V. El
//! **adelanto** de una trama es cuánto antes de que la reproducción la pida llegó, y su **adelanto
//! nominal**, ese adelanto más su propio jitter: el que habría tenido de llegar con el mejor
//! tránsito de la ventana. El adelanto nominal es el mismo para todas las tramas de un emisor
//! estable (cada una suma lo que su llegada le quitó), así que su mínimo en el último segundo
//! ([`TRIM_WINDOW_FRAMES`]) menos el margen mide cuánto sobra sin volver a descontar el jitter
//! (compararlo con el menor adelanto, que ya trae descontado el peor jitter, exigía el doble de
//! margen y dejaba una trama de más en cola cuando el desfase con el dispositivo caía corto). El
//! buffer mantiene esa **holgura** (adelanto nominal menos margen) entre cero y un paquete:
//! **recorta** una trama cuando sobra un paquete entero (como mucho una cada
//! [`ADJUST_PERIOD_FRAMES`] tramas, con una pizca de histéresis para no recortar lo que habrá que
//! volver a estirar; la cola que deja un parón se recorta de golpe en cuanto diez tramas seguidas
//! la confirman, sin esperar el segundo) y
//! **estira** con una trama de PLC cuando la holgura típica (suavizada) baja de cero, o en el
//! acto si alguna trama reciente quedó una trama entera por debajo. Como la holgura se
//! calcula con el margen de ahora, un margen que sube con una ráfaga de tardías estira en el acto
//! y uno que baja recorta al pasar la ventana. El objetivo que publica ([`JitterBuffer::target`])
//! es el margen más las tramas de un paquete: de [`MIN_TARGET`] (un paquete de 10 ms sin jitter)
//! a [`MAX_TARGET`], más el ajuste de la sincronía A/V ([`JitterBuffer::set_extra_delay`]), que
//! nunca lo baja del jitter medido.
//!
//! Medir el adelanto en tiempo, y no cuántas tramas hay en cola, es lo que permite un objetivo de
//! una sola trama: con la cola contada en tramas enteras, el desfase entre la llegada y la
//! petición del dispositivo (de 0 a 10 ms, que deriva con los relojes) obligaba a tener siempre
//! una trama de más (de 10 a 20 ms de espera con jitter de 1 ms); ahora la espera es el jitter
//! más ese desfase. El margen es el p99 y no el máximo porque cada cambio del objetivo cuesta
//! artefactos (estirar al subir, recortar al bajar): un paquete suelto que llega tarde se oculta
//! una vez y no mueve el objetivo durante dos segundos; a partir de tres en la ventana, sí.

use std::time::{Duration, Instant};

use crate::FRAME_DURATION;

/// Huecos del buffer (640 ms de tramas de 10 ms).
pub const SLOTS: usize = 64;

/// Bytes máximos por paquete (un paquete Opus de 10 ms cabe de sobra).
pub const MAX_PACKET: usize = 1500;

/// Objetivo mínimo: una trama (el paquete que suena) sin margen, con un enlace sin jitter.
pub const MIN_TARGET: Duration = FRAME_DURATION;

/// Objetivo máximo (sin contar el ajuste de la sincronía A/V).
pub const MAX_TARGET: Duration = Duration::from_millis(60);

/// Tramas de 10 ms con las que se estima el jitter (2 s): una muestra de tránsito por trama.
pub const JITTER_WINDOW: usize = 200;

/// Tramas de 10 ms que lleva como mucho un paquete (20 ms, como la cabecera de audio).
pub const MAX_PACKET_FRAMES: u8 = 2;

/// Tramas entre dos recortes o estiramientos.
pub const ADJUST_PERIOD_FRAMES: u32 = 10;

/// Tramas (1 s) en las que la holgura no puede bajar de un paquete para recortar: un cambio en
/// las llegadas o en el ritmo del dispositivo tiene que verse en la ventana (si no, se recorta,
/// falta la siguiente y se vuelve a llenar), y un vacío de hace más no frena el recorte.
pub const TRIM_WINDOW_FRAMES: u32 = 100;

/// Tramas seguidas sin paquete que se dan por **perdidas** (se ocultan y se pasa a la siguiente)
/// cuando la cola está vacía, antes de tratarlo como un parón. Con un paquete de cola, que la
/// trama que toca falte y no haya nada detrás es lo normal cuando se pierde un paquete: si se
/// esperase, sonarían dos tramas de PLC por cada pérdida y la cola crecería una trama. Si el
/// paquete llega después, se anota como tardío (y dos tardíos en la ventana no mueven el p99).
const LOSS_ADVANCE_FRAMES: u32 = 2;

/// Tramas ocultadas seguidas tras vaciarse la cola (sin DTX) antes de volver a cebar (200 ms):
/// a partir de [`LOSS_ADVANCE_FRAMES`] es un parón de la red, que se rellena con PLC sin pasar
/// de trama, y el audio sigue en cuanto llega lo siguiente (con más retardo, que después se
/// recorta).
const UNDERRUN_CONCEAL: u32 = 20;

/// Ajuste máximo que puede pedir la sincronía A/V, en tramas.
const MAX_EXTRA_FRAMES: i32 = 20;

/// Muestras de tránsito necesarias para fiarse del percentil del jitter; antes, el margen es una
/// trama o el mayor jitter visto, lo que sea más.
const MIN_SAMPLES: usize = 10;

/// Percentil del jitter que hace de margen.
const JITTER_PERCENTILE: f64 = 0.99;

/// Histéresis del recorte, en µs (un quinto de trama, 2 ms): la holgura tiene que pasar de un
/// paquete en esto para recortar, y así el recorte la deja por encima de cero con algo de aire.
/// Sin ella, el ruido del ritmo del dispositivo (las peticiones de PipeWire con un cuanto de 128
/// caen hasta 2 ms fuera de la rejilla de 10 ms) o la deriva lenta entre los relojes provocaban
/// pares recorte/estiramiento (dos artefactos) sin que nada hubiera cambiado. Basta con poco,
/// porque el jitter de cada llegada ya no entra en la holgura (el adelanto nominal lo descuenta).
/// El estiramiento, en cambio, no espera: por debajo del margen llega tarde más del 1 % y cada
/// tardío es una trama ocultada.
const TRIM_HYSTERESIS_US: i64 = FRAME_US / 5;

/// Aire con que se ceba, en µs: se empieza a reproducir cuando la trama que toca lleva el margen
/// más esto. La holgura con que se arranca depende de la fase entre la llegada y la petición del
/// dispositivo (de 0 a una trama); sin el aire, un arranque con menos de 2 ms de holgura llegaba
/// tarde en su primer segundo con las peticiones fuera de la rejilla. Igual a la histéresis del
/// recorte para que el arranque nunca quede en la banda que se recorta un segundo después.
const PRIME_GUARD_US: i64 = TRIM_HYSTERESIS_US;

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
    /// Objetivo actual en ms (el margen más un paquete, con el ajuste de la sincronía).
    pub target_ms: f64,
    /// p95 del jitter medido, en ms (informativo).
    pub jitter_p95_ms: f64,
    /// p99 del jitter medido, en ms: el margen que mantiene el buffer.
    pub jitter_p99_ms: f64,
    /// Nivel suavizado (tramas en cola).
    pub level_frames: f64,
    /// Adelanto típico (suavizado) con que llega la trama que toca, en ms.
    pub lead_ms: f64,
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
    /// Cuándo fue la última petición desde que se cebó (ninguna justo después de cebar): con
    /// `elapsed` da el ritmo del dispositivo, y una petición que se retrasa una trama entera o
    /// más es un parón del propio dispositivo.
    last_pop: Option<Instant>,
    /// Margen: p99 del jitter de la ventana, en µs (una trama mientras no hay medidas).
    jitter_us: i64,
    /// Menor tránsito relativo de la ventana, en µs: el cero del jitter.
    min_transit_us: i64,
    extra_frames: i32,
    level: f64,
    /// Adelanto nominal suavizado de la trama que toca, en µs.
    nominal: Option<f64>,
    /// Menor adelanto nominal visto en la ventana actual ([`TRIM_WINDOW_FRAMES`] tramas o desde
    /// el último ajuste), en µs; `i64::MIN` si llegó algo tarde en ella.
    min_nominal_us: i64,
    /// Tramas de la ventana actual de `min_nominal_us`.
    window: u32,
    /// Menor adelanto nominal visto en las últimas [`ADJUST_PERIOD_FRAMES`] tramas, en µs.
    min_recent_us: i64,
    /// Tramas de la ventana actual de `min_recent_us`.
    recent: u32,
    since_adjust: u32,
    run: u32,
    underrun_run: u32,
    transit: [i64; JITTER_WINDOW],
    transit_len: usize,
    transit_pos: usize,
    scratch: [i64; JITTER_WINDOW],
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
            last_pop: None,
            jitter_us: FRAME_US,
            min_transit_us: 0,
            extra_frames: 0,
            level: 0.0,
            nominal: None,
            min_nominal_us: i64::MAX,
            window: 0,
            min_recent_us: i64::MAX,
            recent: 0,
            since_adjust: 0,
            run: 0,
            underrun_run: 0,
            transit: [0; JITTER_WINDOW],
            transit_len: 0,
            transit_pos: 0,
            scratch: [0; JITTER_WINDOW],
            stats: JitterStats::default(),
        };
        buffer.stats.jitter_p99_ms = buffer.jitter_us as f64 / 1000.0;
        buffer.stats.target_ms = buffer.target().as_secs_f64() * 1000.0;
        buffer
    }

    /// Contadores.
    pub fn stats(&self) -> JitterStats {
        self.stats
    }

    /// Objetivo actual: el margen (el p99 del jitter y el ajuste de la sincronía) más las tramas
    /// de un paquete.
    pub fn target(&self) -> Duration {
        Duration::from_micros((self.desired_lead_us() + self.packet_us()) as u64)
    }

    fn packet_us(&self) -> i64 {
        i64::from(self.packet_frames) * FRAME_US
    }

    /// Margen que se puede pedir al jitter medido sin pasar de [`MAX_TARGET`] con un paquete.
    fn floor_us(&self) -> i64 {
        let max_lead = MAX_TARGET.as_micros() as i64 - self.packet_us();
        self.jitter_us.clamp(0, max_lead.max(0))
    }

    /// Adelanto que se mantiene, en µs: el jitter medido más lo que añada la sincronía. La
    /// sincronía puede retrasar el audio, pero no bajar del jitter medido: con menos margen,
    /// cualquier llegada un poco tarde se oculta con PLC y una racha de ellas suena robótica.
    fn desired_lead_us(&self) -> i64 {
        let floor = self.floor_us();
        (floor + i64::from(self.extra_frames) * FRAME_US).max(floor)
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

    /// Ajuste que se puede pedir ahora con [`JitterBuffer::set_extra_delay`], en ms: nada por
    /// debajo (el objetivo ya es el jitter medido) y hasta [`MAX_EXTRA_FRAMES`] tramas por encima.
    pub fn extra_delay_range_ms(&self) -> (i32, i32) {
        (0, MAX_EXTRA_FRAMES * crate::FRAME_DURATION_MS as i32)
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
        self.nominal = None;
    }

    /// Empieza de cero las ventanas de holgura, el adelanto nominal suavizado (lo vuelve a sembrar
    /// la siguiente trama: corregirlo a mano tras un recorte grande lo dejaba muy por debajo de la
    /// realidad y pedía un estiramiento falso) y el periodo de ajuste (tras cebar, recortar o
    /// estirar).
    fn reset_windows(&mut self) {
        self.nominal = None;
        self.min_nominal_us = i64::MAX;
        self.window = 0;
        self.min_recent_us = i64::MAX;
        self.recent = 0;
        self.since_adjust = 0;
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
                // Llegó después de su turno: no suena, pero su tránsito es justo lo que el margen
                // tiene que cubrir, y hasta que pase la ventana no se recorta nada.
                self.stats.late += 1;
                self.record_transit(capture_us, frames, arrived);
                self.min_nominal_us = i64::MIN;
                self.min_recent_us = i64::MIN;
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
        self.record_transit(capture_us, frames, arrived);
        true
    }

    /// Anota el tránsito relativo de un paquete: llegada (reloj local) − captura (reloj del
    /// emisor), en 32 bits. Una muestra por trama: la ventana cubre el mismo tiempo de audio con
    /// paquetes de 10 o 20 ms.
    fn record_transit(&mut self, capture_us: u32, frames: u8, arrived: Instant) {
        let transit = self.transit_us(capture_us, arrived);
        for _ in 0..frames {
            self.transit[self.transit_pos] = transit;
            self.transit_pos = (self.transit_pos + 1) % JITTER_WINDOW;
        }
        self.transit_len = (self.transit_len + usize::from(frames)).min(JITTER_WINDOW);
    }

    /// Tránsito relativo de un paquete (llegada − captura), en µs.
    fn transit_us(&self, capture_us: u32, arrived: Instant) -> i64 {
        let arrived_us = arrived.saturating_duration_since(self.origin).as_micros() as u32;
        i64::from(arrived_us.wrapping_sub(capture_us) as i32)
    }

    /// Recalcula el margen con los percentiles del jitter de la ventana (ordena una copia en la
    /// propia estructura: unos microsegundos, sin asignar). Hasta [`MIN_SAMPLES`] muestras, el
    /// margen es una trama o el mayor jitter visto, lo que sea más; después, el p99 acotado por la
    /// tercera mayor muestra.
    fn update_target(&mut self) {
        let n = self.transit_len;
        if n == 0 {
            self.jitter_us = FRAME_US;
        } else {
            let window = &mut self.scratch[..n];
            window.copy_from_slice(&self.transit[..n]);
            window.sort_unstable();
            let min = window[0];
            self.min_transit_us = min;
            let rank = |p: f64| window[((p * n as f64).ceil() as usize).clamp(1, n) - 1] - min;
            self.stats.jitter_p95_ms = rank(0.95).max(0) as f64 / 1000.0;
            self.jitter_us = if n < MIN_SAMPLES {
                (window[n - 1] - min).max(FRAME_US)
            } else {
                // El p99, y nunca por encima de la tercera mayor: con la ventana llena son la
                // misma muestra, y mientras se llena dos tardíos sueltos tampoco mueven el margen
                // (el p99 de menos de 100 muestras sería el máximo).
                let index = ((JITTER_PERCENTILE * n as f64).ceil() as usize)
                    .min(n - 2)
                    .clamp(1, n);
                (window[index - 1] - min).max(0)
            };
        }
        self.stats.jitter_p99_ms = self.jitter_us as f64 / 1000.0;
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
        // El margen sigue a cada llegada: una ráfaga de tramas tardías lo sube en cuanto son
        // tres, no en la siguiente pasada de un periodo largo.
        self.update_target();
        let Some(next) = self.next else {
            if self.started && self.last_played.is_none_or(|(_, flags)| flags & DTX == 0) {
                self.note_gap(1);
            }
            return Pop::Wait;
        };
        let queued = self.queued();
        let desired = self.desired_lead_us();
        // La trama que toca, si está: sus tramas, su adelanto (cuánto hace que llegó) y su
        // adelanto nominal (más lo que su llegada se apartó del mejor tránsito de la ventana), en
        // µs.
        let head = self.slot(next).map(|slot| {
            let lead = now.saturating_duration_since(slot.arrived).as_micros() as i64;
            let jitter =
                (self.transit_us(slot.capture_us, slot.arrived) - self.min_transit_us).max(0);
            (i64::from(slot.frames), lead, lead + jitter)
        });
        if !self.primed {
            // Se empieza cuando lo que toca ya lleva esperando el margen y el aire del cebado (o
            // la primera llegada, si lo que toca no está), o cuando hay en cola más de un
            // objetivo.
            let ready = head.is_some_and(|(_, _, nominal)| nominal >= desired + PRIME_GUARD_US)
                || self.prime_from.is_some_and(|from| {
                    now.saturating_duration_since(from).as_micros() as i64
                        >= desired + PRIME_GUARD_US
                })
                || i64::from(queued) * FRAME_US >= desired + self.packet_us();
            if !ready {
                if self.started && self.last_played.is_none_or(|(_, flags)| flags & DTX == 0) {
                    self.note_gap(1);
                }
                return Pop::Wait;
            }
            self.primed = true;
            self.level = f64::from(queued);
            self.last_pop = None;
            self.reset_windows();
        }
        // Parón del dispositivo: esta petición llega una trama entera o más después de lo que le
        // tocaba por su ritmo (solo cuenta el ritmo de las peticiones, nunca una llegada tardía;
        // la primera petición tras cebar no tiene con qué compararse). Todo lo que se encoló
        // mientras tanto sobra con certeza.
        let device_stall_us = self.last_pop.map_or(0, |previous| {
            now.saturating_duration_since(previous).as_micros() as i64
                - i64::from(elapsed) * FRAME_US
        });
        let stalled = device_stall_us >= FRAME_US;
        self.last_pop = Some(now);
        for _ in 0..elapsed {
            self.level = self.level * 0.9 + f64::from(queued) * 0.1;
        }
        self.stats.level_frames = self.level;
        // Las ventanas son las de las últimas tramas: un vacío de hace segundos no frena el
        // recorte.
        if self.window >= TRIM_WINDOW_FRAMES {
            self.window = 0;
            self.min_nominal_us = i64::MAX;
        }
        self.window += elapsed;
        if self.recent >= ADJUST_PERIOD_FRAMES {
            self.recent = 0;
            self.min_recent_us = i64::MAX;
        }
        self.recent += elapsed;
        if let Some((_, lead, nominal)) = head {
            self.min_nominal_us = self.min_nominal_us.min(nominal);
            self.min_recent_us = self.min_recent_us.min(nominal);
            let nominal = nominal as f64;
            self.nominal = Some(
                self.nominal
                    .map_or(nominal, |previous| previous * 0.9 + nominal * 0.1),
            );
            self.stats.lead_ms = lead as f64 / 1000.0;
        }
        self.since_adjust = self.since_adjust.saturating_add(elapsed);
        let adjust = self.since_adjust >= ADJUST_PERIOD_FRAMES;

        // Sobra holgura (adelanto nominal menos el margen de ahora): se recorta el paquete que
        // toca si está (y hay más detrás). Sobran dos paquetes o más si en las últimas
        // [`ADJUST_PERIOD_FRAMES`] la holgura no bajó del paquete más una trama (una ráfaga tras
        // un parón del dispositivo o de la red): se recortan de una vez todos los que sobran
        // salvo uno (dejando al menos una trama de holgura), porque cada trama de más es latencia
        // durante todo lo que tarde en irse y un salto de 50 ms suena como uno de 10, y lo que
        // quede se recorta un periodo después. Sobra un paquete si en todo un
        // [`TRIM_WINDOW_FRAMES`] la holgura no bajó nunca del paquete: así no se recorta el que
        // hace falta cuando las llegadas se mueven o el ritmo del dispositivo vacila. Y tras un
        // parón del dispositivo (`stalled`) se recorta en esta misma petición lo que sobra según
        // la holgura de la trama que toca, sin esperar las diez tramas: el retraso lo midió
        // nuestro propio reloj, así que no hay duda de que sobra.
        if let Some((head_frames, _, nominal)) = head {
            let head_us = head_frames * FRAME_US;
            let surplus = self
                .min_recent_us
                .saturating_sub(desired)
                .saturating_sub(FRAME_US);
            let burst = self.recent >= ADJUST_PERIOD_FRAMES && surplus >= head_us;
            let one_extra = self.window >= TRIM_WINDOW_FRAMES
                && self.min_nominal_us.saturating_sub(desired) >= head_us + TRIM_HYSTERESIS_US;
            // Tras un parón del dispositivo se deja la holgura justo en el margen (paquetes
            // enteros): el sobrante se midió, no se estimó.
            let stall_surplus = nominal.saturating_sub(desired);
            let stall = stalled && stall_surplus >= head_us;
            if ((adjust && (burst || one_extra)) || stall) && i64::from(queued) > head_frames {
                let mut budget = match (stall, burst) {
                    (true, true) => (stall_surplus / head_us).max(surplus / head_us),
                    (true, false) => stall_surplus / head_us,
                    (false, true) => surplus / head_us,
                    (false, false) => 1,
                };
                let mut seq = next;
                while budget > 0 {
                    // Solo paquetes que están (un hueco es una pérdida, no sobra) y dejando
                    // siempre uno detrás.
                    let Some(frames) = self.slot(seq).map(|slot| i64::from(slot.frames)) else {
                        break;
                    };
                    if i64::from(self.queued()) <= frames {
                        break;
                    }
                    self.take(seq);
                    seq = seq.wrapping_add(1);
                    self.next = Some(seq);
                    self.stats.trimmed += 1;
                    self.level -= frames as f64;
                    budget -= 1;
                }
                self.reset_windows();
                return self.pop_inner();
            }
        }
        // Falta holgura: una trama de PLC sin avanzar. En el acto si alguna de las últimas tramas
        // quedó una trama entera o más por debajo del margen (el margen acaba de subir con una
        // ráfaga de tardías, o las llegadas se retrasaron de golpe: la siguiente puede llegar
        // tarde); si no, cuando el adelanto nominal típico ha bajado del margen, una por periodo.
        // (`i64::MIN` marca que llegó algo tarde en la ventana: eso frena el recorte, pero un
        // tardío suelto no es motivo para estirar, o cada uno costaría tres artefactos.)
        let critical = self.min_recent_us != i64::MIN && self.min_recent_us < desired - FRAME_US;
        let short = self.nominal.is_some_and(|nominal| nominal < desired as f64);
        if (critical || (adjust && short)) && queued > 0 {
            self.stats.stretched += 1;
            self.level += 1.0;
            self.reset_windows();
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
        if self.started && spoke_last && self.run < LOSS_ADVANCE_FRAMES {
            // Lo que toca no llegó y no hay nada detrás: una pérdida (o un tardío), con la cola
            // de un paquete lo normal. Se oculta y se pasa a la siguiente.
            let frames = self.packet_frames;
            self.next = Some(next.wrapping_add(1));
            self.stats.concealed += 1;
            self.note_gap(frames);
            self.elapsed = frames;
            return Pop::Plc(Lost {
                seq: next,
                capture_us: estimate,
                next: None,
                stretch: false,
                frames,
            });
        }
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
        // El emisor calló (DTX) o la cola lleva vacía demasiado: se vuelve a cebar con lo primero
        // que llegue, sea cual sea su secuencia (si el parón fue del emisor, lo que envíe al
        // volver no es tardío).
        self.primed = false;
        self.prime_from = None;
        self.next = None;
        self.highest = None;
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
        // Sin medidas aún, el cebado espera una trama de margen desde la primera llegada.
        assert!((2..=4).contains(&first), "{first}: {out:?}");
        // La trama de margen del arranque se recortó al pasar la ventana (un recorte es una
        // trama que no suena); las demás sonaron todas.
        assert_eq!((stats.trimmed, stats.stretched), (1, 0), "{stats:?}");
        assert_eq!(stats.played + stats.trimmed, 200, "{out:?}");
        assert_eq!(stats.concealed, 0);
        assert_eq!(stats.max_gap_frames, 0, "{out:?}");
        // Sin jitter, el objetivo es el mínimo: la trama que suena, sin margen.
        assert!((stats.target_ms - 10.0).abs() < 1e-6, "{stats:?}");
        assert_eq!(stats.jitter_p99_ms, 0.0, "{stats:?}");
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
        // El margen es el jitter medido (15 ms) y el objetivo, ese margen más la trama.
        assert!((stats.jitter_p99_ms - 15.0).abs() < 1e-6, "{stats:?}");
        assert!(
            stats.target_ms >= 25.0 && stats.target_ms <= 60.0,
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
        assert!((calm.target_ms - 10.0).abs() < 1e-6, "{calm:?}");
        assert!(wild.target_ms <= 60.0);
        assert!(jittery.jitter_p95_ms > 30.0);
        assert!(jittery.jitter_p99_ms >= jittery.jitter_p95_ms);
    }

    #[test]
    fn a_few_late_packets_raise_the_margin_within_the_burst() {
        // Llegadas exactas salvo las tramas 150, 152 y 154, que llegan 25 ms tarde: la primera se
        // oculta y no mueve el margen (un paquete suelto no cuesta dos segundos de retardo); con
        // la segunda (el p99 de menos de 200 muestras es la segunda mayor) el margen ya las cubre
        // y el objetivo sube en el acto, y a los dos segundos de la primera baja solo.
        let mut buffer = JitterBuffer::new();
        let base = Instant::now();
        let late = |seq: u32| matches!(seq, 150 | 152 | 154);
        let mut raised_at = None;
        let mut lowered_at = None;
        for tick in 0..500u32 {
            let now = base + FRAME_DURATION * tick + 5 * MS;
            // Las tramas llegan 3 ms después de completarse (o 28 ms, las tardías).
            for seq in 0..tick + 3 {
                let at = base + FRAME_DURATION * seq + if late(seq) { 28 * MS } else { 3 * MS };
                if at <= now && at > now - FRAME_DURATION {
                    buffer.push(seq, seq * 10_000, 0, &packet(seq), at);
                }
            }
            let _ = buffer.pop(now);
            let stats = buffer.stats();
            if raised_at.is_none() && stats.jitter_p99_ms > 20.0 {
                raised_at = Some(tick);
            }
            if let Some(raised) = raised_at
                && lowered_at.is_none()
                && tick > raised
                && stats.jitter_p99_ms < 1.0
            {
                lowered_at = Some(tick);
            }
        }
        let stats = buffer.stats();
        println!("{stats:?}, sube en {raised_at:?}, baja en {lowered_at:?}");
        // Sube con la segunda tardía (152 llega en la trama 155), no con la primera (150, en la
        // 153) ni 25 tramas después.
        assert!(
            raised_at.is_some_and(|tick| (155..=157).contains(&tick)),
            "{raised_at:?}"
        );
        // Baja cuando la primera tardía sale de la ventana de 2 s (unas 200 tramas después).
        assert!(
            lowered_at.is_some_and(|tick| (350..=360).contains(&tick)),
            "{lowered_at:?}"
        );
        assert!(stats.late >= 1, "{stats:?}");
        assert!(stats.concealed + stats.underruns >= 1, "{stats:?}");
        assert!(stats.max_gap_frames <= 3, "{stats:?}");
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
        // La frase siguiente vuelve a cebarse y suena (con el mismo tránsito: no es jitter).
        let later = base + Duration::from_secs(1);
        buffer.push(5, 1_000_000, 0, b"w", later);
        assert_eq!(buffer.pop(later), Pop::Wait);
        assert!(matches!(
            buffer.pop(later + 25 * MS),
            Pop::Frame(FrameRef { seq: 5, .. })
        ));
    }

    #[test]
    fn extra_delay_moves_the_target() {
        // Sin medidas, el margen es una trama: objetivo de 20 ms.
        let mut buffer = JitterBuffer::new();
        assert_eq!(buffer.target(), Duration::from_millis(20));
        buffer.set_extra_delay(30);
        assert_eq!(buffer.target(), Duration::from_millis(50));
        assert_eq!(buffer.extra_delay_ms(), 30);
        buffer.set_extra_delay(-500);
        assert_eq!(
            buffer.target(),
            Duration::from_millis(20),
            "nunca menos del margen"
        );
        // Medido: el jitter y la trama.
        buffer.jitter_us = 0;
        assert_eq!(buffer.target(), MIN_TARGET);
        buffer.jitter_us = 25_000;
        assert_eq!(buffer.target(), Duration::from_millis(35));
        assert_eq!(buffer.extra_delay_range_ms(), (0, 200));
        buffer.set_extra_delay(20);
        assert_eq!(buffer.target(), Duration::from_millis(55));
    }

    #[test]
    fn av_sync_never_takes_the_target_below_the_jitter() {
        // La sincronía pide todo lo que puede quitar: el objetivo se queda en el jitter medido
        // más la trama; con menos, cada llegada un poco tarde era una trama de PLC (audio
        // robótico).
        let mut buffer = JitterBuffer::new();
        let (min, _) = buffer.extra_delay_range_ms();
        assert_eq!(min, 0, "no hay nada que quitar");
        buffer.set_extra_delay(-200);
        buffer.jitter_us = 0;
        assert_eq!(buffer.target(), MIN_TARGET);
        buffer.jitter_us = 8_000;
        assert_eq!(buffer.target(), Duration::from_millis(18));
        // Llegadas que bailan 1-9 ms con la sincronía pidiendo −50 ms: nada se oculta.
        let wobble = |seq: u32| MS * (seq * 7 % 9);
        let (_, stats) = hold(MS, wobble, -50);
        assert!(stats.target_ms >= 18.0, "{stats:?}");
        assert_eq!(stats.concealed + stats.underruns, 0, "{stats:?}");
    }

    /// Tramas cada 10 ms que llegan `delay` después de capturarse (más `wobble(seq)`), con la
    /// reproducción a mitad de cada trama; `extra` es el ajuste de la sincronía que se pide desde
    /// la trama 50. Devuelve la espera media captura → reproducción del último 1,5 s (de 6 s) y
    /// los contadores.
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
    fn the_queue_settles_at_the_jitter_not_one_frame_above() {
        // Llegan 3 ms después de capturarse y se reproducen a los 5 ms de cada trama: sin jitter
        // la espera es la fase, 5 ms (con el objetivo de dos tramas eran 15, y antes 25). La trama
        // de margen del arranque se recorta al pasar la primera ventana.
        let (wait, stats) = hold(3 * MS, |_| Duration::ZERO, 0);
        assert!((wait - 5.0).abs() < 0.5, "{wait} ms {stats:?}");
        assert_eq!((stats.trimmed, stats.stretched), (1, 0), "{stats:?}");
        // Con la sincronía pidiendo una trama menos, no hay nada que quitar.
        let (wait, stats) = hold(3 * MS, |_| Duration::ZERO, -10);
        assert!((wait - 5.0).abs() < 0.5, "{wait} ms {stats:?}");
        assert_eq!((stats.trimmed, stats.stretched), (1, 0), "{stats:?}");
        // Pidiendo dos tramas más, se mantienen (25 ms) sin estirar ni recortar después.
        let (wait, stats) = hold(3 * MS, |_| Duration::ZERO, 20);
        assert!((wait - 25.0).abs() < 0.5, "{wait} ms {stats:?}");
        assert!(stats.stretched <= 2 && stats.trimmed <= 1, "{stats:?}");
    }

    #[test]
    fn a_burst_after_a_stall_drains_in_well_under_a_second() {
        // Un parón de 60 ms: las tramas 100 a 106 llegan juntas con la 106. Las dos primeras que
        // faltan se dan por perdidas (se ocultan y, al llegar, son tardías); a partir de la
        // tercera es un parón y se espera. Al llegar la ráfaga sobran cuatro tramas: el p99 las
        // cubre mientras están en la ventana de 2 s y, en cuanto salen de ella, se recortan de una
        // vez (con una por segundo tardaba 6 s, y el audio sonaba 60 ms tarde todo ese tiempo).
        let stall = |seq: u32| {
            if (100..106).contains(&seq) {
                FRAME_DURATION * (106 - seq)
            } else {
                Duration::ZERO
            }
        };
        let (wait, stats) = hold(3 * MS, stall, 0);
        assert!((wait - 5.0).abs() < 0.5, "{wait} ms {stats:?}");
        // Las seis tramas de más se fueron: dos como pérdidas y el resto recortadas (más la del
        // arranque).
        assert_eq!((stats.concealed, stats.late), (2, 2), "{stats:?}");
        assert!(stats.trimmed + stats.late >= 6, "{stats:?}");
        // El propio parón vacía la cola una vez; los recortes no añaden ninguna.
        assert!(stats.underruns <= 1, "{stats:?}");
    }

    #[test]
    fn a_two_frame_hiccup_is_concealed_without_growing_the_queue() {
        // La trama 60 llega 25 ms tarde y, en esta simulación, retiene a la 61: dos tramas que
        // faltan con la cola vacía. Se ocultan y se pasa de ellas (al llegar son tardías), la
        // cola no crece ni una trama y la espera sigue siendo la fase: antes el vacío frenaba el
        // recorte para siempre, y después se esperaba y quedaban dos tramas de más durante 2 s.
        let late = |seq: u32| if seq == 60 { 25 * MS } else { Duration::ZERO };
        let (wait, stats) = hold(3 * MS, late, 0);
        assert!((wait - 5.0).abs() < 0.5, "{wait} ms {stats:?}");
        assert_eq!((stats.concealed, stats.late), (2, 2), "{stats:?}");
        assert_eq!((stats.trimmed, stats.stretched), (1, 0), "{stats:?}");
        assert_eq!(stats.underruns, 0, "{stats:?}");
    }

    #[test]
    fn twenty_millisecond_packets_are_measured_in_frames() {
        // Paquetes de 20 ms (dos tramas) que llegan 3 ms después de completarse; la reproducción
        // pide cada 10 ms salvo cuando aún suena la segunda trama del paquete anterior (como
        // `Stream::tick`). El objetivo cuenta el paquete entero: 20 ms sin jitter, y con el
        // adelanto de 12 ms del arranque (menos de un paquete sobre el margen) no hace falta
        // estirar ni recortar.
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
        assert!((stats.lead_ms - 12.0).abs() < 0.5, "{stats:?}");
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
        // Llegan entre 4 y 6 ms, a un lado u otro de la reproducción (5 ms): la trama que toca a
        // veces ya está y a veces no. Hace falta una trama de cola (el adelanto queda en 9-11 ms,
        // más que el jitter de 2 ms) y no se recorta (cada recorte quita 10 ms de audio) ni falta.
        let wobble = |seq: u32| {
            Duration::from_micros(u64::from(seq.wrapping_mul(2_654_435_761) >> 22) % 2000)
        };
        let (wait, stats) = hold(4 * MS, wobble, -10);
        assert_eq!((stats.trimmed, stats.stretched), (0, 0), "{stats:?}");
        assert_eq!(stats.concealed + stats.underruns, 0, "{stats:?}");
        assert!(wait < 16.0 && wait > 14.0, "{wait} ms");
    }

    #[test]
    fn slow_clock_drift_costs_one_adjustment_per_frame_of_drift() {
        // Los relojes del emisor y del dispositivo derivan: las llegadas se adelantan 1 ms por
        // segundo respecto a la reproducción (mucho más que un cristal real). Cada 10 s sobra una
        // trama y se recorta una vez; entre medias, nada (ni pares recorte/estiramiento).
        let mut buffer = JitterBuffer::new();
        let base = Instant::now();
        let mut seq = 0u32;
        for tick in 0..3_000u32 {
            let now = base + FRAME_DURATION * tick + 5 * MS;
            loop {
                // 3 ms tras la captura, menos la deriva acumulada (1 ms por segundo).
                let drift = Duration::from_micros(u64::from(seq) * 10);
                let at = base + FRAME_DURATION * seq + 3 * MS - drift.min(3 * MS - MS);
                if at > now {
                    break;
                }
                buffer.push(seq, seq * 10_000, 0, &packet(seq), at);
                seq += 1;
            }
            let _ = buffer.pop(now);
        }
        let stats = buffer.stats();
        println!("{stats:?}");
        assert_eq!(stats.concealed + stats.underruns, 0, "{stats:?}");
        assert_eq!(stats.stretched, 0, "{stats:?}");
        // El arranque y, como mucho, uno por trama de deriva (2 ms en 30 s no llegan a una).
        assert!(stats.trimmed <= 2, "{stats:?}");
    }

    #[test]
    fn arrivals_getting_later_cost_a_few_late_frames_and_one_stretch() {
        // Llegadas 1-9 ms después de la captura (jitter de 8 ms) y reproducción a los 15 ms: el
        // adelanto nominal es de 14 ms, 6 de holgura. A partir de la trama 200 se retrasan 8 ms de
        // golpe (cambio de ruta): las primeras que pasan de la holgura llegan tarde, el p99 las
        // recoge en cuanto son tres (la ventana no cambia hasta que entra la segunda, y el p99 de
        // 200 muestras es la tercera mayor), el buffer estira una trama y ya no llega ninguna tarde.
        let shift = |seq: u32| MS * (1 + u32::from(seq >= 200) * 8 + seq * 7 % 9);
        let mut buffer = JitterBuffer::new();
        let base = Instant::now();
        let mut seq = 0u32;
        let mut settled = None;
        for tick in 0..400u32 {
            let now = base + FRAME_DURATION * tick + 5 * MS;
            loop {
                let at = base + FRAME_DURATION * seq + shift(seq);
                if at > now {
                    break;
                }
                buffer.push(seq, seq * 10_000, 0, &packet(seq), at);
                seq += 1;
            }
            let _ = buffer.pop(now);
            if tick == 230 {
                settled = Some(buffer.stats());
            }
        }
        let stats = buffer.stats();
        println!("{stats:?}");
        assert_eq!(stats.underruns, 0, "{stats:?}");
        assert!(stats.late <= 3, "{stats:?}");
        assert!(stats.concealed <= 3, "{stats:?}");
        assert_eq!(stats.stretched, 1, "{stats:?}");
        // Tras el estirón (dentro de los 300 ms del cambio), nada más se pierde ni llega tarde; a
        // los dos segundos la ventana olvida la ruta vieja y el margen vuelve a ser el jitter.
        let settled = settled.expect("foto a la trama 230");
        assert!((settled.jitter_p99_ms - 16.0).abs() < 1e-6, "{settled:?}");
        assert!(stats.jitter_p99_ms <= 9.0, "{stats:?}");
        assert_eq!(
            (stats.late, stats.concealed),
            (settled.late, settled.concealed),
            "{settled:?}"
        );
    }

    #[test]
    fn a_device_stall_is_trimmed_on_the_next_request() {
        // Enlace estable (llegadas 2 ms después de la captura, peticiones a los 0,5 ms de cada
        // trama: espera de 10,5 ms en régimen). El dispositivo deja de pedir 50 ms (las
        // peticiones 200 a 204 no llegan): la siguiente petición encuentra cinco tramas de más y
        // las recorta todas en el acto, sin esperar diez tramas; la espera vuelve a ser la de
        // antes desde esa misma petición y después no se recorta ni estira nada más.
        let mut buffer = JitterBuffer::new();
        let base = Instant::now();
        let mut seq = 0u32;
        let mut waits = Vec::new();
        let mut after_stall = None;
        for tick in 0..400u32 {
            let now = base + FRAME_DURATION * tick + Duration::from_micros(500);
            loop {
                let at = base + FRAME_DURATION * seq + 2 * MS;
                if at > now {
                    break;
                }
                buffer.push(seq, seq * 10_000, 0, &packet(seq), at);
                seq += 1;
            }
            if (200..205).contains(&tick) {
                continue;
            }
            match buffer.pop(now) {
                Pop::Frame(frame) => {
                    let wait = (now - (base + FRAME_DURATION * frame.seq)).as_secs_f64() * 1000.0;
                    waits.push((tick, wait));
                }
                // Cebando, aún no suena nada.
                Pop::Wait if waits.is_empty() => {}
                other => panic!("tick {tick}: {other:?}"),
            }
            if tick == 205 {
                after_stall = Some(buffer.stats());
            }
        }
        let stats = buffer.stats();
        println!("{stats:?}");
        let after_stall = after_stall.expect("foto tras el parón");
        // Antes del parón (una vez recortada la trama del arranque) y desde la primera petición
        // tras él, la misma espera.
        for (tick, wait) in &waits {
            if (150..200).contains(tick) || *tick >= 205 {
                assert!(
                    (wait - 10.5).abs() < 0.6,
                    "tick {tick}: {wait} ms {stats:?}"
                );
            }
        }
        // Cinco tramas recortadas en esa petición (más la del arranque), y nada más después.
        assert_eq!(after_stall.trimmed, 6, "{after_stall:?}");
        assert_eq!(stats.trimmed, after_stall.trimmed, "{stats:?}");
        assert_eq!(
            (
                stats.concealed,
                stats.stretched,
                stats.late,
                stats.underruns
            ),
            (0, 0, 0, 0),
            "{stats:?}"
        );
    }

    #[test]
    fn one_late_packet_costs_one_plc_and_nothing_else() {
        // Enlace estable con un paquete suelto que llega 25 ms tarde: se oculta una vez y ya. Ni
        // estira (el tardío marca la ventana para no recortar, no para estirar), ni recorta de
        // más, ni mueve el margen (un tardío entre 200 no cambia el p99).
        let (_, steady) = run(300, |_| Some(2 * MS));
        let (out, stats) = run(300, |seq| Some(if seq == 30 { 27 * MS } else { 2 * MS }));
        println!("{stats:?}");
        assert_eq!(stats.late, 1, "{stats:?}");
        assert_eq!(stats.concealed, 1, "{stats:?}");
        assert_eq!(
            out.iter().filter(|s| s.starts_with('L')).count(),
            1,
            "{out:?}"
        );
        assert_eq!(stats.stretched, steady.stretched, "{stats:?}");
        assert_eq!(stats.trimmed, steady.trimmed, "{stats:?}");
        assert_eq!(stats.underruns, 0, "{stats:?}");
        assert_eq!(stats.max_gap_frames, 1, "{stats:?}");
        assert!(
            (stats.target_ms - steady.target_ms).abs() < 1e-6,
            "{stats:?}"
        );
    }

    #[test]
    fn priming_leaves_air_for_requests_off_the_grid() {
        // Llegadas 8,6 ms después de la captura y peticiones a los 0,5 ms de cada trama: sin
        // aire, el cebado dejaría 1,9 ms de holgura. Cada tercera petición cae 2,5 ms antes (como
        // las de PipeWire fuera de la rejilla): con 1,9 ms de holgura llegarían tarde; con el aire
        // del cebado, ninguna.
        let mut buffer = JitterBuffer::new();
        let base = Instant::now();
        let mut seq = 0u32;
        for tick in 0..300u32 {
            let early = if tick % 3 == 2 { 2_500 } else { 0 };
            let now = base + FRAME_DURATION * tick + Duration::from_micros(500)
                - Duration::from_micros(early);
            loop {
                let at = base + FRAME_DURATION * seq + Duration::from_micros(8_600);
                if at > now {
                    break;
                }
                buffer.push(seq, seq * 10_000, 0, &packet(seq), at);
                seq += 1;
            }
            let _ = buffer.pop(now);
        }
        let stats = buffer.stats();
        println!("{stats:?}");
        assert_eq!(
            (stats.late, stats.concealed, stats.underruns),
            (0, 0, 0),
            "{stats:?}"
        );
        assert!(stats.played >= 290, "{stats:?}");
    }
}
