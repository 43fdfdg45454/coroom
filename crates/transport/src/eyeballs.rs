//! Conexión al servidor por varias direcciones a la vez, escalonadas (*Happy Eyeballs*,
//! RFC 8305): `localhost` o un nombre con IPv6 e IPv4 resuelve a varias direcciones, y probarlas
//! una tras otra con el tiempo límite de cada una deja al usuario esperando ese tiempo entero
//! cuando la primera no contesta (un servidor que solo escucha en IPv4 y `::1` primero: QUIC no
//! se entera del ICMP de puerto cerrado). Aquí cada intento empieza [`ATTEMPT_DELAY`] después del
//! anterior, o en cuanto el anterior falla, y gana el primero que conecta. La dirección que ganó
//! se recuerda para ese servidor y la próxima conexión empieza por ella (el historial que permite
//! RFC 8305 §4): solo la primera conexión paga la espera del intento que no contesta. Con
//! [`remember_addresses_in`], también entre arranques.

use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{LazyLock, Mutex, OnceLock};
use std::task::Poll;
use std::time::Duration;

use crate::TransportError;

/// Lo que se espera a un intento antes de lanzar el siguiente: el *Connection Attempt Delay*
/// recomendado por RFC 8305 §5.
pub(crate) const ATTEMPT_DELAY: Duration = Duration::from_millis(250);

/// Un intento de conexión en curso.
pub(crate) type Attempt<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, TransportError>> + Send + 'a>>;

/// Ordena `addrs` alternando familias (RFC 8305 §4), empezando por la de la primera que dio el
/// sistema: `[::1, ::2, 127.0.0.1]` queda `[::1, 127.0.0.1, ::2]`.
pub(crate) fn interleave(addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let Some(first) = addrs.first().copied() else {
        return addrs;
    };
    let (mut same, mut other): (Vec<_>, Vec<_>) = addrs
        .into_iter()
        .partition(|addr| addr.is_ipv6() == first.is_ipv6());
    same.reverse();
    other.reverse();
    let mut out = Vec::with_capacity(same.len() + other.len());
    loop {
        match (same.pop(), other.pop()) {
            (None, None) => return out,
            (a, b) => out.extend(a.into_iter().chain(b)),
        }
    }
}

/// La dirección que ganó la última vez, por nombre de servidor y puerto.
static WINNERS: LazyLock<Mutex<HashMap<(String, u16), IpAddr>>> = LazyLock::new(Mutex::default);

/// Fichero donde se guardan entre arranques ([`remember_addresses_in`]).
static WINNERS_FILE: OnceLock<PathBuf> = OnceLock::new();

/// Guarda en `path` la dirección que ganó con cada servidor y carga las que ya hubiera: así el
/// primer arranque con un servidor que solo contesta por una familia paga la espera del intento
/// que no contesta, y los siguientes no. Hay que darlo antes de la primera conexión; después no
/// cambia nada. Una línea por servidor: `nombre puerto dirección`.
pub fn remember_addresses_in(path: PathBuf) {
    let loaded = std::fs::read_to_string(&path)
        .map(|text| parse_winners(&text))
        .unwrap_or_default();
    if WINNERS_FILE.set(path).is_ok()
        && let Ok(mut winners) = WINNERS.lock()
    {
        for (key, ip) in loaded {
            winners.entry(key).or_insert(ip);
        }
    }
}

fn parse_winners(text: &str) -> Vec<((String, u16), IpAddr)> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let host = fields.next()?.to_owned();
            let port = fields.next()?.parse().ok()?;
            let ip = fields.next()?.parse().ok()?;
            Some(((host, port), ip))
        })
        .collect()
}

/// Escribe `winners` en `path` (un temporal y lo renombra: nunca queda a medias).
fn store_winners(path: &Path, winners: &HashMap<(String, u16), IpAddr>) -> std::io::Result<()> {
    let mut text = String::new();
    for ((host, port), ip) in winners {
        text.push_str(&format!("{host} {port} {ip}\n"));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path)
}

