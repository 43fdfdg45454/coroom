//! Certificado TLS del servidor: autofirmado persistido, ficheros PEM o ACME, servido por un
//! resolutor propio ([`CertResolver`]) que comparten QUIC y TCP.
//!
//! El resolutor guarda el certificado en uso tras un `RwLock` (se cambia en caliente al renovar
//! por ACME) y los certificados temporales del reto **TLS-ALPN-01** (RFC 8737): si el
//! `ClientHello` ofrece el ALPN `acme-tls/1`, entrega el certificado del reto del SNI pedido (con
//! la extensión crítica `acmeIdentifier`, OID 1.3.6.1.5.5.7.1.31, con el SHA-256 de la
//! autorización) o nada; el resto del tráfico recibe el certificado normal. Solo la configuración
//! TCP anuncia `acme-tls/1`.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use rustls::crypto::aws_lc_rs;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;

use coroom_transport::crypto::sha256_fingerprint;
use coroom_transport::proto::Alpn;

use crate::ServerError;
use crate::config::{Config, TlsMode};

/// Nombre del certificado autofirmado dentro de `COROOM_DATA_DIR`.
pub const SELF_SIGNED_CERT_FILE: &str = "server.crt";

/// Nombre de la clave del certificado autofirmado dentro de `COROOM_DATA_DIR`.
pub const SELF_SIGNED_KEY_FILE: &str = "server.key";

/// Subdirectorio de `COROOM_DATA_DIR` con la cuenta y los certificados ACME.
pub const ACME_DIR: &str = "acme";

/// Certificado ACME (cadena completa) dentro de [`ACME_DIR`].
pub const ACME_CERT_FILE: &str = "cert.pem";

/// Clave del certificado ACME dentro de [`ACME_DIR`].
pub const ACME_KEY_FILE: &str = "key.pem";

/// ALPN del reto TLS-ALPN-01 (RFC 8737).
pub const ACME_TLS_ALPN: &[u8] = b"acme-tls/1";

/// Certificado y clave listos para TLS.
#[derive(Debug)]
pub struct TlsMaterial {
    /// Cadena de certificados, empezando por el del servidor.
    pub certs: Vec<CertificateDer<'static>>,
    /// Clave privada del certificado del servidor.
    pub key: PrivateKeyDer<'static>,
    /// SHA-256 del DER del certificado del servidor, en hexadecimal.
    pub fingerprint: String,
}

impl TlsMaterial {
    /// Obtiene el certificado según `config.tls_mode`. En modo `acme` es el último emitido o,
    /// si todavía no hay ninguno, uno autofirmado temporal (en memoria) hasta que llegue.
    pub fn load(config: &Config) -> Result<Self, ServerError> {
        match config.tls_mode {
            TlsMode::SelfSigned => {
                load_or_generate_self_signed(&config.data_dir, &config.public_host)
            }
            TlsMode::Files => {
                let cert = config
                    .tls_cert
                    .as_deref()
                    .ok_or(ServerError::MissingSetting("COROOM_TLS_CERT"))?;
                let key = config
                    .tls_key
                    .as_deref()
                    .ok_or(ServerError::MissingSetting("COROOM_TLS_KEY"))?;
                read_pem(cert, key)
            }
            TlsMode::Acme => {
                let dir = config.data_dir.join(ACME_DIR);
                let (cert, key) = (dir.join(ACME_CERT_FILE), dir.join(ACME_KEY_FILE));
                if cert.is_file() && key.is_file() {
                    read_pem(&cert, &key)
                } else {
                    generate_self_signed(&config.public_host)
                }
            }
        }
    }

    /// Clave lista para `rustls`.
    pub fn certified_key(&self) -> Result<Arc<CertifiedKey>, ServerError> {
        let key = aws_lc_rs::sign::any_supported_type(&self.key)
            .map_err(|err| ServerError::Tls(err.to_string()))?;
        Ok(Arc::new(CertifiedKey::new(self.certs.clone(), key)))
    }

