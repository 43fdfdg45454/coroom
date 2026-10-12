//! Objetos de las pistas de audio: [`Track::Audio`](crate::media::Track::Audio) (el juego,
//! del anfitrión) y [`Track::Voice`](crate::media::Track::Voice) (la voz de cada miembro).
//!
//! Cada objeto es un datagrama con la [`ObjectHeader`](crate::media::ObjectHeader) de siempre
//! (`group` = `seq`, un solo objeto por grupo, sin FEC RaptorQ: la redundancia es la de Opus) y,
//! como carga, el paquete SFrame del emisor (`coroom_crypto::sframe`, con la pista como metadato).
//! El texto en claro que cifra SFrame empieza por la [`AudioHeader`] y sigue con las tramas Opus:
//!
//! | Bytes | Campo |
//! |---|---|
//! | 0..4 | `seq` (u32 BE): paquetes enviados por este emisor en esta pista, sin huecos |
//! | 4..8 | `capture_us` (u32 BE): captura de la primera muestra, en µs del **reloj del anfitrión** (32 bits bajos) |
//! | 8 | `frames`: tramas Opus de 10 ms en el paquete (1 o 2) |
//! | 9 | `flags`: [`AudioHeader::DTX`], [`AudioHeader::FEC`], [`AudioHeader::DRED`] |
//!
//! El diseño inicial preveía 8 bytes, pero sus campos (`u32` + `u32` + `u8` + `u8`) suman 10; se mantienen
//! los tipos. Con dos tramas, cada una va precedida de su longitud (u16 BE), salvo la última.
//! Ir dentro del cifrado autentica la marca de tiempo y la secuencia, y el anfitrión reenvía la voz
//! de un invitado a los demás sin tocarla (no necesita leerla para reenviarla).

use std::fmt;

use crate::media::{ObjectHeader, Track};

/// Bytes de la cabecera de audio.
pub const AUDIO_HEADER_LEN: usize = 10;

/// Tramas por paquete como mucho (20 ms).
pub const MAX_FRAMES: u8 = 2;

/// Error al leer una cabecera de audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioHeaderError {
    /// Faltan bytes.
    Truncated(usize),
    /// `frames` es 0 o mayor que [`MAX_FRAMES`].
    BadFrames(u8),
    /// Banderas no definidas.
    UnknownFlags(u8),
}

impl fmt::Display for AudioHeaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioHeaderError::Truncated(len) => {
                write!(
                    f,
                    "cabecera de audio truncada: {len} de {AUDIO_HEADER_LEN} bytes"
                )
            }
            AudioHeaderError::BadFrames(frames) => {
                write!(f, "número de tramas de audio no válido: {frames}")
            }
            AudioHeaderError::UnknownFlags(flags) => {
                write!(f, "banderas de audio no definidas: {flags:#010b}")
            }
        }
    }
}

impl std::error::Error for AudioHeaderError {}

/// Cabecera de un paquete de audio (dentro del cifrado).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioHeader {
    /// Secuencia del emisor en la pista.
    pub seq: u32,
    /// Captura de la primera muestra en µs del reloj del anfitrión (32 bits bajos, circular).
    pub capture_us: u32,
    /// Tramas de 10 ms.
    pub frames: u8,
    /// Banderas.
    pub flags: u8,
}

impl AudioHeader {
    /// El emisor deja de enviar después de este paquete (silencio por VAD o DTX de Opus): que la
    /// cola del receptor se vacíe no es una pérdida.
    pub const DTX: u8 = 0b0000_0001;
    /// El paquete lleva FEC en banda (LBRR) de la trama anterior.
    pub const FEC: u8 = 0b0000_0010;
    /// El paquete lleva redundancia profunda (DRED) de Opus 1.5+.
    pub const DRED: u8 = 0b0000_0100;
    /// Banderas definidas.
    pub const KNOWN_FLAGS: u8 = Self::DTX | Self::FEC | Self::DRED;

    /// Escribe la cabecera en big-endian.
    pub fn write_to(&self, out: &mut [u8; AUDIO_HEADER_LEN]) {
        out[0..4].copy_from_slice(&self.seq.to_be_bytes());
        out[4..8].copy_from_slice(&self.capture_us.to_be_bytes());
        out[8] = self.frames;
        out[9] = self.flags;
    }

    /// Cabecera serializada.
    pub fn to_bytes(&self) -> [u8; AUDIO_HEADER_LEN] {
        let mut out = [0; AUDIO_HEADER_LEN];
        self.write_to(&mut out);
        out
    }

