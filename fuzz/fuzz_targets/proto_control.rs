//! Mensajes de control (`proto::control`) enmarcados por longitud (`proto::codec::FrameBuffer`):
//! trozos arbitrarios de un stream no producen pánicos ni más memoria que la cota de un mensaje, y
//! cada mensaje aceptado vuelve a codificarse y decodificarse igual.

#![no_main]

use coroom_fuzz::Bytes;
use coroom_proto::codec::{self, FrameBuffer, MAX_MESSAGE_LEN, PREFIX_LEN};
use coroom_proto::control::ControlMessage;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut input = Bytes::new(data);
    let mut buffer = FrameBuffer::new();
    while !input.is_empty() {
        buffer.extend(input.chunk());
        loop {
            match buffer.next_message::<ControlMessage>() {
                Ok(Some(message)) => {
                    let frame = codec::encode(&message).expect("se vuelve a codificar");
                    let decoded: ControlMessage =
                        codec::decode(&frame[PREFIX_LEN..]).expect("y a decodificar");
                    assert_eq!(decoded, message);
                }
                Ok(None) => break,
                Err(_) => return,
            }
        }
        assert!(buffer.pending() <= PREFIX_LEN + MAX_MESSAGE_LEN + usize::from(u16::MAX));
    }
});