    /// Validez (`notBefore`, `notAfter`) del certificado del servidor, en segundos Unix.
    pub fn validity(&self) -> Option<(i64, i64)> {
        certificate_validity(self.certs.first()?.as_ref())
    }

    /// Configuración `rustls` para QUIC con un resolutor fijo (tests y herramientas).
    pub fn quic_server_config(&self) -> Result<rustls::ServerConfig, ServerError> {
        quic_server_config(Arc::new(CertResolver::new(self)?))
    }
}

/// Certificado en uso y certificados del reto TLS-ALPN-01.
#[derive(Debug)]
pub struct CertResolver {
    current: RwLock<Arc<CertifiedKey>>,
    fingerprint: RwLock<String>,
    validity: RwLock<Option<(i64, i64)>>,
    challenges: RwLock<HashMap<String, Arc<CertifiedKey>>>,
}

impl CertResolver {
    /// Resolutor que empieza con `material`.
    pub fn new(material: &TlsMaterial) -> Result<Self, ServerError> {
        Ok(Self {
            current: RwLock::new(material.certified_key()?),
            fingerprint: RwLock::new(material.fingerprint.clone()),
            validity: RwLock::new(material.validity()),
            challenges: RwLock::new(HashMap::new()),
        })
    }

    /// Cambia el certificado en caliente (las conexiones nuevas reciben el nuevo).
    pub fn replace(&self, material: &TlsMaterial) -> Result<(), ServerError> {
        let key = material.certified_key()?;
        if let Ok(mut current) = self.current.write() {
            *current = key;
        }
        if let Ok(mut fingerprint) = self.fingerprint.write() {
            fingerprint.clone_from(&material.fingerprint);
        }
        if let Ok(mut validity) = self.validity.write() {
            *validity = material.validity();
        }
        Ok(())
    }

    /// Huella SHA-256 del certificado en uso.
    pub fn fingerprint(&self) -> String {
        self.fingerprint
            .read()
            .map(|fingerprint| fingerprint.clone())
            .unwrap_or_default()
    }

    /// Validez del certificado en uso.
    pub fn validity(&self) -> Option<(i64, i64)> {
        self.validity.read().ok().and_then(|validity| *validity)
    }

    /// Instala el certificado del reto TLS-ALPN-01 de `domain`.
    pub fn set_challenge(&self, domain: &str, key: Arc<CertifiedKey>) {
        if let Ok(mut challenges) = self.challenges.write() {
            challenges.insert(domain.to_ascii_lowercase(), key);
        }
    }

    /// Retira el certificado del reto de `domain`.
    pub fn clear_challenge(&self, domain: &str) {
        if let Ok(mut challenges) = self.challenges.write() {
            challenges.remove(&domain.to_ascii_lowercase());
        }
    }
}

impl ResolvesServerCert for CertResolver {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let acme = client_hello
            .alpn()
            .is_some_and(|mut protocols| protocols.any(|protocol| protocol == ACME_TLS_ALPN));
        if acme {
            let name = client_hello.server_name()?.to_ascii_lowercase();
            return self.challenges.read().ok()?.get(&name).cloned();
        }
        self.current.read().ok().map(|key| Arc::clone(&key))
    }
}

/// Certificado temporal del reto TLS-ALPN-01 para `domain` con el SHA-256 de la autorización.
pub fn challenge_certificate(
    domain: &str,
    digest: &[u8],
) -> Result<Arc<CertifiedKey>, ServerError> {
    let tls = |err: rcgen::Error| ServerError::Tls(format!("certificado del reto ACME: {err}"));
    let mut params = rcgen::CertificateParams::new(vec![domain.to_owned()]).map_err(tls)?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params.custom_extensions = vec![rcgen::CustomExtension::new_acme_identifier(digest)];
    let key = rcgen::KeyPair::generate().map_err(tls)?;
    let cert = params.self_signed(&key).map_err(tls)?;
    let material = TlsMaterial {
        fingerprint: sha256_fingerprint(cert.der()),
        certs: vec![cert.der().clone()],
        key: PrivateKeyDer::try_from(key.serialize_der())
            .map_err(|err| ServerError::Tls(err.to_owned()))?,
    };
    material.certified_key()
}

