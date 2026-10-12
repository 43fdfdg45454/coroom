//! Cabecera de los paquetes de audio (`proto::audio`): sin pánicos y con ida y vuelta exacta.

#![no_main]

use coroom_proto::audio::{AudioHeader, is_audio_datagram};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = is_audio_datagram(data);
    if let Ok((header, rest)) = AudioHeader::parse(data) {
        let bytes = header.to_bytes();
        assert_eq!(&data[..bytes.len()], &bytes[..]);
        assert_eq!(rest.len(), data.len() - bytes.len());
        assert_eq!(AudioHeader::parse(&bytes).map(|(h, _)| h), Ok(header));
    }
});
