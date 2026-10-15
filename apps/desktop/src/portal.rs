//! Portales del escritorio fuera de la captura: el portal `Secret` dentro de Flatpak y el permiso
//! del portal `ScreenCast` que se recuerda entre ejecuciones.
//!
//! **Secret.** En el sandbox no se ve el llavero del sistema sin `--talk-name=org.freedesktop.secrets`
//! (que abriría todos los secretos del usuario). El portal `Secret` da a la aplicación un secreto
//! propio, guardado por el llavero del escritorio; de él sale la clave con la que se sellan la
//! identidad y las credenciales en el directorio de datos ([`coroom_transport::crypto::sealed`]).
//! Fuera de Flatpak no se usa: ahí está el llavero de siempre.
//!
//! **ScreenCast.** El portal recuerda qué eligió el usuario en su diálogo si se le devuelve el
//! token de restauración que dio; sin él, pregunta en cada arranque. [`RestoreTokens`] guarda esos
//! tokens en `portal.json` del directorio de datos, uno por tipo de fuente (el portal da tokens
//! distintos para un monitor y para una ventana), con permisos 0600: quien tenga el token comparte
//! la pantalla sin que el sistema pregunte, así que es tan sensible como una credencial. La
//! aplicación y la consola (`coroom host`, `coroom probe`) pasan por aquí, y el backend de captura
//! avisa de cada token nuevo (el portal lo renueva en cada arranque) y de los que ya no valen
//! ([`coroom_session::capture::RestoreTokenSink`]).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use coroom_session::capture::ShareKind;
use coroom_transport::crypto::SealKey;
use serde::{Deserialize, Serialize};

/// Si la aplicación corre dentro de Flatpak.
pub fn in_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some_and(|id| !id.is_empty())
        || std::path::Path::new("/.flatpak-info").exists()
}

/// Clave de sellado del portal `Secret` si la aplicación corre en Flatpak y el portal responde
/// (se pide una vez por proceso). `None` fuera de Flatpak o si el portal falla (entonces se usa el
/// llavero o un fichero, como siempre).
pub fn seal_key() -> Option<SealKey> {
    static KEY: OnceLock<Option<SealKey>> = OnceLock::new();
    KEY.get_or_init(|| {
        if !in_flatpak() {
            return None;
        }
        match retrieve() {
            Ok(key) => Some(key),
            Err(err) => {
                tracing::warn!(error = %err, "el portal Secret no respondió; se usa un fichero");
                None
            }
        }
    })
    .clone()
}

/// Espera máxima del portal `Secret`. Sin un backend que lo atienda (KDE Plasma sin
/// gnome-keyring) la petición no termina nunca.
#[cfg(target_os = "linux")]
const SECRET_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(target_os = "linux")]
fn retrieve() -> Result<SealKey, String> {
    // En un hilo propio: quien llama puede estar dentro de un runtime de `tokio`.
    std::thread::spawn(|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| err.to_string())?;
        let secret = runtime
            .block_on(async {
                tokio::time::timeout(SECRET_TIMEOUT, ashpd::desktop::secret::retrieve()).await
            })
            .map_err(|_| {
                format!(
                    "sin respuesta en {} s (¿hay un servicio de secretos con portal, como \
                     gnome-keyring?)",
                    SECRET_TIMEOUT.as_secs()
                )
            })?
            .map_err(|err| err.to_string())?;
        SealKey::derive(&secret).map_err(|err| err.to_string())
    })
    .join()
    .map_err(|_| "el hilo del portal Secret terminó mal".to_owned())?
}

#[cfg(not(target_os = "linux"))]
fn retrieve() -> Result<SealKey, String> {
    Err("el portal Secret solo existe en Linux".to_owned())
}

// --- ScreenCast: el permiso recordado --------------------------------------------------------------

/// Nombre del fichero del directorio de datos con los tokens de restauración del portal
/// `ScreenCast`. Aparte de `settings.toml`, que es del usuario.
pub const RESTORE_TOKENS_FILE: &str = "portal.json";

/// Si este sistema recuerda el permiso de compartir la pantalla entre ejecuciones: en Linux, por el
/// portal; en Windows la captura no pregunta.
pub const REMEMBERS_SHARE_PERMISSION: bool = cfg!(target_os = "linux");