/// Configuración `rustls` para QUIC: solo TLS 1.3, ALPN `h3` y `coroom-relay/1` (el socket
/// demultiplexa por el ALPN negociado) y el proveedor `aws-lc-rs` (con el intercambio híbrido
/// poscuántico X25519MLKEM768 por delante).
pub fn quic_server_config(
    resolver: Arc<CertResolver>,
) -> Result<rustls::ServerConfig, ServerError> {
    let provider = Arc::new(aws_lc_rs::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|err| ServerError::Tls(err.to_string()))?
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    config.alpn_protocols = vec![
        Alpn::H3.as_bytes().to_vec(),
        Alpn::Relay.as_bytes().to_vec(),
    ];
    Ok(config)
}

/// Configuración `rustls` para TCP: TLS 1.3 y 1.2, ALPN `h2`, `http/1.1` y `acme-tls/1`.
pub fn tcp_server_config(resolver: Arc<CertResolver>) -> Result<rustls::ServerConfig, ServerError> {
    let provider = Arc::new(aws_lc_rs::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|err| ServerError::Tls(err.to_string()))?
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec(), ACME_TLS_ALPN.to_vec()];
    Ok(config)
}

/// Certificado autofirmado para `public_host` (y `localhost`) con sus PEM.
fn generate_self_signed_pem(
    public_host: &str,
) -> Result<(TlsMaterial, String, String), ServerError> {
    let mut names = vec![public_host.to_owned()];
    if public_host != "localhost" {
        names.push("localhost".to_owned());
    }
    let generated = rcgen::generate_simple_self_signed(names)
        .map_err(|err| ServerError::Tls(format!("no se pudo generar el certificado: {err}")))?;
    let cert = generated.cert.der().clone();
    let key = PrivateKeyDer::try_from(generated.signing_key.serialize_der())
        .map_err(|err| ServerError::Tls(err.to_owned()))?;
    let material = TlsMaterial {
        fingerprint: sha256_fingerprint(&cert),
        certs: vec![cert],
        key,
    };
    Ok((
        material,
        generated.cert.pem(),
        generated.signing_key.serialize_pem(),
    ))
}

fn generate_self_signed(public_host: &str) -> Result<TlsMaterial, ServerError> {
    generate_self_signed_pem(public_host).map(|(material, _, _)| material)
}

/// Lee el certificado autofirmado de `data_dir` o lo genera si no existe.
fn load_or_generate_self_signed(
    data_dir: &Path,
    public_host: &str,
) -> Result<TlsMaterial, ServerError> {
    let cert_path = data_dir.join(SELF_SIGNED_CERT_FILE);
    let key_path = data_dir.join(SELF_SIGNED_KEY_FILE);
    match (cert_path.exists(), key_path.exists()) {
        (true, true) => return read_pem(&cert_path, &key_path),
        (false, false) => {}
        // No se sobrescribe nada a medias: el operador decide qué hacer con el fichero suelto.
        (true, false) => return Err(ServerError::IncompleteSelfSigned(key_path)),
        (false, true) => return Err(ServerError::IncompleteSelfSigned(cert_path)),
    }

    let (material, cert_pem, key_pem) = generate_self_signed_pem(public_host)?;
    fs::create_dir_all(data_dir).map_err(|err| ServerError::io("crear", data_dir, err))?;
    write_file(&cert_path, cert_pem.as_bytes(), false)?;
    write_file(&key_path, key_pem.as_bytes(), true)?;
    Ok(material)
}

