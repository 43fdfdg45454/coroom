//! Credenciales e identidad del invitado Android, selladas con AES-256-GCM.
//!
//! La clave sale de un secreto aleatorio que la aplicación guarda cifrado con una clave del
//! Android Keystore (que nunca sale del hardware seguro) y que entrega descifrado al arrancar
//! ([`crate::init`]). Con él se sellan, en el directorio privado de la aplicación, la identidad
//! (`identity.sealed`) y el token de cada servidor (`servers/<host>.sealed`), como hace el
//! escritorio con el secreto del portal en Flatpak. El último servidor usado, sin secretos, en
//! `servers/last`.

use std::path::{Path, PathBuf};

use coroom_session::account::{AccountStorage, Zeroizing};
use coroom_transport::crypto::{CredentialStore, Identity, IdentityStore, SealKey, SealedFile};

/// Almacén sellado de la aplicación.
#[derive(Debug, Clone)]
pub struct SealedStore {
    dir: PathBuf,
    key: SealKey,
}

impl SealedStore {
    /// Almacén en `dir` (el `filesDir` de la aplicación) con la clave derivada de `secret`.
    pub fn new(dir: impl Into<PathBuf>, secret: &[u8]) -> Result<Self, String> {
        if secret.len() < 16 {
            return Err("el secreto de la aplicación es demasiado corto".to_owned());
        }
        let key = SealKey::derive(secret).map_err(|err| err.to_string())?;
        Ok(Self {
            dir: dir.into(),
            key,
        })
    }

    /// Directorio de datos.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn credentials(&self, host: &str) -> CredentialStore {
        CredentialStore::sealed(&self.dir, host, self.key.clone())
    }

    fn last_path(&self) -> PathBuf {
        self.dir.join("servers").join("last")
    }
}

impl AccountStorage for SealedStore {
    fn load_secret(&self, host: &str) -> Result<Option<Zeroizing<String>>, String> {
        self.credentials(host).load().map_err(|err| err.to_string())
    }

    fn save_secret(&self, host: &str, secret: &str) -> Result<String, String> {
        let store = self.credentials(host);
        store.save(secret).map_err(|err| err.to_string())?;
        Ok(store.describe())
    }

    fn delete_secret(&self, host: &str) -> Result<(), String> {
        self.credentials(host)
            .delete()
            .map_err(|err| err.to_string())
    }

    fn last_server(&self) -> Option<String> {
        std::fs::read_to_string(self.last_path())
            .ok()
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
    }

    fn set_last_server(&self, url: &str) {
        let path = self.last_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, format!("{url}\n"));
    }

    fn load_identity(&self) -> Result<(Identity, String), String> {
        let store = SealedFile::identity(&self.dir, self.key.clone());
        let identity = Identity::load_or_create(&store as &dyn IdentityStore)
            .map_err(|err| format!("no se pudo cargar la identidad: {err}"))?;
        Ok((identity, store.path().display().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coroom_session::account::Credentials;

    fn credentials(server: &str) -> Credentials {
        Credentials {
            server: server.to_owned(),
            username: "ana".to_owned(),
            display_name: "Ana".to_owned(),
            user_id: 7,
            token: "change-me".to_owned(),
            expires_at: 0,
            trust_fingerprint: None,
            tcp_port: None,
        }
    }

    #[test]
    fn credentials_and_identity_survive_a_restart_and_need_the_same_secret() {
        let dir = tempfile::tempdir().expect("directorio");
        let secret = [7u8; 32];
        let store = SealedStore::new(dir.path(), &secret).expect("almacén");
        let saved = credentials("https://coroom.example.com:4433");
        store.save(&saved).expect("guardar");
        let (identity, _) = store.load_identity().expect("identidad");

        let again = SealedStore::new(dir.path(), &secret).expect("almacén");
        assert_eq!(
            again.load("https://COROOM.example.com").expect("cargar"),
            Some(saved)
        );
        assert_eq!(
            again.last_server().as_deref(),
            Some("https://coroom.example.com:4433")
        );
        let (same, _) = again.load_identity().expect("identidad");
        assert_eq!(same.public_key(), identity.public_key());

        // Nada en claro en el disco: otro secreto no abre lo sellado.
        let other = SealedStore::new(dir.path(), &[8u8; 32]).expect("almacén");
        assert!(other.load("https://coroom.example.com").is_err());
        assert!(other.load_identity().is_err());
    }

    #[test]
    fn a_short_secret_is_rejected() {
        assert!(SealedStore::new("/nonexistent", &[1u8; 8]).is_err());
    }
}
