//! Rutas HTTP compartidas por la pila HTTP/3 y la pila TCP.
//!
//! Las dos pilas convierten cada petición en un [`Request`] (método, ruta, consulta, cabecera
//! `Authorization`, cuerpo de como mucho [`MAX_BODY`] y la IP del cliente) y devuelven lo que dé
//! [`dispatch`]: `/health` y `/version`, la interfaz de administración `/admin` ([`crate::admin`])
//! y la API `/api/v1` ([`crate::api`]).

use std::net::IpAddr;

use bytes::Bytes;
use http::{Method, StatusCode};
use serde::Serialize;

use coroom_transport::proto::PROTOCOL_VERSION;
use coroom_transport::proto::api::{API_PREFIX, ApiError};

use crate::ServerState;

/// Nombre del servidor en `GET /version`.
pub const SERVER_NAME: &str = "coroom-server";

/// Versión del servidor en `GET /version` (la del paquete Cargo).
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Tipo de contenido de las respuestas JSON.
pub const CONTENT_TYPE_JSON: &str = "application/json";

/// Cuerpo máximo de una petición.
pub const MAX_BODY: usize = 64 * 1024;

/// Petición ya leída, igual para HTTP/3 y TCP.
#[derive(Debug, Clone)]
pub struct Request {
    /// Método.
    pub method: Method,
    /// Ruta (sin la consulta).
    pub path: String,
    /// Consulta (lo que va tras `?`).
    pub query: Option<String>,
    /// Cabecera `Authorization`.
    pub authorization: Option<String>,
    /// Cuerpo.
    pub body: Bytes,
    /// IP del cliente.
    pub remote: IpAddr,
}

impl Request {
    /// Petición sin cuerpo ni autenticación (tests).
    pub fn get(path: &str) -> Self {
        Self {
            method: Method::GET,
            path: path.to_owned(),
            query: None,
            authorization: None,
            body: Bytes::new(),
            remote: IpAddr::from([127, 0, 0, 1]),
        }
    }

    /// Valor de un parámetro de la consulta.
    pub fn query_param(&self, name: &str) -> Option<&str> {
        self.query.as_deref()?.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key == name).then_some(value)
        })
    }
}

/// Respuesta.
#[derive(Debug, Clone)]
pub struct Response {
    /// Estado.
    pub status: StatusCode,
    /// Tipo de contenido.
    pub content_type: &'static str,
    /// Cabeceras extra.
    pub headers: Vec<(&'static str, &'static str)>,
    /// Cuerpo.
    pub body: Vec<u8>,
}

/// `Strict-Transport-Security` de todas las respuestas: el servidor solo habla TLS (HTTP/3 y
/// HTTP/2 con TLS), así que el navegador no debe intentar nunca HTTP en claro con este nombre. Sin
/// `includeSubDomains`: el operador puede tener otros servicios bajo el mismo dominio.
pub const HSTS: (&str, &str) = ("strict-transport-security", "max-age=31536000");

/// Cabeceras de todas las respuestas de la API: nada en cachés, nada que un navegador pueda
/// interpretar como página ni enmarcar, y sin CORS (las rutas no aceptan peticiones de otros
/// orígenes: la sesión va en `Authorization`, nunca en cookies).
pub const API_HEADERS: &[(&str, &str)] = &[
    ("cache-control", "no-store"),
    ("x-content-type-options", "nosniff"),
    (
        "content-security-policy",
        "default-src 'none'; frame-ancestors 'none'",
    ),
    ("x-frame-options", "DENY"),
    ("referrer-policy", "no-referrer"),
    HSTS,
];

