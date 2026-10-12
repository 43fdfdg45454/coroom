//! Cliente WebTransport sobre HTTP/3 (draft-ietf-webtrans-http3) encima de una `quinn::Connection`.
//!
//! WebTransport sigue siendo un borrador IETF (`docs/ARQUITECTURA.md` §3). El servidor usa
//! `h3-webtransport` 0.1.2, de la misma familia que `h3` 0.0.8. En el cliente, `wtransport` crea su
//! propio `quinn::Endpoint` y su propio socket, y el diseño exige que la señalización salga **del
//! mismo socket** que los medios (para que `Observed` sea la dirección reflejada útil para la
//! apertura de NAT); el cliente de `h3` tampoco puede anunciar `SETTINGS_ENABLE_WEBTRANSPORT`. Por
//! eso este módulo implementa el lado cliente mínimo del protocolo directamente sobre la conexión
//! del `Endpoint`, con el códec QPACK de `wtransport-proto` (misma familia que `wtransport`):
//!
//! 1. stream unidireccional de control con `SETTINGS` (QPACK sin tabla dinámica, datagramas H3 y
//!    WebTransport activados);
//! 2. espera los `SETTINGS` del servidor y comprueba que admite WebTransport;
//! 3. `CONNECT` extendido (`:protocol = webtransport`) en un stream bidireccional, que queda
//!    abierto como stream de sesión;
//! 4. cada stream bidireccional de la sesión empieza por `WEBTRANSPORT_STREAM (0x41)` y el id de la
//!    sesión.

use std::borrow::Cow;
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::debug;
use wtransport_proto::frame::Frame;
use wtransport_proto::headers::Headers;

use crate::TransportError;

/// Tipos de stream y de trama de HTTP/3 (RFC 9114) y WebTransport.
const STREAM_TYPE_CONTROL: u64 = 0x00;
const FRAME_HEADERS: u64 = 0x01;
const FRAME_SETTINGS: u64 = 0x04;
const FRAME_WEBTRANSPORT_STREAM: u64 = 0x41;

/// Identificadores de `SETTINGS`.
const SETTING_QPACK_MAX_TABLE_CAPACITY: u64 = 0x01;
const SETTING_QPACK_BLOCKED_STREAMS: u64 = 0x07;
const SETTING_H3_DATAGRAM: u64 = 0x33;
const SETTING_ENABLE_WEBTRANSPORT: u64 = 0x2b60_3742;
/// `SETTINGS_WEBTRANSPORT_MAX_SESSIONS` del borrador 02-06 (el que usa `h3`) y del 07 en adelante.
const SETTING_WT_MAX_SESSIONS_OLD: u64 = 0x2b60_3743;
const SETTING_WT_MAX_SESSIONS: u64 = 0xc671_706a;

/// Tamaño máximo de una trama que se acepta del servidor.
const MAX_FRAME: u64 = 64 * 1024;

/// Espera máxima de los `SETTINGS` y de la respuesta al `CONNECT`.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Escribe un entero de longitud variable de QUIC (RFC 9000 §16).
pub(crate) fn put_varint(out: &mut Vec<u8>, value: u64) {
    if value < 1 << 6 {
        out.push(value as u8);
    } else if value < 1 << 14 {
        out.extend_from_slice(&((value as u16) | 0x4000).to_be_bytes());
    } else if value < 1 << 30 {
        out.extend_from_slice(&((value as u32) | 0x8000_0000).to_be_bytes());
    } else {
        out.extend_from_slice(&(value | 0xc000_0000_0000_0000).to_be_bytes());
    }
}

/// Lee un entero de longitud variable del principio de `bytes`: `(valor, bytes usados)`.
pub(crate) fn get_varint(bytes: &[u8]) -> Option<(u64, usize)> {
    let first = *bytes.first()?;
    let len = 1usize << (first >> 6);
    let raw = bytes.get(..len)?;
    let mut value = u64::from(first & 0x3f);
    for byte in &raw[1..] {
        value = (value << 8) | u64::from(*byte);
    }
    Some((value, len))
}

