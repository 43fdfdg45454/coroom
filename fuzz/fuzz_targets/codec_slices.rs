//! Separación de slices de un paquete del codificador (`codec::slices::split_slices`) con bytes
//! arbitrarios: H.264 y HEVC (Annex B) y AV1 (OBU). Los rangos cubren el paquete en orden y sin
//! huecos.

#![no_main]

use coroom_codec::Codec;
use coroom_codec::slices::split_slices;
use coroom_fuzz::Bytes;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut input = Bytes::new(data);
    let codec = match input.u8() % 3 {
        0 => Codec::H264,
        1 => Codec::Hevc,
        _ => Codec::Av1,
    };
    let packet = input.rest();
    let ranges = split_slices(codec, packet);
    let mut next = 0;
    for range in &ranges {
        assert_eq!(range.start, next, "{ranges:?}");
        assert!(range.end > range.start, "{ranges:?}");
        next = range.end;
    }
    assert_eq!(next, packet.len(), "{ranges:?}");
});
