//! Mensajes de control entre pares (y a través del relay), codificados con `postcard`.
//!
//! Viajan enmarcados por longitud ([`crate::codec`]) en el stream bidireccional de control de cada
//! conexión. El invitado lo abre y su primer mensaje es siempre [`ControlMessage::Hello`]; el relay
//! solo interpreta `Hello` (para autenticar con el token) y `Nack` (para responder desde su caché).
//!
//! Desde la versión 3 del protocolo el invitado anuncia lo que decodifica
//! ([`ControlMessage::Capabilities`]) justo después del saludo, el anfitrión anuncia el códec que
//! elige ([`ControlMessage::CodecSelected`]) y el cursor viaja aparte del vídeo:
//! [`ControlMessage::Cursor`] con la posición (como mucho a [`CURSOR_MAX_HZ`], solo si cambia) y
//! [`ControlMessage::CursorShape`] con la imagen, una vez por forma.
//!
//! Desde la versión 4 el anfitrión dice a cada invitado qué entrada se le permite
//! ([`ControlMessage::InputPermissions`]), el tamaño del fotograma para el modo absoluto
//! ([`ControlMessage::HostFrameSize`]) y, cada segundo, cómo le llega su entrada
//! ([`ControlMessage::InputReport`], para el overlay del invitado).
//!
//! Desde la versión 5 el anfitrión anuncia el formato de cada pista de audio
//! ([`ControlMessage::AudioConfig`]), si un invitado puede hablar
//! ([`ControlMessage::VoicePermission`]) y quién habla ([`ControlMessage::VoiceState`]: el invitado
//! se lo dice al anfitrión y el anfitrión lo reenvía a los demás con el id del que habla).
//!
//! Desde la versión 7 `CodecSelected` lleva el formato del flujo ([`Chroma`] y
//! [`ColorSpace`]: BT.709 SDR o BT.2020 PQ, 4:2:0 o 4:4:4) y `Capabilities` dice con qué códecs el
//! invitado acepta 4:4:4 y HDR10 ([`Capabilities::yuv444`], [`Capabilities::hdr10`]).

use serde::{Deserialize, Serialize};

use crate::media::{ObjectHeader, ProtoError, Track};

/// Frecuencia máxima de [`ControlMessage::Cursor`].
pub const CURSOR_MAX_HZ: u32 = 125;

/// Lado máximo de una forma de cursor en píxeles (256 × 256 × 4 = 256 KiB, por debajo de
/// [`crate::codec::MAX_MESSAGE_LEN`]).
pub const CURSOR_MAX_SIDE: u16 = 256;

/// Códec de vídeo (el mismo que `coroom_codec::Codec`; `proto` no depende de otras crates).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Codec {
    /// H.264 / AVC.
    H264,
    /// H.265 / HEVC.
    Hevc,
    /// AV1.
    Av1,
}

/// Backend de decodificación (el mismo que `coroom_codec::DecoderBackend`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DecoderBackend {
    /// Vulkan Video.
    Vulkan,
    /// NVIDIA NVDEC.
    Nvdec,
    /// Direct3D 12 Video.
    D3D12va,
    /// Direct3D 11 Video.
    D3D11va,
    /// VA-API.
    Vaapi,
    /// Software.
    Software,
}

/// Lo que decodifica un invitado, en su orden de preferencia, y sus límites de vídeo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Códecs que decodifica y con qué backend.
    pub decoders: Vec<(Codec, DecoderBackend)>,
    /// Ancho máximo que decodifica y presenta.
    pub max_width: u32,
    /// Alto máximo.
    pub max_height: u32,
    /// Fotogramas por segundo máximos.
    pub max_fps: u32,
    /// Códecs que decodifica en 4:4:4 y quiere recibir así (vacío si no lo pidió en Ajustes).
    pub yuv444: Vec<Codec>,
    /// Códecs que decodifica en HDR10 (10 bits, BT.2020 PQ) y quiere recibir así.
    pub hdr10: Vec<Codec>,
}

