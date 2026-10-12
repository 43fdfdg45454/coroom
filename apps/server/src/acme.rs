//! Certificado automático por ACME (RFC 8555) con `instant-acme`.
//!
//! - **Retos**: TLS-ALPN-01 (RFC 8737) por defecto, resuelto en el propio [`CertResolver`] de la
//!   pila TCP con TLS (puerto 443), o HTTP-01 (`/.well-known/acme-challenge/<token>` en la pila TCP
//!   sin TLS de `COROOM_BIND_HTTP`, puerto 80) con [`Http01Store`].
//! - **Cuenta y certificados** en `COROOM_DATA_DIR/acme/`: `account.json` (credenciales de la
//!   cuenta con su directorio, permisos `0600`), `cert.pem` (cadena) y `key.pem` (`0600`). La
//!   clave del certificado es ECDSA P-256 generada con `rcgen`; la CSR también.
//! - **Renovación** ([`run`]): una tarea de tokio calcula cuándo toca ([`next_renewal`]: 30 días
//!   antes de caducar o, en certificados cortos, al pasar dos tercios de su vida), pide el
//!   certificado y lo cambia en caliente; si falla, reintenta con espera exponencial
//!   ([`Backoff`]: de 1 minuto a 6 horas). Mientras no hay certificado, el servidor sirve uno
//!   autofirmado temporal y la primera emisión se intenta al arrancar.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, HttpClient, Identifier,
    NewAccount, NewOrder, OrderStatus, RetryPolicy,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tracing::{info, warn};

use coroom_transport::crypto::sha256_fingerprint;

use crate::clock;
use crate::config::AcmeChallenge;
use crate::tls::{
    ACME_CERT_FILE, ACME_DIR, ACME_KEY_FILE, CertResolver, TlsMaterial, challenge_certificate,
    replace_file,
};

/// Cuenta ACME dentro de `COROOM_DATA_DIR/acme/`.
pub const ACCOUNT_FILE: &str = "account.json";

/// Renovar este tiempo antes de la caducidad (como mucho).
pub const RENEW_BEFORE: Duration = Duration::from_secs(30 * 24 * 3600);

/// Primera espera tras un fallo.
pub const BACKOFF_START: Duration = Duration::from_secs(60);

/// Espera máxima entre reintentos.
pub const BACKOFF_MAX: Duration = Duration::from_secs(6 * 3600);

/// Espera máxima entre dos comprobaciones de la caducidad.
pub const CHECK_PERIOD: Duration = Duration::from_secs(12 * 3600);

/// Respuestas del reto HTTP-01: token → autorización.
#[derive(Debug, Default)]
pub struct Http01Store {
    tokens: RwLock<HashMap<String, String>>,
}

impl Http01Store {
    /// La autorización de `token`, si hay un reto en curso.
    pub fn get(&self, token: &str) -> Option<String> {
        self.tokens.read().ok()?.get(token).cloned()
    }

    fn set(&self, token: &str, key_authorization: &str) {
        if let Ok(mut tokens) = self.tokens.write() {
            tokens.insert(token.to_owned(), key_authorization.to_owned());
        }
    }

    fn clear(&self) {
        if let Ok(mut tokens) = self.tokens.write() {
            tokens.clear();
        }
    }
}

/// Cliente HTTP con el que se habla con la autoridad ACME.
#[derive(Clone)]
pub enum AcmeHttp {
    /// `hyper-rustls` con las raíces de la plataforma (el de producción).
    Default,
    /// Otro (los tests usan HTTP sin TLS contra un directorio falso en proceso).
    Custom(Arc<dyn Fn() -> Box<dyn HttpClient> + Send + Sync>),
}

impl std::fmt::Debug for AcmeHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            AcmeHttp::Default => "AcmeHttp::Default",
            AcmeHttp::Custom(_) => "AcmeHttp::Custom",
        })
    }
}

/// Ajustes de ACME.
#[derive(Debug, Clone)]
pub struct AcmeSettings {
    /// URL del directorio.
    pub directory: String,
    /// Contacto (`mailto:`), opcional.
    pub contact: Option<String>,
    /// Reto.
    pub challenge: AcmeChallenge,
    /// Dominio (`COROOM_PUBLIC_HOST`).
    pub domain: String,
    /// `COROOM_DATA_DIR/acme`.
    pub dir: PathBuf,
    /// Cliente HTTP.
    pub http: AcmeHttp,
}

