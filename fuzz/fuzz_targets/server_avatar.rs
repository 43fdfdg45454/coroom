//! Validación de los avatares que se suben (`coroom_server::avatar::validate`) con bytes
//! arbitrarios: nunca un pánico ni más memoria de la que fijan sus límites (cabeceras que dicen
//! medir 60 000 píxeles, fragmentos que descomprimen de más). Lo que acepta es un PNG cuadrado del
//! tamaño permitido, y volver a validarlo da los mismos bytes y la misma versión.
//!
//! Entrada: el fichero entero.

#![no_main]

use coroom_proto::api::{AVATAR_SIDE_MAX, AVATAR_SIDE_MIN};
use coroom_server::avatar::validate;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(avatar) = validate(data) else {
        return;
    };
    assert!((AVATAR_SIDE_MIN..=AVATAR_SIDE_MAX).contains(&avatar.side));
    let again = validate(&avatar.png).expect("el PNG propio del servidor vale");
    assert_eq!(again.png, avatar.png, "volver a codificarlo no lo cambia");
    assert_eq!(again.version, avatar.version);
});