impl Response {
    /// Respuesta JSON.
    pub fn json<T: Serialize + ?Sized>(status: StatusCode, body: &T) -> Self {
        match serde_json::to_vec(body) {
            Ok(bytes) => Self {
                status,
                content_type: CONTENT_TYPE_JSON,
                headers: API_HEADERS.to_vec(),
                body: bytes,
            },
            // Los tipos de la API son structs planos con campos serializables: no puede fallar,
            // pero si fallara se responde un 500 en lugar de abortar el proceso.
            Err(_) => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                content_type: CONTENT_TYPE_JSON,
                headers: API_HEADERS.to_vec(),
                body: b"{}".to_vec(),
            },
        }
    }

    /// Error `{ "error": código, "message": texto }`.
    pub fn error(status: StatusCode, code: &str, message: impl Into<String>) -> Self {
        Self::json(
            status,
            &ApiError {
                error: code.to_owned(),
                message: message.into(),
            },
        )
    }

    /// 404 genérico.
    pub fn not_found() -> Self {
        Self::error(
            StatusCode::NOT_FOUND,
            "not_found",
            "no existe esa ruta en el servidor",
        )
    }
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    /// Resultado de la autocomprobación de UDP contra `COROOM_PUBLIC_HOST` (`null` hasta la
    /// primera).
    udp_reachable: Option<bool>,
    /// Cuándo se hizo (RFC 3339).
    udp_checked_at: Option<String>,
    /// Usuarios registrados.
    users: u64,
    /// Salas abiertas en la señalización.
    rooms: usize,
    /// Bytes reenviados por el relay desde el arranque.
    relay_bytes: u64,
    /// Bytes reenviados por el relay hoy (UTC), de todos los usuarios.
    relay_bytes_today: u64,
    /// Bytes reenviados en cada sala activa del relay, de mayor a menor (sin identificarlas).
    relay_rooms: Vec<u64>,
}

#[derive(Serialize)]
struct Version {
    name: &'static str,
    version: &'static str,
    protocol: u16,
    /// Código fuente del servidor (AGPL §13; `COROOM_SOURCE_URL`).
    source: String,
}

/// Resuelve una petición; la usan HTTP/3 y TCP para que las dos pilas respondan igual.
pub(crate) async fn dispatch(state: &ServerState, request: Request) -> Response {
    let path = request.path.clone();
    if let Some(rest) = path.strip_prefix(API_PREFIX)
        && (rest.is_empty() || rest.starts_with('/'))
    {
        return crate::api::handle(state, &request, rest).await;
    }
    if path == "/admin" || path.starts_with("/admin/") {
        return crate::admin::static_file(&request);
    }
    match (&request.method, path.as_str()) {
        (&Method::GET, "/health") => Response::json(StatusCode::OK, &health(state).await),
        (&Method::GET, "/version") => Response::json(
            StatusCode::OK,
            &Version {
                name: SERVER_NAME,
                version: SERVER_VERSION,
                protocol: PROTOCOL_VERSION,
                source: state.config.source_url.clone(),
            },
        ),
        (&Method::GET, "/") => Response {
            status: StatusCode::SEE_OTHER,
            content_type: "text/plain; charset=utf-8",
            headers: vec![("location", "/admin/"), HSTS],
            body: b"/admin/".to_vec(),
        },
        _ => Response::not_found(),
    }
}

async fn health(state: &ServerState) -> Health {
    let (udp_reachable, udp_checked_at) = state.udp.get();
    let today = crate::clock::day_of(crate::clock::now());
    let stored = state.db.usage_total_day(today).await.unwrap_or(0);
    Health {
        status: "ok",
        udp_reachable,
        udp_checked_at: udp_checked_at.map(crate::clock::rfc3339),
        users: state.db.count_users().await.unwrap_or(0),
        rooms: state.hub.rooms(),
        relay_bytes: state.relay.total_bytes(),
        relay_bytes_today: stored + state.quota.pending_today(),
        relay_rooms: state.relay.room_bytes(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_parameters_are_found() {
        let mut request = Request::get("/api/v1/admin/usage");
        assert_eq!(request.query_param("month"), None);
        request.query = Some("month=2026-10&x".to_owned());
        assert_eq!(request.query_param("month"), Some("2026-10"));
        assert_eq!(request.query_param("x"), Some(""));
    }

    #[test]
    fn errors_have_code_and_spanish_message() {
        let response = Response::error(StatusCode::BAD_REQUEST, "bad_request", "petición mal");
        assert_eq!(
            String::from_utf8(response.body.clone()).expect("UTF-8"),
            r#"{"error":"bad_request","message":"petición mal"}"#
        );
        assert!(response.headers.contains(&("cache-control", "no-store")));
        assert!(response.headers.contains(&HSTS));
        assert!(response.headers.contains(&("x-frame-options", "DENY")));
        assert!(
            !response
                .headers
                .iter()
                .any(|(name, _)| name.starts_with("access-control-"))
        );
    }
}
