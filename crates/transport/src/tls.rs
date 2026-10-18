//! TLS 1.3 de las conexiones QUIC: claves públicas crudas entre pares y confianza en el servidor.
//!
//! **Entre pares** se usan **claves públicas crudas** (RFC 7250), como hace iroh con `rustls`: el
//! "certificado" de cada extremo es la SPKI Ed25519 de su [`Identity`] y la firma del handshake se
//! hace con esa misma clave (importada desde su PKCS#8 con `aws-lc-rs`). Los verificadores propios
//! exigen el tipo de certificado `RawPublicKey` en los dos sentidos:
//!
//! - el cliente comprueba que la clave cruda del servidor es **exactamente** la identidad esperada
//!   (`PeerServerVerifier`);
//! - el servidor exige certificado de cliente y comprueba que su clave está en la lista de pares
//!   admitidos de la sala (`AdmittedClientVerifier`).
//!
//! Con `rustls` 0.23 + `quinn` 0.11 esto funciona de extremo a extremo, así que no hace falta el
//! respaldo X.509 autofirmado previsto.
//!
//! **Hacia el servidor** de coroom (señalización por HTTP/3 y relay) el certificado es X.509: o se
//! fija su huella SHA-256 ([`ServerTrust::Fingerprint`], desarrollo con certificado autofirmado) o se
//! valida contra raíces de confianza ([`ServerTrust::Roots`]). El cliente no presenta certificado:
//! el servidor autentica por token.
//!
//! En los dos casos: solo TLS 1.3 y el proveedor `aws-lc-rs` con el intercambio híbrido poscuántico
//! `X25519MLKEM768` por delante (`rustls` con `prefer-post-quantum`).

use std::collections::HashSet;
use std::fmt;
use std::sync::{Arc, RwLock};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms, aws_lc_rs};
use rustls::pki_types::{
    CertificateDer, PrivatePkcs8KeyDer, ServerName, SubjectPublicKeyInfoDer, UnixTime, alg_id,
};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::sign::CertifiedKey;
use rustls::{
    CertificateError, DigitallySignedStruct, DistinguishedName, RootCertStore, SignatureScheme,
};

use coroom_crypto::{Identity, PUBLIC_KEY_LENGTH, sha256_fingerprint};

use crate::TransportError;

/// Clave pública Ed25519 de una identidad.
pub type PublicKey = [u8; PUBLIC_KEY_LENGTH];

/// Nombre de servidor sintético de las conexiones entre pares. Con claves crudas no se valida
/// ningún nombre y el SNI va desactivado, así que nunca sale en el `ClientHello`.
pub(crate) const PEER_SERVER_NAME: &str = "peer.coroom.invalid";

/// Prefijo DER de una SPKI Ed25519 (RFC 8410): `SEQUENCE { SEQUENCE { OID 1.3.101.112 } BIT STRING }`.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// Proveedor criptográfico de todas las conexiones: `aws-lc-rs` con X25519MLKEM768 preferido y las
/// suites en el orden que mejor cifra esta CPU ([`provider_for`]).
pub fn provider() -> Arc<CryptoProvider> {
    provider_for(aes_gcm_accelerated())
}

/// Proveedor para una CPU que acelera AES-GCM o no. Sin aceleración, ChaCha20-Poly1305 (RFC 8439)
/// va primero: en software cifra varias veces más rápido que AES-GCM y no depende de tablas, así
/// que tampoco filtra tiempos. Con aceleración se queda el orden de `rustls` (AES-GCM primero).
pub fn provider_for(aes_gcm_accelerated: bool) -> Arc<CryptoProvider> {
    let mut provider = aws_lc_rs::default_provider();
    if !aes_gcm_accelerated {
        provider
            .cipher_suites
            .sort_by_key(|suite| !is_chacha20(suite.suite()));
    }
    Arc::new(provider)
}

/// Si la suite cifra con ChaCha20-Poly1305 (TLS 1.3 o 1.2).
fn is_chacha20(suite: rustls::CipherSuite) -> bool {
    matches!(
        suite,
        rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256
            | rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256
            | rustls::CipherSuite::TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256
    )
}