impl AcmeSettings {
    /// Ajustes de la configuración del servidor.
    pub fn from_config(config: &crate::Config) -> Self {
        Self {
            directory: config.acme_directory.clone(),
            // Vacío equivale a sin contacto (Compose pasa la variable aunque no se defina).
            contact: config
                .acme_contact
                .clone()
                .filter(|contact| !contact.trim().is_empty()),
            challenge: config.acme_challenge,
            domain: config.public_host.to_ascii_lowercase(),
            dir: config.data_dir.join(ACME_DIR),
            http: AcmeHttp::Default,
        }
    }

    /// Si ya hay un certificado emitido en disco.
    pub fn has_certificate(&self) -> bool {
        self.dir.join(ACME_CERT_FILE).is_file() && self.dir.join(ACME_KEY_FILE).is_file()
    }
}

/// Error de ACME (texto en español).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcmeError(pub String);

impl std::fmt::Display for AcmeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AcmeError {}

fn acme(what: &str) -> impl Fn(instant_acme::Error) -> AcmeError + '_ {
    move |err| AcmeError(format!("{what}: {err}"))
}

#[derive(Serialize, Deserialize)]
struct StoredAccount {
    directory: String,
    credentials: AccountCredentials,
}

fn builder(http: &AcmeHttp) -> Result<instant_acme::AccountBuilder, AcmeError> {
    match http {
        AcmeHttp::Default => Account::builder().map_err(acme("cliente HTTP de ACME")),
        AcmeHttp::Custom(make) => Ok(Account::builder_with_http(make())),
    }
}

