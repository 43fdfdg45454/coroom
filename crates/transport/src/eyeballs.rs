//! Conexión al servidor por varias direcciones a la vez, escalonadas (*Happy Eyeballs*,
//! RFC 8305): `localhost` o un nombre con IPv6 e IPv4 resuelve a varias direcciones, y probarlas
//! una tras otra con el tiempo límite de cada una deja al usuario esperando ese tiempo entero
//! cuando la primera no contesta (un servidor que solo escucha en IPv4 y `::1` primero: QUIC no
//! se entera del ICMP de puerto cerrado). Aquí cada intento empieza [`ATTEMPT_DELAY`] después del
//! anterior, o en cuanto el anterior falla, y gana el primero que conecta.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
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

/// Conecta con la primera de `addrs` que conteste (ver el módulo); los demás intentos se
/// cancelan. Si fallan todos, el error del último que falló.
pub(crate) async fn first_to_connect<'a, T>(
    addrs: Vec<SocketAddr>,
    attempt: impl Fn(SocketAddr) -> Attempt<'a, T>,
) -> Result<T, TransportError> {
    let mut pending = interleave(addrs).into_iter();
    let mut running: Vec<Attempt<'a, T>> = Vec::new();
    let mut last = None;
    loop {
        if running.is_empty() {
            match pending.next() {
                Some(addr) => running.push(attempt(addr)),
                None => return Err(last.unwrap_or(TransportError::Unreachable)),
            }
        }
        let next = tokio::time::sleep(ATTEMPT_DELAY);
        tokio::pin!(next);
        let has_more = pending.len() > 0;
        // `Some((i, resultado))` si terminó el intento `i`; `None` si toca lanzar el siguiente.
        let done = std::future::poll_fn(|cx| {
            for (index, attempt) in running.iter_mut().enumerate() {
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
            Some((_, Ok(connected))) => return Ok(connected),
            Some((index, Err(err))) => {
                drop(running.swap_remove(index));
                last = Some(err);
                // Uno que falla deja paso al siguiente sin esperar.
                if let Some(addr) = pending.next() {
                    running.push(attempt(addr));
                }
            }
            None => {
                if let Some(addr) = pending.next() {
                    running.push(attempt(addr));
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
        let connected = first_to_connect(vec![silent, addr("127.0.0.1:4433")], |target| {
            Box::pin(async move {
                if target == silent {
                    std::future::pending::<()>().await;
                }
                Ok(target)
            })
        })
        .await;
        assert_eq!(connected.ok(), Some(addr("127.0.0.1:4433")));
        assert_eq!(start.elapsed(), ATTEMPT_DELAY);
    }

    /// Una que falla deja paso a la siguiente en el acto; si fallan todas, el último error.
    #[tokio::test(start_paused = true)]
    async fn a_failed_attempt_starts_the_next_at_once() {
        let start = tokio::time::Instant::now();
        let refused = addr("[::1]:4433");
        let connected = first_to_connect(vec![refused, addr("127.0.0.1:4433")], |target| {
            Box::pin(async move {
                if target == refused {
                    return Err(TransportError::Unreachable);
                }
                Ok(target)
            })
        })
        .await;
        assert_eq!(connected.ok(), Some(addr("127.0.0.1:4433")));
        assert_eq!(start.elapsed(), Duration::ZERO);

        let all_fail = first_to_connect(vec![addr("[::1]:1"), addr("127.0.0.1:1")], |_| {
            Box::pin(async { Err::<(), _>(TransportError::Timeout("prueba")) })
        })
        .await;
        assert!(matches!(all_fail, Err(TransportError::Timeout("prueba"))));
        let none = first_to_connect(Vec::new(), |_| Box::pin(async { Ok(()) })).await;
        assert!(matches!(none, Err(TransportError::Unreachable)));
    }
}
