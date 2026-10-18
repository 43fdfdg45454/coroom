//! Las funciones nativas de `coroom.android.Native` (Kotlin), con los nombres que busca la JVM.
//!
//! Cada una convierte sus argumentos y llama a la lógica; nada de lo que hace la sesión ocurre
//! aquí. Un pánico o un error de JNI se convierte en una excepción de Java en lugar de cruzar la
//! frontera (que abortaría el proceso).

// `#[unsafe(no_mangle)]`: la JVM resuelve cada método nativo por el nombre exportado
// `Java_<paquete>_<clase>_<método>`; el resto del módulo no usa `unsafe`.
#![allow(unsafe_code)]

use std::time::Duration;

use jni::EnvUnowned;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JByteArray, JClass, JFloatArray, JIntArray, JObject, JString};
use jni::sys::{jboolean, jfloat, jint, jlong};

use coroom_session::input::gamepad::PadKey;
use coroom_session::input::touch::TouchPointer;

/// Error de la frontera: texto en español que llega a Java como `RuntimeException`.
#[derive(Debug)]
struct BridgeError(String);

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BridgeError {}

impl From<jni::errors::Error> for BridgeError {
    fn from(err: jni::errors::Error) -> Self {
        BridgeError(format!("JNI: {err}"))
    }
}

impl From<String> for BridgeError {
    fn from(err: String) -> Self {
        BridgeError(err)
    }
}

