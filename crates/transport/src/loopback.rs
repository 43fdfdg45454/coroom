//! Bucle UDP local: fragmentación, reensamblado y dos sockets en `127.0.0.1`.
//!
//! Sirve para medir el pipeline completo sin red real. Cada fotograma comprimido se parte en
//! datagramas de como mucho [`ObjectHeader::MAX_PACKET`] bytes con la cabecera de objetos v2 de
//! [`coroom_proto::media`] (pista de vídeo, un grupo por fotograma); el receptor los junta con un
//! [`Reassembler`] que descarta los fotogramas incompletos en cuanto llega uno posterior completo.
//! Sin FEC ni NACK: la ruta por red con reparación y fecha límite es [`crate::media`].
//!
//! Los `frame_id` (el `group` de la cabecera) son de 32 bits y se comparan sin aritmética circular:
//! a 60 fps se agotan tras más de dos años de sesión continua, y quien los genera los reinicia antes.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use coroom_proto::media::{ObjectHeader, ProtoError, Track};

/// Máximo de fotogramas incompletos que guarda el [`Reassembler`].
pub const MAX_FRAMES_IN_FLIGHT: usize = 8;

/// Tamaño máximo de un fotograma que cabe en una cabecera v2 (`u16::MAX` objetos).
pub const MAX_FRAME_LEN: usize = u16::MAX as usize * ObjectHeader::MAX_PAYLOAD;

/// Fotograma comprimido completo, tal como salió del codificador.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteFrame {
    /// Identificador de 32 bits que viajó en la cabecera (su `group`).
    pub frame_id: u32,
    /// Si el fotograma es intra.
    pub keyframe: bool,
    /// Datos comprimidos reensamblados.
    pub data: Vec<u8>,
}

/// Error del bucle local.
#[derive(Debug)]
pub enum LoopbackError {
    /// Error del socket.
    Io(io::Error),
    /// Datagrama con una cabecera no válida.
    Proto(ProtoError),
    /// El fotograma no cabe en [`MAX_FRAME_LEN`] bytes.
    FrameTooLarge(usize),
}

impl fmt::Display for LoopbackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoopbackError::Io(err) => write!(f, "error del socket UDP local: {err}"),
            LoopbackError::Proto(err) => write!(f, "datagrama no válido: {err}"),
            LoopbackError::FrameTooLarge(len) => {
                write!(
                    f,
                    "fotograma de {len} bytes, el máximo es {MAX_FRAME_LEN} bytes"
                )
            }
        }
    }
}

impl std::error::Error for LoopbackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LoopbackError::Io(err) => Some(err),
            LoopbackError::Proto(err) => Some(err),
            LoopbackError::FrameTooLarge(_) => None,
        }
    }
}

impl From<io::Error> for LoopbackError {
    fn from(err: io::Error) -> Self {
        LoopbackError::Io(err)
    }
}

impl From<ProtoError> for LoopbackError {
    fn from(err: ProtoError) -> Self {
        LoopbackError::Proto(err)
    }
}

/// Parte fotogramas comprimidos en datagramas con cabecera v2.
#[derive(Debug, Clone, Copy, Default)]
pub struct Fragmenter;

impl Fragmenter {
    /// Pista de la cabecera.
    pub const TRACK: Track = Track::Video;

