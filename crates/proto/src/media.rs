//! Cabecera de medios v2 con el modelo de datos de Media over QUIC (pistas, grupos, objetos).
//!
//! Cada datagrama de medios lleva una [`ObjectHeader`] de 16 bytes en big-endian seguida de su
//! carga útil. El modelo sigue el de MoQ (`docs/ARQUITECTURA.md` §3): una **pista** ([`Track`]) es un flujo
//! de medios; un **grupo** es un fotograma de vídeo o un lote de audio; un **objeto** es un trozo
//! del grupo que cabe en un datagrama. Los objetos de reparación FEC (RaptorQ, RFC 6330) llevan la
//! bandera [`ObjectHeader::REPAIR`] y en `object` el índice del símbolo de reparación.
//!
//! El protocolo propio es el elegido para 1.0; `moq-transport` es un borrador en evolución
//! (`docs/ARQUITECTURA.md` §3) y esta disposición permite adoptarlo sin cambiar el resto de la pila.
//!
//! Disposición (desplazamientos en bytes):
//!
//! | Bytes | Campo |
//! |---|---|
//! | 0 | `version` (= 2) |
//! | 1 | `track` ([`Track`]) |
//! | 2 | `flags` ([`ObjectHeader::KEYFRAME`], [`ObjectHeader::LAST_OBJECT`], [`ObjectHeader::REPAIR`]) |
//! | 3 | reservado (0) |
//! | 4..8 | `group` |
//! | 8..10 | `object` |
//! | 10..12 | `object_count` (objetos de origen del grupo) |
//! | 12..14 | `payload_len` |
//! | 14..16 | `epoch_hint` (16 bits bajos de la época SFrame; solo informativo para el relay) |

use std::fmt;

use serde::{Deserialize, Serialize};

/// Error al interpretar una cabecera de medios.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtoError {
    /// El búfer es más corto que la cabecera o que la carga que anuncia.
    Truncated {
        /// Bytes necesarios.
        needed: usize,
        /// Bytes disponibles.
        available: usize,
    },
    /// Versión de cabecera desconocida.
    UnsupportedVersion(u8),
    /// Pista desconocida.
    UnknownTrack(u8),
    /// Banderas no definidas en esta versión.
    UnknownFlags(u8),
    /// `object_count` es cero.
    ZeroObjectCount,
    /// Un objeto de origen con `object` no menor que `object_count`.
    ObjectIndexOutOfRange {
        /// Índice recibido.
        index: u16,
        /// Número de objetos anunciado.
        count: u16,
    },
    /// La carga anunciada supera [`ObjectHeader::MAX_PAYLOAD`].
    PayloadTooLarge(u16),
    /// Un objeto contradice a los anteriores del mismo grupo o a sí mismo.
    Inconsistent(&'static str),
}

impl fmt::Display for ProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtoError::Truncated { needed, available } => {
                write!(
                    f,
                    "paquete truncado: hacen falta {needed} bytes y hay {available}"
                )
            }
            ProtoError::UnsupportedVersion(version) => {
                write!(f, "versión de cabecera de medios no admitida: {version}")
            }
            ProtoError::UnknownTrack(track) => write!(f, "pista de medios desconocida: {track}"),
            ProtoError::UnknownFlags(flags) => {
                write!(f, "banderas de medios no definidas: {flags:#010b}")
            }
            ProtoError::ZeroObjectCount => f.write_str("el grupo anuncia cero objetos"),
            ProtoError::ObjectIndexOutOfRange { index, count } => {
                write!(f, "objeto {index} fuera de rango (hay {count})")
            }
            ProtoError::PayloadTooLarge(len) => {
                write!(
                    f,
                    "carga de {len} bytes, el máximo es {}",
                    ObjectHeader::MAX_PAYLOAD
                )
            }
            ProtoError::Inconsistent(what) => write!(f, "objeto incoherente: {what}"),
        }
    }
}

impl std::error::Error for ProtoError {}

/// Pista de medios (el `track` de MoQ).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum Track {
    /// Vídeo del anfitrión.
    Video = 0,
    /// Audio del juego.
    Audio = 1,
    /// Voz de la sala.
    Voice = 2,
    /// Entrada del invitado.
    Input = 3,
    /// Control en datagramas.
    Control = 4,
    /// Sonda del enlace: trenes de datagramas de relleno con los que el receptor mide lo que el
    /// camino entrega (`ControlMessage::ProbeResult`). No llevan contenido ni cuentan como medios.
    Probe = 5,
}