/// Tokens de restauración del portal `ScreenCast`, uno por tipo de fuente.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct RestoreTokens {
    /// Compartir un monitor.
    #[serde(skip_serializing_if = "Option::is_none")]
    screen: Option<String>,
    /// Compartir una ventana.
    #[serde(skip_serializing_if = "Option::is_none")]
    window: Option<String>,
}

impl RestoreTokens {
    /// Lee `path`. Sin fichero, o con uno que no se entiende (se avisa en el registro), no hay
    /// tokens: el portal volverá a preguntar y lo que conteste se guardará encima.
    pub fn load(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "no se pudo leer el permiso recordado del portal; se preguntará de nuevo"
                );
                return Self::default();
            }
        };
        serde_json::from_str(&text).unwrap_or_else(|err| {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "el permiso recordado del portal no se entiende; se preguntará de nuevo"
            );
            Self::default()
        })
    }

    /// Token guardado para `kind`.
    pub fn get(&self, kind: ShareKind) -> Option<&str> {
        self.slot(kind).as_deref()
    }

    /// Guarda (o, con `None`, olvida) el token de `kind`.
    pub fn set(&mut self, kind: ShareKind, token: Option<String>) {
        *self.slot_mut(kind) = token.filter(|token| !token.is_empty());
    }

    /// Si no hay ningún token guardado.
    pub fn is_empty(&self) -> bool {
        self.screen.is_none() && self.window.is_none()
    }

    /// Escribe en `path` con permisos 0600 (en un temporal que se renombra, nunca a medias); sin
    /// tokens, borra el fichero.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if self.is_empty() {
            return match std::fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(t!("portal-no-se-pudo-olvidar-el-permiso", err = err)),
            };
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|err| t!("portal-no-se-pudo-guardar-el-permiso", err = err))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|err| t!("portal-no-se-pudo-guardar-el-permiso", err = err))?;
        }
        let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
        write_private(&temporary, text.as_bytes())
            .and_then(|()| std::fs::rename(&temporary, path))
            .map_err(|err| {
                let _ = std::fs::remove_file(&temporary);
                t!("portal-no-se-pudo-guardar-el-permiso", err = err)
            })
    }

    fn slot(&self, kind: ShareKind) -> &Option<String> {
        match kind {
            ShareKind::Screen => &self.screen,
            ShareKind::App => &self.window,
        }
    }

    fn slot_mut(&mut self, kind: ShareKind) -> &mut Option<String> {
        match kind {
            ShareKind::Screen => &mut self.screen,
            ShareKind::App => &mut self.window,
        }
    }
}

/// Crea `path` solo legible por el usuario (0600 en Unix) y escribe `bytes`.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}

/// Ruta de `portal.json` en el directorio de datos.
pub fn restore_tokens_path() -> PathBuf {
    crate::ui::settings::data_dir().join(RESTORE_TOKENS_FILE)
}

#[cfg(target_os = "linux")]
mod share {
    use coroom_session::capture::{RestoreTokenSink, ShareKind};

    use super::{RestoreTokens, restore_tokens_path};

    /// Los tokens guardados en el directorio de datos.
    pub fn load_restore_tokens() -> RestoreTokens {
        RestoreTokens::load(&restore_tokens_path())
    }

    /// El token guardado para `kind`, el que se pasa al portal para que no pregunte.
    pub fn saved_restore_token(kind: ShareKind) -> Option<String> {
        load_restore_tokens().get(kind).map(str::to_owned)
    }

    /// Guarda el token de `kind` (o, con `None`, lo olvida) en el directorio de datos; si no se
    /// puede, queda en el registro: la captura ya está en marcha y solo se perdería el recuerdo.
    pub fn remember_restore_token(kind: ShareKind, token: Option<String>) {
        let mut tokens = load_restore_tokens();
        tokens.set(kind, token);
        if let Err(err) = tokens.save(&restore_tokens_path()) {
            tracing::warn!(error = %err, "no se pudo guardar el permiso del portal");
        }
    }

    /// Aviso del backend de captura que guarda directamente en el directorio de datos (la
    /// consola; la aplicación pasa por su estado para que Ajustes lo vea al momento).
    pub fn restore_token_sink(kind: ShareKind) -> Option<RestoreTokenSink> {
        Some(Box::new(move |token| remember_restore_token(kind, token)))
    }
}

