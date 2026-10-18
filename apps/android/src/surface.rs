//! La `Surface` de la vista del vídeo hacia MediaCodec, y el reloj de los eventos de Android.

use jni::Env;
use jni::objects::JObject;

use crate::guest::Devices;

/// Pone (o quita, con `null`) la superficie de la vista en el sitio que usan los decodificadores.
/// Se llama desde `surfaceChanged` y desde `surfaceDestroyed`, antes de que Android la destruya.
#[cfg(target_os = "android")]
#[allow(unsafe_code)]
pub fn set(env: &Env<'_>, surface: &JObject<'_>, devices: &Devices) -> Result<(), String> {
    use coroom_session::codec::mediacodec::OutputSurface;
    if surface.is_null() {
        devices.surface.clear();
        return Ok(());
    }
    // SAFETY: `env` es el `JNIEnv` del hilo que llama y `surface` una referencia local válida a
    // un `android.view.Surface` durante la llamada; `ANativeWindow_fromSurface` toma su propia
    // referencia a la ventana, que suelta `NativeWindow` al soltarse.
    let window = unsafe {
        ndk::native_window::NativeWindow::from_surface(
            env.get_raw().cast(),
            surface.as_raw().cast(),
        )
    };
    match window {
        Some(window) => {
            devices.surface.set(OutputSurface::new(window));
            Ok(())
        }
        None => Err("la vista del vídeo no tiene superficie".to_owned()),
    }
}

/// Fuera de Android no hay `Surface`: solo se acepta `null`.
#[cfg(not(target_os = "android"))]
pub fn set(_env: &Env<'_>, surface: &JObject<'_>, devices: &Devices) -> Result<(), String> {
    if surface.is_null() {
        devices.surface.clear();
        Ok(())
    } else {
        Err("sin Android no hay superficie de vídeo".to_owned())
    }
}

/// `SystemClock.uptimeMillis()` de ahora: `CLOCK_MONOTONIC` en ms, el reloj de `getEventTime()`.
#[cfg(target_os = "android")]
#[allow(unsafe_code)]
pub fn uptime_ms() -> Option<u64> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` es un `timespec` válido y escribible.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    if status != 0 {
        return None;
    }
    let secs = u64::try_from(now.tv_sec).ok()?;
    let nanos = u64::try_from(now.tv_nsec).ok()?;
    Some(secs * 1000 + nanos / 1_000_000)
}

/// Fuera de Android los eventos no traen ese reloj.
#[cfg(not(target_os = "android"))]
pub fn uptime_ms() -> Option<u64> {
    None
}
