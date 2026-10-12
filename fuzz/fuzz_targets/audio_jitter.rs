//! Buffer de jitter del audio (`audio::jitter::JitterBuffer`) con secuencias de paquetes
//! arbitrarias (desorden, duplicados, saltos y vueltas de la secuencia de 32 bits) y un reloj que
//! avanza: sin pánicos.
//!
//! Entrada: órdenes. Byte `op`: si `op % 3 == 0`, `pop` tras avanzar `u8` ms; si no, `push` con
//! secuencia `u32`, captura `u32`, banderas `u8`, avance `u8` ms y el paquete (`u16` + bytes).

#![no_main]

use std::time::{Duration, Instant};

use coroom_audio::jitter::{JitterBuffer, Pop};
use coroom_fuzz::Bytes;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut input = Bytes::new(data);
    let mut buffer = JitterBuffer::new();
    let mut now = Instant::now();
    while !input.is_empty() {
        let op = input.u8();
        if op.is_multiple_of(3) {
            now += Duration::from_millis(u64::from(input.u8()));
            match buffer.pop(now) {
                Pop::Frame(frame) => assert!(!frame.data.is_empty()),
                Pop::Plc(_) | Pop::Wait => {}
            }
            if op.is_multiple_of(2) {
                buffer.set_extra_delay(i32::from(op) - 128);
            }
        } else {
            let seq = input.u32();
            let capture = input.u32();
            let flags = input.u8();
            now += Duration::from_millis(u64::from(input.u8()));
            let packet = input.chunk();
            let _ = buffer.push(seq, capture, flags, packet, now);
        }
    }
    let _ = buffer.stats();
});
