//! Identidad persistente de un par: un par de claves Ed25519 cuya huella SHA-256 lo identifica.
//!
//! La misma clave es la credencial MLS del miembro ([`crate::mls`]), la clave pública cruda del
//! certificado QUIC y, convertida a X25519, la clave HPKE del par ([`crate::hpke`]). Se guarda
//! en el llavero del sistema ([`KeyringStore`]) o, en equipos sin llavero y en los tests, en un
//! fichero con permisos `0600` ([`FileStore`]).

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// Longitud en bytes de una clave pública Ed25519.
pub const PUBLIC_KEY_LENGTH: usize = ed25519_dalek::PUBLIC_KEY_LENGTH;

/// Longitud en bytes de una firma Ed25519.
pub const SIGNATURE_LENGTH: usize = ed25519_dalek::SIGNATURE_LENGTH;

/// Error al crear, cargar o guardar una identidad.
#[derive(Debug)]
pub enum IdentityError {
    /// El generador aleatorio del sistema operativo no respondió.
    Random(getrandom::Error),
    /// El documento PKCS#8 no contiene una clave Ed25519 válida.
    Pkcs8(String),
    /// La clave pública no tiene 32 bytes o no es un punto válido.
    InvalidPublicKey,
    /// El almacén de la identidad (llavero o fichero) falló.
    Store(StoreError),
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdentityError::Random(err) => {
                write!(f, "no se pudo obtener aleatoriedad del sistema: {err}")
            }
            IdentityError::Pkcs8(err) => write!(f, "clave PKCS#8 no válida: {err}"),
            IdentityError::InvalidPublicKey => f.write_str("clave pública Ed25519 no válida"),
            IdentityError::Store(err) => write!(f, "almacén de la identidad: {err}"),
        }
    }
}

impl std::error::Error for IdentityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IdentityError::Random(err) => Some(err),
            IdentityError::Store(err) => Some(err),
            IdentityError::Pkcs8(_) | IdentityError::InvalidPublicKey => None,
        }
    }
}

impl From<StoreError> for IdentityError {
    fn from(err: StoreError) -> Self {
        IdentityError::Store(err)
    }
}

/// Comprueba una firma Ed25519 de `message` hecha por la identidad con clave pública `public_key`.
pub fn verify_signature(
    public_key: &[u8; PUBLIC_KEY_LENGTH],
    message: &[u8],
    signature: &[u8; SIGNATURE_LENGTH],
) -> Result<(), IdentityError> {
    let key = VerifyingKey::from_bytes(public_key).map_err(|_| IdentityError::InvalidPublicKey)?;
    key.verify(message, &Signature::from_bytes(signature))
        .map_err(|_| IdentityError::InvalidPublicKey)
}

/// Identidad criptográfica de un par: un par de claves Ed25519.
///
/// La clave privada nunca sale de este tipo salvo como PKCS#8 para guardarla; `Debug` solo
/// muestra la huella.
pub struct Identity {
    signing_key: SigningKey,
}

impl Identity {
    /// Genera una identidad nueva con el generador aleatorio del sistema operativo.
    pub fn generate() -> Result<Self, IdentityError> {
        let mut seed = Zeroizing::new([0u8; ed25519_dalek::SECRET_KEY_LENGTH]);
        getrandom::fill(seed.as_mut()).map_err(IdentityError::Random)?;
        // La semilla vive también dentro de `SigningKey`, que la borra al soltarse; `Zeroizing`
        // borra esta copia.
        let signing_key = SigningKey::from_bytes(&seed);
        Ok(Self { signing_key })
    }

    /// Carga la identidad de `store` o, si todavía no hay ninguna, genera una nueva y la guarda.
    pub fn load_or_create(store: &dyn IdentityStore) -> Result<Self, IdentityError> {
        if let Some(der) = store.load()? {
            return Self::from_pkcs8_der(&der);
        }
        let identity = Self::generate()?;
        let der = Zeroizing::new(identity.to_pkcs8_der()?);
        store.save(&der)?;
        Ok(identity)
    }

