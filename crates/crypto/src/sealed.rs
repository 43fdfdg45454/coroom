//! Ficheros sellados con una clave de la aplicación (Flatpak).
//!
//! Dentro de Flatpak el llavero del sistema no se ve (haría falta `--talk-name=
//! org.freedesktop.secrets`, que da acceso a todos los secretos del usuario). En su lugar, el portal
//! `Secret` (`org.freedesktop.portal.Secret`) entrega a cada aplicación un secreto propio y
//! estable, guardado por el llavero del escritorio. [`SealKey::derive`] saca de él una clave
//! AES-256-GCM (HKDF-SHA256) y [`SealedFile`] guarda con ella la identidad y las credenciales en
//! ficheros `0600` del directorio de datos del sandbox: en disco solo hay texto cifrado y
//! autenticado, y nada que sirva sin la sesión del usuario.
//!
//! Formato: `coroom-sealed-1\n` + nonce de 12 bytes aleatorio + texto cifrado con la etiqueta de
//! 16 bytes, con la ruta lógica del secreto (`identity`, `server:<host>`) como datos asociados
//! (un fichero no se puede hacer pasar por otro).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::identity::{IdentityStore, StoreError, write_secret_file};

/// Cabecera de los ficheros sellados.
pub(crate) const MAGIC: &[u8] = b"coroom-sealed-1\n";

/// Sal de HKDF para la clave de sellado.
pub(crate) const SALT: &[u8] = b"coroom";

/// Información de HKDF para la clave de sellado.
pub(crate) const INFO: &[u8] = b"coroom/portal-secret/seal/v1";

/// Clave AES-256-GCM derivada del secreto de la aplicación.
#[derive(Clone)]
pub struct SealKey(Zeroizing<[u8; 32]>);

impl std::fmt::Debug for SealKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SealKey(…)")
    }
}

impl SealKey {
    /// Deriva la clave del secreto que da el portal (al menos 16 bytes; el portal da 64).
    pub fn derive(secret: &[u8]) -> Result<SealKey, StoreError> {
        if secret.len() < 16 {
            return Err(StoreError::Encoding(
                "el secreto del portal es demasiado corto".to_owned(),
            ));
        }
        let hkdf = hkdf::Hkdf::<Sha256>::new(Some(SALT), secret);
        let mut key = Zeroizing::new([0u8; 32]);
        hkdf.expand(INFO, key.as_mut())
            .map_err(|_| StoreError::Encoding("HKDF no pudo derivar la clave".to_owned()))?;
        Ok(SealKey(key))
    }

    fn aead(&self) -> Result<LessSafeKey, StoreError> {
        UnboundKey::new(&AES_256_GCM, self.0.as_ref())
            .map(LessSafeKey::new)
            .map_err(|_| StoreError::Encoding("clave AES no válida".to_owned()))
    }

    /// Sella `plaintext` con `label` como datos asociados.
    pub fn seal(&self, label: &str, plaintext: &[u8]) -> Result<Vec<u8>, StoreError> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce)
            .map_err(|err| StoreError::Encoding(format!("sin aleatoriedad: {err}")))?;
        let mut body = plaintext.to_vec();
        self.aead()?
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(label.as_bytes()),
                &mut body,
            )
            .map_err(|_| StoreError::Encoding("no se pudo sellar el secreto".to_owned()))?;
        let mut out = Vec::with_capacity(MAGIC.len() + NONCE_LEN + body.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Abre lo que selló [`SealKey::seal`] con el mismo `label`.
    pub fn open(&self, label: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        let rest = sealed.strip_prefix(MAGIC).ok_or_else(|| {
            StoreError::Encoding("el fichero no es un secreto sellado de coroom".to_owned())
        })?;
        if rest.len() < NONCE_LEN + AES_256_GCM.tag_len() {
            return Err(StoreError::Encoding("secreto sellado truncado".to_owned()));
        }
        let (nonce, body) = rest.split_at(NONCE_LEN);
        let mut nonce_bytes = [0u8; NONCE_LEN];
        nonce_bytes.copy_from_slice(nonce);
        let mut body = Zeroizing::new(body.to_vec());
        let len = self
            .aead()?
            .open_in_place(
                Nonce::assume_unique_for_key(nonce_bytes),
                Aad::from(label.as_bytes()),
                body.as_mut_slice(),
            )
            .map_err(|_| {
                StoreError::Encoding(
                    "no se pudo abrir el secreto sellado (¿otra clave del portal?)".to_owned(),
                )
            })?
            .len();
        body.truncate(len);
        Ok(body)
    }
}