/// Si la CPU cifra AES-GCM por hardware: instrucciones AES y multiplicación sin acarreo (GHASH).
/// Las placas ARM sin extensiones criptográficas no las tienen.
pub fn aes_gcm_accelerated() -> bool {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    {
        std::arch::is_x86_feature_detected!("aes")
            && std::arch::is_x86_feature_detected!("pclmulqdq")
    }
    #[cfg(target_arch = "aarch64")]
    {
        std::arch::is_aarch64_feature_detected!("aes")
            && std::arch::is_aarch64_feature_detected!("pmull")
    }
    #[cfg(all(target_arch = "arm", target_os = "linux"))]
    {
        // `std::arch::is_arm_feature_detected!` es inestable: se leen los bits de `AT_HWCAP2`.
        const HWCAP2_AES: usize = 1 << 0;
        const HWCAP2_PMULL: usize = 1 << 1;
        let (_, hwcap2) = rustix::param::linux_hwcap();
        hwcap2 & (HWCAP2_AES | HWCAP2_PMULL) == HWCAP2_AES | HWCAP2_PMULL
    }
    #[cfg(not(any(
        target_arch = "x86_64",
        target_arch = "x86",
        target_arch = "aarch64",
        all(target_arch = "arm", target_os = "linux")
    )))]
    {
        false
    }
}

/// Clave pública Ed25519 contenida en una SPKI DER, si lo es.
pub fn ed25519_key_from_spki(spki: &[u8]) -> Option<PublicKey> {
    let rest = spki.strip_prefix(&ED25519_SPKI_PREFIX)?;
    rest.try_into().ok()
}

/// SPKI DER de una clave pública Ed25519.
pub fn spki_from_ed25519_key(key: &PublicKey) -> Vec<u8> {
    rustls::sign::public_key_to_spki(&alg_id::ED25519, key).to_vec()
}

/// Material TLS de la identidad local: la clave de firma y su SPKI como "certificado" crudo.
#[derive(Clone)]
pub(crate) struct IdentityKey {
    certified: Arc<CertifiedKey>,
    public_key: PublicKey,
}

impl fmt::Debug for IdentityKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IdentityKey")
            .field("fingerprint", &sha256_fingerprint(&self.public_key))
            .finish_non_exhaustive()
    }
}

impl IdentityKey {
    pub(crate) fn new(identity: &Identity) -> Result<Self, TransportError> {
        let mut der = identity
            .to_pkcs8_der()
            .map_err(|err| TransportError::Tls(err.to_string()))?;
        let key = aws_lc_rs::sign::any_eddsa_type(&PrivatePkcs8KeyDer::from(der.as_slice()));
        // El PKCS#8 ya vive dentro de la clave de `aws-lc-rs`; se borra la copia.
        der.fill(0);
        let key = key?;
        let public_key = identity.public_key();
        let certified = CertifiedKey::new(
            vec![CertificateDer::from(spki_from_ed25519_key(&public_key))],
            key,
        );
        Ok(Self {
            certified: Arc::new(certified),
            public_key,
        })
    }

    pub(crate) fn public_key(&self) -> PublicKey {
        self.public_key
    }
}

/// Entrega siempre la clave cruda de la identidad, como cliente y como servidor.
#[derive(Debug)]
struct RawKeyResolver(Arc<CertifiedKey>);

impl rustls::client::ResolvesClientCert for RawKeyResolver {
    fn resolve(
        &self,
        _root_hint_subjects: &[&[u8]],
        _sigschemes: &[SignatureScheme],
    ) -> Option<Arc<CertifiedKey>> {
        Some(Arc::clone(&self.0))
    }

    fn only_raw_public_keys(&self) -> bool {
        true
    }

    fn has_certs(&self) -> bool {
        true
    }
}

impl rustls::server::ResolvesServerCert for RawKeyResolver {
    fn resolve(&self, _client_hello: rustls::server::ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(Arc::clone(&self.0))
    }

    fn only_raw_public_keys(&self) -> bool {
        true
    }
}

fn signature_algorithms() -> WebPkiSupportedAlgorithms {
    aws_lc_rs::default_provider().signature_verification_algorithms
}

fn tls12_rejected() -> Result<HandshakeSignatureValid, rustls::Error> {
    Err(rustls::Error::PeerIncompatible(
        rustls::PeerIncompatible::Tls12NotOffered,
    ))
}