/// Sin portal no hay nada que recordar: las mismas funciones, sin hacer nada.
#[cfg(not(target_os = "linux"))]
mod share {
    use coroom_session::capture::{RestoreTokenSink, ShareKind};

    use super::RestoreTokens;

    pub fn load_restore_tokens() -> RestoreTokens {
        RestoreTokens::default()
    }

    pub fn saved_restore_token(_kind: ShareKind) -> Option<String> {
        None
    }

    pub fn remember_restore_token(_kind: ShareKind, _token: Option<String>) {}

    pub fn restore_token_sink(_kind: ShareKind) -> Option<RestoreTokenSink> {
        None
    }
}

pub use share::{
    load_restore_tokens, remember_restore_token, restore_token_sink, saved_restore_token,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_kept_per_source_kind() {
        let dir = tempfile::tempdir().expect("directorio");
        let path = dir.path().join(RESTORE_TOKENS_FILE);
        assert_eq!(RestoreTokens::load(&path), RestoreTokens::default());

        let mut tokens = RestoreTokens::default();
        tokens.set(ShareKind::Screen, Some("pantalla".to_owned()));
        tokens.save(&path).expect("guardar");
        let loaded = RestoreTokens::load(&path);
        assert_eq!(loaded.get(ShareKind::Screen), Some("pantalla"));
        assert_eq!(
            loaded.get(ShareKind::App),
            None,
            "la ventana tiene su propio token"
        );

        let mut tokens = loaded;
        tokens.set(ShareKind::App, Some("ventana".to_owned()));
        tokens.set(ShareKind::Screen, None);
        tokens.save(&path).expect("guardar");
        let loaded = RestoreTokens::load(&path);
        assert_eq!(loaded.get(ShareKind::Screen), None);
        assert_eq!(loaded.get(ShareKind::App), Some("ventana"));
        assert!(!loaded.is_empty());

        // Un token vacío no es un token.
        let mut tokens = loaded;
        tokens.set(ShareKind::App, Some(String::new()));
        assert!(tokens.is_empty());
    }

    #[test]
    fn forgetting_everything_removes_the_file() {
        let dir = tempfile::tempdir().expect("directorio");
        let path = dir.path().join(RESTORE_TOKENS_FILE);
        let mut tokens = RestoreTokens::default();
        tokens.set(ShareKind::Screen, Some("pantalla".to_owned()));
        tokens.save(&path).expect("guardar");
        assert!(path.exists());
        RestoreTokens::default().save(&path).expect("borrar");
        assert!(!path.exists());
        RestoreTokens::default()
            .save(&path)
            .expect("borrar lo que no existe no es un error");
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_only_readable_by_the_user() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("directorio");
        let path = dir.path().join("anidado").join(RESTORE_TOKENS_FILE);
        let mut tokens = RestoreTokens::default();
        tokens.set(ShareKind::App, Some("ventana".to_owned()));
        tokens.save(&path).expect("guardar");
        let mode = std::fs::metadata(&path)
            .expect("metadatos")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "permisos {mode:o}");
        // Volver a guardar encima mantiene los permisos (el temporal se crea con ellos).
        tokens.set(ShareKind::Screen, Some("pantalla".to_owned()));
        tokens.save(&path).expect("guardar");
        let mode = std::fs::metadata(&path)
            .expect("metadatos")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "permisos {mode:o}");
        assert!(
            std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .all(|entry| entry.unwrap().file_name() == RESTORE_TOKENS_FILE),
            "sin temporales sueltos"
        );
    }

    #[test]
    fn a_corrupt_file_means_no_tokens() {
        let dir = tempfile::tempdir().expect("directorio");
        let path = dir.path().join(RESTORE_TOKENS_FILE);
        std::fs::write(&path, "{ esto no es JSON").expect("escribir");
        assert_eq!(RestoreTokens::load(&path), RestoreTokens::default());
        std::fs::write(&path, "[1, 2, 3]").expect("escribir");
        assert_eq!(RestoreTokens::load(&path), RestoreTokens::default());
        // Lo que no se conoce se ignora; lo conocido se lee.
        std::fs::write(&path, r#"{"screen": "pantalla", "futuro": true}"#).expect("escribir");
        assert_eq!(
            RestoreTokens::load(&path).get(ShareKind::Screen),
            Some("pantalla")
        );
    }
}
