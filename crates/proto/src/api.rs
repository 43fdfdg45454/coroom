//! Tipos JSON de la API HTTP del servidor, compartidos por `coroom-server` y el cliente
//! (`coroom_transport::api`).
//!
//! Todas las rutas cuelgan de [`API_PREFIX`] y van por HTTP/3 o por HTTP/2 sobre TLS en TCP. Los
//! errores son `{ "error": "codigo", "message": "texto en español" }` ([`ApiError`]) y la
//! autenticación es `Authorization: Bearer <token>`. Las claves públicas, firmas y desafíos van en
//! hexadecimal ([`crate::hex`]); las fechas, en segundos Unix (UTC).

use serde::{Deserialize, Serialize};

/// Prefijo de todas las rutas de la API.
pub const API_PREFIX: &str = "/api/v1";

/// Longitud mínima de una contraseña.
pub const MIN_PASSWORD_LEN: usize = 12;

/// Longitud máxima de una contraseña (Argon2 acepta más, pero no tiene sentido).
pub const MAX_PASSWORD_LEN: usize = 256;

/// Cuerpo de todas las respuestas de error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    /// Código estable (`invalid_credentials`, `not_found`…).
    pub error: String,
    /// Texto en español para mostrar.
    pub message: String,
}

/// `GET /setup`: si falta crear el primer administrador.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupStatus {
    /// `true` hasta que exista un administrador.
    pub needed: bool,
    /// Nombre público del servidor.
    pub server_host: String,
    /// Versión del servidor.
    pub version: String,
}

/// `POST /setup`: crea el primer administrador (solo si no hay ninguno).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupRequest {
    /// Nombre de usuario (3-32 caracteres: letras, dígitos, `.`, `-`, `_`).
    pub username: String,
    /// Contraseña (al menos [`MIN_PASSWORD_LEN`] caracteres).
    pub password: String,
    /// Nombre visible (por defecto, el de usuario).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// Un usuario visto por él mismo o por el administrador.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    /// Identificador.
    pub id: u64,
    /// Nombre de usuario (único, sin distinguir mayúsculas).
    pub username: String,
    /// Nombre visible.
    pub display_name: String,
    /// Si es administrador.
    pub is_admin: bool,
}

/// `POST /auth/login`.
///
/// Con `public_key` (la de la identidad Ed25519 del dispositivo) el servidor responde con un
/// [`LoginChallenge`] que el dispositivo firma en `POST /auth/login/complete`; sin ella, solo para
/// administradores, con un [`SessionToken`] de la interfaz web (`/admin`), que no abre la
/// señalización ni el relay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginRequest {
    /// Nombre de usuario.
    pub username: String,
    /// Contraseña.
    pub password: String,
    /// Clave pública Ed25519 del dispositivo, en hexadecimal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
    /// Nombre del dispositivo (el del equipo).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
}

/// Desafío de dispositivo: el cliente devuelve `Identity::sign(server_host || challenge)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginChallenge {
    /// Identificador del desafío.
    pub challenge_id: String,
    /// 32 bytes aleatorios en hexadecimal.
    pub challenge: String,
    /// `COROOM_PUBLIC_HOST`, que va delante del desafío en lo firmado.
    pub server_host: String,
    /// Segundos que le quedan (60).
    pub expires_in: u64,
}

/// Lo que firma el dispositivo para completar el login.
pub fn challenge_message(server_host: &str, challenge: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(server_host.len() + challenge.len());
    message.extend_from_slice(server_host.as_bytes());
    message.extend_from_slice(challenge);
    message
}

/// `POST /auth/login/complete`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginComplete {
    /// El `challenge_id` del [`LoginChallenge`].
    pub challenge_id: String,
    /// Firma Ed25519 en hexadecimal.
    pub signature: String,
}

/// Token de sesión (opaco, 32 bytes en hexadecimal).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionToken {
    /// El token para `Authorization: Bearer`.
    pub token: String,
    /// Caducidad (segundos Unix).
    pub expires_at: i64,
    /// El usuario.
    pub user: User,
}

/// Un dispositivo vinculado.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// Identificador.
    pub id: u64,
    /// Nombre del dispositivo.
    pub name: String,
    /// Huella SHA-256 de su clave pública.
    pub fingerprint: String,
    /// Alta (segundos Unix).
    pub created_at: i64,
    /// Último uso (segundos Unix).
    pub last_seen_at: i64,
}

