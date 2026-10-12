//! Descifrado SFrame (`crypto::sframe`) de bytes arbitrarios con claves instaladas (dos épocas,
//! tres emisores). El primer byte elige el modo:
//!
//! - `0`: paquetes arbitrarios (`u16` de longitud + bytes) por el mismo descifrador: ninguno se
//!   autentica (salvo que el fuzzer reproduzca una semilla válida) y nada entra en pánico;
//! - `1`: el resto se cifra como emisor `resto[0] % 3` y se descifra igual; repetirlo es
//!   `Replay`; cambiar cualquier byte lo invalida.

#![no_main]

use std::sync::OnceLock;

use coroom_crypto::sframe::{MEDIA_KEY_LENGTH, MediaError, MediaKey, MediaKeyRing};
use coroom_fuzz::Bytes;
use libfuzzer_sys::fuzz_target;

fn ring() -> &'static MediaKeyRing {
    static RING: OnceLock<MediaKeyRing> = OnceLock::new();
    RING.get_or_init(|| {
        let ring = MediaKeyRing::new();
        for epoch in [1u64, 2] {
            let members: Vec<(u32, MediaKey)> = (0..3u32)
                .map(|leaf| {
                    (
                        leaf,
                        MediaKey::new([epoch as u8 * 16 + leaf as u8; MEDIA_KEY_LENGTH]),
                    )
                })
                .collect();
            if ring.install_epoch(epoch, &members).is_err() {
                panic!("instalar la época {epoch}");
            }
        }
        ring
    })
}

fuzz_target!(|data: &[u8]| {
    let ring = ring();
    let mut input = Bytes::new(data);
    let mut decryptor = ring.decryptor();
    let mut out = Vec::new();
    match input.u8() % 2 {
        0 => {
            while !input.is_empty() {
                let _ = decryptor.decrypt_into(input.chunk(), &mut out);
            }
        }
        _ => {
            let leaf = u32::from(input.u8() % 3);
            let track = input.u8();
            let flip = usize::from(input.u16());
            let plaintext = input.rest();
            let sealed = ring
                .encryptor(leaf)
                .encrypt(track, plaintext)
                .expect("cifrar con clave instalada");
            let opened = decryptor.decrypt(&sealed).expect("descifrar lo propio");
            assert_eq!(opened.payload, plaintext);
            assert_eq!(opened.sender_leaf, leaf);
            assert_eq!(opened.track, track);
            assert!(matches!(
                decryptor.decrypt(&sealed),
                Err(MediaError::Replay { .. })
            ));
            let mut tampered = sealed.clone();
            let at = flip % tampered.len();
            tampered[at] ^= 0x80;
            assert!(ring.decryptor().decrypt(&tampered).is_err());
        }
    }
});
