//! `UdpLossy`: proxy UDP en proceso con pérdida, retardo, jitter y cuello de botella opcional.
//!
//! Es el sustituto de `tc netem` para los tests en proceso (feature `test-util`). Reenvía los
//! datagramas de un cliente hacia `target` y las respuestas de `target` al último cliente visto,
//! aplicando en los dos sentidos:
//!
//! - pérdida aleatoria uniforme con probabilidad `loss`;
//! - retardo `delay ± jitter` (uniforme; como netem, puede reordenar);
//! - opcionalmente, un enlace de `rate_bps` con cola FIFO que descarta lo que esperaría más de
//!   `queue_limit` (para medir cuánta cola añade cada controlador de congestión).

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// Condiciones del enlace simulado.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LossyConfig {
    /// Probabilidad de perder cada datagrama (0..1).
    pub loss: f64,
    /// Retardo base en cada sentido.
    pub delay: Duration,
    /// Variación máxima del retardo (±).
    pub jitter: Duration,
    /// Capacidad del enlace en bit/s en cada sentido; `None` = ilimitada.
    pub rate_bps: Option<u64>,
    /// Cola máxima del enlace limitado antes de descartar.
    pub queue_limit: Duration,
    /// Semilla del generador pseudoaleatorio.
    pub seed: u64,
}

/// Perfiles de la matriz de red, los mismos que `tools/netem.sh`.
pub const PROFILES: [&str; 5] = ["lan", "buena", "media", "mala", "cuello"];

impl LossyConfig {
    /// Perfil de la matriz de red por su nombre (`lan`, `buena`, `media`, `mala`, `cuello`), con
    /// los valores de `tools/netem.sh`. El retardo es el de cada sentido. Como netem con jitter, el
    /// proxy reordena los datagramas cuyo retardo sorteado los adelanta; en `mala` eso ya supera el
    /// 1 % de reordenación del perfil de netem.
    pub fn profile(name: &str) -> Option<Self> {
        let base = Self {
            loss: 0.0,
            delay: Duration::ZERO,
            jitter: Duration::ZERO,
            rate_bps: None,
            queue_limit: Duration::from_millis(200),
            seed: 0x5EED_C0DE,
        };
        let ms = Duration::from_millis;
        Some(match name {
            "lan" => Self {
                delay: ms(1),
                ..base
            },
            "buena" => Self {
                loss: 0.005,
                delay: ms(15),
                jitter: ms(3),
                ..base
            },
            "media" => Self {
                loss: 0.02,
                delay: ms(30),
                jitter: ms(10),
                ..base
            },
            "mala" => Self {
                loss: 0.05,
                delay: ms(60),
                jitter: ms(20),
                ..base
            },
            "cuello" => Self {
                delay: ms(10),
                rate_bps: Some(8_000_000),
                ..base
            },
            _ => return None,
        })
    }

    /// Las condiciones de referencia de los tests: 2 % de pérdida y 10 ± 5 ms.
    pub fn contract() -> Self {
        Self {
            loss: 0.02,
            delay: Duration::from_millis(10),
            jitter: Duration::from_millis(5),
            rate_bps: None,
            queue_limit: Duration::from_millis(200),
            seed: 0x5EED_C0DE,
        }
    }
}

/// Contadores del proxy.
#[derive(Debug, Default)]
pub struct LossyStats {
    forwarded: AtomicU64,
    dropped: AtomicU64,
    queue_drops: AtomicU64,
}

impl LossyStats {
    /// Datagramas reenviados.
    pub fn forwarded(&self) -> u64 {
        self.forwarded.load(Ordering::Relaxed)
    }

    /// Datagramas perdidos a propósito.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Datagramas descartados por la cola del enlace limitado.
    pub fn queue_drops(&self) -> u64 {
        self.queue_drops.load(Ordering::Relaxed)
    }
}

/// Proxy UDP con pérdida y retardo.
#[derive(Debug)]
pub struct UdpLossy {
    addr: SocketAddr,
    stats: Arc<LossyStats>,
    task: JoinHandle<()>,
}

impl Drop for UdpLossy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl UdpLossy {
    /// Arranca el proxy hacia `target` en un puerto efímero de loopback.
    pub async fn start(target: SocketAddr, config: LossyConfig) -> io::Result<Self> {
        let local: SocketAddr = if target.is_ipv6() {
            (Ipv6Addr::LOCALHOST, 0).into()
        } else {
            (Ipv4Addr::LOCALHOST, 0).into()
        };
        let socket = UdpSocket::bind(local).await?;
        let addr = socket.local_addr()?;
        let stats = Arc::new(LossyStats::default());
        let task = tokio::spawn(run(socket, target, config, Arc::clone(&stats)));
        Ok(Self { addr, stats, task })
    }

    /// Dirección a la que debe conectar el cliente.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Contadores.
    pub fn stats(&self) -> &LossyStats {
        &self.stats
    }
}

/// Xorshift64* determinista: no hace falta una crate de azar para simular pérdidas.
struct Rng(u64);