    /// Devuelve los datagramas (cabecera + carga, ≤ [`ObjectHeader::MAX_PACKET`] bytes) de un
    /// fotograma. Todos los objetos menos el último llevan [`ObjectHeader::MAX_PAYLOAD`] bytes; un
    /// fotograma vacío produce un único objeto sin carga.
    pub fn fragment(
        frame_id: u32,
        keyframe: bool,
        data: &[u8],
    ) -> Result<Vec<Vec<u8>>, LoopbackError> {
        if data.len() > MAX_FRAME_LEN {
            return Err(LoopbackError::FrameTooLarge(data.len()));
        }
        let count = data.len().div_ceil(ObjectHeader::MAX_PAYLOAD).max(1);
        let object_count =
            u16::try_from(count).map_err(|_| LoopbackError::FrameTooLarge(data.len()))?;
        let mut packets = Vec::with_capacity(count);
        for index in 0..object_count {
            let start = usize::from(index) * ObjectHeader::MAX_PAYLOAD;
            let end = (start + ObjectHeader::MAX_PAYLOAD).min(data.len());
            let payload = data.get(start..end).unwrap_or_default();
            let mut flags = 0;
            if keyframe {
                flags |= ObjectHeader::KEYFRAME;
            }
            if index + 1 == object_count {
                flags |= ObjectHeader::LAST_OBJECT;
            }
            let header = ObjectHeader {
                track: Self::TRACK,
                flags,
                group: frame_id,
                object: index,
                object_count,
                // Cabe: `payload.len() <= MAX_PAYLOAD < u16::MAX`.
                payload_len: payload.len() as u16,
                epoch_hint: 0,
            };
            let mut packet = Vec::with_capacity(ObjectHeader::LEN + payload.len());
            packet.extend_from_slice(&header.to_bytes());
            packet.extend_from_slice(payload);
            packets.push(packet);
        }
        Ok(packets)
    }
}

/// Fotograma a medio llegar.
#[derive(Debug)]
struct PartialFrame {
    keyframe: bool,
    fragments: Vec<Option<Vec<u8>>>,
    missing: usize,
}

/// Reensambla datagramas en fotogramas completos.
///
/// Guarda los fragmentos por `frame_id`. Al completar un fotograma descarta los incompletos con un
/// `frame_id` menor; si hay más de [`MAX_FRAMES_IN_FLIGHT`] incompletos, descarta los más antiguos.
/// Los fragmentos tardíos de un fotograma ya completado o descartado se ignoran sin error.
#[derive(Debug, Default)]
pub struct Reassembler {
    partial: BTreeMap<u32, PartialFrame>,
    /// Fotogramas por debajo de este id ya se entregaron o se descartaron.
    floor: u32,
    dropped: u64,
}

impl Reassembler {
    /// Crea un reensamblador vacío.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fotogramas descartados por incompletos desde la creación.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Fotogramas incompletos que se están guardando.
    pub fn in_flight(&self) -> usize {
        self.partial.len()
    }

    /// Entrega un datagrama. Devuelve el fotograma si este fragmento lo completa.
    pub fn push(&mut self, packet: &[u8]) -> Result<Option<CompleteFrame>, ProtoError> {
        let (header, payload) = ObjectHeader::parse_datagram(packet)?;
        if header.is_repair() || header.track != Fragmenter::TRACK {
            return Err(ProtoError::Inconsistent(
                "el bucle local solo lleva objetos de origen de vídeo",
            ));
        }
        let frame_id = header.group;
        if frame_id < self.floor {
            return Ok(None);
        }

        let count = usize::from(header.object_count);
        let partial = self
            .partial
            .entry(frame_id)
            .or_insert_with(|| PartialFrame {
                keyframe: header.is_keyframe(),
                fragments: vec![None; count],
                missing: count,
            });
        if partial.fragments.len() != count || partial.keyframe != header.is_keyframe() {
            return Err(ProtoError::Inconsistent(
                "los fragmentos de un fotograma no coinciden",
            ));
        }
        let slot = &mut partial.fragments[usize::from(header.object)];
        if slot.is_none() {
            *slot = Some(payload.to_vec());
            partial.missing -= 1;
        }

        if partial.missing == 0 {
            let Some(done) = self.partial.remove(&frame_id) else {
                return Ok(None);
            };
            let older = self.partial.range(..frame_id).count();
            self.dropped += older as u64;
            self.partial = self.partial.split_off(&frame_id);
            self.floor = frame_id.saturating_add(1);
            let len = done.fragments.iter().flatten().map(Vec::len).sum();
            let mut data = Vec::with_capacity(len);
            for fragment in done.fragments.iter().flatten() {
                data.extend_from_slice(fragment);
            }
            return Ok(Some(CompleteFrame {
                frame_id,
                keyframe: done.keyframe,
                data,
            }));
        }

        while self.partial.len() > MAX_FRAMES_IN_FLIGHT {
            if let Some((oldest, _)) = self.partial.pop_first() {
                self.dropped += 1;
                self.floor = self.floor.max(oldest.saturating_add(1));
            }
        }
        Ok(None)
    }
}

