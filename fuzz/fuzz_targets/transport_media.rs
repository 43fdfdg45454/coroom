//! Reensamblado de medios con FEC y NACK (`transport::media::ReceiverCore`) a partir de
//! datagramas arbitrarios y de un reloj que avanza: sin pánicos y con la memoria acotada por
//! `MAX_OPEN_GROUPS` y `MAX_PENDING_OBJECTS` (hallazgo M-1 de `docs/seguridad.md`).
//!
//! Entrada: una serie de órdenes. Byte `op`: si `op % 4 == 0`, avanza el reloj `u8` ms y vence
//! plazos (`poll`, con un RTT de 4 a 256 ms); con `op == 0xFF`, fija el intervalo del vídeo a un
//! `u16` × 10 µs (como `CodecSelected`, que el receptor acota); si no, entrega el datagrama que
//! sigue (`u16` de longitud + bytes).

#![no_main]

use std::time::{Duration, Instant};

use coroom_fuzz::Bytes;
use coroom_transport::media::{MAX_OPEN_GROUPS, MAX_PENDING_OBJECTS, ReceiverCore};
use coroom_transport::proto::media::Track;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut input = Bytes::new(data);
    let mut core = ReceiverCore::new(Duration::from_micros(16_667));
    let mut now = Instant::now();
    while !input.is_empty() {
        let op = input.u8();
        if op.is_multiple_of(4) {
            now += Duration::from_millis(u64::from(input.u8()));
            core.poll(now, Duration::from_millis((u64::from(op) / 4 + 1) * 4));
        } else if op == 0xFF {
            let interval = Duration::from_micros(u64::from(input.u16()) * 10);
            core.set_track_interval(Track::Video, interval);
        } else {
            let datagram = input.chunk();
            core.push(datagram, now, Some(i32::from(op)));
        }
        while core.pop_frame().is_some() {}
        let _ = core.take_messages();
        let _ = core.next_timer();
        assert!(core.pending_objects() <= MAX_PENDING_OBJECTS);
        assert!(core.open_group_count() <= MAX_OPEN_GROUPS * Track::ALL.len());
    }
    let _ = core.feedback();
});