/// `GET /me/devices`: los dispositivos vinculados a la cuenta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Devices {
    /// El de la sesión de la petición (`None` desde la interfaz web).
    pub current: Option<u64>,
    /// Todos, del último usado al primero.
    pub devices: Vec<Device>,
}

/// `GET /me`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    /// El usuario.
    pub user: User,
    /// El dispositivo de este token (`None` en la interfaz web).
    pub device: Option<Device>,
    /// Caducidad del token (segundos Unix).
    pub expires_at: i64,
}

/// `POST /invitations/redeem`: crea la cuenta con un código de invitación.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedeemRequest {
    /// Código de invitación (`XXXX-XXXX-XXXX`).
    pub code: String,
    /// Nombre de usuario nuevo.
    pub username: String,
    /// Contraseña.
    pub password: String,
    /// Nombre visible (por defecto, el de usuario).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// Un amigo (o quien pide serlo).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Friend {
    /// Usuario.
    pub id: u64,
    /// Nombre de usuario.
    pub username: String,
    /// Nombre visible.
    pub display_name: String,
    /// Conectado ahora a la señalización (solo de los amigos; `false` en las solicitudes).
    pub online: bool,
}

/// Solicitud de amistad pendiente.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FriendRequest {
    /// Identificador de la solicitud.
    pub id: u64,
    /// El otro usuario (quien la envió o a quien se envió).
    pub user: Friend,
    /// Fecha (segundos Unix).
    pub created_at: i64,
}

/// `GET /friends`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Friends {
    /// Amigos.
    pub friends: Vec<Friend>,
    /// Solicitudes recibidas.
    pub incoming: Vec<FriendRequest>,
    /// Solicitudes enviadas.
    pub outgoing: Vec<FriendRequest>,
}

/// `POST /friends/requests` (y su alias `POST /friends`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddFriend {
    /// Nombre de usuario del amigo.
    pub username: String,
}

/// Resultado de pedir amistad: si el otro ya la había pedido, queda aceptada.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddFriendResult {
    /// `true` si ya sois amigos.
    pub accepted: bool,
    /// El otro usuario.
    pub user: Friend,
}

/// Bytes del relay de un día.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayUsage {
    /// Día (`AAAA-MM-DD`, UTC).
    pub day: String,
    /// Bytes reenviados por el relay en las salas del usuario.
    pub bytes: u64,
}

/// `GET /usage`: uso del relay propio en el mes en curso.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Mes (`AAAA-MM`, UTC).
    pub month: String,
    /// Bytes del mes.
    pub bytes: u64,
    /// Cuota mensual en bytes (`None` = sin límite).
    pub quota_bytes: Option<u64>,
    /// Si el servidor tiene relay.
    pub relay_enabled: bool,
    /// Detalle por día.
    pub days: Vec<DayUsage>,
}

/// Usuario en la administración.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminUser {
    /// Identificador.
    pub id: u64,
    /// Nombre de usuario.
    pub username: String,
    /// Nombre visible.
    pub display_name: String,
    /// Administrador.
    pub is_admin: bool,
    /// Deshabilitado (no puede entrar; sus sesiones se cierran).
    pub disabled: bool,
    /// Alta (segundos Unix).
    pub created_at: i64,
    /// Dispositivos vinculados.
    pub devices: u64,
    /// Último uso de un dispositivo (segundos Unix).
    pub last_seen_at: Option<i64>,
    /// Conectado ahora a la señalización.
    pub online: bool,
}

/// `POST /admin/users`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateUser {
    /// Nombre de usuario.
    pub username: String,
    /// Contraseña.
    pub password: String,
    /// Nombre visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Administrador.
    #[serde(default)]
    pub is_admin: bool,
}

/// `PATCH /admin/users/{id}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateUser {
    /// Nombre visible nuevo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Hacer (o dejar de hacer) administrador.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_admin: Option<bool>,
    /// Deshabilitar o habilitar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
}

/// `POST /admin/users/{id}/password`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetPassword {
    /// Contraseña nueva.
    pub password: String,
}