/// Crea un par de sockets UDP conectados entre sí en `127.0.0.1` con puertos efímeros.
#[derive(Debug)]
pub struct UdpLoopback;

impl UdpLoopback {
    /// Abre los dos extremos del bucle.
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> Result<(UdpSender, UdpReceiver), LoopbackError> {
        let localhost = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        let send = UdpSocket::bind(localhost)?;
        let recv = UdpSocket::bind(localhost)?;
        send.connect(recv.local_addr()?)?;
        recv.connect(send.local_addr()?)?;
        Ok((
            UdpSender { socket: send },
            UdpReceiver {
                socket: recv,
                reassembler: Reassembler::new(),
                buffer: vec![0; ObjectHeader::MAX_PACKET + 1],
            },
        ))
    }
}

/// Extremo emisor del bucle.
#[derive(Debug)]
pub struct UdpSender {
    socket: UdpSocket,
}

impl UdpSender {
    /// Fragmenta y envía un fotograma; vuelve tras enviar el último datagrama.
    pub fn send_frame(
        &self,
        frame_id: u32,
        keyframe: bool,
        data: &[u8],
    ) -> Result<(), LoopbackError> {
        for packet in Fragmenter::fragment(frame_id, keyframe, data)? {
            self.socket.send(&packet)?;
        }
        Ok(())
    }

    /// Dirección local del socket.
    pub fn local_addr(&self) -> Result<SocketAddr, LoopbackError> {
        Ok(self.socket.local_addr()?)
    }
}

/// Extremo receptor del bucle.
#[derive(Debug)]
pub struct UdpReceiver {
    socket: UdpSocket,
    reassembler: Reassembler,
    buffer: Vec<u8>,
}

