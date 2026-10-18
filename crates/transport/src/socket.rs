//! Socket UDP de los `quinn::Endpoint` de coroom, con un gancho para las tramas del relay.
//!
//! El relay de reenvío ([`coroom_proto::relay`]) comparte el socket con QUIC en los dos extremos:
//! en el servidor, junto a la señalización por HTTP/3; en el cliente, junto a las conexiones
//! directas y la señalización. [`HookedSocket`] es el socket de `quinn` de siempre
//! (`quinn::udp::UdpSocketState` sobre el socket de tokio, con GSO y GRO) más un [`SegmentHook`]
//! que mira cada datagrama recibido de las direcciones que le interesan y decide si `quinn` lo ve
//! tal cual, si se lo queda el gancho (una trama del relay) o si `quinn` lo recibe como paquete
//! QUIC de otra dirección (un [`Frame::Data`] del relay, que llega como del par de origen). Al
//! enviar, el gancho puede desviar un destino: el cliente manda al relay lo dirigido a la dirección
//! virtual de un par ([`virtual_addr`]).
//!
//! Lo que no pasa por el relay no se toca: un envío a una dirección real y un lote recibido de
//! una dirección que el gancho no mira van derechos al socket y a `quinn`, byte por byte como sin
//! gancho.
//!
//! [`Frame::Data`]: coroom_proto::relay::Frame::Data

use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::io::{self, IoSliceMut};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};

use quinn::udp::{RecvMeta, Transmit, UdpSocketState};
use quinn::{AsyncUdpSocket, UdpPoller};
use tokio::io::Interest;

use coroom_proto::relay::{DATA_HEADER, data_header};

/// Bytes como mucho de un envío con GSO (el límite de un datagrama UDP con su cabecera).
const MAX_GSO_BYTES: usize = 64_000;

/// Primer octeto de las direcciones virtuales de los pares por el relay: 240.0.0.0/4 está
/// reservado (RFC 1112 §4) y nunca sale al cable; `quinn` lo acepta como dirección de destino.
const VIRTUAL_OCTET: u8 = 240;

/// Puerto de las direcciones virtuales (cualquiera distinto de 0).
const VIRTUAL_PORT: u16 = 1;

/// Dirección virtual del par `peer` del relay (hasta 2^24 pares por sala).
pub fn virtual_addr(peer: u32) -> SocketAddr {
    let [_, a, b, c] = peer.to_be_bytes();
    SocketAddr::new(Ipv4Addr::new(VIRTUAL_OCTET, a, b, c).into(), VIRTUAL_PORT)
}

/// El par de una dirección virtual ([`virtual_addr`]), también escrita como IPv4 mapeada en IPv6.
pub fn virtual_peer(addr: SocketAddr) -> Option<u32> {
    match addr.ip().to_canonical() {
        IpAddr::V4(ip) if ip.octets()[0] == VIRTUAL_OCTET && addr.port() == VIRTUAL_PORT => {
            let [_, a, b, c] = ip.octets();
            Some(u32::from_be_bytes([0, a, b, c]))
        }
        _ => None,
    }
}

/// Qué hacer con un datagrama recibido.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    /// Paquete QUIC de su dirección de origen: a `quinn` tal cual.
    Quic,
    /// Se lo queda el gancho.
    Consumed,
    /// A `quinn` como paquete QUIC de `from`, sin los primeros `skip` bytes.
    Deliver {
        /// Dirección con la que lo ve `quinn`.
        from: SocketAddr,
        /// Bytes de cabecera que se quitan.
        skip: usize,
    },
}

/// Gancho de un [`HookedSocket`].
pub trait SegmentHook: Send + Sync + fmt::Debug + 'static {
    /// Si los datagramas de `from` pasan por [`SegmentHook::segment`] (si no, van a `quinn`).
    fn inspects(&self, from: SocketAddr) -> bool;

    /// Decide qué hacer con un datagrama de `from` que llegó a la dirección local `to` (si el
    /// sistema la da). Puede reescribirlo en su sitio y enviar por `io` (sin bloquear).
    fn segment(
        &self,
        io: &UdpIo,
        from: SocketAddr,
        to: Option<IpAddr>,
        datagram: &mut [u8],
    ) -> Segment;

    /// Fin de un lote recibido: para enviar lo que el gancho acumuló en [`SegmentHook::segment`].
    fn flush(&self, _io: &UdpIo) {}

    /// Por dónde sale un envío a `destination`.
    fn divert(&self, _destination: SocketAddr) -> Divert {
        Divert::Direct
    }
}