/// Código de invitación (sin el código: solo se muestra al crearlo).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invitation {
    /// Identificador.
    pub id: u64,
    /// Últimos caracteres del código, para reconocerlo.
    pub hint: String,
    /// Nota del administrador (para quién es).
    pub note: String,
    /// Alta (segundos Unix).
    pub created_at: i64,
    /// Caducidad (segundos Unix).
    pub expires_at: i64,
    /// `pending`, `used`, `expired` o `revoked`.
    pub state: String,
    /// Usuario que lo canjeó.
    pub used_by: Option<String>,
}

/// `POST /admin/invitations`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateInvitation {
    /// Nota (para quién es).
    #[serde(default)]
    pub note: String,
    /// Horas de validez (por defecto 72, como mucho 720).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hours: Option<u32>,
}

/// Respuesta de `POST /admin/invitations`: el código, que no se vuelve a mostrar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedInvitation {
    /// Código (`XXXX-XXXX-XXXX`).
    pub code: String,
    /// La invitación.
    pub invitation: Invitation,
}

/// Uso del relay de un usuario en un mes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserUsage {
    /// Usuario.
    pub user_id: u64,
    /// Nombre de usuario.
    pub username: String,
    /// Bytes del mes.
    pub bytes: u64,
}

/// `GET /admin/usage[?month=AAAA-MM]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminUsage {
    /// Mes.
    pub month: String,
    /// Cuota mensual por usuario en bytes (`None` = sin límite).
    pub quota_bytes: Option<u64>,
    /// Total del mes.
    pub total: u64,
    /// Por usuario, de más a menos.
    pub users: Vec<UserUsage>,
}

/// Sala activa (sin identificarla: ni código ni miembros).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomInfo {
    /// Invitados en la sala.
    pub guests: u64,
    /// Si usa el relay.
    pub relayed: bool,
    /// Bytes reenviados por el relay.
    pub relay_bytes: u64,
    /// Datagramas descartados por pasar de `COROOM_RELAY_MAX_KBPS`.
    pub relay_dropped: u64,
}

/// `GET /admin/rooms`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rooms {
    /// Salas.
    pub rooms: Vec<RoomInfo>,
}

/// `GET /admin/settings`: ajustes de solo lectura.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Versión del servidor.
    pub version: String,
    /// Versión del protocolo.
    pub protocol: u16,
    /// URL del código fuente de este servidor (`COROOM_SOURCE_URL`): la AGPL obliga a ofrecerlo
    /// a quien lo usa por la red.
    pub source_url: String,
    /// `COROOM_PUBLIC_HOST`.
    pub public_host: String,
    /// Puerto UDP público.
    pub public_udp_port: u16,
    /// Puerto TCP público.
    pub public_tcp_port: u16,
    /// Modo TLS (`self-signed` o `files`).
    pub tls_mode: String,
    /// Huella SHA-256 del certificado en uso.
    pub certificate_fingerprint: String,
    /// Caducidad del certificado en uso (RFC 3339), si se conoce.
    pub certificate_not_after: Option<String>,
    /// Alta de usuarios (`admin` o `invite`).
    pub registration: String,
    /// Relay habilitado.
    pub relay_enabled: bool,
    /// Cuota mensual por usuario en GB (0 = sin límite).
    pub relay_quota_gb_per_month: f64,
    /// Límite por sala en kbit/s (0 = sin límite).
    pub relay_max_kbps: u32,
    /// Invitados como mucho por sala.
    pub max_guests_per_room: u32,
    /// Resultado de la autocomprobación de UDP.
    pub udp_reachable: Option<bool>,
    /// Cuándo se hizo (RFC 3339).
    pub udp_checked_at: Option<String>,
    /// Modo desarrollo (`COROOM_DEV_TOKEN` definido).
    pub dev_mode: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_fields_are_optional_in_json() {
        let login: LoginRequest =
            serde_json::from_str(r#"{"username":"ana","password":"change-me-please"}"#)
                .expect("JSON");
        assert_eq!(login.public_key, None);
        let text = serde_json::to_string(&login).expect("JSON");
        assert!(!text.contains("public_key"), "{text}");
        let update: UpdateUser = serde_json::from_str(r#"{"disabled":true}"#).expect("JSON");
        assert_eq!(update.disabled, Some(true));
        assert_eq!(update.is_admin, None);
    }

    #[test]
    fn the_challenge_message_is_host_then_challenge() {
        assert_eq!(
            challenge_message("coroom.example.com", &[1, 2]),
            b"coroom.example.com\x01\x02".to_vec()
        );
    }
}