    /// Clave pública Ed25519 en bruto (32 bytes).
    pub fn public_key(&self) -> [u8; PUBLIC_KEY_LENGTH] {
        self.verifying_key().to_bytes()
    }

    /// Huella de la identidad: SHA-256 de la clave pública, en hexadecimal en minúsculas.
    pub fn fingerprint(&self) -> String {
        sha256_fingerprint(&self.public_key())
    }

    /// Firma `message` con la clave privada (Ed25519 puro, RFC 8032).
    pub fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_LENGTH] {
        self.signing_key.sign(message).to_bytes()
    }

    /// Clave privada como documento PKCS#8 v2 en DER (incluye la clave pública), el formato que
    /// entienden `rustls` y el llavero. El llamante debe borrar el búfer al terminar.
    pub fn to_pkcs8_der(&self) -> Result<Vec<u8>, IdentityError> {
        let document = self
            .signing_key
            .to_pkcs8_der()
            .map_err(|err| IdentityError::Pkcs8(err.to_string()))?;
        Ok(document.as_bytes().to_vec())
    }

    /// Reconstruye la identidad desde un documento PKCS#8 en DER producido por
    /// [`Identity::to_pkcs8_der`].
    pub fn from_pkcs8_der(der: &[u8]) -> Result<Self, IdentityError> {
        let signing_key =
            SigningKey::from_pkcs8_der(der).map_err(|err| IdentityError::Pkcs8(err.to_string()))?;
        Ok(Self { signing_key })
    }

    /// Clave de firma, para los módulos de la crate que firman en nombre de la identidad (MLS) o
    /// derivan de ella la clave X25519 (HPKE).
    pub(crate) fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("fingerprint", &self.fingerprint())
            .finish_non_exhaustive()
    }
}

/// SHA-256 de `data` (la versión de los avatares sale de él: `coroom_proto::api::avatar_version`).
pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// SHA-256 de `data` en hexadecimal en minúsculas (64 caracteres).
///
/// Es el formato de todas las huellas de coroom: identidades y certificados (sobre su DER).
pub fn sha256_fingerprint(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        hex.push(HEX_DIGITS[usize::from(byte >> 4)]);
        hex.push(HEX_DIGITS[usize::from(byte & 0x0f)]);
    }
    hex
}

const HEX_DIGITS: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
];

// --- Almacenes ---------------------------------------------------------------------------------

/// Error de un [`IdentityStore`].
#[derive(Debug)]
pub enum StoreError {
    /// Fallo de entrada/salida en el fichero de la identidad.
    Io {
        /// Fichero afectado.
        path: PathBuf,
        /// Error del sistema.
        source: io::Error,
    },
    /// El llavero del sistema no está disponible o rechazó la operación.
    Keyring(String),
    /// Lo guardado no es un PKCS#8 en base64.
    Encoding(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Io { path, source } => {
                write!(f, "no se pudo acceder a {}: {source}", path.display())
            }
            StoreError::Keyring(err) => write!(f, "el llavero del sistema falló: {err}"),
            StoreError::Encoding(err) => {
                write!(f, "la identidad guardada está dañada: {err}")
            }
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Io { source, .. } => Some(source),
            StoreError::Keyring(_) | StoreError::Encoding(_) => None,
        }
    }
}

/// Dónde se guarda la clave privada de la identidad (documento PKCS#8 en DER).
pub trait IdentityStore {
    /// Lee la identidad guardada; `Ok(None)` si todavía no hay ninguna.
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError>;

    /// Guarda (o sustituye) la identidad.
    fn save(&self, pkcs8_der: &[u8]) -> Result<(), StoreError>;
}

