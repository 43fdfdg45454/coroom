//! Búferes de imagen reutilizados entre fotogramas ([`BufferPool`]).
//!
//! Reservar y liberar varios megabytes por fotograma cuesta (en Windows, cada reserva grande es un
//! `VirtualAlloc` con sus fallos de página; en Linux, `calloc` rellena de ceros) y es trabajo que
//! no hace falta: el decodificador escribe la imagen entera. El pool guarda unos pocos búferes del
//! tamaño en uso; el decodificador toma uno ([`BufferPool::take`]) y quien consume el fotograma lo
//! devuelve ([`BufferPool::give`]) cuando ya lo subió a la GPU. Los que no vuelven (descartes en
//! los canales) no pasan nada: el pool reserva otro.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

/// Búferes que guarda un pool como mucho: los que hay en vuelo entre el decodificador y la
/// presentación (uno decodificándose, hasta dos en el canal, uno presentándose).
pub const POOL_CAPACITY: usize = 4;

/// Pool de búferes de bytes del mismo tamaño.
pub struct BufferPool {
    free: Mutex<Vec<Vec<u8>>>,
    capacity: usize,
}

impl Default for BufferPool {
    fn default() -> Self {
        Self::new(POOL_CAPACITY)
    }
}

impl fmt::Debug for BufferPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BufferPool")
            .field("free", &self.free_count())
            .field("capacity", &self.capacity)
            .finish()
    }
}

/// Dos pools son «iguales» si son el mismo (sirve para que [`crate::RawFrame`] siga siendo
/// comparable sin mirar dentro).
impl PartialEq for BufferPool {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Eq for BufferPool {}

impl BufferPool {
    /// Pool que guarda hasta `capacity` búferes.
    pub fn new(capacity: usize) -> Self {
        Self {
            free: Mutex::new(Vec::with_capacity(capacity)),
            capacity: capacity.max(1),
        }
    }

    /// Pool compartido ([`Arc`]) con la capacidad por defecto.
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Un búfer de exactamente `len` bytes: uno guardado si cabe (su contenido es el del fotograma
    /// anterior, que el que lo toma sobrescribe entero), si no uno nuevo a cero.
    pub fn take(&self, len: usize) -> Vec<u8> {
        let reused = {
            let mut free = self.free.lock().unwrap_or_else(PoisonError::into_inner);
            let at = free.iter().position(|buffer| buffer.capacity() >= len);
            at.map(|at| free.swap_remove(at))
        };
        match reused {
            Some(mut buffer) => {
                // `resize` solo escribe los bytes que faltan para llegar a `len`.
                buffer.resize(len, 0);
                buffer
            }
            None => vec![0u8; len],
        }
    }

    /// Devuelve un búfer para reutilizarlo. Si el pool está lleno, el más pequeño se libera.
    pub fn give(&self, buffer: Vec<u8>) {
        if buffer.capacity() == 0 {
            return;
        }
        let mut free = self.free.lock().unwrap_or_else(PoisonError::into_inner);
        if free.len() >= self.capacity {
            let smallest = free
                .iter()
                .enumerate()
                .min_by_key(|(_, held)| held.capacity())
                .map(|(at, _)| at);
            match smallest {
                Some(at) if free[at].capacity() < buffer.capacity() => {
                    free.swap_remove(at);
                }
                _ => return,
            }
        }
        free.push(buffer);
    }

    /// Búferes guardados ahora.
    pub fn free_count(&self) -> usize {
        self.free
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffers_come_back_with_the_right_length() {
        let pool = BufferPool::new(2);
        let mut a = pool.take(16);
        assert_eq!(a.len(), 16);
        a[0] = 7;
        pool.give(a);
        assert_eq!(pool.free_count(), 1);
        // Se reutiliza (misma capacidad) y vuelve con la longitud pedida.
        let b = pool.take(8);
        assert_eq!(b.len(), 8);
        assert_eq!(
            b[0], 7,
            "el contenido anterior se sobrescribe fuera del pool"
        );
        let c = pool.take(32);
        assert_eq!(
            (c.len(), c[0]),
            (32, 0),
            "sin uno que quepa, uno nuevo a cero"
        );
        pool.give(b);
        pool.give(c);
        pool.give(vec![0; 64]);
        assert_eq!(pool.free_count(), 2, "no guarda más que su capacidad");
        assert!(pool.take(64).len() == 64, "se quedó con el grande");
    }
}
