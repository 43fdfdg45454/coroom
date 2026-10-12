//! Autenticación: contraseñas con Argon2id, tokens de sesión opacos, desafíos de
//! dispositivo, códigos de invitación y límite de intentos.
//!
//! - **Contraseñas**: Argon2id con los parámetros de OWASP (19 MiB, 2 pasadas, 1 hilo), en formato
//!   PHC; se calculan en `spawn_blocking`. Mínimo [`MIN_PASSWORD_LEN`] caracteres. Un usuario que
//!   no existe se compara contra un hash de relleno para que la respuesta tarde lo mismo.
//! - **Tokens**: 32 bytes aleatorios en hexadecimal; la base solo guarda su SHA-256.
//! - **Desafíos de dispositivo**: 32 bytes aleatorios válidos [`CHALLENGE_TTL`]; el cliente firma
//!   `COROOM_PUBLIC_HOST || desafío` con la identidad cuya clave pública presentó.
//! - **Límite de intentos**: [`MAX_FAILURES`] fallos por minuto por cuenta y por IP; después, la
//!   misma respuesta para todo (también para la contraseña buena) hasta que pase el minuto.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::{Algorithm, Argon2, Params, Version};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use coroom_transport::proto::api::{MAX_PASSWORD_LEN, MIN_PASSWORD_LEN};
use coroom_transport::proto::hex;
use coroom_transport::proto::signal::ROOM_CODE_ALPHABET;

/// Validez de un token de sesión de dispositivo.
pub const SESSION_TTL: Duration = Duration::from_secs(30 * 24 * 3600);

/// Validez de un token de la interfaz web de administración.
pub const WEB_SESSION_TTL: Duration = Duration::from_secs(12 * 3600);

/// Validez de un desafío de dispositivo.
pub const CHALLENGE_TTL: Duration = Duration::from_secs(60);

/// Validez por defecto de un código de invitación.
pub const INVITATION_TTL_HOURS: u32 = 72;

/// Validez máxima de un código de invitación.
pub const INVITATION_MAX_HOURS: u32 = 720;

/// Fallos de login permitidos por minuto, por cuenta y por IP.
pub const MAX_FAILURES: usize = 5;

/// Ventana del límite de intentos.
pub const FAILURE_WINDOW: Duration = Duration::from_secs(60);

/// Desafíos pendientes como mucho (protección de memoria).
const MAX_CHALLENGES: usize = 4096;

/// Argon2id con los parámetros recomendados por OWASP (m = 19 MiB, t = 2, p = 1).
fn argon2() -> Argon2<'static> {
    let params = Params::new(19 * 1024, 2, 1, None).unwrap_or_default();
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// Hash PHC de una contraseña (lento: llamar desde `spawn_blocking`).
pub fn hash_password(password: &str) -> Result<String, String> {
    argon2()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|err| format!("no se pudo calcular el hash de la contraseña: {err}"))
}

/// Comprueba una contraseña contra su hash PHC (lento).
pub fn verify_password(password: &str, phc: &str) -> bool {
    let Ok(hash) = PasswordHash::new(phc) else {
        return false;
    };
    argon2().verify_password(password.as_bytes(), &hash).is_ok()
}

/// Igual de lento que [`verify_password`] y siempre `false`: para usuarios que no existen.
pub fn dummy_verify(password: &str) {
    static DUMMY: OnceLock<Option<String>> = OnceLock::new();
    let dummy = DUMMY.get_or_init(|| {
        let mut seed = [0u8; 16];
        getrandom::fill(&mut seed).ok()?;
        hash_password(&hex::encode(&seed)).ok()
    });
    if let Some(phc) = dummy {
        let _ = verify_password(password, phc);
    }
}

/// Bytes aleatorios del sistema.
pub fn random_bytes<const N: usize>() -> Option<[u8; N]> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).ok()?;
    Some(bytes)
}

/// SHA-256.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// Token nuevo: el texto para el cliente y su hash para la base.
pub fn new_token() -> Option<(String, [u8; 32])> {
    let token = hex::encode(&random_bytes::<32>()?);
    let hash = token_hash(&token);
    Some((token, hash))
}

/// Hash con el que se guarda (y se busca) un token.
pub fn token_hash(token: &str) -> [u8; 32] {
    sha256(token.trim().as_bytes())
}