/// Lee un certificado (cadena) y su clave PEM.
pub fn read_pem(cert_path: &Path, key_path: &Path) -> Result<TlsMaterial, ServerError> {
    let certs = CertificateDer::pem_file_iter(cert_path)
        .and_then(|iter| iter.collect::<Result<Vec<_>, _>>())
        .map_err(|err| ServerError::Pem(cert_path.to_owned(), err.to_string()))?;
    let leaf = certs
        .first()
        .ok_or_else(|| ServerError::Pem(cert_path.to_owned(), "sin certificados".to_owned()))?;
    let fingerprint = sha256_fingerprint(leaf);
    let key = PrivateKeyDer::from_pem_file(key_path)
        .map_err(|err| ServerError::Pem(key_path.to_owned(), err.to_string()))?;
    Ok(TlsMaterial {
        certs,
        key,
        fingerprint,
    })
}

/// Escribe un fichero nuevo; si es secreto, solo legible por el propietario en Unix.
fn write_file(path: &Path, contents: &[u8], secret: bool) -> Result<(), ServerError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if secret {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = secret;
    let mut file = options
        .open(path)
        .map_err(|err| ServerError::io("crear", path, err))?;
    file.write_all(contents)
        .and_then(|()| file.sync_all())
        .map_err(|err| ServerError::io("escribir", path, err))
}

/// Sustituye un fichero de forma atómica (temporal y renombrado); `0600` si es secreto.
pub fn replace_file(path: &Path, contents: &[u8], secret: bool) -> Result<(), ServerError> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|err| ServerError::io("crear", dir, err))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let _ = fs::remove_file(&tmp);
    write_file(&tmp, contents, secret)?;
    fs::rename(&tmp, path).map_err(|err| ServerError::io("renombrar", path, err))
}

/// Ruta del certificado autofirmado dentro de `data_dir` (la usan los clientes de prueba).
pub fn self_signed_cert_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SELF_SIGNED_CERT_FILE)
}

// --- Validez de un certificado X.509 (DER mínimo) -------------------------------------------------

/// Lee un elemento DER: `(etiqueta, contenido, resto)`.
fn der_element(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 || rest.len() < count {
            return None;
        }
        let len = rest[..count]
            .iter()
            .fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
        (len, &rest[count..])
    };
    if rest.len() < len {
        return None;
    }
    Some((tag, &rest[..len], &rest[len..]))
}

/// `UTCTime` (0x17) o `GeneralizedTime` (0x18) en segundos Unix.
fn der_time(tag: u8, value: &[u8]) -> Option<i64> {
    let text = std::str::from_utf8(value).ok()?.strip_suffix('Z')?;
    let (year, rest) = match tag {
        0x17 => {
            let yy: i64 = text.get(0..2)?.parse().ok()?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, text.get(2..)?)
        }
        0x18 => (text.get(0..4)?.parse().ok()?, text.get(4..)?),
        _ => return None,
    };
    let field = |range: std::ops::Range<usize>| -> Option<u32> { rest.get(range)?.parse().ok() };
    let (month, day, hour, minute, second) = (
        field(0..2)?,
        field(2..4)?,
        field(4..6)?,
        field(6..8)?,
        field(8..10)?,
    );
    let days = crate::clock::days_from_civil(year, month, day);
    Some(days * crate::clock::DAY + i64::from(hour * 3600 + minute * 60 + second))
}

