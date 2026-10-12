//! Reensamblado del bucle local (`transport::loopback::Reassembler`) con datagramas
//! arbitrarios (`u16` de longitud + bytes cada uno): sin pánicos y con los fotogramas en vuelo
//! acotados.

#![no_main]

use coroom_fuzz::Bytes;
use coroom_transport::loopback::{MAX_FRAMES_IN_FLIGHT, Reassembler};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut input = Bytes::new(data);
    let mut reassembler = Reassembler::new();
    while !input.is_empty() {
        let _ = reassembler.push(input.chunk());
        assert!(reassembler.in_flight() <= MAX_FRAMES_IN_FLIGHT);
    }
});