fn raw_key_of(
    end_entity: &CertificateDer<'_>,
    intermediates: &[CertificateDer<'_>],
) -> Result<PublicKey, rustls::Error> {
    if !intermediates.is_empty() {
        return Err(rustls::Error::InvalidCertificate(
            CertificateError::UnknownIssuer,
        ));
    }
    ed25519_key_from_spki(end_entity.as_ref()).ok_or(rustls::Error::InvalidCertificate(
        CertificateError::BadEncoding,
    ))
}

/// Verificador del cliente: la clave cruda del servidor debe ser la identidad esperada.
#[derive(Debug)]
pub(crate) struct PeerServerVerifier {
    expected: PublicKey,
    algorithms: WebPkiSupportedAlgorithms,
}

impl PeerServerVerifier {
    pub(crate) fn new(expected: PublicKey) -> Self {
        Self {
            expected,
            algorithms: signature_algorithms(),
        }
    }
}

impl ServerCertVerifier for PeerServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if raw_key_of(end_entity, intermediates)? != self.expected {
            return Err(rustls::Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        tls12_rejected()
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature_with_raw_key(
            message,
            &SubjectPublicKeyInfoDer::from(cert.as_ref()),
            dss,
            &self.algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }

    fn requires_raw_public_keys(&self) -> bool {
        true
    }
}

/// Conjunto de identidades que el `Endpoint` acepta como clientes (los pares de la sala).
#[derive(Debug, Clone, Default)]
pub(crate) struct Admitted(Arc<RwLock<HashSet<PublicKey>>>);

impl Admitted {
    pub(crate) fn insert(&self, key: PublicKey) {
        if let Ok(mut set) = self.0.write() {
            set.insert(key);
        }
    }

    pub(crate) fn remove(&self, key: &PublicKey) {
        if let Ok(mut set) = self.0.write() {
            set.remove(key);
        }
    }

    pub(crate) fn contains(&self, key: &PublicKey) -> bool {
        self.0.read().map(|set| set.contains(key)).unwrap_or(false)
    }
}

/// Verificador del servidor: exige clave cruda de cliente y que sea una identidad admitida.
#[derive(Debug)]
pub(crate) struct AdmittedClientVerifier {
    admitted: Admitted,
    algorithms: WebPkiSupportedAlgorithms,
}

impl AdmittedClientVerifier {
    pub(crate) fn new(admitted: Admitted) -> Self {
        Self {
            admitted,
            algorithms: signature_algorithms(),
        }
    }
}

impl ClientCertVerifier for AdmittedClientVerifier {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        let key = raw_key_of(end_entity, intermediates)?;
        if !self.admitted.contains(&key) {
            return Err(rustls::Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ));
        }
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        tls12_rejected()
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature_with_raw_key(
            message,
            &SubjectPublicKeyInfoDer::from(cert.as_ref()),
            dss,
            &self.algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }

    fn requires_raw_public_keys(&self) -> bool {
        true
    }
}

/// Cómo confía el cliente en el certificado X.509 del servidor de coroom.
#[derive(Debug, Clone)]
pub enum ServerTrust {
    /// Huella SHA-256 (hex, 64 caracteres) del DER del certificado: fija el servidor de
    /// desarrollo con certificado autofirmado (`--trust-fingerprint`).
    Fingerprint(String),
    /// Raíces de confianza (el certificado del operador o su autoridad).
    Roots(Arc<RootCertStore>),
    /// Solo para comprobar un servidor antes de confiar en él ("Añadir servidor"):
    /// acepta cualquier certificado y apunta su huella y si lo validan las raíces del sistema
    /// ([`CertificateInspection`]). Únicamente la usa [`crate::api::ApiClient::probe`], que lee
    /// `/version` y `/health` sin credenciales; la señalización, el relay y las peticiones con token
    /// la rechazan.
    Inspect(Arc<CertificateInspection>),
}

/// Lo que vio [`ServerTrust::Inspect`] del certificado del servidor.
#[derive(Debug, Default)]
pub struct CertificateInspection {
    seen: std::sync::Mutex<Option<InspectedCertificate>>,
}

/// Certificado visto al comprobar un servidor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedCertificate {
    /// Huella SHA-256 (hex, minúsculas) del DER del certificado.
    pub fingerprint: String,
    /// `None` si lo validan las raíces del sistema con el nombre del servidor; si no, por qué.
    pub untrusted: Option<String>,
}