/// Por dónde sale un envío de `quinn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Divert {
    /// A su destino, tal cual.
    Direct,
    /// Al relay en `relay`, con la cabecera del par `peer`.
    Relay {
        /// Dirección del relay.
        relay: SocketAddr,
        /// Par de destino.
        peer: u32,
    },
    /// A ninguna parte: una dirección virtual sin relay ligado (`quinn` lo da por enviado y
    /// la conexión lo trata como una pérdida).
    Drop,
}

/// Gancho que no mira nada: el socket se comporta como el de `quinn`.
#[derive(Debug, Default)]
pub struct NoHook;

impl SegmentHook for NoHook {
    fn inspects(&self, _from: SocketAddr) -> bool {
        false
    }

    fn segment(
        &self,
        _io: &UdpIo,
        _from: SocketAddr,
        _to: Option<IpAddr>,
        _datagram: &mut [u8],
    ) -> Segment {
        Segment::Quic
    }
}

/// Socket de tokio con el estado de `quinn-udp` (GSO, GRO, ECN).
#[derive(Debug)]
pub struct UdpIo {
    io: tokio::net::UdpSocket,
    state: UdpSocketState,
    /// Si el socket es de doble pila (las direcciones IPv4 van mapeadas en IPv6).
    dual_stack: bool,
}

impl UdpIo {
    fn new(socket: std::net::UdpSocket) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        let state = UdpSocketState::new((&socket).into())?;
        let dual_stack = socket.local_addr()?.is_ipv6();
        Ok(Self {
            io: tokio::net::UdpSocket::from_std(socket)?,
            state,
            dual_stack,
        })
    }

    /// `addr` en la familia del socket.
    pub fn map(&self, addr: SocketAddr) -> SocketAddr {
        match addr {
            SocketAddr::V4(v4) if self.dual_stack => {
                SocketAddr::new(v4.ip().to_ipv6_mapped().into(), v4.port())
            }
            _ => addr,
        }
    }

    /// Envía `contents` a `destination` desde la dirección local `from` (o la que elija el
    /// sistema), en segmentos de `segment_size` con GSO, sin bloquear.
    pub fn send(
        &self,
        destination: SocketAddr,
        from: Option<IpAddr>,
        contents: &[u8],
        segment_size: Option<usize>,
    ) -> io::Result<()> {
        self.send_transmit(&Transmit {
            destination: self.map(destination),
            ecn: None,
            contents,
            segment_size: segment_size.filter(|size| *size < contents.len()),
            src_ip: from,
        })
    }

    fn send_transmit(&self, transmit: &Transmit<'_>) -> io::Result<()> {
        self.io.try_io(Interest::WRITABLE, || {
            self.state.send((&self.io).into(), transmit)
        })
    }

    /// Segmentos como mucho de un envío con GSO.
    pub fn max_gso_segments(&self) -> usize {
        self.state.max_gso_segments()
    }
}

/// Un trozo de un lote recibido que no cabía en su sitio, para la próxima vuelta.
#[derive(Debug)]
struct Pending {
    meta: RecvMeta,
    data: Vec<u8>,
}

/// Socket de los `quinn::Endpoint` de coroom con un [`SegmentHook`].
pub struct HookedSocket<H> {
    io: UdpIo,
    hook: Arc<H>,
    pending: Mutex<VecDeque<Pending>>,
}

impl<H: SegmentHook> fmt::Debug for HookedSocket<H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HookedSocket")
            .field("io", &self.io)
            .field("hook", &self.hook)
            .finish_non_exhaustive()
    }
}