/// Matriz de color de un flujo (la de `coroom_codec::color::Matrix`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorMatrix {
    /// BT.709.
    Bt709,
    /// BT.2020 sin luminancia constante.
    Bt2020,
}

/// Rango de los valores YCbCr.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorRange {
    /// Limitado (16-235 a 8 bits).
    Limited,
    /// Completo.
    Full,
}

/// Función de transferencia.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorTransfer {
    /// SDR.
    Sdr,
    /// PQ (SMPTE ST 2084).
    Pq,
}

/// Espacio de color de un flujo de vídeo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ColorSpace {
    /// Matriz y primarios.
    pub matrix: ColorMatrix,
    /// Rango.
    pub range: ColorRange,
    /// Transferencia.
    pub transfer: ColorTransfer,
}

impl ColorSpace {
    /// BT.709 en rango limitado, SDR.
    pub const SDR: ColorSpace = ColorSpace {
        matrix: ColorMatrix::Bt709,
        range: ColorRange::Limited,
        transfer: ColorTransfer::Sdr,
    };
}

/// Submuestreo de croma.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Chroma {
    /// 4:2:0.
    Yuv420,
    /// 4:4:4.
    Yuv444,
}

/// Papel de un par en la sala.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Role {
    /// Anfitrión: emite los medios.
    Host,
    /// Invitado: recibe los medios.
    Guest,
}

/// Saludo inicial de un stream de control.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// Versión del protocolo de quien saluda ([`crate::PROTOCOL_VERSION`]).
    pub protocol_version: u16,
    /// Clave pública Ed25519 de la identidad de quien saluda.
    pub identity_pk: [u8; 32],
    /// Papel de quien saluda.
    pub role: Role,
    /// Token del relay (`RelayToken` de la señalización). Solo hacia el relay; en directo, `None`.
    pub relay_token: Option<Vec<u8>>,
}

/// Informe del receptor para el control de tasa y la FEC (cada 50 ms).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feedback {
    /// Grupo más alto visto en cualquier pista.
    pub highest_group: u32,
    /// Bytes de medios recibidos desde el inicio (contador acumulado).
    pub received_bytes: u64,
    /// Objetos de origen que no llegaron por la red (aunque la FEC o el NACK los recuperasen),
    /// contador acumulado.
    pub lost_objects: u32,
    /// Muestras de llegada por grupo: `(grupo, µs)` con la hora de llegada del primer objeto
    /// expresada en el reloj del emisor (desfase estimado con `Ping`/`Pong`) y truncada a 32 bits
    /// con aritmética circular. El emisor resta su hora de envío para obtener el retardo de ida.
    pub owd_samples: Vec<(u32, i32)>,
    /// Paquetes marcados con ECN-CE vistos por el receptor (contador acumulado).
    pub ecn_ce: u32,
}

/// Miembro de la sala en [`ControlMessage::Members`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    /// Id del invitado (0 = el anfitrión): el de `VoiceState`.
    pub guest: u32,
    /// Hoja MLS: el emisor SFrame de su voz.
    pub leaf: u32,
    /// Nombre visible (como mucho [`MEMBER_NAME_MAX`] bytes).
    pub name: String,
}

/// Miembros como mucho en [`ControlMessage::Members`].
pub const MEMBERS_MAX: usize = 64;

/// Bytes como mucho del nombre de un [`Member`].
pub const MEMBER_NAME_MAX: usize = 128;