/// Identidad en el llavero del sistema (crate `keyring`): Secret Service en Linux, Credential
/// Manager en Windows. Guarda el PKCS#8 en base64 en el servicio `coroom`, usuario `identity`.
///
/// En Linux el llavero se habla por D-Bus con la API bloqueante de `zbus`, que levanta su propio
/// runtime: no llamar desde dentro de un runtime `tokio` (usar `spawn_blocking` o hacerlo antes
/// de arrancarlo).
#[derive(Debug, Clone)]
pub struct KeyringStore {
    service: String,
    user: String,
}

impl KeyringStore {
    /// Servicio del llavero.
    pub const SERVICE: &'static str = "coroom";
    /// Usuario del llavero.
    pub const USER: &'static str = "identity";

    /// Entrada por defecto (`coroom` / `identity`).
    pub fn new() -> Self {
        Self::with_entry(Self::SERVICE, Self::USER)
    }

    /// Entrada con otro servicio y usuario (perfiles de desarrollo y tests).
    pub fn with_entry(service: &str, user: &str) -> Self {
        Self {
            service: service.to_owned(),
            user: user.to_owned(),
        }
    }

    /// `true` si el llavero del sistema responde en esta sesión.
    pub fn is_available() -> bool {
        keyring::Entry::store_status().is_ok()
    }

    /// Borra la entrada (no es error si no existía).
    pub fn delete(&self) -> Result<(), StoreError> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(StoreError::Keyring(err.to_string())),
        }
    }

    fn entry(&self) -> Result<keyring::Entry, StoreError> {
        keyring::Entry::new(&self.service, &self.user)
            .map_err(|err| StoreError::Keyring(err.to_string()))
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

impl IdentityStore for KeyringStore {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError> {
        let encoded = match self.entry()?.get_password() {
            Ok(encoded) => Zeroizing::new(encoded),
            Err(keyring::Error::NoEntry) => return Ok(None),
            Err(err) => return Err(StoreError::Keyring(err.to_string())),
        };
        let der = BASE64
            .decode(encoded.trim())
            .map_err(|err| StoreError::Encoding(err.to_string()))?;
        Ok(Some(Zeroizing::new(der)))
    }

    fn save(&self, pkcs8_der: &[u8]) -> Result<(), StoreError> {
        let encoded = Zeroizing::new(BASE64.encode(pkcs8_der));
        self.entry()?
            .set_password(&encoded)
            .map_err(|err| StoreError::Keyring(err.to_string()))
    }
}

/// Identidad en el fichero `identity.p8` (PKCS#8 en DER) de un directorio, con permisos `0600`
/// en Unix. Para equipos sin llavero y para los tests.
#[derive(Debug, Clone)]
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    /// Nombre del fichero dentro del directorio.
    pub const FILE_NAME: &'static str = "identity.p8";

    /// Almacén en `dir/identity.p8`; el directorio se crea al guardar si no existe.
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self {
            path: dir.as_ref().join(Self::FILE_NAME),
        }
    }

    /// Ruta completa del fichero.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn io_error(&self, source: io::Error) -> StoreError {
        StoreError::Io {
            path: self.path.clone(),
            source,
        }
    }
}

impl IdentityStore for FileStore {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError> {
        match fs::read(&self.path) {
            Ok(der) => Ok(Some(Zeroizing::new(der))),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(self.io_error(err)),
        }
    }

    fn save(&self, pkcs8_der: &[u8]) -> Result<(), StoreError> {
        write_secret_file(&self.path, pkcs8_der)
    }
}