impl<H: SegmentHook> HookedSocket<H> {
    /// Envuelve `socket` (lo pasa a no bloqueante) con `hook`.
    pub fn new(socket: std::net::UdpSocket, hook: Arc<H>) -> io::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            io: UdpIo::new(socket)?,
            hook,
            pending: Mutex::new(VecDeque::new()),
        }))
    }

    /// El socket, para enviar fuera de `quinn`.
    pub fn io(&self) -> &UdpIo {
        &self.io
    }

    /// Envía un `transmit` de `quinn` dirigido a una dirección virtual: cada segmento con la
    /// cabecera de [`data_header`] hacia el relay.
    fn send_diverted(
        &self,
        transmit: &Transmit<'_>,
        relay: SocketAddr,
        peer: u32,
    ) -> io::Result<()> {
        thread_local! {
            static WRAPPED: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        let header = data_header(peer);
        let segment = transmit
            .segment_size
            .unwrap_or(transmit.contents.len())
            .max(1);
        let per_send = (MAX_GSO_BYTES / (segment + DATA_HEADER)).max(1);
        WRAPPED.with_borrow_mut(|wrapped| {
            let segments: Vec<&[u8]> = transmit.contents.chunks(segment).collect();
            for chunk in segments.chunks(per_send) {
                wrapped.clear();
                for part in chunk {
                    wrapped.extend_from_slice(&header);
                    wrapped.extend_from_slice(part);
                }
                self.io.send_transmit(&Transmit {
                    destination: self.io.map(relay),
                    ecn: transmit.ecn,
                    contents: wrapped,
                    segment_size: (chunk.len() > 1).then_some(segment + DATA_HEADER),
                    src_ip: transmit.src_ip,
                })?;
            }
            Ok(())
        })
    }

    /// Pasa por el gancho los lotes que vienen de direcciones que mira; devuelve cuántos de
    /// `metas` quedan para `quinn` (los que sobran van a [`HookedSocket::pending`]).
    fn filter(&self, bufs: &mut [IoSliceMut<'_>], metas: &mut [RecvMeta], count: usize) -> usize {
        let mut extra: Vec<Pending> = Vec::new();
        let mut any = false;
        for (buf, meta) in bufs.iter_mut().zip(metas.iter_mut()).take(count) {
            if !self.hook.inspects(meta.addr) {
                continue;
            }
            any = true;
            self.filter_one(&mut buf[..meta.len], meta, &mut extra);
        }
        if any {
            self.hook.flush(&self.io);
        }
        if !extra.is_empty()
            && let Ok(mut pending) = self.pending.lock()
        {
            pending.extend(extra);
        }
        count + self.take_pending(&mut bufs[count..], &mut metas[count..])
    }

    /// Separa un lote en tramos de datagramas consecutivos que van a `quinn` con la misma
    /// dirección: el primero se queda en su sitio (compactado) y los demás van a `extra`.
    fn filter_one(&self, buf: &mut [u8], meta: &mut RecvMeta, extra: &mut Vec<Pending>) {
        let stride = meta.stride.max(1);
        let len = buf.len();
        // (dirección, inicio y fin de cada datagrama que se entrega).
        let mut runs: Vec<(SocketAddr, Vec<(usize, usize)>)> = Vec::new();
        let mut start = 0;
        while start < len {
            let end = (start + stride).min(len);
            let verdict = self
                .hook
                .segment(&self.io, meta.addr, meta.dst_ip, &mut buf[start..end]);
            let (from, skip) = match verdict {
                Segment::Quic => (meta.addr, 0),
                Segment::Consumed => {
                    start = end;
                    continue;
                }
                Segment::Deliver { from, skip } => (from, skip.min(end - start)),
            };
            let piece = (start + skip, end);
            match runs.last_mut() {
                Some((addr, pieces)) if *addr == from => pieces.push(piece),
                _ => runs.push((from, vec![piece])),
            }
            start = end;
        }
        let mut runs = runs.into_iter();
        let Some((addr, pieces)) = runs.next() else {
            meta.len = 0;
            return;
        };
        // Los datagramas de un lote GRO miden `stride` salvo el último; quitada la misma cabecera
        // a todos, el tramo sigue siendo un lote válido con el paso de su primer datagrama.
        let new_stride = pieces[0].1 - pieces[0].0;
        let mut written = 0;
        for (from, to) in &pieces {
            buf.copy_within(*from..*to, written);
            written += to - from;
        }
        meta.addr = addr;
        meta.len = written;
        meta.stride = new_stride.max(1);
        for (addr, pieces) in runs {
            let stride = pieces[0].1 - pieces[0].0;
            let mut data = Vec::with_capacity(pieces.iter().map(|(a, b)| b - a).sum());
            for (from, to) in &pieces {
                data.extend_from_slice(&buf[*from..*to]);
            }
            extra.push(Pending {
                meta: RecvMeta {
                    addr,
                    len: data.len(),
                    stride: stride.max(1),
                    ecn: meta.ecn,
                    dst_ip: meta.dst_ip,
                },
                data,
            });
        }
    }

    /// Copia en los huecos libres lo que quedó pendiente; devuelve cuántos ocupó.
    fn take_pending(&self, bufs: &mut [IoSliceMut<'_>], metas: &mut [RecvMeta]) -> usize {
        let Ok(mut pending) = self.pending.lock() else {
            return 0;
        };
        let mut used = 0;
        for (buf, meta) in bufs.iter_mut().zip(metas.iter_mut()) {
            let Some(next) = pending.front() else {
                break;
            };
            if next.data.len() > buf.len() {
                // No cabe entero: se parte por su paso (nunca pasa con los búferes de `quinn`,
                // que caben un lote GRO entero).
                let take = (buf.len() / next.meta.stride).max(1) * next.meta.stride;
                if take > buf.len() {
                    pending.pop_front();
                    continue;
                }
                let Some(front) = pending.front_mut() else {
                    break;
                };
                buf[..take].copy_from_slice(&front.data[..take]);
                front.data.drain(..take);
                front.meta.len = front.data.len();
                *meta = RecvMeta {
                    len: take,
                    ..front.meta
                };
                used += 1;
                continue;
            }
            let Some(next) = pending.pop_front() else {
                break;
            };
            buf[..next.data.len()].copy_from_slice(&next.data);
            *meta = next.meta;
            used += 1;
        }
        used
    }
}

impl<H: SegmentHook> AsyncUdpSocket for HookedSocket<H> {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(Poller {
            socket: self,
            future: None,
        })
    }

    fn try_send(&self, transmit: &Transmit<'_>) -> io::Result<()> {
        match self.hook.divert(transmit.destination) {
            Divert::Direct => self.io.send_transmit(transmit),
            Divert::Relay { relay, peer } => self.send_diverted(transmit, relay, peer),
            Divert::Drop => Ok(()),
        }
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        loop {
            let waiting = self.take_pending(bufs, meta);
            if waiting > 0 {
                return Poll::Ready(Ok(waiting));
            }
            ready!(self.io.io.poll_recv_ready(cx))?;
            if let Ok(count) = self.io.io.try_io(Interest::READABLE, || {
                self.io.state.recv((&self.io.io).into(), bufs, meta)
            }) {
                let count = self.filter(bufs, meta, count);
                if meta.iter().take(count).any(|meta| meta.len > 0) {
                    return Poll::Ready(Ok(count));
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.io.io.local_addr()
    }

    fn may_fragment(&self) -> bool {
        self.io.state.may_fragment()
    }

    fn max_transmit_segments(&self) -> usize {
        self.io.state.max_gso_segments()
    }

    fn max_receive_segments(&self) -> usize {
        self.io.state.gro_segments()
    }
}

type Writable = Pin<Box<dyn Future<Output = io::Result<()>> + Send + Sync>>;

/// [`UdpPoller`] de un [`HookedSocket`]: espera a que el socket admita escrituras.
struct Poller<H> {
    socket: Arc<HookedSocket<H>>,
    future: Option<Writable>,
}

impl<H> fmt::Debug for Poller<H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Poller").finish_non_exhaustive()
    }
}

impl<H: SegmentHook> UdpPoller for Poller<H> {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let future = this.future.get_or_insert_with(|| {
            let socket = Arc::clone(&this.socket);
            Box::pin(async move { socket.io.io.writable().await })
        });
        let result = ready!(future.as_mut().poll(cx));
        this.future = None;
        Poll::Ready(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_addresses_round_trip_and_never_match_real_ones() {
        for peer in [0, 1, 7, 0x00FF_FFFF] {
            let addr = virtual_addr(peer);
            assert_eq!(virtual_peer(addr), Some(peer));
            let mapped = SocketAddr::new(
                match addr.ip() {
                    IpAddr::V4(v4) => v4.to_ipv6_mapped().into(),
                    ip => ip,
                },
                addr.port(),
            );
            assert_eq!(virtual_peer(mapped), Some(peer));
        }
        for real in [
            "192.0.2.1:4433",
            "[2001:db8::1]:1",
            "240.0.0.1:4433",
            "10.0.0.1:1",
        ] {
            assert_eq!(
                virtual_peer(real.parse().expect("dirección")),
                None,
                "{real}"
            );
        }
    }
}
