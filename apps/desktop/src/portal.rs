//! El portal `Secret` dentro de Flatpak.
//!
//! En el sandbox no se ve el llavero del sistema sin `--talk-name=org.freedesktop.secrets` (que
//! abriría todos los secretos del usuario). El portal `Secret` da a la aplicación un secreto propio,
//! guardado por el llavero del escritorio; de él sale la clave con la que se sellan la identidad y
//! las credenciales en el directorio de datos ([`coroom_transport::crypto::sealed`]). Fuera de
//! Flatpak no se usa: ahí está el llavero de siempre.

use std::sync::OnceLock;

use coroom_transport::crypto::SealKey;

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

#[cfg(target_os = "linux")]
fn retrieve() -> Result<SealKey, String> {
    // En un hilo propio: quien llama puede estar dentro de un runtime de `tokio`.
    std::thread::spawn(|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| err.to_string())?;
        let secret = runtime
            .block_on(ashpd::desktop::secret::retrieve())
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
