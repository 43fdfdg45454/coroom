//! Enmarcado por longitud de los mensajes `postcard` de control y señalización.
//!
//! Cada mensaje viaja en un stream fiable como `longitud (u32 big-endian) || postcard(mensaje)`.
//! La longitud cuenta solo el cuerpo y no puede pasar de [`MAX_MESSAGE_LEN`]. Este módulo no hace
//! IO: [`encode`] produce los bytes y [`FrameBuffer`] los recompone a partir de trozos arbitrarios.

use std::fmt;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Longitud del prefijo de cada mensaje.
pub const PREFIX_LEN: usize = 4;

/// Tamaño máximo del cuerpo de un mensaje (1 MiB): de sobra para un `Welcome` MLS de 8 miembros.
pub const MAX_MESSAGE_LEN: usize = 1 << 20;

/// Error al codificar o decodificar un mensaje.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    /// El cuerpo no es un mensaje `postcard` válido de ese tipo.
    Postcard(String),
    /// El mensaje anuncia o mide más de [`MAX_MESSAGE_LEN`] bytes.
    TooLarge(usize),
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodecError::Postcard(err) => write!(f, "mensaje no válido: {err}"),
            CodecError::TooLarge(len) => write!(
                f,
                "mensaje de {len} bytes, el máximo es {MAX_MESSAGE_LEN} bytes"
            ),
        }
    }
}

impl std::error::Error for CodecError {}

/// Serializa `message` con su prefijo de longitud.
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, CodecError> {
    let mut out = vec![0; PREFIX_LEN];
    out = postcard::to_extend(message, out).map_err(|err| CodecError::Postcard(err.to_string()))?;
    let body = out.len() - PREFIX_LEN;
    if body > MAX_MESSAGE_LEN {
        return Err(CodecError::TooLarge(body));
    }
    // Cabe: `body <= MAX_MESSAGE_LEN < u32::MAX`.
    out[..PREFIX_LEN].copy_from_slice(&(body as u32).to_be_bytes());
    Ok(out)
}

/// Lee el cuerpo de un mensaje (sin prefijo). Los bytes sobrantes son un error.
pub fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, CodecError> {
    let (message, rest) =
        postcard::take_from_bytes(body).map_err(|err| CodecError::Postcard(err.to_string()))?;
    if !rest.is_empty() {
        return Err(CodecError::Postcard(format!(
            "sobran {} bytes tras el mensaje",
            rest.len()
        )));
    }
    Ok(message)
}

/// Longitud del cuerpo que anuncia un prefijo, validada contra [`MAX_MESSAGE_LEN`].
pub fn body_len(prefix: [u8; PREFIX_LEN]) -> Result<usize, CodecError> {
    body_len_within(prefix, MAX_MESSAGE_LEN)
}

/// Longitud del cuerpo que anuncia un prefijo, validada contra `max` (nunca más que
/// [`MAX_MESSAGE_LEN`]).
pub fn body_len_within(prefix: [u8; PREFIX_LEN], max: usize) -> Result<usize, CodecError> {
    let len = u32::from_be_bytes(prefix) as usize;
    if len > max.min(MAX_MESSAGE_LEN) {
        return Err(CodecError::TooLarge(len));
    }
    Ok(len)
}

/// Recompone mensajes enmarcados a partir de trozos de un stream.
///
/// El prefijo se valida en cuanto llega, antes de esperar al cuerpo: un mensaje que anuncia más
/// del máximo es un error sin haber guardado nada de él.
#[derive(Debug)]
pub struct FrameBuffer {
    buffer: Vec<u8>,
    max: usize,
}

impl Default for FrameBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameBuffer {
    /// Búfer vacío con el máximo general, [`MAX_MESSAGE_LEN`].
    pub fn new() -> Self {
        Self::with_max(MAX_MESSAGE_LEN)
    }

    /// Búfer vacío que rechaza los cuerpos de más de `max` bytes (p. ej. la señalización del
    /// servidor, que no necesita el MiB de los mensajes entre pares).
    pub fn with_max(max: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max: max.min(MAX_MESSAGE_LEN),
        }
    }

    /// Cuerpo máximo admitido.
    pub fn max(&self) -> usize {
        self.max
    }

    /// Añade bytes leídos del stream.
    pub fn extend(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Bytes pendientes de formar un mensaje.
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }

    /// Saca el siguiente cuerpo completo (sin prefijo), si ya ha llegado entero.
    pub fn next_body(&mut self) -> Result<Option<Vec<u8>>, CodecError> {
        let Some(prefix) = self.buffer.first_chunk::<PREFIX_LEN>() else {
            return Ok(None);
        };
        let len = body_len_within(*prefix, self.max)?;
        if self.buffer.len() < PREFIX_LEN + len {
            return Ok(None);
        }
        let body = self.buffer[PREFIX_LEN..PREFIX_LEN + len].to_vec();
        self.buffer.drain(..PREFIX_LEN + len);
        Ok(Some(body))
    }

    /// Saca y decodifica el siguiente mensaje completo, si ya ha llegado entero.
    pub fn next_message<T: DeserializeOwned>(&mut self) -> Result<Option<T>, CodecError> {
        match self.next_body()? {
            Some(body) => decode(&body).map(Some),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_survive_arbitrary_chunking() {
        let messages: Vec<(u32, String)> = (0..20).map(|i| (i, "x".repeat(i as usize))).collect();
        let mut stream = Vec::new();
        for message in &messages {
            stream.extend(encode(message).expect("codificar"));
        }
        for chunk in [1, 3, 7, 1000] {
            let mut buffer = FrameBuffer::new();
            let mut decoded = Vec::new();
            for part in stream.chunks(chunk) {
                buffer.extend(part);
                while let Some(message) = buffer.next_message::<(u32, String)>().expect("mensaje") {
                    decoded.push(message);
                }
            }
            assert_eq!(decoded, messages);
            assert_eq!(buffer.pending(), 0);
        }
    }

    #[test]
    fn oversized_prefixes_are_rejected() {
        let mut buffer = FrameBuffer::new();
        buffer.extend(&((MAX_MESSAGE_LEN as u32) + 1).to_be_bytes());
        assert!(matches!(buffer.next_body(), Err(CodecError::TooLarge(_))));
    }

    #[test]
    fn a_smaller_limit_rejects_the_prefix_before_the_body() {
        let mut buffer = FrameBuffer::with_max(16);
        buffer.extend(&17u32.to_be_bytes());
        assert_eq!(buffer.next_body(), Err(CodecError::TooLarge(17)));
        let mut buffer = FrameBuffer::with_max(usize::MAX);
        assert_eq!(buffer.max(), MAX_MESSAGE_LEN);
        buffer.extend(&encode(&"hola").expect("codificar"));
        assert!(buffer.next_message::<String>().expect("mensaje").is_some());
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut body = postcard::to_stdvec(&7u32).expect("postcard");
        body.push(0);
        assert!(decode::<u32>(&body).is_err());
    }
}
