//! Señalización del servidor (`coroom_server::signal::Hub::run` por el arnés `fuzzing`): una
//! sesión recibe bytes arbitrarios (con un `Auth` de desarrollo válido delante si el primer byte es
//! impar) y se cierra; nada entra en pánico, la sesión termina y no quedan salas abiertas.

#![no_main]

use std::sync::OnceLock;

use coroom_fuzz::Bytes;
use coroom_server::fuzzing::Harness;
use libfuzzer_sys::fuzz_target;

fn harness() -> &'static Harness {
    static HARNESS: OnceLock<Harness> = OnceLock::new();
    HARNESS.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("coroom-fuzz-signal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Harness::new(&dir).expect("arnés del servidor")
    })
}

fuzz_target!(|data: &[u8]| {
    let harness = harness();
    let mut input = Bytes::new(data);
    let authenticate = !input.u8().is_multiple_of(2);
    let _replies = harness.signal(authenticate, input.rest());
    assert_eq!(harness.open_rooms(), 0, "la sala se cierra con su sesión");
});