impl Track {
    /// Todas las pistas, en orden de identificador.
    pub const ALL: [Track; 6] = [
        Track::Video,
        Track::Audio,
        Track::Voice,
        Track::Input,
        Track::Control,
        Track::Probe,
    ];

    /// Identificador en la cabecera.
    pub const fn id(self) -> u8 {
        self as u8
    }

    /// Pista con ese identificador, si existe.
    pub const fn from_id(id: u8) -> Option<Track> {
        match id {
            0 => Some(Track::Video),
            1 => Some(Track::Audio),
            2 => Some(Track::Voice),
            3 => Some(Track::Input),
            4 => Some(Track::Control),
            5 => Some(Track::Probe),
            _ => None,
        }
    }
}

/// Cabecera fija de un objeto de medios (versión 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjectHeader {
    /// Pista del objeto.
    pub track: Track,
    /// Banderas: [`ObjectHeader::KEYFRAME`], [`ObjectHeader::LAST_OBJECT`] y
    /// [`ObjectHeader::REPAIR`].
    pub flags: u8,
    /// Grupo: fotograma de vídeo o lote de audio.
    pub group: u32,
    /// Índice del objeto de origen dentro del grupo o, con [`ObjectHeader::REPAIR`], índice del
    /// símbolo de reparación.
    pub object: u16,
    /// Número de objetos de origen del grupo.
    pub object_count: u16,
    /// Bytes de carga útil que siguen a la cabecera.
    pub payload_len: u16,
    /// 16 bits bajos de la época SFrame del contenido; solo informativo (el relay no descifra).
    pub epoch_hint: u16,
}

impl ObjectHeader {
    /// Versión de esta disposición.
    pub const VERSION: u8 = 2;
    /// Longitud de la cabecera en bytes.
    pub const LEN: usize = 16;
    /// Tamaño máximo de un datagrama (cabecera + carga). Si el camino QUIC negocia menos, el
    /// emisor usa el valor negociado.
    pub const MAX_PACKET: usize = 1200;
    /// Carga útil máxima por datagrama con [`ObjectHeader::MAX_PACKET`].
    pub const MAX_PAYLOAD: usize = Self::MAX_PACKET - Self::LEN;
    /// Bandera: el grupo es intra (punto de acceso aleatorio).
    pub const KEYFRAME: u8 = 0b0000_0001;
    /// Bandera: último objeto de origen del grupo.
    pub const LAST_OBJECT: u8 = 0b0000_0010;
    /// Bandera: objeto de reparación FEC.
    pub const REPAIR: u8 = 0b0000_0100;
    /// Banderas definidas en esta versión.
    pub const KNOWN_FLAGS: u8 = Self::KEYFRAME | Self::LAST_OBJECT | Self::REPAIR;

    /// Indica si el grupo es intra.
    pub const fn is_keyframe(&self) -> bool {
        self.flags & Self::KEYFRAME != 0
    }

    /// Indica si es el último objeto de origen del grupo.
    pub const fn is_last_object(&self) -> bool {
        self.flags & Self::LAST_OBJECT != 0
    }

    /// Indica si es un objeto de reparación FEC.
    pub const fn is_repair(&self) -> bool {
        self.flags & Self::REPAIR != 0
    }

    /// Escribe la cabecera en big-endian.
    pub fn write_to(&self, out: &mut [u8; Self::LEN]) {
        out[0] = Self::VERSION;
        out[1] = self.track.id();
        out[2] = self.flags;
        out[3] = 0;
        out[4..8].copy_from_slice(&self.group.to_be_bytes());
        out[8..10].copy_from_slice(&self.object.to_be_bytes());
        out[10..12].copy_from_slice(&self.object_count.to_be_bytes());
        out[12..14].copy_from_slice(&self.payload_len.to_be_bytes());
        out[14..16].copy_from_slice(&self.epoch_hint.to_be_bytes());
    }

