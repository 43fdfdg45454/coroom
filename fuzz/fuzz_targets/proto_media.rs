//! Cabecera de objetos de medios v2 (`proto::media`): `parse` y `parse_datagram` de bytes
//! arbitrarios no entran en pánico, y lo que se acepta vuelve a serializarse igual.

#![no_main]

use coroom_proto::media::ObjectHeader;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(header) = ObjectHeader::parse(data) {
        let bytes = header.to_bytes();
        assert_eq!(ObjectHeader::parse(&bytes), Ok(header));
        // El byte reservado se ignora al leer y se escribe a 0: el resto coincide.
        assert_eq!(bytes[..3], data[..3]);
        assert_eq!(bytes[4..], data[4..ObjectHeader::LEN]);
    }
    if let Ok((header, payload)) = ObjectHeader::parse_datagram(data) {
        assert_eq!(payload.len(), usize::from(header.payload_len));
        assert!(payload.len() <= ObjectHeader::MAX_PAYLOAD);
    }
});
