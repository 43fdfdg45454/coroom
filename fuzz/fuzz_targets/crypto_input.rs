//! Cifrado de la entrada (`crypto::input::InputCipher`) con un secreto pareja a pareja fijo. El
//! primer byte elige el modo:
//!
//! - `0`: sellados arbitrarios (`u16` de longitud + bytes) que abre el anfitrión: ninguno se
//!   autentica y la ventana anti-repetición no entra en pánico con secuencias arbitrarias;
//! - `1`: el invitado sella el resto con secuencias que salta el fuzzer y el anfitrión lo abre
//!   igual; reabrirlo es una repetición.

#![no_main]

use coroom_crypto::hpke::{PAIRWISE_SECRET_LENGTH, PairwiseSecret};
use coroom_crypto::input::{Channel, InputCipher, InputCryptoError, Role};
use coroom_fuzz::Bytes;
use libfuzzer_sys::fuzz_target;

fn cipher(role: Role) -> InputCipher {
    let secret = PairwiseSecret::new([0x42; PAIRWISE_SECRET_LENGTH]);
    InputCipher::from_pairwise(&secret, role).expect("claves de entrada")
}

fuzz_target!(|data: &[u8]| {
    let mut input = Bytes::new(data);
    let mut host = cipher(Role::Host);
    let mut out = Vec::new();
    match input.u8() % 2 {
        0 => {
            while !input.is_empty() {
                let _ = host.open(input.chunk(), &mut out);
            }
        }
        _ => {
            let mut guest = cipher(Role::Guest);
            let mut sealed = Vec::new();
            while !input.is_empty() {
                let channel = if input.u8().is_multiple_of(2) {
                    Channel::Stream
                } else {
                    Channel::Datagram
                };
                let skip = u64::from(input.u8());
                let plaintext = input.chunk();
                let seq = guest.next_seq(channel).saturating_add(skip);
                sealed.clear();
                if guest.seal(channel, seq, plaintext, &mut sealed).is_err() {
                    continue;
                }
                let opened = host.open(&sealed, &mut out).expect("abrir lo sellado");
                assert_eq!(opened.seq, seq);
                assert_eq!(opened.plaintext, plaintext);
                assert!(matches!(
                    host.open(&sealed, &mut out),
                    Err(InputCryptoError::Replayed(_))
                ));
            }
        }
    }
});