/// `(notBefore, notAfter)` de un certificado X.509 en DER, en segundos Unix.
pub fn certificate_validity(der: &[u8]) -> Option<(i64, i64)> {
    let (_, certificate, _) = der_element(der)?;
    let (_, tbs, _) = der_element(certificate)?;
    let mut rest = tbs;
    // [0] version (opcional), serialNumber, signature, issuer, validity.
    let (tag, _, after) = der_element(rest)?;
    if tag == 0xa0 {
        rest = after;
    }
    for _ in 0..3 {
        let (_, _, after) = der_element(rest)?;
        rest = after;
    }
    let (_, validity, _) = der_element(rest)?;
    let (tag, not_before, after) = der_element(validity)?;
    let not_before = der_time(tag, not_before)?;
    let (tag, not_after, _) = der_element(after)?;
    Some((not_before, der_time(tag, not_after)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(data_dir: &Path) -> Config {
        let any = "[::1]:0".parse().expect("dirección");
        Config::self_signed(any, any, data_dir.to_owned())
    }

    #[test]
    fn self_signed_certificate_is_persisted_and_reused() {
        let dir = tempfile::tempdir().expect("directorio temporal");
        let first = TlsMaterial::load(&config(dir.path())).expect("primer arranque");
        assert!(dir.path().join(SELF_SIGNED_CERT_FILE).is_file());
        assert!(dir.path().join(SELF_SIGNED_KEY_FILE).is_file());
        let second = TlsMaterial::load(&config(dir.path())).expect("segundo arranque");
        assert_eq!(first.fingerprint, second.fingerprint);
        assert_eq!(first.fingerprint.len(), 64);
        first.quic_server_config().expect("configuración rustls");
    }

    #[test]
    fn half_written_self_signed_material_is_rejected() {
        let dir = tempfile::tempdir().expect("directorio temporal");
        fs::write(dir.path().join(SELF_SIGNED_CERT_FILE), b"").expect("fichero");
        let err = TlsMaterial::load(&config(dir.path())).expect_err("debe fallar");
        assert!(matches!(err, ServerError::IncompleteSelfSigned(_)));
    }

    #[test]
    fn acme_starts_with_a_temporary_certificate() {
        let dir = tempfile::tempdir().expect("directorio temporal");
        let mut config = config(dir.path());
        config.tls_mode = TlsMode::Acme;
        let material = TlsMaterial::load(&config).expect("temporal");
        assert!(!dir.path().join(SELF_SIGNED_CERT_FILE).exists());
        assert!(material.certified_key().is_ok());
    }

    #[test]
    fn files_mode_requires_both_paths() {
        let dir = tempfile::tempdir().expect("directorio temporal");
        let mut config = config(dir.path());
        config.tls_mode = TlsMode::Files;
        let err = TlsMaterial::load(&config).expect_err("debe fallar");
        assert!(matches!(
            err,
            ServerError::MissingSetting("COROOM_TLS_CERT")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn private_key_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("directorio temporal");
        TlsMaterial::load(&config(dir.path())).expect("certificado");
        let mode = fs::metadata(dir.path().join(SELF_SIGNED_KEY_FILE))
            .expect("metadatos")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn replaced_pem_files_read_back() {
        let dir = tempfile::tempdir().expect("directorio temporal");
        let (material, cert_pem, key_pem) =
            generate_self_signed_pem("coroom.example.com").expect("certificado");
        let (cert, key) = (dir.path().join("c.pem"), dir.path().join("k.pem"));
        replace_file(&cert, cert_pem.as_bytes(), false).expect("cert");
        replace_file(&key, key_pem.as_bytes(), true).expect("clave");
        replace_file(&key, key_pem.as_bytes(), true).expect("sustituir");
        let read = read_pem(&cert, &key).expect("lectura");
        assert_eq!(read.fingerprint, material.fingerprint);
        assert_eq!(read.key.secret_der(), material.key.secret_der());
    }

    /// Verificador de cliente que acepta todo y guarda el certificado del servidor.
    #[derive(Debug, Default)]
    struct Capture(std::sync::Mutex<Option<Vec<u8>>>);

    impl rustls::client::danger::ServerCertVerifier for Capture {
        fn verify_server_cert(
            &self,
            end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &rustls::pki_types::ServerName<'_>,
            _ocsp: &[u8],
            _now: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            if let Ok(mut cert) = self.0.lock() {
                *cert = Some(end_entity.as_ref().to_vec());
            }
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            aws_lc_rs::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }

    /// Handshake en memoria con `server`; el certificado presentado si se completa.
    fn handshake(server: &Arc<rustls::ServerConfig>, name: &str, alpn: &[u8]) -> Option<Vec<u8>> {
        let capture = Arc::new(Capture::default());
        let mut config =
            rustls::ClientConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
                .with_safe_default_protocol_versions()
                .ok()?
                .dangerous()
                .with_custom_certificate_verifier(Arc::clone(&capture) as Arc<_>)
                .with_no_client_auth();
        config.alpn_protocols = vec![alpn.to_vec()];
        let name = rustls::pki_types::ServerName::try_from(name.to_owned()).ok()?;
        let mut client = rustls::ClientConnection::new(Arc::new(config), name).ok()?;
        let mut server = rustls::ServerConnection::new(Arc::clone(server)).ok()?;
        for _ in 0..8 {
            let mut buffer = Vec::new();
            client.write_tls(&mut buffer).ok()?;
            server.read_tls(&mut buffer.as_slice()).ok()?;
            server.process_new_packets().ok()?;
            buffer.clear();
            server.write_tls(&mut buffer).ok()?;
            client.read_tls(&mut buffer.as_slice()).ok()?;
            client.process_new_packets().ok()?;
            if !client.is_handshaking() && !server.is_handshaking() {
                assert_eq!(client.alpn_protocol(), Some(alpn));
                return capture.0.lock().ok()?.clone();
            }
        }
        None
    }

    #[test]
    fn the_tls_alpn_01_certificate_is_served_only_for_its_alpn_and_name() {
        let domain = "coroom.example.com";
        let material = generate_self_signed(domain).expect("certificado");
        let resolver = Arc::new(CertResolver::new(&material).expect("resolutor"));
        let config = Arc::new(tcp_server_config(Arc::clone(&resolver)).expect("TCP"));
        let digest = [7u8; 32];
        resolver.set_challenge(
            "Coroom.Example.COM",
            challenge_certificate(domain, &digest).expect("reto"),
        );

        // Con `acme-tls/1` y el nombre del reto: el certificado temporal con la extensión
        // `acmeIdentifier` crítica (OID 1.3.6.1.5.5.7.1.31) y el SHA-256 de la autorización.
        let cert = handshake(&config, domain, ACME_TLS_ALPN).expect("reto servido");
        let mut extension = vec![
            0x06, 0x08, 0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x01, 0x1f, 0x01, 0x01, 0xff, 0x04,
            0x22, 0x04, 0x20,
        ];
        extension.extend_from_slice(&digest);
        assert!(
            cert.windows(extension.len())
                .any(|w| w == extension.as_slice())
        );
        assert_ne!(sha256_fingerprint(&cert), material.fingerprint);

        // Sin ese ALPN, el certificado normal; con otro nombre o sin reto, nada.
        let normal = handshake(&config, domain, b"h2").expect("normal");
        assert_eq!(sha256_fingerprint(&normal), material.fingerprint);
        assert!(handshake(&config, "otro.example.com", ACME_TLS_ALPN).is_none());
        resolver.clear_challenge(domain);
        assert!(handshake(&config, domain, ACME_TLS_ALPN).is_none());

        // Cambio en caliente.
        let other = generate_self_signed(domain).expect("otro");
        resolver.replace(&other).expect("sustituir");
        let swapped = handshake(&config, domain, b"http/1.1").expect("nuevo");
        assert_eq!(sha256_fingerprint(&swapped), other.fingerprint);
        assert_eq!(resolver.fingerprint(), other.fingerprint);
    }

    #[test]
    fn certificate_validity_is_read_from_the_der() {
        let mut params =
            rcgen::CertificateParams::new(vec!["coroom.example.com".to_owned()]).expect("params");
        params.not_before = rcgen::date_time_ymd(2026, 1, 2);
        params.not_after = rcgen::date_time_ymd(2051, 3, 4);
        let key = rcgen::KeyPair::generate().expect("clave");
        let cert = params.self_signed(&key).expect("certificado");
        let (not_before, not_after) = certificate_validity(cert.der()).expect("validez");
        assert_eq!(crate::clock::rfc3339(not_before), "2026-01-02T00:00:00Z");
        assert_eq!(crate::clock::rfc3339(not_after), "2051-03-04T00:00:00Z");
        assert_eq!(certificate_validity(b"\x30\x03\x02\x01"), None);
    }
}