/// Comparación en tiempo constante (token de desarrollo).
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// Código de invitación `XXXX-XXXX-XXXX` (60 bits del alfabeto sin caracteres ambiguos).
pub fn new_invitation_code() -> Option<String> {
    let bytes = random_bytes::<12>()?;
    let chars: Vec<char> = bytes
        .iter()
        .map(|b| char::from(ROOM_CODE_ALPHABET[usize::from(b & 31)]))
        .collect();
    Some(
        chars
            .chunks(4)
            .map(|chunk| chunk.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("-"),
    )
}

/// Código de invitación sin guiones ni espacios y en mayúsculas.
pub fn normalize_invitation_code(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Hash con el que se guarda un código de invitación.
pub fn invitation_hash(code: &str) -> [u8; 32] {
    sha256(normalize_invitation_code(code).as_bytes())
}

/// Comprueba un nombre de usuario (3-32 caracteres: letras y dígitos ASCII, `.`, `-` y `_`).
pub fn validate_username(username: &str) -> Result<String, &'static str> {
    let username = username.trim();
    let len = username.chars().count();
    if !(3..=32).contains(&len) {
        return Err("el nombre de usuario debe tener entre 3 y 32 caracteres");
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err("el nombre de usuario solo admite letras, dígitos, «.», «-» y «_»");
    }
    Ok(username.to_owned())
}

/// Comprueba un nombre visible (1-64 caracteres, sin caracteres de control).
pub fn validate_display_name(name: &str) -> Result<String, &'static str> {
    let name = name.trim();
    let len = name.chars().count();
    if !(1..=64).contains(&len) {
        return Err("el nombre visible debe tener entre 1 y 64 caracteres");
    }
    if name.chars().any(char::is_control) {
        return Err("el nombre visible no admite caracteres de control");
    }
    Ok(name.to_owned())
}

/// Comprueba la longitud de una contraseña.
pub fn validate_password(password: &str) -> Result<(), String> {
    let len = password.chars().count();
    if len < MIN_PASSWORD_LEN {
        return Err(format!(
            "la contraseña debe tener al menos {MIN_PASSWORD_LEN} caracteres"
        ));
    }
    if len > MAX_PASSWORD_LEN {
        return Err(format!(
            "la contraseña no puede pasar de {MAX_PASSWORD_LEN} caracteres"
        ));
    }
    Ok(())
}

/// Nombre de dispositivo limpio (1-64 caracteres; por defecto «dispositivo»).
pub fn clean_device_name(name: Option<&str>) -> String {
    let name: String = name
        .unwrap_or_default()
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    if name.is_empty() {
        "dispositivo".to_owned()
    } else {
        name
    }
}

/// Desafío de dispositivo pendiente.
#[derive(Debug, Clone)]
pub struct PendingChallenge {
    /// Usuario que pasó la contraseña.
    pub user_id: i64,
    /// Clave pública presentada.
    pub public_key: [u8; 32],
    /// Nombre del dispositivo.
    pub device_name: String,
    /// Los 32 bytes del desafío.
    pub challenge: [u8; 32],
    expires: Instant,
}

/// Fallos recientes por clave (cuenta o IP).
#[derive(Debug, Default)]
struct Failures(HashMap<String, VecDeque<Instant>>);

impl Failures {
    fn prune(&mut self, now: Instant) {
        self.0.retain(|_, times| {
            while times
                .front()
                .is_some_and(|t| now.duration_since(*t) >= FAILURE_WINDOW)
            {
                times.pop_front();
            }
            !times.is_empty()
        });
    }

    fn count(&self, key: &str) -> usize {
        self.0.get(key).map_or(0, VecDeque::len)
    }

    fn add(&mut self, key: String, now: Instant) {
        self.0.entry(key).or_default().push_back(now);
    }
}

/// Estado en memoria de la autenticación: desafíos pendientes y límite de intentos.
#[derive(Debug, Default)]
pub struct AuthState {
    challenges: Mutex<HashMap<String, PendingChallenge>>,
    failures: Mutex<Failures>,
}

fn account_key(username: &str) -> String {
    format!("cuenta:{}", username.trim().to_ascii_lowercase())
}

fn ip_key(ip: IpAddr) -> String {
    format!("ip:{}", ip.to_canonical())
}

impl AuthState {
    /// `true` si la cuenta o la IP ya agotaron sus intentos.
    pub fn limited(&self, username: &str, ip: IpAddr) -> bool {
        let Ok(mut failures) = self.failures.lock() else {
            return true;
        };
        failures.prune(Instant::now());
        failures.count(&account_key(username)) >= MAX_FAILURES
            || failures.count(&ip_key(ip)) >= MAX_FAILURES
    }

    /// Apunta un fallo de la cuenta y de la IP.
    pub fn failed(&self, username: &str, ip: IpAddr) {
        if let Ok(mut failures) = self.failures.lock() {
            let now = Instant::now();
            failures.add(account_key(username), now);
            failures.add(ip_key(ip), now);
        }
    }