/// Carga la cuenta de `account.json` (si es del mismo directorio) o la crea y la guarda.
async fn account(settings: &AcmeSettings) -> Result<Account, AcmeError> {
    let path = settings.dir.join(ACCOUNT_FILE);
    if let Ok(text) = std::fs::read_to_string(&path)
        && let Ok(stored) = serde_json::from_str::<StoredAccount>(&text)
        && stored.directory == settings.directory
    {
        return builder(&settings.http)?
            .from_credentials(stored.credentials)
            .await
            .map_err(acme("cuenta ACME guardada"));
    }
    let contact = settings.contact.as_deref().map(|contact| {
        if contact.starts_with("mailto:") {
            contact.to_owned()
        } else {
            format!("mailto:{contact}")
        }
    });
    let contacts: Vec<&str> = contact.iter().map(String::as_str).collect();
    let (account, credentials) = builder(&settings.http)?
        .create(
            &NewAccount {
                contact: &contacts,
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            settings.directory.clone(),
            None,
        )
        .await
        .map_err(acme("alta de la cuenta ACME"))?;
    let stored = StoredAccount {
        directory: settings.directory.clone(),
        credentials,
    };
    let text = serde_json::to_vec_pretty(&stored)
        .map_err(|err| AcmeError(format!("cuenta ACME: {err}")))?;
    replace_file(&path, &text, true).map_err(|err| AcmeError(err.to_string()))?;
    info!(directory = %settings.directory, "cuenta ACME creada");
    Ok(account)
}

/// Pide un certificado para el dominio y lo guarda en disco (no lo instala).
pub async fn obtain(
    settings: &AcmeSettings,
    resolver: &CertResolver,
    http01: &Http01Store,
) -> Result<TlsMaterial, AcmeError> {
    let account = account(settings).await?;
    let identifiers = [Identifier::Dns(settings.domain.clone())];
    let mut order = account
        .new_order(&NewOrder::new(&identifiers))
        .await
        .map_err(acme("pedido ACME"))?;
    let kind = match settings.challenge {
        AcmeChallenge::TlsAlpn01 => ChallengeType::TlsAlpn01,
        AcmeChallenge::Http01 => ChallengeType::Http01,
    };
    let result = async {
        let mut authorizations = order.authorizations();
        while let Some(authorization) = authorizations.next().await {
            let mut authorization = authorization.map_err(acme("autorización ACME"))?;
            match authorization.status {
                AuthorizationStatus::Valid => continue,
                AuthorizationStatus::Pending => {}
                other => {
                    return Err(AcmeError(format!(
                        "la autorización ACME está en estado {other:?}"
                    )));
                }
            }
            let mut challenge = authorization.challenge(kind.clone()).ok_or_else(|| {
                AcmeError(format!(
                    "la autoridad ACME no ofrece el reto {}",
                    settings.challenge
                ))
            })?;
            let key_authorization = challenge.key_authorization();
            match settings.challenge {
                AcmeChallenge::TlsAlpn01 => {
                    let key = challenge_certificate(
                        &settings.domain,
                        key_authorization.digest().as_ref(),
                    )
                    .map_err(|err| AcmeError(err.to_string()))?;
                    resolver.set_challenge(&settings.domain, key);
                }
                AcmeChallenge::Http01 => {
                    http01.set(&challenge.token, key_authorization.as_str());
                }
            }
            challenge
                .set_ready()
                .await
                .map_err(acme("aviso del reto ACME"))?;
        }
        let retries = RetryPolicy::new()
            .initial_delay(Duration::from_millis(250))
            .timeout(Duration::from_secs(120));
        let status = order
            .poll_ready(&retries)
            .await
            .map_err(acme("validación ACME"))?;
        if status != OrderStatus::Ready {
            return Err(AcmeError(format!(
                "la autoridad ACME no validó el dominio (estado {status:?}); revisa que \
                 {} apunta a este servidor y que el puerto del reto está abierto",
                settings.domain
            )));
        }
        let key = rcgen::KeyPair::generate()
            .map_err(|err| AcmeError(format!("clave del certificado: {err}")))?;
        let mut params = rcgen::CertificateParams::new(vec![settings.domain.clone()])
            .map_err(|err| AcmeError(format!("CSR: {err}")))?;
        params.distinguished_name = rcgen::DistinguishedName::new();
        let csr = params
            .serialize_request(&key)
            .map_err(|err| AcmeError(format!("CSR: {err}")))?;
        order
            .finalize_csr(csr.der())
            .await
            .map_err(acme("finalización ACME"))?;
        let chain = order
            .poll_certificate(&retries)
            .await
            .map_err(acme("descarga del certificado"))?;
        Ok((chain, key.serialize_pem()))
    };
    let outcome = result.await;
    resolver.clear_challenge(&settings.domain);
    http01.clear();
    let (chain, key_pem) = outcome?;

    let certs = CertificateDer::pem_slice_iter(chain.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| AcmeError(format!("cadena de certificados no válida: {err}")))?;
    let leaf = certs
        .first()
        .ok_or_else(|| AcmeError("la autoridad ACME no devolvió certificados".to_owned()))?;
    let material = TlsMaterial {
        fingerprint: sha256_fingerprint(leaf),
        key: PrivateKeyDer::from_pem_slice(key_pem.as_bytes())
            .map_err(|err| AcmeError(format!("clave del certificado: {err}")))?,
        certs,
    };
    save(&settings.dir, &chain, &key_pem)?;
    Ok(material)
}

fn save(dir: &Path, chain: &str, key_pem: &str) -> Result<(), AcmeError> {
    // Primero la clave: si se corta entre las dos, al arrancar se lee un par incoherente y
    // `rustls` lo rechaza, pero no se pierde la clave del certificado nuevo.
    replace_file(&dir.join(ACME_KEY_FILE), key_pem.as_bytes(), true)
        .and_then(|()| replace_file(&dir.join(ACME_CERT_FILE), chain.as_bytes(), false))
        .map_err(|err| AcmeError(err.to_string()))
}

/// Cuánto esperar hasta la siguiente renovación con la validez `validity` en `now`
/// (`Duration::ZERO` = ya). Sin certificado emitido (`None`), ya.
pub fn next_renewal(validity: Option<(i64, i64)>, now: i64) -> Duration {
    let Some((not_before, not_after)) = validity else {
        return Duration::ZERO;
    };
    let lifetime = (not_after - not_before).max(0);
    let before = i64::try_from(RENEW_BEFORE.as_secs())
        .unwrap_or(i64::MAX)
        .min(lifetime / 3);
    let at = not_after - before;
    Duration::from_secs(u64::try_from(at - now).unwrap_or(0))
}

/// Espera exponencial entre reintentos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backoff {
    next: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            next: BACKOFF_START,
        }
    }
}

impl Backoff {
    /// La espera de este fallo (y dobla la del siguiente, hasta [`BACKOFF_MAX`]).
    pub fn fail(&mut self) -> Duration {
        let wait = self.next;
        self.next = (self.next * 2).min(BACKOFF_MAX);
        wait
    }

