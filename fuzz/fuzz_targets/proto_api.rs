//! Cuerpos JSON de la API (`proto::api`): el primer byte elige el tipo; lo que se acepta vuelve a
//! serializarse de forma estable.

#![no_main]

use coroom_fuzz::Bytes;
use coroom_proto::api::*;
use coroom_proto::hex;
use libfuzzer_sys::fuzz_target;
use serde::Serialize;
use serde::de::DeserializeOwned;

fn round_trip<T: Serialize + DeserializeOwned>(body: &[u8]) {
    if let Ok(value) = serde_json::from_slice::<T>(body) {
        let once = serde_json::to_vec(&value).expect("se vuelve a serializar");
        let again: T = serde_json::from_slice(&once).expect("y a leer");
        assert_eq!(serde_json::to_vec(&again).expect("estable"), once);
    }
}

fuzz_target!(|data: &[u8]| {
    let mut input = Bytes::new(data);
    let kind = input.u8();
    let body = input.rest();
    match kind % 24 {
        0 => round_trip::<SetupRequest>(body),
        1 => round_trip::<LoginRequest>(body),
        2 => round_trip::<LoginChallenge>(body),
        3 => round_trip::<LoginComplete>(body),
        4 => round_trip::<SessionToken>(body),
        5 => round_trip::<RedeemRequest>(body),
        6 => round_trip::<AddFriend>(body),
        7 => round_trip::<CreateUser>(body),
        8 => round_trip::<UpdateUser>(body),
        9 => round_trip::<SetPassword>(body),
        10 => round_trip::<CreateInvitation>(body),
        11 => round_trip::<Me>(body),
        12 => round_trip::<Friends>(body),
        13 => round_trip::<Usage>(body),
        14 => round_trip::<AdminUsage>(body),
        15 => round_trip::<Rooms>(body),
        16 => round_trip::<Settings>(body),
        17 => round_trip::<ApiError>(body),
        18 => round_trip::<SetupStatus>(body),
        19 => round_trip::<Devices>(body),
        20 => round_trip::<AdminUser>(body),
        21 => round_trip::<Invitation>(body),
        22 => round_trip::<CreatedInvitation>(body),
        _ => {
            // Hexadecimal de claves y firmas.
            if let Ok(text) = std::str::from_utf8(body)
                && let Some(bytes) = hex::decode(text)
            {
                assert_eq!(hex::encode(&bytes), text.to_ascii_lowercase());
                let _ = hex::decode_array::<32>(text);
            }
        }
    }
});