impl CertificateInspection {
    /// Lo último que se vio (`None` si no hubo handshake).
    pub fn certificate(&self) -> Option<InspectedCertificate> {
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn record(&self, certificate: InspectedCertificate) {
        *self
            .seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(certificate);
    }
}

/// Verificador de [`ServerTrust::Inspect`]: comprueba la firma del handshake (el servidor tiene
/// la clave del certificado), valida la cadena con las raíces del sistema solo para informar y
/// acepta.
#[derive(Debug)]
struct InspectingVerifier {
    inspection: Arc<CertificateInspection>,
    roots: Option<Arc<dyn ServerCertVerifier>>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for InspectingVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let untrusted = match &self.roots {
            Some(roots) => roots
                .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
                .err()
                .map(|err| err.to_string()),
            None => Some("no hay raíces de confianza del sistema".to_owned()),
        };
        self.inspection.record(InspectedCertificate {
            fingerprint: sha256_fingerprint(end_entity.as_ref()),
            untrusted,
        });
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        tls12_rejected()
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// Configuración TLS de cliente de [`ServerTrust::Inspect`] con estos ALPN.
pub(crate) fn inspecting_client_config(
    inspection: &Arc<CertificateInspection>,
    alpns: &[&[u8]],
) -> Result<rustls::ClientConfig, TransportError> {
    let roots = match ServerTrust::system_roots() {
        Ok(ServerTrust::Roots(roots)) => {
            rustls::client::WebPkiServerVerifier::builder_with_provider(roots, provider())
                .build()
                .ok()
                .map(|verifier| verifier as Arc<dyn ServerCertVerifier>)
        }
        _ => None,
    };
    let mut config = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(InspectingVerifier {
            inspection: Arc::clone(inspection),
            roots,
            algorithms: signature_algorithms(),
        }))
        .with_no_client_auth();
    config.alpn_protocols = alpns.iter().map(|alpn| alpn.to_vec()).collect();
    Ok(config)
}

impl ServerTrust {
    /// Confianza de inspección ([`ServerTrust::Inspect`]) con su registro.
    pub fn inspect() -> (ServerTrust, Arc<CertificateInspection>) {
        let inspection = Arc::new(CertificateInspection::default());
        (ServerTrust::Inspect(Arc::clone(&inspection)), inspection)
    }