/// Mensaje de control.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlMessage {
    /// Saludo inicial (primer mensaje del stream de control).
    Hello(Hello),
    /// Informe del receptor.
    Feedback(Feedback),
    /// Objetos de origen que faltan en un grupo. `missing` vacío pide el grupo entero.
    Nack {
        /// Pista del grupo.
        track: Track,
        /// Grupo incompleto.
        group: u32,
        /// Índices de los objetos de origen que faltan.
        missing: Vec<u16>,
    },
    /// Petición de fotograma intra tras perder el grupo `group`.
    KeyframeRequest {
        /// Grupo perdido.
        group: u32,
    },
    /// Medida de reloj: `t` es la hora de quien envía, en µs de su reloj.
    Ping {
        /// Hora de envío en µs del reloj del emisor del `Ping`.
        t: u64,
    },
    /// Respuesta a `Ping`: devuelve `t` y la hora de recepción en el reloj de quien responde.
    Pong {
        /// `t` del `Ping`.
        t: u64,
        /// Hora de recepción del `Ping` en µs del reloj de quien responde.
        t_recv: u64,
    },
    /// Bitrate objetivo que el emisor aplica (informativo para el receptor).
    TargetBitrate {
        /// kbit/s.
        kbps: u32,
    },
    /// Receptor → emisor: lo que llegó de un tren de sonda ([`Track::Probe`]). El emisor manda el
    /// tren más deprisa de lo que espera que pase el camino; la separación entre el primer y el
    /// último datagrama recibidos la pone el cuello de botella (dispersión de un tren de paquetes),
    /// así que `bytes / span_us` es lo que el camino entrega, medido y no supuesto. Si llegaron
    /// menos de los enviados, también mide la pérdida de una ráfaga.
    ProbeResult {
        /// Identificador del tren (el `group` de sus cabeceras).
        train: u32,
        /// Datagramas del tren según su cabecera (`object_count`).
        sent: u16,
        /// Datagramas que llegaron.
        received: u16,
        /// Bytes recibidos (cabeceras incluidas).
        bytes: u32,
        /// Tiempo entre la llegada del primero y la del último, en µs del reloj del receptor.
        span_us: u32,
    },
    /// Cierre ordenado.
    Bye,
    /// Emisor → receptor: marcas de tiempo de las etapas del emisor de un fotograma ya enviado,
    /// en µs del reloj del emisor (el que contesta a los `Ping`), para que el receptor las sitúe
    /// en su reloj y muestre el desglose completo de latencia. Solo son tiempos: el contenido va
    /// cifrado aparte. El relay las reenvía sin mirarlas.
    FrameTiming {
        /// Pista del grupo.
        track: Track,
        /// Grupo (fotograma).
        group: u32,
        /// Captura.
        captured_us: u64,
        /// Fin de la codificación.
        encoded_us: u64,
        /// Fin del cifrado SFrame.
        encrypted_us: u64,
        /// Salida del último datagrama.
        sent_us: u64,
    },
    /// Invitado → anfitrión, justo después del `Hello`: lo que decodifica.
    Capabilities(Capabilities),
    /// Anfitrión → invitado: códec del flujo de vídeo a partir de ahora (el siguiente fotograma de
    /// ese códec es intra). Se repite si cambia.
    CodecSelected {
        /// Códec.
        codec: Codec,
        /// Ancho configurado.
        width: u32,
        /// Alto configurado.
        height: u32,
        /// Fotogramas por segundo.
        fps: u32,
        /// Submuestreo de croma.
        chroma: Chroma,
        /// Espacio de color; con PQ, el flujo es de 10 bits.
        color: ColorSpace,
    },
    /// Anfitrión → invitado: posición del cursor en píxeles del fotograma.
    Cursor {
        /// Horizontal.
        x: i32,
        /// Vertical.
        y: i32,
        /// Si se ve.
        visible: bool,
        /// Forma ([`ControlMessage::CursorShape`] con el mismo id).
        shape_id: u64,
    },
    /// Anfitrión → invitado: imagen de una forma de cursor, una vez por forma.
    CursorShape {
        /// Identificador de la forma.
        shape_id: u64,
        /// Ancho en píxeles (como mucho [`CURSOR_MAX_SIDE`]).
        width: u16,
        /// Alto en píxeles (como mucho [`CURSOR_MAX_SIDE`]).
        height: u16,
        /// Punto activo, horizontal.
        hotspot_x: u16,
        /// Punto activo, vertical.
        hotspot_y: u16,
        /// `width × height` píxeles RGBA sin premultiplicar, fila a fila.
        rgba: Vec<u8>,
    },
    /// Anfitrión → invitado: qué entrada se le permite (la ventana solo captura eso). Se repite
    /// cada vez que cambia y al conectar.
    InputPermissions {
        /// Teclado y ratón.
        keyboard_mouse: bool,
        /// Ranura de mando asignada (desde 1).
        gamepad_slot: Option<u8>,
    },
    /// Anfitrión → invitado: tamaño del fotograma capturado, para el modo absoluto.
    HostFrameSize {
        /// Ancho en píxeles.
        width: u32,
        /// Alto en píxeles.
        height: u32,
    },
    /// Anfitrión → invitado, cada segundo mientras llega entrada: latencia de entrada medida en el
    /// anfitrión (`recibido − enviado` con el reloj sincronizado) y eventos aplicados y
    /// descartados de ese invitado.
    InputReport {
        /// Muestras de latencia en la ventana.
        samples: u32,
        /// Mediana, en µs.
        p50_us: u32,
        /// Percentil 95, en µs.
        p95_us: u32,
        /// Eventos aplicados (acumulado).
        applied: u64,
        /// Eventos descartados por permisos, pausa o antigüedad (acumulado).
        dropped: u64,
    },
    /// Anfitrión → invitado: formato de una pista de audio (`Audio` o `Voice`), al conectar y si
    /// cambia.
    AudioConfig {
        /// Pista.
        track: Track,
        /// Frecuencia de muestreo en Hz (48 000).
        sample_rate: u32,
        /// Canales (2 en el juego, 1 en la voz).
        channels: u8,
        /// Duración de cada paquete en ms (10 o 20).
        frame_ms: u8,
        /// Bitrate de Opus en bit/s.
        bitrate: u32,
    },
    /// Anfitrión → invitado: si puede hablar (sin permiso, el anfitrión descarta su voz y no le
    /// reenvía la de los demás).
    VoicePermission {
        /// Permiso de voz.
        allowed: bool,
    },
    /// Quién habla: invitado → anfitrión con `guest` = 0 (el anfitrión pone el id real) y
    /// anfitrión → invitados con el id del que habla (0 = el anfitrión).
    VoiceState {
        /// Invitado que habla (0 = el anfitrión).
        guest: u32,
        /// Si habla ahora.
        speaking: bool,
    },
    /// Anfitrión → invitado: si el mando de su ranura va tal cual (el invitado manda los informes
    /// HID, `PadData::Report`) o convertido (los eventos de Xbox). Se manda al recibir su
    /// `PadData::Device` y cada vez que cambia.
    GamepadMode {
        /// Ranura.
        slot: u8,
        /// Tal cual.
        raw: bool,
    },
    /// Anfitrión → invitado: informe de salida para su mando tal cual (vibración, luces), como lo
    /// pidió el driver del anfitrión, con su identificador si lo hay. Como mucho
    /// `hid::MAX_REPORT_BYTES` bytes.
    GamepadOutput {
        /// Ranura.
        slot: u8,
        /// Bytes del informe.
        data: Vec<u8>,
    },
    /// Anfitrión → invitados: quién está en la sala (el anfitrión incluido), cada vez que alguien
    /// entra o sale. Cada uno decide a quién oír; no cambia nada en el anfitrión.
    Members {
        /// Miembros (como mucho [`MEMBERS_MAX`]).
        members: Vec<Member>,
    },
}