impl UdpReceiver {
    /// Lee datagramas hasta completar un fotograma o agotar `timeout` (`Ok(None)`).
    ///
    /// Los datagramas con cabecera no válida se ignoran: en el bucle local solo pueden venir del
    /// emisor conectado, así que indican un fallo que el test detecta por los descartes.
    pub fn recv_frame(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<CompleteFrame>, LoopbackError> {
        let deadline = Instant::now() + timeout;
        loop {
            let now = Instant::now();
            let Some(remaining) = deadline
                .checked_duration_since(now)
                .filter(|d| !d.is_zero())
            else {
                return Ok(None);
            };
            self.socket.set_read_timeout(Some(remaining))?;
            let len = match self.socket.recv(&mut self.buffer) {
                Ok(len) => len,
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None);
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err.into()),
            };
            if let Ok(Some(frame)) = self.reassembler.push(&self.buffer[..len]) {
                return Ok(Some(frame));
            }
        }
    }

    /// Fotogramas descartados por incompletos.
    pub fn dropped(&self) -> u64 {
        self.reassembler.dropped()
    }

    /// Dirección local del socket.
    pub fn local_addr(&self) -> Result<SocketAddr, LoopbackError> {
        Ok(self.socket.local_addr()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
    }

    /// Barajado determinista (xorshift) para no depender de una crate de azar.
    fn shuffle<T>(items: &mut [T], mut state: u64) {
        for i in (1..items.len()).rev() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let j = (state % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
    }

    #[test]
    fn fragments_respect_the_packet_limit() {
        let data = payload(ObjectHeader::MAX_PAYLOAD * 3 + 10, 1);
        let packets = Fragmenter::fragment(7, true, &data).unwrap();
        assert_eq!(packets.len(), 4);
        for (i, packet) in packets.iter().enumerate() {
            assert!(packet.len() <= ObjectHeader::MAX_PACKET);
            let header = ObjectHeader::parse(packet).unwrap();
            assert_eq!(header.track, Track::Video);
            assert_eq!(header.group, 7);
            assert_eq!(usize::from(header.object), i);
            assert_eq!(header.object_count, 4);
            assert!(header.is_keyframe());
            assert!(!header.is_repair());
            assert_eq!(header.is_last_object(), i == 3);
        }
        assert_eq!(packets[3].len(), ObjectHeader::LEN + 10);

        let empty = Fragmenter::fragment(1, false, &[]).unwrap();
        assert_eq!(empty.len(), 1);
        assert_eq!(empty[0].len(), ObjectHeader::LEN);
    }

    #[test]
    fn shuffled_fragments_reassemble() {
        let data = payload(20_000, 9);
        let mut packets = Fragmenter::fragment(42, false, &data).unwrap();
        shuffle(&mut packets, 0x9E37_79B9_7F4A_7C15);
        let mut reassembler = Reassembler::new();
        let mut done = None;
        for (i, packet) in packets.iter().enumerate() {
            let result = reassembler.push(packet).unwrap();
            if i + 1 < packets.len() {
                assert!(result.is_none());
            } else {
                done = result;
            }
        }
        let frame = done.expect("fotograma completo");
        assert_eq!(frame.frame_id, 42);
        assert!(!frame.keyframe);
        assert_eq!(frame.data, data);
        assert_eq!(reassembler.dropped(), 0);
        assert_eq!(reassembler.in_flight(), 0);
    }

    #[test]
    fn a_lost_fragment_drops_the_frame_when_the_next_one_completes() {
        let first = Fragmenter::fragment(1, true, &payload(5_000, 1)).unwrap();
        let second = Fragmenter::fragment(2, false, &payload(3_000, 2)).unwrap();
        let mut reassembler = Reassembler::new();
        for packet in first.iter().skip(1) {
            assert_eq!(reassembler.push(packet).unwrap(), None);
        }
        let mut completed = Vec::new();
        for packet in &second {
            if let Some(frame) = reassembler.push(packet).unwrap() {
                completed.push(frame.frame_id);
            }
        }
        assert_eq!(completed, vec![2]);
        assert_eq!(reassembler.dropped(), 1);
        assert_eq!(reassembler.in_flight(), 0);
        // El fragmento perdido llega tarde: se ignora sin error ni descarte extra.
        assert_eq!(reassembler.push(&first[0]).unwrap(), None);
        assert_eq!(reassembler.dropped(), 1);
        assert_eq!(reassembler.in_flight(), 0);
    }

    #[test]
    fn at_most_eight_frames_stay_in_flight() {
        let mut reassembler = Reassembler::new();
        for id in 0..12u32 {
            let packets = Fragmenter::fragment(id, false, &payload(3_000, id as u8)).unwrap();
            assert_eq!(reassembler.push(&packets[0]).unwrap(), None);
        }
        assert_eq!(reassembler.in_flight(), MAX_FRAMES_IN_FLIGHT);
        assert_eq!(reassembler.dropped(), 4);
    }

    #[test]
    fn malformed_packets_are_rejected() {
        let mut reassembler = Reassembler::new();
        let mut packet = Fragmenter::fragment(1, false, &payload(100, 0))
            .unwrap()
            .remove(0);
        packet.pop();
        assert!(matches!(
            reassembler.push(&packet),
            Err(ProtoError::Truncated { .. })
        ));
        packet[0] = 1;
        assert!(matches!(
            reassembler.push(&packet),
            Err(ProtoError::UnsupportedVersion(1))
        ));
    }

    #[test]
    fn frames_round_trip_through_real_sockets() {
        let (sender, mut receiver) = UdpLoopback::new().unwrap();
        assert_ne!(sender.local_addr().unwrap(), receiver.local_addr().unwrap());
        for id in 0..5u32 {
            let data = payload(10_000 + id as usize * 100, id as u8);
            sender.send_frame(id, id == 0, &data).unwrap();
            let frame = receiver
                .recv_frame(Duration::from_secs(5))
                .unwrap()
                .expect("fotograma recibido");
            assert_eq!(frame.frame_id, id);
            assert_eq!(frame.keyframe, id == 0);
            assert_eq!(frame.data, data);
        }
        assert_eq!(receiver.dropped(), 0);
        assert_eq!(
            receiver.recv_frame(Duration::from_millis(20)).unwrap(),
            None
        );
    }
}