    /// Tras un éxito, vuelve a empezar.
    pub fn reset(&mut self) {
        self.next = BACKOFF_START;
    }
}

/// Lo que hace la tarea de renovación en cada vuelta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Esperar este tiempo y volver a mirar.
    Wait(Duration),
    /// Pedir el certificado ahora.
    Renew,
}

/// Siguiente paso: renovar si toca, o esperar (como mucho [`CHECK_PERIOD`]).
pub fn step(validity: Option<(i64, i64)>, issued: bool, now: i64) -> Step {
    let wait = if issued {
        next_renewal(validity, now)
    } else {
        Duration::ZERO
    };
    if wait.is_zero() {
        Step::Renew
    } else {
        Step::Wait(wait.min(CHECK_PERIOD))
    }
}

/// Renueva el certificado cuando toca y lo instala en `resolver`, hasta que `stop` cambie.
pub async fn run(
    settings: AcmeSettings,
    resolver: Arc<CertResolver>,
    http01: Arc<Http01Store>,
    mut stop: watch::Receiver<bool>,
) {
    let mut backoff = Backoff::default();
    let mut issued = settings.has_certificate();
    loop {
        let wait = match step(resolver.validity(), issued, clock::now()) {
            Step::Wait(wait) => wait,
            Step::Renew => {
                info!(domain = %settings.domain, directory = %settings.directory, "pidiendo certificado ACME");
                match obtain(&settings, &resolver, &http01).await {
                    Ok(material) => match resolver.replace(&material) {
                        Ok(()) => {
                            issued = true;
                            backoff.reset();
                            info!(fingerprint = %material.fingerprint, "certificado ACME instalado");
                            continue;
                        }
                        Err(err) => {
                            warn!(error = %err, "certificado ACME no válido");
                            backoff.fail()
                        }
                    },
                    Err(err) => {
                        let wait = backoff.fail();
                        warn!(
                            error = %err,
                            retry_in_s = wait.as_secs(),
                            "no se pudo obtener el certificado ACME; se reintenta"
                        );
                        wait
                    }
                }
            }
        };
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            _ = stop.changed() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    #[test]
    fn renewal_is_thirty_days_before_or_a_third_of_short_lifetimes() {
        let now = 1_000 * DAY;
        // 90 días: renovar a los 60.
        assert_eq!(
            next_renewal(Some((now, now + 90 * DAY)), now),
            Duration::from_secs(60 * 86_400)
        );
        // 6 días: renovar a los 4.
        assert_eq!(
            next_renewal(Some((now, now + 6 * DAY)), now),
            Duration::from_secs(4 * 86_400)
        );
        assert_eq!(
            next_renewal(Some((now - 89 * DAY, now + DAY)), now),
            Duration::ZERO
        );
        assert_eq!(next_renewal(None, now), Duration::ZERO);
    }

    #[test]
    fn steps_follow_the_certificate_state() {
        let now = 1_000 * DAY;
        assert_eq!(step(Some((now, now + 90 * DAY)), false, now), Step::Renew);
        assert_eq!(
            step(Some((now, now + 90 * DAY)), true, now),
            Step::Wait(CHECK_PERIOD)
        );
        assert_eq!(
            step(Some((now - 80 * DAY, now + 10 * DAY)), true, now),
            Step::Renew
        );
        assert_eq!(
            step(Some((now - 59 * DAY, now + 31 * DAY)), true, now),
            Step::Wait(Duration::from_secs(86_400).min(CHECK_PERIOD))
        );
    }

    #[test]
    fn backoff_doubles_up_to_six_hours_and_resets() {
        let mut backoff = Backoff::default();
        let waits: Vec<u64> = (0..12).map(|_| backoff.fail().as_secs()).collect();
        assert_eq!(&waits[..4], &[60, 120, 240, 480]);
        assert_eq!(*waits.last().expect("espera"), 6 * 3600);
        backoff.reset();
        assert_eq!(backoff.fail(), BACKOFF_START);
    }

    #[test]
    fn http01_tokens_are_served_until_cleared() {
        let store = Http01Store::default();
        store.set("token", "token.huella");
        assert_eq!(store.get("token").as_deref(), Some("token.huella"));
        assert_eq!(store.get("otro"), None);
        store.clear();
        assert_eq!(store.get("token"), None);
    }
}