/// Un secreto en un fichero sellado con [`SealKey`].
#[derive(Debug, Clone)]
pub struct SealedFile {
    path: PathBuf,
    label: String,
    key: SealKey,
}

impl SealedFile {
    /// Fichero `path` con la etiqueta `label` (los datos asociados del sellado).
    pub fn new(path: impl Into<PathBuf>, label: impl Into<String>, key: SealKey) -> Self {
        Self {
            path: path.into(),
            label: label.into(),
            key,
        }
    }

    /// Identidad sellada en `dir/identity.sealed`.
    pub fn identity(dir: impl AsRef<Path>, key: SealKey) -> Self {
        Self::new(dir.as_ref().join("identity.sealed"), "identity", key)
    }

    /// Ruta del fichero.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Lee y abre el secreto; `Ok(None)` si no hay.
    pub fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError> {
        match fs::read(&self.path) {
            Ok(bytes) => self.key.open(&self.label, &bytes).map(Some),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(StoreError::Io {
                path: self.path.clone(),
                source,
            }),
        }
    }

    /// Sella y guarda (o sustituye) el secreto.
    pub fn save(&self, secret: &[u8]) -> Result<(), StoreError> {
        let sealed = self.key.seal(&self.label, secret)?;
        write_secret_file(&self.path, &sealed)
    }

    /// Borra el fichero (no es error si no existía).
    pub fn delete(&self) -> Result<(), StoreError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(StoreError::Io {
                path: self.path.clone(),
                source,
            }),
        }
    }
}

/// Dos ficheros sellados son el mismo si tienen la misma ruta y etiqueta (la clave no se compara).
impl PartialEq for SealedFile {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && self.label == other.label
    }
}

impl Eq for SealedFile {}

impl IdentityStore for SealedFile {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError> {
        SealedFile::load(self)
    }

    fn save(&self, pkcs8_der: &[u8]) -> Result<(), StoreError> {
        SealedFile::save(self, pkcs8_der)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_files_round_trip_and_reject_tampering() {
        let dir = tempfile::tempdir().expect("directorio");
        let key = SealKey::derive(&[7u8; 64]).expect("clave");
        let file = SealedFile::identity(dir.path(), key.clone());
        assert!(file.load().expect("leer").is_none());
        file.save(b"pkcs8 de prueba").expect("guardar");
        let raw = fs::read(file.path()).expect("fichero");
        assert!(raw.starts_with(MAGIC));
        assert!(
            !raw.windows(5).any(|w| w == b"pkcs8"),
            "en disco no hay texto en claro"
        );
        assert_eq!(
            file.load().expect("leer").expect("hay").as_slice(),
            b"pkcs8 de prueba"
        );
        // Otra clave, otra etiqueta o un byte cambiado: no abre.
        let other = SealKey::derive(&[8u8; 64]).expect("clave");
        assert!(SealedFile::identity(dir.path(), other).load().is_err());
        let relabelled = SealedFile::new(file.path(), "server:otro", key.clone());
        assert!(relabelled.load().is_err());
        let mut tampered = raw.clone();
        if let Some(last) = tampered.last_mut() {
            *last ^= 1;
        }
        assert!(key.open("identity", &tampered).is_err());
        assert!(key.open("identity", b"coroom-sealed-1\nxx").is_err());
        assert!(SealKey::derive(b"corto").is_err());
        file.delete().expect("borrar");
        file.delete().expect("borrar dos veces");
        assert!(file.load().expect("leer").is_none());
    }

    #[test]
    fn identities_live_in_sealed_files() {
        let dir = tempfile::tempdir().expect("directorio");
        let key = SealKey::derive(&[1u8; 64]).expect("clave");
        let store = SealedFile::identity(dir.path(), key.clone());
        let first = crate::Identity::load_or_create(&store).expect("crear");
        let again =
            crate::Identity::load_or_create(&SealedFile::identity(dir.path(), key)).expect("leer");
        assert_eq!(first.public_key(), again.public_key());
    }
}