    /// Cabecera serializada.
    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut out = [0; Self::LEN];
        self.write_to(&mut out);
        out
    }

    /// Lee una cabecera del principio de `bytes` y valida sus campos.
    ///
    /// No comprueba que la carga esté completa; para eso está [`ObjectHeader::parse_datagram`].
    /// El byte reservado se ignora (se escribe a 0).
    pub fn parse(bytes: &[u8]) -> Result<ObjectHeader, ProtoError> {
        let Some(raw) = bytes.first_chunk::<{ Self::LEN }>() else {
            return Err(ProtoError::Truncated {
                needed: Self::LEN,
                available: bytes.len(),
            });
        };
        if raw[0] != Self::VERSION {
            return Err(ProtoError::UnsupportedVersion(raw[0]));
        }
        let track = Track::from_id(raw[1]).ok_or(ProtoError::UnknownTrack(raw[1]))?;
        let flags = raw[2];
        if flags & !Self::KNOWN_FLAGS != 0 {
            return Err(ProtoError::UnknownFlags(flags));
        }
        let u16_at = |at: usize| u16::from_be_bytes([raw[at], raw[at + 1]]);
        let header = ObjectHeader {
            track,
            flags,
            group: u32::from_be_bytes([raw[4], raw[5], raw[6], raw[7]]),
            object: u16_at(8),
            object_count: u16_at(10),
            payload_len: u16_at(12),
            epoch_hint: u16_at(14),
        };
        if header.object_count == 0 {
            return Err(ProtoError::ZeroObjectCount);
        }
        if usize::from(header.payload_len) > Self::MAX_PAYLOAD {
            return Err(ProtoError::PayloadTooLarge(header.payload_len));
        }
        if header.is_repair() {
            if header.is_last_object() {
                return Err(ProtoError::Inconsistent(
                    "un objeto de reparación no puede ser el último de origen",
                ));
            }
        } else {
            if header.object >= header.object_count {
                return Err(ProtoError::ObjectIndexOutOfRange {
                    index: header.object,
                    count: header.object_count,
                });
            }
            if header.is_last_object() != (header.object + 1 == header.object_count) {
                return Err(ProtoError::Inconsistent(
                    "la marca de último objeto no coincide con su índice",
                ));
            }
        }
        Ok(header)
    }

    /// Lee un datagrama completo: cabecera y carga, que debe medir exactamente `payload_len`.
    pub fn parse_datagram(bytes: &[u8]) -> Result<(ObjectHeader, &[u8]), ProtoError> {
        let header = Self::parse(bytes)?;
        let expected = Self::LEN + usize::from(header.payload_len);
        if bytes.len() < expected {
            return Err(ProtoError::Truncated {
                needed: expected,
                available: bytes.len(),
            });
        }
        if bytes.len() > expected {
            return Err(ProtoError::Inconsistent(
                "el datagrama es más largo que su carga anunciada",
            ));
        }
        Ok((header, &bytes[Self::LEN..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn track() -> impl Strategy<Value = Track> {
        prop::sample::select(Track::ALL.to_vec())
    }

    fn source_header() -> impl Strategy<Value = ObjectHeader> {
        (
            track(),
            any::<bool>(),
            any::<u32>(),
            1..=u16::MAX,
            0..=ObjectHeader::MAX_PAYLOAD as u16,
            any::<u16>(),
        )
            .prop_flat_map(|(track, keyframe, group, count, payload_len, epoch)| {
                (0..count).prop_map(move |object| {
                    let mut flags = if keyframe { ObjectHeader::KEYFRAME } else { 0 };
                    if object + 1 == count {
                        flags |= ObjectHeader::LAST_OBJECT;
                    }
                    ObjectHeader {
                        track,
                        flags,
                        group,
                        object,
                        object_count: count,
                        payload_len,
                        epoch_hint: epoch,
                    }
                })
            })
    }

    fn repair_header() -> impl Strategy<Value = ObjectHeader> {
        (
            track(),
            any::<bool>(),
            any::<u32>(),
            any::<u16>(),
            1..=u16::MAX,
            0..=ObjectHeader::MAX_PAYLOAD as u16,
            any::<u16>(),
        )
            .prop_map(
                |(track, keyframe, group, object, count, payload_len, epoch)| ObjectHeader {
                    track,
                    flags: ObjectHeader::REPAIR | if keyframe { ObjectHeader::KEYFRAME } else { 0 },
                    group,
                    object,
                    object_count: count,
                    payload_len,
                    epoch_hint: epoch,
                },
            )
    }

    proptest! {
        #[test]
        fn source_headers_round_trip(header in source_header()) {
            let bytes = header.to_bytes();
            prop_assert_eq!(ObjectHeader::parse(&bytes), Ok(header));
            prop_assert_eq!(bytes[0], ObjectHeader::VERSION);
            prop_assert_eq!(bytes[3], 0);
        }

        #[test]
        fn repair_headers_round_trip(header in repair_header()) {
            prop_assert_eq!(ObjectHeader::parse(&header.to_bytes()), Ok(header));
        }

        #[test]
        fn datagrams_round_trip(header in source_header(), seed in any::<u8>()) {
            let payload: Vec<u8> = (0..header.payload_len).map(|i| (i as u8) ^ seed).collect();
            let mut datagram = header.to_bytes().to_vec();
            datagram.extend_from_slice(&payload);
            let (parsed, body) = ObjectHeader::parse_datagram(&datagram).expect("datagrama");
            prop_assert_eq!(parsed, header);
            prop_assert_eq!(body, &payload[..]);
            datagram.push(0);
            prop_assert!(ObjectHeader::parse_datagram(&datagram).is_err());
        }

        #[test]
        fn any_other_version_is_rejected(header in source_header(), version in any::<u8>()) {
            prop_assume!(version != ObjectHeader::VERSION);
            let mut bytes = header.to_bytes();
            bytes[0] = version;
            prop_assert_eq!(
                ObjectHeader::parse(&bytes),
                Err(ProtoError::UnsupportedVersion(version))
            );
        }

        #[test]
        fn unknown_tracks_are_rejected(header in source_header(), id in 6u8..) {
            let mut bytes = header.to_bytes();
            bytes[1] = id;
            prop_assert_eq!(ObjectHeader::parse(&bytes), Err(ProtoError::UnknownTrack(id)));
        }

        #[test]
        fn unknown_flags_are_rejected(header in source_header(), extra in 3u8..8) {
            let mut bytes = header.to_bytes();
            bytes[2] |= 1 << extra;
            prop_assert!(matches!(ObjectHeader::parse(&bytes), Err(ProtoError::UnknownFlags(_))));
        }

        #[test]
        fn arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
            let _ = ObjectHeader::parse(&bytes);
            let _ = ObjectHeader::parse_datagram(&bytes);
        }
    }

    fn header() -> ObjectHeader {
        ObjectHeader {
            track: Track::Video,
            flags: ObjectHeader::KEYFRAME | ObjectHeader::LAST_OBJECT,
            group: 0x0102_0304,
            object: 2,
            object_count: 3,
            payload_len: 100,
            epoch_hint: 0x0A0B,
        }
    }

    #[test]
    fn layout_is_big_endian() {
        let bytes = header().to_bytes();
        assert_eq!(
            bytes,
            [2, 0, 3, 0, 1, 2, 3, 4, 0, 2, 0, 3, 0, 100, 0x0A, 0x0B],
            "disposición fija de 16 bytes"
        );
        assert!(header().is_keyframe());
        assert!(header().is_last_object());
        assert!(!header().is_repair());
    }

    #[test]
    fn sizes_match_the_contract() {
        assert_eq!(ObjectHeader::LEN, 16);
        assert_eq!(ObjectHeader::MAX_PACKET, 1200);
        assert_eq!(ObjectHeader::MAX_PAYLOAD, 1184);
    }

    #[test]
    fn track_ids_match_the_contract() {
        let ids: Vec<u8> = Track::ALL.iter().map(|track| track.id()).collect();
        assert_eq!(ids, [0, 1, 2, 3, 4, 5]);
        for track in Track::ALL {
            assert_eq!(Track::from_id(track.id()), Some(track));
        }
        assert_eq!(Track::from_id(6), None);
    }

    #[test]
    fn invalid_fields_are_rejected() {
        let mut zero = header();
        zero.object_count = 0;
        zero.object = 0;
        assert_eq!(
            ObjectHeader::parse(&zero.to_bytes()),
            Err(ProtoError::ZeroObjectCount)
        );

        let mut out_of_range = header();
        out_of_range.object = 3;
        assert_eq!(
            ObjectHeader::parse(&out_of_range.to_bytes()),
            Err(ProtoError::ObjectIndexOutOfRange { index: 3, count: 3 })
        );

        let mut not_last = header();
        not_last.object = 1;
        assert!(matches!(
            ObjectHeader::parse(&not_last.to_bytes()),
            Err(ProtoError::Inconsistent(_))
        ));

        let mut repair_last = header();
        repair_last.flags |= ObjectHeader::REPAIR;
        assert!(matches!(
            ObjectHeader::parse(&repair_last.to_bytes()),
            Err(ProtoError::Inconsistent(_))
        ));

        let mut too_large = header();
        too_large.payload_len = 1185;
        assert_eq!(
            ObjectHeader::parse(&too_large.to_bytes()),
            Err(ProtoError::PayloadTooLarge(1185))
        );

        assert_eq!(
            ObjectHeader::parse(&[2, 0, 0]),
            Err(ProtoError::Truncated {
                needed: 16,
                available: 3
            })
        );
    }

    #[test]
    fn repair_objects_may_use_any_index() {
        let repair = ObjectHeader {
            flags: ObjectHeader::REPAIR,
            object: 40,
            ..header()
        };
        assert_eq!(ObjectHeader::parse(&repair.to_bytes()), Ok(repair));
    }
}
