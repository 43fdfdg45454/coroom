//! Relay del servidor (`coroom_server::relay::Relay` como gancho del socket, por el arnés
//! `fuzzing`): tramas arbitrarias desde cuatro direcciones, intercaladas con ligaduras válidas del
//! anfitrión y del invitado de una sala, no producen pánicos ni más ligaduras que pares tiene la
//! sala, y lo que no es del relay se deja a QUIC.

#![no_main]

use std::sync::OnceLock;

use coroom_fuzz::Bytes;
use coroom_server::fuzzing::Harness;
use libfuzzer_sys::fuzz_target;

fn harness() -> &'static Harness {
    static HARNESS: OnceLock<Harness> = OnceLock::new();
    HARNESS.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("coroom-fuzz-relay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Harness::new(&dir).expect("arnés del servidor")
    })
}

fuzz_target!(|data: &[u8]| {
    let harness = harness();
    let mut input = Bytes::new(data);
    while !input.is_empty() {
        let op = input.u8();
        let from = op >> 4;
        match op % 4 {
            0 => harness.relay_bind(from, false),
            1 => harness.relay_bind(from, true),
            _ => {
                let frame = input.chunk();
                let relay = harness.relay_frame(from, frame);
                if frame.first().is_some_and(|first| first & 0x40 != 0) {
                    assert!(!relay, "un paquete de QUIC no es del relay");
                }
            }
        }
        assert!(harness.relay_bound() <= 2, "solo se ligan los dos pares de la sala");
    }
});