    /// `true` si la IP ya agotó sus intentos (canje de invitaciones).
    pub fn limited_ip(&self, ip: IpAddr) -> bool {
        let Ok(mut failures) = self.failures.lock() else {
            return true;
        };
        failures.prune(Instant::now());
        failures.count(&ip_key(ip)) >= MAX_FAILURES
    }

    /// Apunta un fallo de la IP.
    pub fn failed_ip(&self, ip: IpAddr) {
        if let Ok(mut failures) = self.failures.lock() {
            failures.add(ip_key(ip), Instant::now());
        }
    }

    /// Guarda un desafío nuevo y devuelve su identificador y sus bytes.
    pub fn issue_challenge(
        &self,
        user_id: i64,
        public_key: [u8; 32],
        device_name: String,
    ) -> Option<(String, [u8; 32])> {
        let challenge = random_bytes::<32>()?;
        let id = hex::encode(&random_bytes::<16>()?);
        let mut challenges = self.challenges.lock().ok()?;
        let now = Instant::now();
        challenges.retain(|_, pending| pending.expires > now);
        if challenges.len() >= MAX_CHALLENGES {
            return None;
        }
        challenges.insert(
            id.clone(),
            PendingChallenge {
                user_id,
                public_key,
                device_name,
                challenge,
                expires: now + CHALLENGE_TTL,
            },
        );
        Some((id, challenge))
    }

    /// Retira un desafío (un solo uso); `None` si no existe o caducó.
    pub fn take_challenge(&self, id: &str) -> Option<PendingChallenge> {
        let pending = self.challenges.lock().ok()?.remove(id)?;
        (pending.expires > Instant::now()).then_some(pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_hash_with_argon2id_and_verify() {
        let hash = hash_password("change-me-please").expect("hash");
        assert!(
            hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "{hash}"
        );
        assert!(verify_password("change-me-please", &hash));
        assert!(!verify_password("change-me-pleasE", &hash));
        assert!(!verify_password("change-me-please", "no es PHC"));
        dummy_verify("lo que sea");
    }

    #[test]
    fn tokens_are_random_hex_and_hashed() {
        let (a, hash_a) = new_token().expect("token");
        let (b, _) = new_token().expect("token");
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
        assert_eq!(token_hash(&a), hash_a);
        assert!(constant_time_eq("change-me", "change-me"));
        assert!(!constant_time_eq("change-me", "change-mf"));
        assert!(!constant_time_eq("change", "change-me"));
    }

    #[test]
    fn invitation_codes_are_readable_and_normalised() {
        let code = new_invitation_code().expect("código");
        assert_eq!(code.len(), 14);
        assert_eq!(code.matches('-').count(), 2);
        let lower = code.to_ascii_lowercase().replace('-', " ");
        assert_eq!(invitation_hash(&code), invitation_hash(&lower));
    }

    #[test]
    fn names_and_passwords_are_validated() {
        assert_eq!(validate_username(" ana_1 "), Ok("ana_1".to_owned()));
        assert!(validate_username("an").is_err());
        assert!(validate_username("ana pérez").is_err());
        assert!(validate_display_name("Ana Pérez").is_ok());
        assert!(validate_display_name("  ").is_err());
        assert!(validate_password("corta").is_err());
        assert!(validate_password("doce-letras").is_err());
        assert!(validate_password("doce-letras!").is_ok());
        assert_eq!(clean_device_name(None), "dispositivo");
        assert_eq!(clean_device_name(Some(" portátil\n")), "portátil");
    }

    #[test]
    fn five_failures_per_minute_lock_the_account_and_the_ip() {
        let state = AuthState::default();
        let ip: IpAddr = "192.0.2.1".parse().expect("ip");
        let other: IpAddr = "192.0.2.2".parse().expect("ip");
        for _ in 0..MAX_FAILURES {
            assert!(!state.limited("ana", ip));
            state.failed("ana", ip);
        }
        assert!(state.limited("ANA", other), "por cuenta");
        assert!(state.limited("bea", ip), "por IP");
        assert!(!state.limited("bea", other));
    }

    #[test]
    fn challenges_are_single_use() {
        let state = AuthState::default();
        let (id, bytes) = state
            .issue_challenge(1, [2; 32], "portátil".into())
            .expect("desafío");
        let pending = state.take_challenge(&id).expect("pendiente");
        assert_eq!(pending.challenge, bytes);
        assert_eq!(pending.public_key, [2; 32]);
        assert!(state.take_challenge(&id).is_none());
        assert!(state.take_challenge("otro").is_none());
    }
}
