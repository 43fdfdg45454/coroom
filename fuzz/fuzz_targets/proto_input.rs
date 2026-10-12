//! Formato de la entrada del invitado (`proto::input`): datagramas, preámbulo y tramas del
//! stream, `InputPacket` e `InputMessage` (postcard). El primer byte elige qué se prueba.

#![no_main]

use coroom_fuzz::Bytes;
use coroom_proto::input::{
    InputMessage, InputPacket, MAX_EVENTS, StreamFrames, parse_datagram, parse_stream_preamble,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut input = Bytes::new(data);
    match input.u8() % 4 {
        0 => {
            if let Ok(packet) = InputPacket::from_bytes(input.rest()) {
                assert!(packet.events.len() <= MAX_EVENTS);
                let bytes = packet.to_bytes().expect("se vuelve a serializar");
                assert_eq!(InputPacket::from_bytes(&bytes).as_ref(), Ok(&packet));
            }
        }
        1 => {
            if let Ok(message) = InputMessage::from_bytes(input.rest()) {
                assert!(message.events.len() <= MAX_EVENTS);
                let bytes = message.to_bytes().expect("se vuelve a serializar");
                assert_eq!(InputMessage::from_bytes(&bytes).as_ref(), Ok(&message));
            }
        }
        2 => {
            let rest = input.rest();
            let _ = parse_datagram(rest);
            if let Some(preamble) = rest.first_chunk() {
                let _ = parse_stream_preamble(*preamble);
            }
        }
        _ => {
            // El stream llega en trozos arbitrarios; lo pendiente nunca pasa de lo recibido.
            let mut frames = StreamFrames::new();
            let mut received = 0;
            while !input.is_empty() {
                let chunk = input.chunk();
                received += chunk.len();
                frames.extend(chunk);
                loop {
                    match frames.next_frame() {
                        Ok(Some(frame)) => assert!(!frame.is_empty()),
                        Ok(None) => break,
                        Err(_) => return,
                    }
                }
                assert!(frames.pending() <= received);
            }
        }
    }
});
