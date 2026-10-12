//! Rutas HTTP del servidor (`coroom_server::routes::dispatch` por el arnés `fuzzing`): cuerpos
//! JSON, rutas y consultas arbitrarias, con y sin la sesión del administrador. Nunca un pánico ni
//! un 5xx: una entrada mala es un 4xx.
//!
//! Entrada: método (`u8`), sesión (`u8`: 0 ninguna, 1 administrador, 2 token falso), ruta hasta
//! el primer `0x00` (si no empieza por `/`, se le pone `/api/v1/` delante) y el cuerpo.

#![no_main]

use std::sync::OnceLock;

use coroom_fuzz::Bytes;
use coroom_server::fuzzing::{Harness, METHODS};
use libfuzzer_sys::fuzz_target;

fn harness() -> &'static Harness {
    static HARNESS: OnceLock<Harness> = OnceLock::new();
    HARNESS.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("coroom-fuzz-api-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Harness::new(&dir).expect("arnés del servidor")
    })
}

fuzz_target!(|data: &[u8]| {
    let harness = harness();
    let mut input = Bytes::new(data);
    let method = &METHODS[usize::from(input.u8()) % METHODS.len()];
    let auth = match input.u8() % 3 {
        0 => None,
        1 => Some(harness.admin_token()),
        _ => Some("00ff00ff"),
    };
    let rest = input.rest();
    let split = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    let path = String::from_utf8_lossy(&rest[..split]);
    let body = rest.get(split + 1..).unwrap_or_default();
    let path = if path.starts_with('/') {
        path.into_owned()
    } else {
        format!("/api/v1/{path}")
    };
    let (status, response) = harness.request(method, &path, auth, body);
    assert!(
        (200..500).contains(&status),
        "{method} {path}: {status} {}",
        String::from_utf8_lossy(&response)
    );
});