impl ControlMessage {
    /// `Members` dentro de sus límites (lo demás no se mira aquí).
    pub fn members_within_limits(members: &[Member]) -> bool {
        members.len() <= MEMBERS_MAX && members.iter().all(|m| m.name.len() <= MEMBER_NAME_MAX)
    }

    /// Si el mensaje puede viajar también en un datagrama de control ([`control_datagram`]): los
    /// que corren prisa y se pueden repetir sin efecto (`Nack` y `KeyframeRequest`).
    pub fn is_urgent(&self) -> bool {
        matches!(
            self,
            ControlMessage::Nack { .. } | ControlMessage::KeyframeRequest { .. }
        )
    }
}

/// Copia de un mensaje urgente ([`ControlMessage::is_urgent`]) en un datagrama de la pista
/// [`Track::Control`]: cabecera de medios de un solo objeto con el mensaje en `postcard` como
/// carga. El mensaje sigue saliendo también por el stream de control; el datagrama solo evita que
/// una pérdida lo retenga detrás del reenvío de QUIC (su PTO, 60-70 ms con un RTT de 20 ms). `None`
/// si no es urgente o no cabe en un datagrama.
pub fn control_datagram(message: &ControlMessage) -> Option<Vec<u8>> {
    if !message.is_urgent() {
        return None;
    }
    let mut out = vec![0; ObjectHeader::LEN];
    out = postcard::to_extend(message, out).ok()?;
    let len = u16::try_from(out.len() - ObjectHeader::LEN).ok()?;
    if usize::from(len) > ObjectHeader::MAX_PAYLOAD {
        return None;
    }
    let header = ObjectHeader {
        track: Track::Control,
        flags: ObjectHeader::LAST_OBJECT,
        group: 0,
        object: 0,
        object_count: 1,
        payload_len: len,
        epoch_hint: 0,
    };
    if let Some(prefix) = out.first_chunk_mut::<{ ObjectHeader::LEN }>() {
        header.write_to(prefix);
    }
    Some(out)
}

