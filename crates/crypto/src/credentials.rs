//! Credenciales de las cuentas: el token de sesión de cada servidor, guardado en el
//! llavero del sistema (servicio `coroom`, usuario `server:<host>`) o, sin llavero, en un fichero
//! `servers/<host>.json` con permisos `0600` dentro del directorio de datos.
//!
//! Dentro de Flatpak va en `servers/<host>.sealed`, sellado con la clave del portal
//! `Secret` ([`crate::sealed`]).
//!
//! El contenido es un texto opaco para esta crate (la aplicación guarda ahí un JSON con el token,
//! el usuario y la huella del servidor). Como [`crate::KeyringStore`], el llavero de Linux es
//! bloqueante: no usar desde dentro de un runtime de `tokio`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

use crate::identity::{KeyringStore, StoreError, write_secret_file};
use crate::sealed::{SealKey, SealedFile};

/// Dónde se guarda la credencial de un servidor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialStore {
    /// Llavero del sistema.
    Keyring {
        /// Servicio (`coroom`).
        service: String,
        /// Usuario (`server:<host>`).
        user: String,
    },
    /// Fichero con permisos `0600`.
    File(PathBuf),
    /// Fichero sellado con la clave del portal `Secret` (Flatpak).
    Sealed(SealedFile),
}

/// Nombre de fichero seguro para un host (letras, dígitos, `.`, `-` y `_`; el resto, `_`).
pub fn host_file_name(host: &str) -> String {
    let name: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() || name.chars().all(|c| c == '.') {
        "servidor".to_owned()
    } else {
        name
    }
}

impl CredentialStore {
    /// Entrada del llavero `coroom` / `server:<host>`.
    pub fn keyring(host: &str) -> Self {
        CredentialStore::Keyring {
            service: KeyringStore::SERVICE.to_owned(),
            user: format!("server:{}", host.to_ascii_lowercase()),
        }
    }

    /// Fichero `dir/servers/<host>.json`.
    pub fn file(dir: impl AsRef<Path>, host: &str) -> Self {
        CredentialStore::File(
            dir.as_ref()
                .join("servers")
                .join(format!("{}.json", host_file_name(host))),
        )
    }

    /// Fichero `dir/servers/<host>.sealed`, sellado con `key`.
    pub fn sealed(dir: impl AsRef<Path>, host: &str, key: SealKey) -> Self {
        let name = host_file_name(host);
        CredentialStore::Sealed(SealedFile::new(
            dir.as_ref().join("servers").join(format!("{name}.sealed")),
            format!("server:{name}"),
            key,
        ))
    }

    /// Dónde está, para mostrarlo.
    pub fn describe(&self) -> String {
        match self {
            CredentialStore::Keyring { service, user } => {
                format!("llavero del sistema ({service}/{user})")
            }
            CredentialStore::File(path) => path.display().to_string(),
            CredentialStore::Sealed(file) => {
                format!("{} (sellado con el portal Secret)", file.path().display())
            }
        }
    }

    fn entry(service: &str, user: &str) -> Result<keyring::Entry, StoreError> {
        keyring::Entry::new(service, user).map_err(|err| StoreError::Keyring(err.to_string()))
    }

    /// Lee la credencial; `Ok(None)` si no hay.
    pub fn load(&self) -> Result<Option<Zeroizing<String>>, StoreError> {
        match self {
            CredentialStore::Keyring { service, user } => {
                match Self::entry(service, user)?.get_password() {
                    Ok(secret) => Ok(Some(Zeroizing::new(secret))),
                    Err(keyring::Error::NoEntry) => Ok(None),
                    Err(err) => Err(StoreError::Keyring(err.to_string())),
                }
            }
            CredentialStore::File(path) => match fs::read(path) {
                Ok(bytes) => String::from_utf8(bytes)
                    .map(|text| Some(Zeroizing::new(text)))
                    .map_err(|_| StoreError::Encoding("la credencial no es UTF-8".to_owned())),
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(source) => Err(StoreError::Io {
                    path: path.clone(),
                    source,
                }),
            },
            CredentialStore::Sealed(file) => match file.load()? {
                Some(bytes) => String::from_utf8(bytes.to_vec())
                    .map(|text| Some(Zeroizing::new(text)))
                    .map_err(|_| StoreError::Encoding("la credencial no es UTF-8".to_owned())),
                None => Ok(None),
            },
        }
    }

