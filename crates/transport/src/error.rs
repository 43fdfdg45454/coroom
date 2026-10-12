//! Errores del transporte, con textos en español.

use std::fmt;
use std::io;

use coroom_proto::codec::CodecError;
use coroom_proto::media::ProtoError;
use coroom_proto::signal::ErrorCode;

/// Error del transporte QUIC, de los medios, de la conexión o de la señalización.
#[derive(Debug)]
pub enum TransportError {
    /// Error del socket UDP o de un stream.
    Io(io::Error),
    /// La conexión QUIC no se pudo iniciar.
    Connect(quinn::ConnectError),
    /// La conexión QUIC se cerró o falló.
    Connection(quinn::ConnectionError),
    /// Error al escribir en un stream.
    Write(quinn::WriteError),
    /// Error al leer de un stream.
    Read(quinn::ReadExactError),
    /// No se pudo enviar un datagrama.
    Datagram(quinn::SendDatagramError),
    /// Configuración TLS no válida.
    Tls(String),
    /// Cabecera de medios no válida.
    Proto(ProtoError),
    /// Mensaje de control o señalización no válido.
    Codec(CodecError),
    /// El par no habla el protocolo esperado.
    Protocol(String),
    /// El fotograma no cabe en un grupo (`u16::MAX` objetos).
    FrameTooLarge(usize),
    /// Se agotó el tiempo de espera.
    Timeout(&'static str),
    /// El otro extremo cerró el stream o la conexión.
    Closed,
    /// El servidor de señalización rechazó la petición.
    Rejected {
        /// Código de error del servidor.
        code: ErrorCode,
        /// Texto del servidor.
        message: String,
    },
    /// Ningún candidato ni el relay respondieron.
    Unreachable,
    /// URL de servidor no válida.
    InvalidUrl(String),
    /// La API del servidor respondió con un error.
    Api {
        /// Estado HTTP.
        status: u16,
        /// Código estable del error (`invalid_credentials`…).
        code: String,
        /// Texto en español del servidor.
        message: String,
    },
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::Io(err) => write!(f, "error de red: {err}"),
            TransportError::Connect(err) => write!(f, "no se pudo iniciar la conexión QUIC: {err}"),
            TransportError::Connection(err) => write!(f, "conexión QUIC perdida: {err}"),
            TransportError::Write(err) => write!(f, "no se pudo escribir en el stream: {err}"),
            TransportError::Read(err) => write!(f, "no se pudo leer del stream: {err}"),
            TransportError::Datagram(err) => write!(f, "no se pudo enviar el datagrama: {err}"),
            TransportError::Tls(err) => write!(f, "configuración TLS no válida: {err}"),
            TransportError::Proto(err) => write!(f, "datagrama de medios no válido: {err}"),
            TransportError::Codec(err) => write!(f, "{err}"),
            TransportError::Protocol(err) => write!(f, "error de protocolo: {err}"),
            TransportError::FrameTooLarge(len) => {
                write!(
                    f,
                    "fotograma de {len} bytes, demasiado grande para un grupo"
                )
            }
            TransportError::Timeout(what) => write!(f, "tiempo de espera agotado: {what}"),
            TransportError::Closed => f.write_str("el otro extremo cerró la conexión"),
            TransportError::Rejected { code, message } => {
                write!(f, "el servidor rechazó la petición ({code}): {message}")
            }
            TransportError::Unreachable => f.write_str(
                "no se pudo conectar con el par ni en directo ni por el relay del servidor",
            ),
            TransportError::InvalidUrl(err) => write!(f, "URL del servidor no válida: {err}"),
            TransportError::Api { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TransportError::Io(err) => Some(err),
            TransportError::Connect(err) => Some(err),
            TransportError::Connection(err) => Some(err),
            TransportError::Write(err) => Some(err),
            TransportError::Read(err) => Some(err),
            TransportError::Datagram(err) => Some(err),
            TransportError::Proto(err) => Some(err),
            TransportError::Codec(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for TransportError {
    fn from(err: io::Error) -> Self {
        TransportError::Io(err)
    }
}

impl From<quinn::ConnectError> for TransportError {
    fn from(err: quinn::ConnectError) -> Self {
        TransportError::Connect(err)
    }
}

impl From<quinn::ConnectionError> for TransportError {
    fn from(err: quinn::ConnectionError) -> Self {
        TransportError::Connection(err)
    }
}

impl From<quinn::WriteError> for TransportError {
    fn from(err: quinn::WriteError) -> Self {
        TransportError::Write(err)
    }
}

impl From<quinn::ReadExactError> for TransportError {
    fn from(err: quinn::ReadExactError) -> Self {
        match err {
            quinn::ReadExactError::FinishedEarly(_) => TransportError::Closed,
            other => TransportError::Read(other),
        }
    }
}

impl From<quinn::SendDatagramError> for TransportError {
    fn from(err: quinn::SendDatagramError) -> Self {
        TransportError::Datagram(err)
    }
}

impl From<ProtoError> for TransportError {
    fn from(err: ProtoError) -> Self {
        TransportError::Proto(err)
    }
}

impl From<CodecError> for TransportError {
    fn from(err: CodecError) -> Self {
        TransportError::Codec(err)
    }
}

impl From<rustls::Error> for TransportError {
    fn from(err: rustls::Error) -> Self {
        TransportError::Tls(err.to_string())
    }
}