/// Escribe `contents` en `path` sin que el secreto quede nunca legible por otros ni a medio
/// escribir: fichero temporal creado ya con `0600` en Unix, `fsync` y renombrado. Crea el
/// directorio si no existe.
pub(crate) fn write_secret_file(path: &Path, contents: &[u8]) -> Result<(), StoreError> {
    let io_error = |source: io::Error| StoreError::Io {
        path: path.to_owned(),
        source,
    };
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(io_error)?;
    }
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp).map_err(io_error)?;
    #[cfg(unix)]
    {
        // `mode` no se aplica si el temporal ya existía.
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(io_error)?;
    }
    file.write_all(contents)
        .and_then(|()| file.sync_all())
        .map_err(io_error)?;
    drop(file);
    fs::rename(&tmp, path).map_err(io_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_identities_differ() {
        let a = Identity::generate().expect("identidad a");
        let b = Identity::generate().expect("identidad b");
        assert_ne!(a.public_key(), b.public_key());
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn fingerprint_is_stable_lowercase_hex() {
        let identity = Identity::generate().expect("identidad");
        let fingerprint = identity.fingerprint();
        assert_eq!(fingerprint.len(), 64);
        assert!(
            fingerprint
                .chars()
                .all(|c| matches!(c, '0'..='9' | 'a'..='f'))
        );
        assert_eq!(fingerprint, identity.fingerprint());
    }

    #[test]
    fn sha256_fingerprint_matches_known_vector() {
        // Vector de FIPS 180-2 para "abc".
        assert_eq!(
            sha256_fingerprint(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn pkcs8_round_trip_keeps_the_identity() {
        let identity = Identity::generate().expect("identidad");
        let der = identity.to_pkcs8_der().expect("pkcs8");
        let restored = Identity::from_pkcs8_der(&der).expect("desde pkcs8");
        assert_eq!(restored.public_key(), identity.public_key());
        assert!(Identity::from_pkcs8_der(&der[..der.len() / 2]).is_err());
    }

    #[test]
    fn signatures_verify_only_with_the_right_key_and_message() {
        let identity = Identity::generate().expect("identidad");
        let other = Identity::generate().expect("otra");
        let signature = identity.sign(b"hola");
        assert!(verify_signature(&identity.public_key(), b"hola", &signature).is_ok());
        assert!(verify_signature(&identity.public_key(), "adios".as_bytes(), &signature).is_err());
        assert!(verify_signature(&other.public_key(), b"hola", &signature).is_err());
    }

    #[test]
    fn debug_does_not_expose_the_secret_key() {
        let identity = Identity::generate().expect("identidad");
        let debug = format!("{identity:?}");
        assert!(debug.contains(&identity.fingerprint()));
        assert!(!debug.contains("signing_key"));
    }

    #[test]
    fn file_store_creates_once_and_reloads_the_same_identity() {
        let dir = tempfile::tempdir().expect("directorio temporal");
        let store = FileStore::new(dir.path().join("perfil"));
        assert!(store.load().expect("leer vacío").is_none());

        let created = Identity::load_or_create(&store).expect("crear");
        assert!(store.path().exists());
        let loaded = Identity::load_or_create(&store).expect("cargar");
        assert_eq!(created.public_key(), loaded.public_key());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.path())
                .expect("metadatos")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn file_store_rejects_a_damaged_file() {
        let dir = tempfile::tempdir().expect("directorio temporal");
        let store = FileStore::new(dir.path());
        fs::write(store.path(), b"no es pkcs8").expect("escribir");
        let err = Identity::load_or_create(&store).expect_err("debe fallar");
        assert!(matches!(err, IdentityError::Pkcs8(_)));
    }

    /// Toca el llavero real: solo con `COROOM_TEST_KEYRING=1` y un llavero disponible.
    #[test]
    fn keyring_store_round_trip_when_enabled() {
        if std::env::var_os("COROOM_TEST_KEYRING").is_none() || !KeyringStore::is_available() {
            return;
        }
        let store = KeyringStore::with_entry("coroom-test", "identity-test");
        store.delete().expect("limpiar");
        let created = Identity::load_or_create(&store).expect("crear");
        let loaded = Identity::load_or_create(&store).expect("cargar");
        assert_eq!(created.public_key(), loaded.public_key());
        store.delete().expect("borrar");
    }
}