async fn read_varint(recv: &mut quinn::RecvStream) -> Result<u64, TransportError> {
    let mut first = [0u8; 1];
    recv.read_exact(&mut first).await?;
    let len = 1usize << (first[0] >> 6);
    let mut raw = [0u8; 8];
    raw[0] = first[0];
    recv.read_exact(&mut raw[1..len]).await?;
    get_varint(&raw[..len])
        .map(|(value, _)| value)
        .ok_or(TransportError::Protocol("entero QUIC no válido".to_owned()))
}

/// Lee una trama HTTP/3: `(tipo, carga)`.
async fn read_frame(recv: &mut quinn::RecvStream) -> Result<(u64, Vec<u8>), TransportError> {
    let kind = read_varint(recv).await?;
    let len = read_varint(recv).await?;
    if len > MAX_FRAME {
        return Err(TransportError::Protocol(format!(
            "trama HTTP/3 de {len} bytes"
        )));
    }
    let mut payload = vec![0u8; len as usize];
    recv.read_exact(&mut payload).await?;
    Ok((kind, payload))
}

fn frame(kind: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 16);
    put_varint(&mut out, kind);
    put_varint(&mut out, payload.len() as u64);
    out.extend_from_slice(payload);
    out
}

fn client_settings() -> Vec<u8> {
    let mut payload = Vec::new();
    for (id, value) in [
        (SETTING_QPACK_MAX_TABLE_CAPACITY, 0),
        (SETTING_QPACK_BLOCKED_STREAMS, 0),
        (SETTING_H3_DATAGRAM, 1),
        (SETTING_ENABLE_WEBTRANSPORT, 1),
        (SETTING_WT_MAX_SESSIONS_OLD, 1),
        (SETTING_WT_MAX_SESSIONS, 1),
    ] {
        put_varint(&mut payload, id);
        put_varint(&mut payload, value);
    }
    let mut out = Vec::new();
    put_varint(&mut out, STREAM_TYPE_CONTROL);
    out.extend(frame(FRAME_SETTINGS, &payload));
    out
}

/// Indica si unos `SETTINGS` del servidor admiten WebTransport.
fn server_supports_webtransport(payload: &[u8]) -> bool {
    let mut rest = payload;
    let mut enabled = false;
    while let Some((id, used)) = get_varint(rest) {
        rest = &rest[used..];
        let Some((value, used)) = get_varint(rest) else {
            return false;
        };
        rest = &rest[used..];
        if matches!(
            id,
            SETTING_ENABLE_WEBTRANSPORT | SETTING_WT_MAX_SESSIONS_OLD | SETTING_WT_MAX_SESSIONS
        ) && value > 0
        {
            enabled = true;
        }
    }
    enabled
}

/// Sesión WebTransport establecida.
pub(crate) struct Session {
    conn: quinn::Connection,
    session_id: u64,
    _control: quinn::SendStream,
    _connect: (quinn::SendStream, quinn::RecvStream),
    uni_task: JoinHandle<()>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.uni_task.abort();
    }
}