impl Rng {
    /// Mezcla la semilla con SplitMix64: con semillas pequeñas, las primeras salidas de xorshift
    /// están correlacionadas y producen ráfagas de pérdida al principio.
    fn new(seed: u64) -> Self {
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        Self((z ^ (z >> 31)).max(1))
    }

    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let value = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (value >> 11) as f64 / (1u64 << 53) as f64
    }
}

struct Pending {
    at: Instant,
    seq: u64,
    to: SocketAddr,
    data: Vec<u8>,
}

impl PartialEq for Pending {
    fn eq(&self, other: &Self) -> bool {
        (self.at, self.seq) == (other.at, other.seq)
    }
}

impl Eq for Pending {}

impl PartialOrd for Pending {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Pending {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.at, self.seq).cmp(&(other.at, other.seq))
    }
}

async fn run(socket: UdpSocket, target: SocketAddr, config: LossyConfig, stats: Arc<LossyStats>) {
    let mut rng = Rng::new(config.seed);
    let mut queue: BinaryHeap<Reverse<Pending>> = BinaryHeap::new();
    let mut client: Option<SocketAddr> = None;
    // Momento en que queda libre el enlace limitado en cada sentido (hacia target, hacia cliente).
    let mut link_free = [Instant::now(), Instant::now()];
    let mut seq = 0u64;
    let mut buffer = vec![0u8; 65_536];
    loop {
        let next = queue.peek().map(|Reverse(pending)| pending.at);
        tokio::select! {
            received = socket.recv_from(&mut buffer) => {
                let Ok((len, from)) = received else { continue };
                let (to, direction) = if from == target {
                    match client {
                        Some(client) => (client, 1),
                        None => continue,
                    }
                } else {
                    client = Some(from);
                    (target, 0)
                };
                if rng.next_f64() < config.loss {
                    stats.dropped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let now = Instant::now();
                let mut at = now;
                if let Some(rate) = config.rate_bps.filter(|rate| *rate > 0) {
                    let start = link_free[direction].max(now);
                    if start.duration_since(now) > config.queue_limit {
                        stats.queue_drops.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    let serialization = Duration::from_secs_f64(len as f64 * 8.0 / rate as f64);
                    link_free[direction] = start + serialization;
                    at = link_free[direction];
                }
                let jitter = config.jitter.as_secs_f64() * (2.0 * rng.next_f64() - 1.0);
                let delay = (config.delay.as_secs_f64() + jitter).max(0.0);
                at += Duration::from_secs_f64(delay);
                seq += 1;
                queue.push(Reverse(Pending { at, seq, to, data: buffer[..len].to_vec() }));
            }
            () = async {
                match next {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                let now = Instant::now();
                while queue.peek().is_some_and(|Reverse(pending)| pending.at <= now) {
                    if let Some(Reverse(pending)) = queue.pop()
                        && socket.send_to(&pending.data, pending.to).await.is_ok()
                    {
                        stats.forwarded.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_profile_exists_and_matches_netem_sh() {
        for name in PROFILES {
            assert!(LossyConfig::profile(name).is_some(), "{name}");
        }
        assert!(LossyConfig::profile("otro").is_none());
        let script = include_str!("../../../tools/netem.sh");
        for name in PROFILES {
            assert!(
                script.contains(&format!("{name})")),
                "tools/netem.sh sin {name}"
            );
        }
        let mala = LossyConfig::profile("mala").expect("mala");
        assert_eq!(mala.loss, 0.05);
        assert_eq!(mala.delay, Duration::from_millis(60));
        assert_eq!(mala.jitter, Duration::from_millis(20));
        assert_eq!(
            LossyConfig::profile("cuello").and_then(|c| c.rate_bps),
            Some(8_000_000)
        );
    }

    #[tokio::test]
    async fn proxy_forwards_both_ways_with_delay_and_loss() {
        let server = UdpSocket::bind("127.0.0.1:0").await.expect("servidor");
        let target = server.local_addr().expect("dirección");
        let proxy = UdpLossy::start(
            target,
            LossyConfig {
                loss: 0.5,
                delay: Duration::from_millis(5),
                jitter: Duration::ZERO,
                rate_bps: None,
                queue_limit: Duration::from_millis(100),
                seed: 42,
            },
        )
        .await
        .expect("proxy");
        let client = UdpSocket::bind("127.0.0.1:0").await.expect("cliente");
        let start = Instant::now();
        for i in 0..200u8 {
            client.send_to(&[i], proxy.addr()).await.expect("envío");
        }
        let mut received = 0;
        let mut buffer = [0u8; 16];
        while let Ok(Ok((_, from))) =
            tokio::time::timeout(Duration::from_millis(200), server.recv_from(&mut buffer)).await
        {
            assert_eq!(from, proxy.addr());
            received += 1;
        }
        assert!(start.elapsed() >= Duration::from_millis(5));
        assert!((60..140).contains(&received), "{received}");
        assert_eq!(proxy.stats().forwarded() + proxy.stats().dropped(), 200);

        // La respuesta vuelve al cliente.
        for _ in 0..16 {
            server
                .send_to(b"ok", proxy.addr())
                .await
                .expect("respuesta");
        }
        let got = tokio::time::timeout(Duration::from_millis(500), client.recv_from(&mut buffer))
            .await
            .map(|result| result.is_ok());
        assert_eq!(got, Ok(true));
    }
}