/// Pone delante la dirección que ganó la última vez con `host` (si sigue entre `addrs`), y con
/// ella su familia en [`interleave`].
fn prefer_last_winner(host: &str, mut addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let Some(port) = addrs.first().map(SocketAddr::port) else {
        return addrs;
    };
    let winner = WINNERS
        .lock()
        .ok()
        .and_then(|winners| winners.get(&(host.to_owned(), port)).copied());
    if let Some(index) = winner.and_then(|ip| addrs.iter().position(|addr| addr.ip() == ip)) {
        let addr = addrs.remove(index);
        addrs.insert(0, addr);
    }
    addrs
}

fn remember_winner(host: &str, addr: SocketAddr) {
    if let Ok(mut winners) = WINNERS.lock() {
        let previous = winners.insert((host.to_owned(), addr.port()), addr.ip());
        if previous != Some(addr.ip())
            && let Some(path) = WINNERS_FILE.get()
        {
            let _ = store_winners(path, &winners);
        }
    }
}

/// Conecta con `host` por la primera de `addrs` que conteste (ver el módulo); los demás intentos
/// se cancelan. Si fallan todos, el error del último que falló.
pub(crate) async fn first_to_connect<'a, T>(
    host: &str,
    addrs: Vec<SocketAddr>,
    attempt: impl Fn(SocketAddr) -> Attempt<'a, T>,
) -> Result<T, TransportError> {
    let mut pending = interleave(prefer_last_winner(host, addrs)).into_iter();
    let mut running: Vec<(SocketAddr, Attempt<'a, T>)> = Vec::new();
    let mut last = None;
    loop {
        if running.is_empty() {
            match pending.next() {
                Some(addr) => running.push((addr, attempt(addr))),
                None => return Err(last.unwrap_or(TransportError::Unreachable)),
            }
        }
        let next = tokio::time::sleep(ATTEMPT_DELAY);
        tokio::pin!(next);
        let has_more = pending.len() > 0;
        // `Some((i, resultado))` si terminó el intento `i`; `None` si toca lanzar el siguiente.
        let done = std::future::poll_fn(|cx| {
            for (index, (_, attempt)) in running.iter_mut().enumerate() {
                if let Poll::Ready(result) = attempt.as_mut().poll(cx) {
                    return Poll::Ready(Some((index, result)));
                }
            }
            if has_more && next.as_mut().poll(cx).is_ready() {
                return Poll::Ready(None);
            }
            Poll::Pending
        })
        .await;
        match done {
            Some((index, Ok(connected))) => {
                remember_winner(host, running[index].0);
                return Ok(connected);
            }
            Some((index, Err(err))) => {
                drop(running.swap_remove(index));
                last = Some(err);
                // Uno que falla deja paso al siguiente sin esperar.
                if let Some(addr) = pending.next() {
                    running.push((addr, attempt(addr)));
                }
            }
            None => {
                if let Some(addr) = pending.next() {
                    running.push((addr, attempt(addr)));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(text: &str) -> SocketAddr {
        text.parse().expect("dirección")
    }

    #[test]
    fn families_alternate_starting_with_the_first() {
        let sorted = interleave(vec![
            addr("[::1]:4433"),
            addr("[::2]:4433"),
            addr("127.0.0.1:4433"),
        ]);
        assert_eq!(
            sorted,
            vec![
                addr("[::1]:4433"),
                addr("127.0.0.1:4433"),
                addr("[::2]:4433")
            ]
        );
        assert_eq!(interleave(Vec::new()), Vec::new());
    }

    /// La primera dirección no contesta nunca: la segunda conecta sin esperar su tiempo límite.
    #[tokio::test(start_paused = true)]
    async fn a_silent_first_address_does_not_hold_up_the_second() {
        let silent = addr("[::1]:4433");
        let start = tokio::time::Instant::now();
        let connected = first_to_connect(
            "silent.example",
            vec![silent, addr("127.0.0.1:4433")],
            |target| {
                Box::pin(async move {
                    if target == silent {
                        std::future::pending::<()>().await;
                    }
                    Ok(target)
                })
            },
        )
        .await;
        assert_eq!(connected.ok(), Some(addr("127.0.0.1:4433")));
        assert_eq!(start.elapsed(), ATTEMPT_DELAY);
    }

    /// Una que falla deja paso a la siguiente en el acto; si fallan todas, el último error.
    #[tokio::test(start_paused = true)]
    async fn a_failed_attempt_starts_the_next_at_once() {
        let start = tokio::time::Instant::now();
        let refused = addr("[::1]:4433");
        let connected = first_to_connect(
            "refused.example",
            vec![refused, addr("127.0.0.1:4433")],
            |target| {
                Box::pin(async move {
                    if target == refused {
                        return Err(TransportError::Unreachable);
                    }
                    Ok(target)
                })
            },
        )
        .await;
        assert_eq!(connected.ok(), Some(addr("127.0.0.1:4433")));
        assert_eq!(start.elapsed(), Duration::ZERO);

        let all_fail = first_to_connect(
            "down.example",
            vec![addr("[::1]:1"), addr("127.0.0.1:1")],
            |_| Box::pin(async { Err::<(), _>(TransportError::Timeout("prueba")) }),
        )
        .await;
        assert!(matches!(all_fail, Err(TransportError::Timeout("prueba"))));
        let none =
            first_to_connect("none.example", Vec::new(), |_| Box::pin(async { Ok(()) })).await;
        assert!(matches!(none, Err(TransportError::Unreachable)));
    }

    #[test]
    fn the_winners_file_reads_back() {
        let dir = std::env::temp_dir().join(format!("coroom-eyeballs-{}", std::process::id()));
        let path = dir.join("addresses");
        let winners = HashMap::from([
            (
                ("coroom.example.com".to_owned(), 4433),
                "127.0.0.1".parse().expect("ip"),
            ),
            (("localhost".to_owned(), 8443), "::1".parse().expect("ip")),
        ]);
        store_winners(&path, &winners).expect("guardar");
        let text = std::fs::read_to_string(&path).expect("leer");
        let read: HashMap<_, _> = parse_winners(&format!("{text}línea rota\n"))
            .into_iter()
            .collect();
        assert_eq!(read, winners);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Tras ganar la segunda dirección, la próxima conexión empieza por ella y no espera a la
    /// primera, que sigue sin contestar.
    #[tokio::test(start_paused = true)]
    async fn the_next_connection_starts_with_the_last_winner() {
        let silent = addr("[::1]:4433");
        let addrs = vec![silent, addr("127.0.0.1:4433")];
        let connect = || {
            first_to_connect("winner.example", addrs.clone(), |target| {
                Box::pin(async move {
                    if target == silent {
                        std::future::pending::<()>().await;
                    }
                    Ok(target)
                })
            })
        };
        let start = tokio::time::Instant::now();
        assert_eq!(connect().await.ok(), Some(addr("127.0.0.1:4433")));
        assert_eq!(start.elapsed(), ATTEMPT_DELAY);
        let again = tokio::time::Instant::now();
        assert_eq!(connect().await.ok(), Some(addr("127.0.0.1:4433")));
        assert_eq!(again.elapsed(), Duration::ZERO);
        // Otro servidor, u otro puerto del mismo, no hereda la preferencia.
        assert_eq!(
            prefer_last_winner("other.example", addrs.clone()),
            addrs.clone()
        );
        assert_eq!(
            prefer_last_winner("winner.example", vec![addr("[::1]:1"), addr("127.0.0.1:1")]),
            vec![addr("[::1]:1"), addr("127.0.0.1:1")]
        );
    }
}
