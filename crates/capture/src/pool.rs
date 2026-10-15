//! Búferes de imagen de la captura en CPU reutilizados entre fotogramas.
//!
//! Copiar un fotograma a memoria de CPU (PipeWire sin DMA-BUF, la textura de *staging* de WGC, la
//! fuente sintética) reservaba y liberaba varios megabytes por fotograma en el hilo del backend
//! (`VirtualAlloc` con sus fallos de página en Windows, `calloc` con su relleno en Linux). El pool
//! guarda [`POOL_CAPACITY`] búferes: el backend toma uno ([`take`]) y quien termina con el fotograma
//! (el hilo `encode` de la sesión, o el buzón al desalojar uno) lo devuelve ([`give`] o
//! [`recycle_frame`]). Es único en el proceso: todas las capturas tienen el mismo tamaño que la
//! pantalla que comparten, y las que no, no se devuelven (el pool se queda con los mayores).

use std::sync::{Mutex, PoisonError};

use crate::{CapturedFrame, FramePixels};

/// Búferes que se guardan como mucho: uno copiándose, uno en el buzón y el que codifica la sesión.
pub const POOL_CAPACITY: usize = 3;

static POOL: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

/// Un búfer de exactamente `len` bytes: uno guardado si cabe (con el contenido del fotograma
/// anterior, que el backend sobrescribe entero), si no uno nuevo a cero.
pub fn take(len: usize) -> Vec<u8> {
    let reused = {
        let mut free = POOL.lock().unwrap_or_else(PoisonError::into_inner);
        let at = free.iter().position(|buffer| buffer.capacity() >= len);
        at.map(|at| free.swap_remove(at))
    };
    match reused {
        Some(mut buffer) => {
            buffer.resize(len, 0);
            buffer
        }
        None => vec![0u8; len],
    }
}

/// Devuelve un búfer para reutilizarlo. Si el pool está lleno, se queda con los mayores.
pub fn give(buffer: Vec<u8>) {
    if buffer.capacity() == 0 {
        return;
    }
    let mut free = POOL.lock().unwrap_or_else(PoisonError::into_inner);
    if free.len() >= POOL_CAPACITY {
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

/// Devuelve al pool el búfer de un fotograma de CPU que ya no se va a usar.
pub fn recycle_frame(frame: CapturedFrame) {
    if let FramePixels::Bgra { data, .. } | FramePixels::Rgba16F { data, .. } = frame.pixels {
        give(data);
    }
}

/// Búferes guardados ahora.
pub fn free_count() -> usize {
    POOL.lock().unwrap_or_else(PoisonError::into_inner).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffers_are_reused_with_the_requested_length() {
        // El pool es del proceso: se mira la diferencia, no el total.
        let before = free_count();
        let mut a = take(4096);
        a[0] = 9;
        give(a);
        assert_eq!(free_count(), before + 1);
        let b = take(4096);
        assert_eq!((b.len(), b[0]), (4096, 9), "el mismo búfer, sin rellenar");
        let c = take(8192);
        assert_eq!(
            (c.len(), c[0]),
            (8192, 0),
            "sin uno que quepa, uno nuevo a cero"
        );
        give(b);
        give(c);
        assert!(free_count() <= POOL_CAPACITY);
    }
}
