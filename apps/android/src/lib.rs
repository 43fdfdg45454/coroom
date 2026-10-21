//! Invitado Android de coroom: la lógica (cuenta, amigos, sala de espera y, dentro de una sala,
//! red, vídeo, audio y entrada) en Rust, bajo la interfaz en Kotlin y Jetpack Compose de
//! `packaging/android`, que la carga como `libcoroom_android.so`.
//!
//! El puente es pequeño a propósito ([`jni`]): la interfaz llama a una operación por su nombre
//! con argumentos en JSON ([`app::App::call`]) y lee los eventos en JSON
//! ([`app::App::next_event`]) desde un hilo suyo. Todo lo demás se prueba en el equipo de
//! desarrollo sin Android.
//!
//! - [`store`]: credenciales e identidad selladas con la clave que guarda el Android Keystore.
//! - [`events`]: los eventos que ve la interfaz.
//! - [`app`]: el estado del proceso y las operaciones.
//! - [`guest`]: la sesión dentro de una sala (vídeo, audio, tacto, teclado, mandos y cursor).
//! - [`mouse`]: el ratón o el touchpad conectado al teléfono y el texto del teclado en pantalla.
//! - [`avatars`]: la caché de disco de los avatares.
//! - [`update`]: la versión nueva (buscar, descargar y comprobar el APK).
//! - `log`: el registro, a logcat y a `<filesDir>/logs/coroom.log` (lo que se comparte).

pub mod app;
pub mod avatars;
pub mod events;
pub mod guest;
pub mod jni;
mod log;
pub mod mouse;
pub mod store;
mod surface;
pub mod update;

use std::sync::{Arc, OnceLock};

/// La lógica del proceso, creada por [`init`] la primera vez.
static APP: OnceLock<Arc<app::App>> = OnceLock::new();

/// Arranca la lógica una vez por proceso: el registro (logcat y `files_dir/logs`), el almacén
/// sellado en `files_dir` con la clave de `secret` y el nombre del dispositivo. Las llamadas
/// siguientes no cambian nada.
pub fn init(files_dir: &str, secret: &[u8], device: &str) -> Result<(), String> {
    if APP.get().is_some() {
        return Ok(());
    }
    log::init(std::path::Path::new(files_dir));
    let store = store::SealedStore::new(files_dir, secret)?;
    let _ = APP.set(Arc::new(app::App::new(store, device.to_owned())));
    Ok(())
}

/// La lógica, si ya se llamó a [`init`].
pub fn app() -> Result<&'static Arc<app::App>, String> {
    APP.get()
        .ok_or_else(|| "la lógica no está iniciada (falta Native.init)".to_owned())
}