    /// Guarda (o sustituye) la credencial.
    pub fn save(&self, secret: &str) -> Result<(), StoreError> {
        match self {
            CredentialStore::Keyring { service, user } => Self::entry(service, user)?
                .set_password(secret)
                .map_err(|err| StoreError::Keyring(err.to_string())),
            CredentialStore::File(path) => write_secret_file(path, secret.as_bytes()),
            CredentialStore::Sealed(file) => file.save(secret.as_bytes()),
        }
    }

    /// Borra la credencial (no es error si no existía).
    pub fn delete(&self) -> Result<(), StoreError> {
        match self {
            CredentialStore::Keyring { service, user } => {
                match Self::entry(service, user)?.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                    Err(err) => Err(StoreError::Keyring(err.to_string())),
                }
            }
            CredentialStore::File(path) => match fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(source) => Err(StoreError::Io {
                    path: path.clone(),
                    source,
                }),
            },
            CredentialStore::Sealed(file) => file.delete(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_credentials_round_trip() {
        let dir = tempfile::tempdir().expect("directorio temporal");
        let key = SealKey::derive(&[3u8; 64]).expect("clave");
        let store = CredentialStore::sealed(dir.path(), "coroom.example.com", key.clone());
        assert_eq!(store.load().expect("lectura"), None);
        store.save("{\"token\":\"change-me\"}").expect("guardar");
        assert_eq!(
            store
                .load()
                .expect("lectura")
                .as_deref()
                .map(String::as_str),
            Some("{\"token\":\"change-me\"}")
        );
        assert!(store.describe().contains("portal Secret"));
        // La credencial de un servidor no abre como la de otro.
        let other = CredentialStore::sealed(dir.path(), "otro.example.com", key);
        assert_eq!(other.load().expect("lectura"), None);
        store.delete().expect("borrar");
        assert_eq!(store.load().expect("lectura"), None);
    }

    #[test]
    fn host_names_become_safe_file_names() {
        assert_eq!(host_file_name("coroom.example.com"), "coroom.example.com");
        assert_eq!(host_file_name("::1"), "__1");
        assert_eq!(host_file_name("Coroom.Example.COM"), "coroom.example.com");
        assert_eq!(host_file_name("../.."), ".._..");
        assert_eq!(host_file_name(""), "servidor");
        assert_eq!(host_file_name(".."), "servidor");
    }

    #[test]
    fn file_credentials_round_trip_with_owner_only_permissions() {
        let dir = tempfile::tempdir().expect("directorio temporal");
        let store = CredentialStore::file(dir.path(), "coroom.example.com");
        assert_eq!(
            store
                .load()
                .expect("lectura")
                .as_deref()
                .map(String::as_str),
            None
        );
        store.save("{\"token\":\"change-me\"}").expect("guardar");
        assert_eq!(
            store
                .load()
                .expect("lectura")
                .as_deref()
                .map(String::as_str),
            Some("{\"token\":\"change-me\"}")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let CredentialStore::File(path) = &store else {
                panic!("se esperaba un fichero");
            };
            let mode = fs::metadata(path).expect("metadatos").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        store.delete().expect("borrar");
        store.delete().expect("borrar dos veces no falla");
        assert!(store.load().expect("lectura").is_none());
    }

    #[test]
    fn keyring_entries_are_per_server() {
        assert_eq!(
            CredentialStore::keyring("Coroom.Example.com"),
            CredentialStore::Keyring {
                service: "coroom".to_owned(),
                user: "server:coroom.example.com".to_owned()
            }
        );
    }
}
