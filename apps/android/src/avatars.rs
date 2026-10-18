//! Los avatares de la cuenta, los amigos y la gente de la sala, en una caché de disco para que la
//! interfaz los lea con `BitmapFactory.decodeFile`.
//!
//! Cada uno en `<datos>/avatars/<usuario>-<versión>.png`. La versión es la del servidor
//! (`avatar_version`, parte del SHA-256 del PNG que vuelve a codificar él): el fichero se comprueba
//! al leerlo y uno que no cuadra se borra, así que la caché nunca da otra imagen que la pedida. Se
//! descarga solo lo que falta, a un temporal que luego se renombra (una caída a medias no deja un
//! fichero cortado), y al guardar una versión se borran las demás de ese usuario.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use coroom_transport::crypto::sha256;
use coroom_transport::proto::api::{AVATAR_MAX_BYTES, avatar_version};

/// Para que dos descargas a la vez del mismo avatar no escriban en el mismo temporal.
static TEMPORARY: AtomicU64 = AtomicU64::new(0);

/// Si `png` es el avatar de `version`.
fn matches_version(png: &[u8], version: u64) -> bool {
    png.len() <= AVATAR_MAX_BYTES && avatar_version(&sha256(png)) == version
}

/// La caché de disco de los avatares.
#[derive(Debug, Clone)]
pub struct AvatarCache {
    dir: PathBuf,
}