    /// Las raíces de confianza del sistema operativo (almacén de certificados de Windows, paquete
    /// de CA de la distribución en Linux, con `SSL_CERT_FILE` y `SSL_CERT_DIR` si están definidas),
    /// cargadas con `rustls-native-certs`. Falla si no hay ninguna utilizable.
    pub fn system_roots() -> Result<ServerTrust, TransportError> {
        let loaded = rustls_native_certs::load_native_certs();
        let mut roots = RootCertStore::empty();
        let (added, _ignored) = roots.add_parsable_certificates(loaded.certs);
        if added == 0 {
            let detail = loaded
                .errors
                .first()
                .map(|err| format!(": {err}"))
                .unwrap_or_default();
            return Err(TransportError::Tls(format!(
                "no hay raíces de confianza del sistema{detail}; usa --trust-fingerprint con un \
                 servidor autofirmado"
            )));
        }
        Ok(ServerTrust::Roots(Arc::new(roots)))
    }
}

/// Verificador por huella del certificado del servidor.
#[derive(Debug)]
struct FingerprintVerifier {
    fingerprint: String,
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // La huella fija el certificado exacto: ni el nombre ni la caducidad añaden nada.
        if sha256_fingerprint(end_entity.as_ref()) != self.fingerprint {
            return Err(rustls::Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        tls12_rejected()
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// Configuración TLS del lado servidor del `Endpoint` entre pares (ALPN `coroom-p2p/1`).
pub(crate) fn peer_server_config(
    identity: &IdentityKey,
    admitted: Admitted,
) -> Result<rustls::ServerConfig, TransportError> {
    let mut config = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(Arc::new(AdmittedClientVerifier::new(admitted)))
        .with_cert_resolver(Arc::new(RawKeyResolver(Arc::clone(&identity.certified))));
    // Sin AES por hardware el servidor impone ChaCha20 aunque el cliente prefiera AES-GCM: basta
    // con que una punta sea lenta con AES para elegir la suite que las dos cifran bien.
    config.ignore_client_order = !aes_gcm_accelerated();
    config.alpn_protocols = vec![coroom_proto::Alpn::P2p.as_bytes().to_vec()];
    Ok(config)
}

/// Configuración TLS de cliente hacia un par cuya identidad debe ser `expected`.
pub(crate) fn peer_client_config(
    identity: &IdentityKey,
    expected: PublicKey,
) -> Result<rustls::ClientConfig, TransportError> {
    let mut config = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PeerServerVerifier::new(expected)))
        .with_client_cert_resolver(Arc::new(RawKeyResolver(Arc::clone(&identity.certified))));
    config.enable_sni = false;
    config.alpn_protocols = vec![coroom_proto::Alpn::P2p.as_bytes().to_vec()];
    Ok(config)
}

/// Configuración TLS de cliente hacia el servidor de coroom con el ALPN `alpn`.
pub(crate) fn server_client_config(
    trust: &ServerTrust,
    alpn: coroom_proto::Alpn,
) -> Result<rustls::ClientConfig, TransportError> {
    server_client_config_with(trust, &[alpn.as_bytes()])
}

/// Como [`server_client_config`], con varios ALPN (la API por TCP ofrece `h2` y `http/1.1`).
pub(crate) fn server_client_config_with(
    trust: &ServerTrust,
    alpns: &[&[u8]],
) -> Result<rustls::ClientConfig, TransportError> {
    let builder = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?;
    let mut config = match trust {
        ServerTrust::Fingerprint(fingerprint) => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(FingerprintVerifier {
                fingerprint: fingerprint.to_ascii_lowercase(),
                algorithms: signature_algorithms(),
            }))
            .with_no_client_auth(),
        ServerTrust::Roots(roots) => builder
            .with_root_certificates(Arc::clone(roots))
            .with_no_client_auth(),
        ServerTrust::Inspect(_) => {
            return Err(TransportError::Tls(
                "la confianza de inspección solo sirve para comprobar un servidor".to_owned(),
            ));
        }
    };
    config.alpn_protocols = alpns.iter().map(|alpn| alpn.to_vec()).collect();
    Ok(config)
}

/// Clave pública del par a partir de la identidad TLS que entrega `quinn` (la SPKI cruda).
pub(crate) fn peer_key_of(connection: &quinn::Connection) -> Option<PublicKey> {
    let identity = connection.peer_identity()?;
    let certs = identity.downcast::<Vec<CertificateDer<'static>>>().ok()?;
    ed25519_key_from_spki(certs.first()?.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chacha20_leads_only_without_aes_hardware() {
        let first = |aes| provider_for(aes).cipher_suites[0].suite();
        assert_eq!(first(true), rustls::CipherSuite::TLS13_AES_256_GCM_SHA384);
        assert_eq!(
            first(false),
            rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256
        );
        let mut accelerated = provider_for(true).cipher_suites.clone();
        let mut software = provider_for(false).cipher_suites.clone();
        accelerated.sort_by_key(|suite| u16::from(suite.suite()));
        software.sort_by_key(|suite| u16::from(suite.suite()));
        assert_eq!(accelerated, software, "mismas suites, otro orden");
    }

    #[test]
    fn spki_round_trips_the_public_key() {
        let identity = Identity::generate().expect("identidad");
        let spki = spki_from_ed25519_key(&identity.public_key());
        assert_eq!(spki.len(), 44);
        assert_eq!(ed25519_key_from_spki(&spki), Some(identity.public_key()));
        assert_eq!(ed25519_key_from_spki(&spki[..43]), None);
        let mut other = spki.clone();
        other[6] ^= 1;
        assert_eq!(ed25519_key_from_spki(&other), None);
    }

    #[test]
    fn identity_key_builds_from_pkcs8() {
        let identity = Identity::generate().expect("identidad");
        let key = IdentityKey::new(&identity).expect("clave TLS");
        assert_eq!(key.public_key(), identity.public_key());
        assert_eq!(
            key.certified.cert[0].as_ref(),
            spki_from_ed25519_key(&identity.public_key()).as_slice()
        );
        assert!(!format!("{key:?}").contains("certified"));
    }

    #[test]
    fn admitted_set_tracks_insertions_and_removals() {
        let admitted = Admitted::default();
        let key = [7; 32];
        assert!(!admitted.contains(&key));
        admitted.insert(key);
        assert!(admitted.contains(&key));
        admitted.remove(&key);
        assert!(!admitted.contains(&key));
    }
}