/// Indica si un datagrama es de control (sin validar el resto).
pub fn is_control_datagram(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == ObjectHeader::VERSION && bytes[1] == Track::Control.id()
}

/// Mensaje de un datagrama de control ([`control_datagram`]); solo se aceptan los urgentes.
pub fn parse_control_datagram(bytes: &[u8]) -> Result<ControlMessage, ProtoError> {
    let (header, payload) = ObjectHeader::parse_datagram(bytes)?;
    if header.track != Track::Control || header.object_count != 1 {
        return Err(ProtoError::Inconsistent("no es un datagrama de control"));
    }
    match crate::codec::decode::<ControlMessage>(payload) {
        Ok(message) if message.is_urgent() => Ok(message),
        Ok(_) => Err(ProtoError::Inconsistent(
            "mensaje de control que no va en datagramas",
        )),
        Err(_) => Err(ProtoError::Inconsistent("mensaje de control no válido")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{FrameBuffer, decode, encode};

    #[test]
    fn urgent_messages_round_trip_in_a_control_datagram() {
        let nack = ControlMessage::Nack {
            track: Track::Video,
            group: 77,
            missing: vec![0, 3, 9],
        };
        let datagram = control_datagram(&nack).expect("cabe");
        assert!(is_control_datagram(&datagram));
        assert!(!crate::input::is_input_datagram(&datagram));
        assert!(!crate::audio::is_audio_datagram(&datagram));
        assert_eq!(parse_control_datagram(&datagram), Ok(nack));
        let request = ControlMessage::KeyframeRequest { group: 5 };
        let datagram = control_datagram(&request).expect("cabe");
        assert_eq!(parse_control_datagram(&datagram), Ok(request));
        // Lo que no es urgente no va en datagramas, ni al enviar ni al recibir.
        assert_eq!(control_datagram(&ControlMessage::Bye), None);
        let mut forged = control_datagram(&ControlMessage::KeyframeRequest { group: 0 })
            .expect("cabe")[..ObjectHeader::LEN]
            .to_vec();
        let body = postcard::to_allocvec(&ControlMessage::Bye).expect("bye");
        forged[12..14].copy_from_slice(&(body.len() as u16).to_be_bytes());
        forged.extend_from_slice(&body);
        assert!(parse_control_datagram(&forged).is_err());
        // Un Nack que no cabe en un datagrama solo va por el stream.
        let huge = ControlMessage::Nack {
            track: Track::Video,
            group: 1,
            missing: (0..1000).collect(),
        };
        assert_eq!(control_datagram(&huge), None);
    }
    use proptest::prelude::*;

    fn track() -> impl Strategy<Value = Track> {
        prop::sample::select(Track::ALL.to_vec())
    }

    fn codec() -> impl Strategy<Value = Codec> {
        prop::sample::select(vec![Codec::H264, Codec::Hevc, Codec::Av1])
    }

    fn decoder_backend() -> impl Strategy<Value = DecoderBackend> {
        prop::sample::select(vec![
            DecoderBackend::Vulkan,
            DecoderBackend::Nvdec,
            DecoderBackend::D3D12va,
            DecoderBackend::D3D11va,
            DecoderBackend::Vaapi,
            DecoderBackend::Software,
        ])
    }

    fn message() -> impl Strategy<Value = ControlMessage> {
        prop_oneof![
            (
                any::<u16>(),
                any::<[u8; 32]>(),
                any::<bool>(),
                prop::option::of(prop::collection::vec(any::<u8>(), 0..64))
            )
                .prop_map(|(protocol_version, identity_pk, host, relay_token)| {
                    ControlMessage::Hello(Hello {
                        protocol_version,
                        identity_pk,
                        role: if host { Role::Host } else { Role::Guest },
                        relay_token,
                    })
                }),
            (
                any::<u32>(),
                any::<u64>(),
                any::<u32>(),
                prop::collection::vec((any::<u32>(), any::<i32>()), 0..16),
                any::<u32>()
            )
                .prop_map(
                    |(highest_group, received_bytes, lost_objects, owd_samples, ecn_ce)| {
                        ControlMessage::Feedback(Feedback {
                            highest_group,
                            received_bytes,
                            lost_objects,
                            owd_samples,
                            ecn_ce,
                        })
                    }
                ),
            (
                track(),
                any::<u32>(),
                prop::collection::vec(any::<u16>(), 0..32)
            )
                .prop_map(|(track, group, missing)| ControlMessage::Nack {
                    track,
                    group,
                    missing
                }),
            any::<u32>().prop_map(|group| ControlMessage::KeyframeRequest { group }),
            any::<u64>().prop_map(|t| ControlMessage::Ping { t }),
            (any::<u64>(), any::<u64>()).prop_map(|(t, t_recv)| ControlMessage::Pong { t, t_recv }),
            any::<u32>().prop_map(|kbps| ControlMessage::TargetBitrate { kbps }),
            (any::<u32>(), any::<[u16; 2]>(), any::<[u32; 2]>()).prop_map(
                |(train, [sent, received], [bytes, span_us])| ControlMessage::ProbeResult {
                    train,
                    sent,
                    received,
                    bytes,
                    span_us,
                }
            ),
            Just(ControlMessage::Bye),
            (track(), any::<u32>(), any::<[u64; 4]>()).prop_map(|(track, group, t)| {
                ControlMessage::FrameTiming {
                    track,
                    group,
                    captured_us: t[0],
                    encoded_us: t[1],
                    encrypted_us: t[2],
                    sent_us: t[3],
                }
            }),
            (
                prop::collection::vec((codec(), decoder_backend()), 0..8),
                any::<[u32; 3]>(),
                prop::collection::vec(codec(), 0..3),
                prop::collection::vec(codec(), 0..3),
            )
                .prop_map(
                    |(decoders, [max_width, max_height, max_fps], yuv444, hdr10)| {
                        ControlMessage::Capabilities(Capabilities {
                            decoders,
                            max_width,
                            max_height,
                            max_fps,
                            yuv444,
                            hdr10,
                        })
                    }
                ),
            (codec(), any::<[u32; 3]>(), any::<[bool; 4]>()).prop_map(
                |(codec, [width, height, fps], [c444, bt2020, full, pq])| {
                    ControlMessage::CodecSelected {
                        codec,
                        width,
                        height,
                        fps,
                        chroma: if c444 { Chroma::Yuv444 } else { Chroma::Yuv420 },
                        color: ColorSpace {
                            matrix: if bt2020 {
                                ColorMatrix::Bt2020
                            } else {
                                ColorMatrix::Bt709
                            },
                            range: if full {
                                ColorRange::Full
                            } else {
                                ColorRange::Limited
                            },
                            transfer: if pq {
                                ColorTransfer::Pq
                            } else {
                                ColorTransfer::Sdr
                            },
                        },
                    }
                }
            ),
            (any::<i32>(), any::<i32>(), any::<bool>(), any::<u64>()).prop_map(
                |(x, y, visible, shape_id)| ControlMessage::Cursor {
                    x,
                    y,
                    visible,
                    shape_id,
                }
            ),
            (
                any::<u64>(),
                any::<[u16; 4]>(),
                prop::collection::vec(any::<u8>(), 0..256)
            )
                .prop_map(|(shape_id, [width, height, hotspot_x, hotspot_y], rgba)| {
                    ControlMessage::CursorShape {
                        shape_id,
                        width,
                        height,
                        hotspot_x,
                        hotspot_y,
                        rgba,
                    }
                }),
            (any::<bool>(), prop::option::of(1u8..=8)).prop_map(
                |(keyboard_mouse, gamepad_slot)| ControlMessage::InputPermissions {
                    keyboard_mouse,
                    gamepad_slot,
                }
            ),
            any::<[u32; 2]>()
                .prop_map(|[width, height]| ControlMessage::HostFrameSize { width, height }),
            (any::<[u32; 3]>(), any::<[u64; 2]>()).prop_map(
                |([samples, p50_us, p95_us], [applied, dropped])| ControlMessage::InputReport {
                    samples,
                    p50_us,
                    p95_us,
                    applied,
                    dropped,
                }
            ),
            (track(), any::<u32>(), any::<[u8; 2]>(), any::<u32>()).prop_map(
                |(track, sample_rate, [channels, frame_ms], bitrate)| ControlMessage::AudioConfig {
                    track,
                    sample_rate,
                    channels,
                    frame_ms,
                    bitrate,
                }
            ),
            any::<bool>().prop_map(|allowed| ControlMessage::VoicePermission { allowed }),
            (any::<u32>(), any::<bool>())
                .prop_map(|(guest, speaking)| ControlMessage::VoiceState { guest, speaking }),
            (1u8..=8, any::<bool>())
                .prop_map(|(slot, raw)| ControlMessage::GamepadMode { slot, raw }),
            (1u8..=8, prop::collection::vec(any::<u8>(), 1..64))
                .prop_map(|(slot, data)| ControlMessage::GamepadOutput { slot, data }),
            prop::collection::vec((any::<u32>(), any::<u32>(), "[a-zñ ]{0,32}"), 0..8).prop_map(
                |members| ControlMessage::Members {
                    members: members
                        .into_iter()
                        .map(|(guest, leaf, name)| Member { guest, leaf, name })
                        .collect(),
                }
            ),
        ]
    }

    #[test]
    fn the_largest_cursor_shape_fits_in_a_message() {
        let side = usize::from(CURSOR_MAX_SIDE);
        let message = ControlMessage::CursorShape {
            shape_id: u64::MAX,
            width: CURSOR_MAX_SIDE,
            height: CURSOR_MAX_SIDE,
            hotspot_x: CURSOR_MAX_SIDE - 1,
            hotspot_y: CURSOR_MAX_SIDE - 1,
            rgba: vec![0xff; side * side * 4],
        };
        let framed = encode(&message).expect("cabe");
        assert!(framed.len() <= crate::codec::MAX_MESSAGE_LEN + crate::codec::PREFIX_LEN);
    }

    proptest! {
        #[test]
        fn control_messages_round_trip(message in message()) {
            let framed = encode(&message).expect("codificar");
            let mut buffer = FrameBuffer::new();
            buffer.extend(&framed);
            prop_assert_eq!(buffer.next_message::<ControlMessage>(), Ok(Some(message)));
        }

        #[test]
        fn garbage_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..128)) {
            let _ = decode::<ControlMessage>(&bytes);
        }

        #[test]
        fn truncated_messages_are_rejected(message in message()) {
            let framed = encode(&message).expect("codificar");
            let body = &framed[crate::codec::PREFIX_LEN..];
            prop_assume!(body.len() > 1);
            prop_assert!(decode::<ControlMessage>(&body[..body.len() - 1]).is_err());
        }
    }
}