/// `Native.init(filesDir, secret, device): String?`: `null` si arrancó; si no, por qué.
// SAFETY: el nombre exportado es el que la JVM espera para este método nativo y la firma es la
// de un método estático (`JNIEnv*`, `jclass`, argumentos); `jni` valida cada referencia.
#[unsafe(no_mangle)]
pub extern "system" fn Java_coroom_android_Native_init<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    files_dir: JString<'caller>,
    secret: JByteArray<'caller>,
    device: JString<'caller>,
) -> JObject<'caller> {
    env.with_env(|env| -> Result<JObject<'caller>, BridgeError> {
        let files_dir = files_dir.try_to_string(env)?;
        let secret = zeroize_bytes(env.convert_byte_array(&secret)?);
        let device = device.try_to_string(env)?;
        match crate::init(&files_dir, &secret, &device) {
            Ok(()) => Ok(JObject::null()),
            Err(err) => Ok(env.new_string(err)?.into()),
        }
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.call(method, args): String`: `{"ok": …}` o `{"error": "…"}`.
// SAFETY: como en `init`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_coroom_android_Native_call<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    method: JString<'caller>,
    args: JString<'caller>,
) -> JObject<'caller> {
    env.with_env(|env| -> Result<JObject<'caller>, BridgeError> {
        let method = method.try_to_string(env)?;
        let args = args.try_to_string(env)?;
        let answer = match crate::app() {
            Ok(app) => app.call(&method, &args),
            Err(err) => serde_json::json!({ "error": err }).to_string(),
        };
        Ok(env.new_string(answer)?.into())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.nextEvent(timeoutMs): String?`: el siguiente evento en JSON o `null` si no llegó
/// ninguno a tiempo.
// SAFETY: como en `init`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_coroom_android_Native_nextEvent<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    timeout_ms: jlong,
) -> JObject<'caller> {
    env.with_env(|env| -> Result<JObject<'caller>, BridgeError> {
        let app = crate::app()?;
        let timeout = Duration::from_millis(u64::try_from(timeout_ms).unwrap_or(0));
        match app.next_event(timeout) {
            Some(event) => Ok(env.new_string(event)?.into()),
            None => Ok(JObject::null()),
        }
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.setSurface(surface, width, height)`: la `Surface` de la vista del vídeo (`null` al
/// destruirse) y su tamaño en píxeles.
// SAFETY: como en `init`; `surface` es una referencia local válida durante la llamada.
#[unsafe(no_mangle)]
pub extern "system" fn Java_coroom_android_Native_setSurface<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    surface: JObject<'caller>,
    width: jint,
    height: jint,
) {
    env.with_env(|env| -> Result<(), BridgeError> {
        let app = crate::app()?;
        let size = (
            u32::try_from(width).unwrap_or(0),
            u32::try_from(height).unwrap_or(0),
        );
        crate::surface::set(env, &surface, app.devices())?;
        *app.devices()
            .view
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = size;
        if let Some(guest) = app.guest() {
            guest.set_view(size.0, size.1);
        }
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.touch(actionMasked, actionPointerId, ids, xs, ys, eventTimeMs): Long`: un
/// `MotionEvent` del tacto sobre el vídeo. Devuelve cuándo llamar a `touchTick` (reloj de
/// `getEventTime()`) o `-1`.
// SAFETY: como en `init`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_coroom_android_Native_touch<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    action_masked: jint,
    action_pointer_id: jint,
    ids: JIntArray<'caller>,
    xs: JFloatArray<'caller>,
    ys: JFloatArray<'caller>,
    event_time_ms: jlong,
) -> jlong {
    env.with_env(|env| -> Result<jlong, BridgeError> {
        let Some(guest) = crate::app()?.guest() else {
            return Ok(-1);
        };
        let count = ids.len(env)?;
        let mut id = vec![0; count];
        let mut x = vec![0.0; count];
        let mut y = vec![0.0; count];
        ids.get_region(env, 0, &mut id)?;
        xs.get_region(env, 0, &mut x)?;
        ys.get_region(env, 0, &mut y)?;
        let pointers: Vec<TouchPointer> = (0..count)
            .map(|i| TouchPointer {
                id: id[i],
                x: x[i],
                y: y[i],
            })
            .collect();
        let deadline = guest.touch(
            action_masked,
            action_pointer_id,
            &pointers,
            u64::try_from(event_time_ms).unwrap_or(0),
        );
        Ok(deadline_to_java(deadline))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.touchTick(timeMs): Long`: decide el gesto con el dedo quieto; el siguiente plazo o `-1`.
// SAFETY: como en `init`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_coroom_android_Native_touchTick<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    time_ms: jlong,
) -> jlong {
    env.with_env(|_env| -> Result<jlong, BridgeError> {
        let Some(guest) = crate::app()?.guest() else {
            return Ok(-1);
        };
        Ok(deadline_to_java(
            guest.touch_tick(u64::try_from(time_ms).unwrap_or(0)),
        ))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.key(deviceId, keyCode, scanCode, down, fromPad, eventTimeMs): Boolean`: una tecla del
/// teclado o un botón de un mando; `true` si se la quedó la sala.
// SAFETY: como en `init`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_coroom_android_Native_key<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    device_id: jint,
    key_code: jint,
    scan_code: jint,
    down: jboolean,
    from_pad: jboolean,
    event_time_ms: jlong,
) -> jboolean {
    env.with_env(|_env| -> Result<jboolean, BridgeError> {
        let Some(guest) = crate::app()?.guest() else {
            return Ok(false);
        };
        if from_pad && guest.pad_key(device_id, key_code, down, event_at(event_time_ms)) {
            return Ok(true);
        }
        Ok(guest.key(key_code, scan_code, down))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.text(text): Boolean`: texto del teclado en pantalla que no es ninguna tecla (ñ, á,
/// emoji); `true` si se mandó algo a la sala.
// SAFETY: como en `init`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_coroom_android_Native_text<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    text: JString<'caller>,
) -> jboolean {
    env.with_env(|env| -> Result<jboolean, BridgeError> {
        let Some(guest) = crate::app()?.guest() else {
            return Ok(false);
        };
        Ok(guest.text(&text.try_to_string(env)?))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.mouseMove(dx, dy)`: movimiento relativo del ratón capturado (`SOURCE_MOUSE_RELATIVE`),
/// con los restos de menos de una unidad guardados para el siguiente.
// SAFETY: como en `init`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_coroom_android_Native_mouseMove<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    dx: jfloat,
    dy: jfloat,
) {
    env.with_env(|_env| -> Result<(), BridgeError> {
        if let Some(guest) = crate::app()?.guest() {
            guest.mouse_move(dx, dy);
        }
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.mouseButton(button, down): Boolean`: un botón del ratón capturado
/// (`MotionEvent.BUTTON_*`); `false` si no es de ratón o no se está en una sala.
// SAFETY: como en `init`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_coroom_android_Native_mouseButton<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    button: jint,
    down: jboolean,
) -> jboolean {
    env.with_env(|_env| -> Result<jboolean, BridgeError> {
        let Some(guest) = crate::app()?.guest() else {
            return Ok(false);
        };
        Ok(guest.mouse_button(button, down))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.mouseWheel(h, v)`: la rueda en muescas de Android (`AXIS_HSCROLL`, `AXIS_VSCROLL`).
// SAFETY: como en `init`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_coroom_android_Native_mouseWheel<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    horizontal: jfloat,
    vertical: jfloat,
) {
    env.with_env(|_env| -> Result<(), BridgeError> {
        if let Some(guest) = crate::app()?.guest() {
            guest.mouse_wheel(horizontal, vertical);
        }
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.mouseRelease()`: suelta los botones del ratón (se perdió la captura del puntero, la
/// sala dejó de tener el foco, se abrió un panel o se sale).
// SAFETY: como en `init`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_coroom_android_Native_mouseRelease<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
) {
    env.with_env(|_env| -> Result<(), BridgeError> {
        if let Some(guest) = crate::app()?.guest() {
            guest.mouse_release();
        }
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.padMotion(deviceId, axes, eventTimeMs): Boolean`: los ejes de un mando
/// (`axes[i] = getAxisValue(i)`).
// SAFETY: como en `init`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_coroom_android_Native_padMotion<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    device_id: jint,
    axes: JFloatArray<'caller>,
    event_time_ms: jlong,
) -> jboolean {
    env.with_env(|env| -> Result<jboolean, BridgeError> {
        let Some(guest) = crate::app()?.guest() else {
            return Ok(false);
        };
        let count = axes.len(env)?;
        let mut values = vec![0.0; count];
        axes.get_region(env, 0, &mut values)?;
        Ok(guest.pad_motion(device_id, &values, event_at(event_time_ms)))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.padAdded(deviceId, vendorId, productId, name, descriptor)`.
// SAFETY: como en `init`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_coroom_android_Native_padAdded<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    device_id: jint,
    vendor_id: jint,
    product_id: jint,
    name: JString<'caller>,
    _descriptor: JString<'caller>,
) {
    env.with_env(|env| -> Result<(), BridgeError> {
        let key = PadKey {
            vendor: u16::try_from(vendor_id).unwrap_or(0),
            product: u16::try_from(product_id).unwrap_or(0),
            name: name.try_to_string(env)?,
        };
        crate::app()?.pad_added(device_id, key);
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.padRemoved(deviceId)`.
// SAFETY: como en `init`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_coroom_android_Native_padRemoved<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    device_id: jint,
) {
    env.with_env(|_env| -> Result<(), BridgeError> {
        crate::app()?.pad_removed(device_id);
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.cursor(): LongArray?`: el cursor del anfitrión en píxeles de la vista,
/// `[cambio, x, y, visible (0/1), forma]`, o `null`. La aplicación lo lee en cada fotograma de la
/// pantalla (`Choreographer`) y solo redibuja si cambió el primero.
// SAFETY: como en `init`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_coroom_android_Native_cursor<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
) -> JObject<'caller> {
    env.with_env(|env| -> Result<JObject<'caller>, BridgeError> {
        let Some((seq, x, y, visible, shape)) = crate::app()?.guest().and_then(|g| g.cursor())
        else {
            return Ok(JObject::null());
        };
        let values = [
            i64::try_from(seq).unwrap_or(i64::MAX),
            x.round() as i64,
            y.round() as i64,
            i64::from(visible),
            i64::try_from(shape).unwrap_or(i64::MAX),
        ];
        let array = env.new_long_array(values.len())?;
        array.set_region(env, 0, &values)?;
        Ok(array.into())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.cursorShape(shapeId): IntArray?`: `[ancho, alto, punto activo x, punto activo y,
/// escala × 1000, píxeles ARGB…]`, o `null` si no llegó. La escala es la del vídeo en la vista.
// SAFETY: como en `init`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_coroom_android_Native_cursorShape<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    shape_id: jlong,
) -> JObject<'caller> {
    env.with_env(|env| -> Result<JObject<'caller>, BridgeError> {
        let shape = crate::app()?
            .guest()
            .and_then(|guest| guest.cursor_shape(u64::try_from(shape_id).unwrap_or(0)));
        let Some((width, height, scale, hot_x, hot_y, pixels)) = shape else {
            return Ok(JObject::null());
        };
        let mut values = Vec::with_capacity(5 + pixels.len());
        values.extend([
            i32::try_from(width).unwrap_or(0),
            i32::try_from(height).unwrap_or(0),
            i32::try_from(hot_x).unwrap_or(0),
            i32::try_from(hot_y).unwrap_or(0),
            (scale * 1000.0).round() as i32,
        ]);
        values.extend(pixels);
        let array = env.new_int_array(values.len())?;
        array.set_region(env, 0, &values)?;
        Ok(array.into())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// El plazo del tacto para Java: `-1` si no hay.
fn deadline_to_java(deadline: Option<u64>) -> jlong {
    deadline.map_or(-1, |ms| i64::try_from(ms).unwrap_or(i64::MAX))
}

/// El `Instant` de un evento con su `getEventTime()`.
fn event_at(event_time_ms: jlong) -> std::time::Instant {
    match crate::surface::uptime_ms() {
        Some(now) => coroom_session::input::android::event_instant(
            u64::try_from(event_time_ms).unwrap_or(0),
            now,
        ),
        None => std::time::Instant::now(),
    }
}

/// El secreto en un búfer que se borra al soltarlo.
fn zeroize_bytes(bytes: Vec<u8>) -> coroom_session::account::Zeroizing<Vec<u8>> {
    coroom_session::account::Zeroizing::new(bytes)
}