    /// Lee la cabecera del principio de `bytes` y devuelve el resto (las tramas Opus).
    pub fn parse(bytes: &[u8]) -> Result<(AudioHeader, &[u8]), AudioHeaderError> {
        let Some((raw, rest)) = bytes.split_first_chunk::<AUDIO_HEADER_LEN>() else {
            return Err(AudioHeaderError::Truncated(bytes.len()));
        };
        let header = AudioHeader {
            seq: u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]),
            capture_us: u32::from_be_bytes([raw[4], raw[5], raw[6], raw[7]]),
            frames: raw[8],
            flags: raw[9],
        };
        if header.frames == 0 || header.frames > MAX_FRAMES {
            return Err(AudioHeaderError::BadFrames(header.frames));
        }
        if header.flags & !Self::KNOWN_FLAGS != 0 {
            return Err(AudioHeaderError::UnknownFlags(header.flags));
        }
        Ok((header, rest))
    }

    /// Si lleva la bandera de DTX.
    pub const fn is_dtx(&self) -> bool {
        self.flags & Self::DTX != 0
    }
}

/// Indica si un datagrama es de una pista de audio (`Audio` o `Voice`), mirando solo su cabecera
/// de medios (sin validar el resto).
pub fn is_audio_datagram(bytes: &[u8]) -> bool {
    bytes.len() >= 2
        && bytes[0] == ObjectHeader::VERSION
        && (bytes[1] == Track::Audio.id() || bytes[1] == Track::Voice.id())
}

/// Cabecera de medios del objeto de audio `seq` de `track` con `payload_len` bytes de carga.
pub fn object_header(track: Track, seq: u32, payload_len: u16, epoch_hint: u16) -> ObjectHeader {
    ObjectHeader {
        track,
        flags: ObjectHeader::LAST_OBJECT,
        group: seq,
        object: 0,
        object_count: 1,
        payload_len,
        epoch_hint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn headers_round_trip(
            seq in any::<u32>(),
            capture_us in any::<u32>(),
            frames in 1u8..=MAX_FRAMES,
            flags in 0u8..=AudioHeader::KNOWN_FLAGS,
            payload in prop::collection::vec(any::<u8>(), 0..64),
        ) {
            let header = AudioHeader { seq, capture_us, frames, flags };
            let mut bytes = header.to_bytes().to_vec();
            bytes.extend_from_slice(&payload);
            let (parsed, rest) = AudioHeader::parse(&bytes).expect("cabecera");
            prop_assert_eq!(parsed, header);
            prop_assert_eq!(rest, &payload[..]);
        }

        #[test]
        fn garbage_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..32)) {
            let _ = AudioHeader::parse(&bytes);
        }
    }

    #[test]
    fn audio_datagrams_are_recognised_by_their_track() {
        let header = object_header(Track::Voice, 9, 3, 1);
        let mut datagram = header.to_bytes().to_vec();
        datagram.extend_from_slice(&[1, 2, 3]);
        assert!(is_audio_datagram(&datagram));
        let (parsed, payload) = ObjectHeader::parse_datagram(&datagram).expect("objeto");
        assert_eq!((parsed.group, payload), (9, &[1u8, 2, 3][..]));
        let video = ObjectHeader {
            track: Track::Video,
            ..header
        };
        assert!(!is_audio_datagram(&video.to_bytes()));
        assert!(is_audio_datagram(
            &object_header(Track::Audio, 0, 0, 0).to_bytes()
        ));
        assert!(!is_audio_datagram(&[2]));
    }

    #[test]
    fn layout_is_fixed() {
        let header = AudioHeader {
            seq: 0x0102_0304,
            capture_us: 0x0A0B_0C0D,
            frames: 1,
            flags: AudioHeader::FEC | AudioHeader::DTX,
        };
        assert_eq!(
            header.to_bytes(),
            [1, 2, 3, 4, 0x0A, 0x0B, 0x0C, 0x0D, 1, 3]
        );
        assert!(header.is_dtx());
        assert_eq!(
            AudioHeader::parse(&[0; 4]),
            Err(AudioHeaderError::Truncated(4))
        );
        let mut bad = header.to_bytes();
        bad[8] = 3;
        assert_eq!(
            AudioHeader::parse(&bad),
            Err(AudioHeaderError::BadFrames(3))
        );
        bad[8] = 1;
        bad[9] = 0x80;
        assert_eq!(
            AudioHeader::parse(&bad),
            Err(AudioHeaderError::UnknownFlags(0x80))
        );
    }
}
