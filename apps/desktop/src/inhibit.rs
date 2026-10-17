//! Que el escritorio no se bloquee ni apague la pantalla mientras se mira o se comparte una
//! transmisión.
//!
//! Quien mira (y juega con un mando, que el sistema no cuenta como actividad) no toca el teclado
//! ni el ratón del equipo, así que sin esto la sesión se bloquea al rato. Al anfitrión le pasa lo
//! mismo cuando el invitado juega con un mando, y con la sesión bloqueada se capturaría la
//! pantalla de bloqueo. [`IdleInhibitor`] lo
//! impide mientras existe:
//!
//! - **Linux**: el portal `Inhibit` con la marca `Idle` (GNOME, KDE y los demás lo atienden, también
//!   dentro de Flatpak). La inhibición dura lo que la petición del portal; un hilo propio la abre y
//!   la cierra al soltar el inhibidor.
//! - **Windows**: `SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED |
//!   ES_SYSTEM_REQUIRED)` en un hilo propio, que vale mientras ese hilo no lo quite.

use std::sync::mpsc;
use std::thread::JoinHandle;

/// Mientras existe, el escritorio no se bloquea por inactividad. Se suelta al soltarlo.
#[derive(Debug)]
pub struct IdleInhibitor {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl IdleInhibitor {
    /// Empieza a impedir el bloqueo; `reason` es lo que el escritorio enseña si lo pregunta.
    pub fn start(reason: &str) -> Self {
        let (stop, stopped) = mpsc::channel();
        let reason = reason.to_owned();
        let thread = std::thread::Builder::new()
            .name("coroom-inhibit".into())
            .spawn(move || hold(&reason, stopped))
            .map_err(|err| tracing::warn!(error = %err, "sin hilo para impedir el bloqueo"))
            .ok();
        Self {
            stop: Some(stop),
            thread,
        }
    }
}

impl Drop for IdleInhibitor {
    fn drop(&mut self) {
        // Al soltar el emisor, el hilo deja de esperar y quita la inhibición. No se lo espera: se
        // suelta desde el hilo de la interfaz y el portal puede tardar en contestar.
        self.stop.take();
        self.thread.take();
    }
}

/// Plazo para que el portal conteste.
#[cfg(target_os = "linux")]
const PORTAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(target_os = "linux")]
fn hold(reason: &str, stopped: mpsc::Receiver<()>) {
    use ashpd::desktop::inhibit::{InhibitFlags, InhibitOptions, InhibitProxy};

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::warn!(error = %err, "sin runtime para el portal Inhibit");
            return;
        }
    };
    runtime.block_on(async {
        let inhibit = async {
            let proxy = InhibitProxy::new().await?;
            let request = proxy
                .inhibit(
                    None,
                    InhibitFlags::Idle.into(),
                    InhibitOptions::default().set_reason(reason),
                )
                .await?;
            Ok::<_, ashpd::Error>((proxy, request))
        };
        let (proxy, request) = match tokio::time::timeout(PORTAL_TIMEOUT, inhibit).await {
            Ok(Ok(held)) => held,
            Ok(Err(err)) => {
                tracing::warn!(error = %err, "el portal Inhibit no impide el bloqueo");
                return;
            }
            Err(_) => {
                tracing::warn!("el portal Inhibit no contestó; la pantalla se puede bloquear");
                return;
            }
        };
        tracing::info!("bloqueo de la pantalla impedido mientras dure la transmisión");
        // Hasta que se suelte el inhibidor (el emisor se cae y `recv` devuelve error). La espera va
        // aparte para que la conexión con el portal siga atendida.
        let _ = tokio::task::spawn_blocking(move || stopped.recv()).await;
        match tokio::time::timeout(PORTAL_TIMEOUT, request.close()).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => tracing::debug!(error = %err, "cerrar la inhibición"),
            // Se cierra igual al soltar la conexión con el portal.
            Err(_) => tracing::debug!("el portal no contestó al cerrar la inhibición"),
        }
        drop(proxy);
        tracing::info!("la pantalla se vuelve a poder bloquear");
    });
}

#[cfg(target_os = "windows")]
fn hold(reason: &str, stopped: mpsc::Receiver<()>) {
    use windows::Win32::System::Power::{
        ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
    };

    let _ = reason;
    // SAFETY: solo cambia el estado de ejecución de este hilo; las marcas son constantes del
    // sistema.
    unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED) };
    tracing::info!("bloqueo de la pantalla impedido mientras dure la transmisión");
    // Hasta que se suelte el inhibidor (el emisor se cae y `recv` devuelve error).
    let _ = stopped.recv();
    // SAFETY: como arriba; `ES_CONTINUOUS` solo quita lo que pidió este hilo.
    unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn hold(reason: &str, stopped: mpsc::Receiver<()>) {
    let _ = reason;
    let _ = stopped.recv();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Con el escritorio de verdad: `cargo test -p coroom --lib inhibit -- --ignored --nocapture`.
    #[test]
    #[ignore = "necesita el portal del escritorio"]
    fn the_desktop_accepts_the_inhibition() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let inhibitor = IdleInhibitor::start("prueba de coroom");
        std::thread::sleep(std::time::Duration::from_secs(
            std::env::var("INHIBIT_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(6),
        ));
        drop(inhibitor);
    }
}