impl AvatarCache {
    /// La caché en `data_dir/avatars`.
    pub fn in_data_dir(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("avatars"),
        }
    }

    /// Dónde va la versión `version` del avatar de `user`.
    pub fn path(&self, user: u64, version: u64) -> PathBuf {
        self.dir.join(format!("{user}-{version}.png"))
    }

    /// El PNG de `version` de `user`: el de la caché si está y es el que dice ser, o el que da
    /// `fetch` (la descarga). `Ok(None)` si el usuario no tiene avatar.
    ///
    /// Si el servidor ya tiene otro (cambió desde que llegó la versión), se guarda y se devuelve
    /// ese con su versión: es la imagen que lleva ahora.
    pub fn get_or_fetch(
        &self,
        user: u64,
        version: u64,
        fetch: impl FnOnce() -> Result<Option<Vec<u8>>, String>,
    ) -> Result<Option<PathBuf>, String> {
        let path = self.path(user, version);
        if let Ok(bytes) = std::fs::read(&path) {
            if matches_version(&bytes, version) {
                return Ok(Some(path));
            }
            tracing::debug!(user, version, "avatar de la caché dañado; se borra");
            let _ = std::fs::remove_file(&path);
        }
        let Some(png) = fetch()? else {
            self.forget(user, None);
            return Ok(None);
        };
        if png.len() > AVATAR_MAX_BYTES {
            return Err("el avatar que llegó es demasiado grande".to_owned());
        }
        let actual = avatar_version(&sha256(&png));
        self.store(user, actual, &png)
            .map_err(|err| format!("no se pudo guardar el avatar: {err}"))
            .map(Some)
    }

    /// Guarda el PNG (de versión `version`) de una vez y borra las demás versiones de `user`.
    fn store(&self, user: u64, version: u64, png: &[u8]) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path(user, version);
        let n = TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let temporary = self.dir.join(format!(".{user}-{version}.{n}.part"));
        if let Err(err) = std::fs::write(&temporary, png) {
            let _ = std::fs::remove_file(&temporary);
            return Err(err);
        }
        if let Err(err) = std::fs::rename(&temporary, &path) {
            let _ = std::fs::remove_file(&temporary);
            return Err(err);
        }
        self.forget(user, Some(version));
        Ok(path)
    }

    /// Borra las versiones guardadas de `user` salvo `keep`.
    fn forget(&self, user: u64, keep: Option<u64>) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let prefix = format!("{user}-");
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(version) = name
                .strip_prefix(&prefix)
                .and_then(|rest| rest.strip_suffix(".png"))
                .and_then(|version| version.parse::<u64>().ok())
            else {
                continue;
            };
            if Some(version) != keep {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unos bytes cualesquiera con su versión (la caché no decodifica la imagen).
    fn png(seed: u8) -> (Vec<u8>, u64) {
        let bytes = vec![0x89, b'P', b'N', b'G', seed, seed, seed];
        let version = avatar_version(&sha256(&bytes));
        (bytes, version)
    }

    fn no_network() -> Result<Option<Vec<u8>>, String> {
        panic!("con el avatar en la caché no se va a la red")
    }

    #[test]
    fn a_cached_avatar_is_returned_without_downloading() {
        let dir = tempfile::tempdir().expect("directorio");
        let cache = AvatarCache::in_data_dir(dir.path());
        let (bytes, version) = png(1);
        let path = cache
            .get_or_fetch(7, version, || Ok(Some(bytes.clone())))
            .expect("descarga")
            .expect("tiene avatar");
        assert_eq!(path, dir.path().join(format!("avatars/7-{version}.png")));
        assert_eq!(std::fs::read(&path).expect("leer"), bytes);
        assert_eq!(
            cache.get_or_fetch(7, version, no_network).expect("caché"),
            Some(path)
        );
    }

    #[test]
    fn a_new_version_removes_the_older_ones_of_that_user_only() {
        let dir = tempfile::tempdir().expect("directorio");
        let cache = AvatarCache::in_data_dir(dir.path());
        let (old, old_version) = png(1);
        let (new, new_version) = png(2);
        let old_path = cache
            .get_or_fetch(7, old_version, || Ok(Some(old.clone())))
            .expect("vieja")
            .expect("avatar");
        let other = cache
            .get_or_fetch(8, old_version, || Ok(Some(old.clone())))
            .expect("de otro")
            .expect("avatar");
        let new_path = cache
            .get_or_fetch(7, new_version, || Ok(Some(new.clone())))
            .expect("nueva")
            .expect("avatar");
        assert!(new_path.exists());
        assert!(!old_path.exists(), "la versión vieja se borra");
        assert!(other.exists(), "los de otro usuario se quedan");
        // Sin temporales a medias.
        let names: Vec<_> = std::fs::read_dir(dir.path().join("avatars"))
            .expect("directorio")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
    }

    #[test]
    fn without_an_avatar_the_cached_ones_go_and_the_answer_is_none() {
        let dir = tempfile::tempdir().expect("directorio");
        let cache = AvatarCache::in_data_dir(dir.path());
        let (bytes, version) = png(3);
        let path = cache
            .get_or_fetch(7, version, || Ok(Some(bytes.clone())))
            .expect("descarga")
            .expect("avatar");
        let (_, other_version) = png(4);
        assert_eq!(cache.get_or_fetch(7, other_version, || Ok(None)), Ok(None));
        assert!(!path.exists());
        assert_eq!(
            cache.get_or_fetch(7, other_version, || Err("sin red".to_owned())),
            Err("sin red".to_owned())
        );
    }

    #[test]
    fn a_damaged_file_is_downloaded_again_and_a_changed_avatar_keeps_its_own_version() {
        let dir = tempfile::tempdir().expect("directorio");
        let cache = AvatarCache::in_data_dir(dir.path());
        let (bytes, version) = png(5);
        let path = cache.path(7, version);
        std::fs::create_dir_all(path.parent().expect("padre")).expect("directorio");
        std::fs::write(&path, b"estropeado").expect("escribir");
        let again = cache
            .get_or_fetch(7, version, || Ok(Some(bytes.clone())))
            .expect("descarga")
            .expect("avatar");
        assert_eq!(std::fs::read(&again).expect("leer"), bytes);

        // Se pidió una versión que el servidor ya cambió: llega la actual con su nombre.
        let (current, current_version) = png(6);
        let stale = version ^ 1;
        let got = cache
            .get_or_fetch(7, stale, || Ok(Some(current.clone())))
            .expect("descarga")
            .expect("avatar");
        assert_eq!(got, cache.path(7, current_version));
        assert!(!again.exists());
    }
}