impl Session {
    /// Abre una sesión WebTransport en `path` sobre `conn` (ALPN `h3`).
    pub(crate) async fn connect(
        conn: quinn::Connection,
        authority: &str,
        path: &str,
    ) -> Result<Self, TransportError> {
        let (settings_tx, settings_rx) = oneshot::channel();
        let uni_task = tokio::spawn(read_server_streams(conn.clone(), settings_tx));

        let mut control = conn.open_uni().await?;
        control.write_all(&client_settings()).await?;

        let settings = tokio::time::timeout(HANDSHAKE_TIMEOUT, settings_rx)
            .await
            .map_err(|_| TransportError::Timeout("SETTINGS de HTTP/3 del servidor"))?
            .map_err(|_| TransportError::Closed)?;
        if !server_supports_webtransport(&settings) {
            return Err(TransportError::Protocol(
                "el servidor no admite WebTransport".to_owned(),
            ));
        }

        let (mut send, mut recv) = conn.open_bi().await?;
        let session_id = u64::from(send.id());
        let headers: Headers = [
            (":method", "CONNECT"),
            (":scheme", "https"),
            (":protocol", "webtransport"),
            (":authority", authority),
            (":path", path),
        ]
        .into_iter()
        .collect();
        let request = headers.generate_frame();
        send.write_all(&frame(FRAME_HEADERS, request.payload()))
            .await?;

        let status = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
            loop {
                let (kind, payload) = read_frame(&mut recv).await?;
                if kind != FRAME_HEADERS {
                    // Tramas reservadas ("grease") u otras extensiones: se ignoran.
                    continue;
                }
                let headers = Headers::with_frame(&Frame::new_headers(Cow::Borrowed(&payload)))
                    .map_err(|err| TransportError::Protocol(format!("QPACK: {err:?}")))?;
                return Ok::<_, TransportError>(headers.get(":status").map(str::to_owned));
            }
        })
        .await
        .map_err(|_| TransportError::Timeout("respuesta al CONNECT de WebTransport"))??;
        match status.as_deref() {
            Some("200") => {}
            other => {
                return Err(TransportError::Protocol(format!(
                    "el servidor rechazó la sesión WebTransport (estado {})",
                    other.unwrap_or("desconocido")
                )));
            }
        }
        Ok(Self {
            conn,
            session_id,
            _control: control,
            _connect: (send, recv),
            uni_task,
        })
    }

    /// Conexión QUIC de la sesión.
    pub(crate) fn connection(&self) -> &quinn::Connection {
        &self.conn
    }

    /// Abre un stream bidireccional de la sesión.
    pub(crate) async fn open_bi(
        &self,
    ) -> Result<(quinn::SendStream, quinn::RecvStream), TransportError> {
        let (mut send, recv) = self.conn.open_bi().await?;
        let mut header = Vec::with_capacity(9);
        put_varint(&mut header, FRAME_WEBTRANSPORT_STREAM);
        put_varint(&mut header, self.session_id);
        send.write_all(&header).await?;
        Ok((send, recv))
    }
}

/// Lee los streams unidireccionales del servidor: los `SETTINGS` del de control y descarta el
/// resto (QPACK sin tabla dinámica no manda nada útil), sin cerrarlos.
async fn read_server_streams(conn: quinn::Connection, settings: oneshot::Sender<Vec<u8>>) {
    let mut settings = Some(settings);
    while let Ok(mut recv) = conn.accept_uni().await {
        let Ok(kind) = read_varint(&mut recv).await else {
            continue;
        };
        if kind == STREAM_TYPE_CONTROL {
            match read_frame(&mut recv).await {
                Ok((FRAME_SETTINGS, payload)) => {
                    if let Some(tx) = settings.take() {
                        let _ = tx.send(payload);
                    }
                }
                Ok((other, _)) => {
                    debug!(kind = other, "el control de HTTP/3 no empieza por SETTINGS");
                    return;
                }
                Err(_) => return,
            }
        }
        tokio::spawn(
            async move { while let Ok(Some(_)) = recv.read_chunk(64 * 1024, true).await {} },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_round_trip() {
        for value in [
            0,
            37,
            63,
            64,
            15_293,
            16_383,
            16_384,
            494_878_333,
            (1 << 30) - 1,
            1 << 30,
            (1 << 62) - 1,
        ] {
            let mut out = Vec::new();
            put_varint(&mut out, value);
            assert_eq!(get_varint(&out), Some((value, out.len())), "{value}");
        }
        // Ejemplos de RFC 9000 §A.1.
        assert_eq!(
            get_varint(&[0x9d, 0x7f, 0x3e, 0x7d]),
            Some((494_878_333, 4))
        );
        assert_eq!(get_varint(&[0x7b, 0xbd]), Some((15_293, 2)));
        assert_eq!(get_varint(&[0x40]), None);
    }

    #[test]
    fn settings_announce_webtransport() {
        let settings = client_settings();
        assert_eq!(settings[0], 0x00, "stream de control");
        let (kind, used) = get_varint(&settings[1..]).expect("tipo");
        assert_eq!(kind, FRAME_SETTINGS);
        let (len, used2) = get_varint(&settings[1 + used..]).expect("longitud");
        let payload = &settings[1 + used + used2..];
        assert_eq!(payload.len() as u64, len);
        assert!(server_supports_webtransport(payload));
        assert!(!server_supports_webtransport(&[0x01, 0x00]));
    }
}
