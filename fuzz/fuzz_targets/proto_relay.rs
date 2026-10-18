//! Tramas del relay (`proto::relay`): ninguna entrada produce pánicos, lo que no es una trama del
//! relay nunca se toma por una (va a QUIC), y cada trama aceptada vuelve a codificarse y
//! decodificarse igual.

#![no_main]

use coroom_proto::relay::{DATA_HEADER, Frame, data_peer, is_relay_frame, set_data_peer};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some(frame) = Frame::parse(data) else {
        return;
    };
    assert!(is_relay_frame(data), "una trama del relay no tiene el bit fijo de QUIC");
    let encoded = frame.encode();
    assert_eq!(Frame::parse(&encoded), Some(frame), "ida y vuelta");
    if let Frame::Data { peer, packet } = frame {
        assert_eq!(data_peer(data), Some(peer));
        assert_eq!(&encoded[DATA_HEADER..], packet);
        let mut rewritten = data.to_vec();
        set_data_peer(&mut rewritten, peer.wrapping_add(1));
        assert_eq!(data_peer(&rewritten), Some(peer.wrapping_add(1)));
    }
});
