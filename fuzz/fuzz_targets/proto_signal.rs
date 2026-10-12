//! Mensajes de señalización (`proto::signal`) con el máximo del servidor
//! (`coroom_server::signal::MAX_SIGNAL_MESSAGE`): sin pánicos y con ida y vuelta exacta.

#![no_main]

use coroom_fuzz::Bytes;
use coroom_proto::codec::{self, FrameBuffer, PREFIX_LEN};
use coroom_proto::signal::{SignalMessage, is_valid_room_code};
use coroom_server::signal::MAX_SIGNAL_MESSAGE;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut input = Bytes::new(data);
    let mut buffer = FrameBuffer::with_max(MAX_SIGNAL_MESSAGE);
    while !input.is_empty() {
        buffer.extend(input.chunk());
        loop {
            match buffer.next_message::<SignalMessage>() {
                Ok(Some(message)) => {
                    if let SignalMessage::JoinRoom { code, .. } = &message {
                        let _ = is_valid_room_code(code);
                    }
                    let frame = codec::encode(&message).expect("se vuelve a codificar");
                    let decoded: SignalMessage =
                        codec::decode(&frame[PREFIX_LEN..]).expect("y a decodificar");
                    assert_eq!(decoded, message);
                }
                Ok(None) => break,
                Err(_) => return,
            }
        }
        assert!(buffer.pending() <= PREFIX_LEN + MAX_SIGNAL_MESSAGE + usize::from(u16::MAX));
    }
});
