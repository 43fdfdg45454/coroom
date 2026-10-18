package coroom.android

import android.view.Surface

/**
 * Puente JNI con `libcoroom_android.so` (crate `apps/android`). Toda la lógica, la red, el vídeo y
 * el audio están en Rust; Kotlin pone la interfaz y le pasa lo que solo Android sabe (la superficie
 * del vídeo, los toques, las teclas, los mandos, la red y las raíces de confianza).
 *
 * Los nombres y las firmas son el contrato con `apps/android/src/jni.rs`: no se cambian sin cambiar
 * los dos lados (R8 tampoco los toca, ver `proguard-rules.pro`).
 */
object Native {
    init {
        System.loadLibrary("coroom_android")
    }

    /** Una vez por proceso. `secret`: 32 bytes del Keystore. `null` si va bien; si no, el error en español. */
    @JvmStatic external fun init(filesDir: String, secret: ByteArray, device: String): String?

    /** Bloquea (red): fuera del hilo principal. `{"ok": …}` o `{"error": "texto"}`. */
    @JvmStatic external fun call(method: String, args: String): String

    /** Espera hasta `timeoutMs` el siguiente evento en JSON, o `null`. */
    @JvmStatic external fun nextEvent(timeoutMs: Long): String?

    /** La superficie del vídeo (`null` al destruirse), con su tamaño en píxeles. */
    @JvmStatic external fun setSurface(surface: Surface?, width: Int, height: Int)

    /**
     * Un MotionEvent de la pantalla táctil sobre el vídeo, en píxeles de la vista. Devuelve el
     * siguiente plazo (reloj de `getEventTime()`, `uptimeMillis`) en el que llamar a [touchTick], o -1.
     */
    @JvmStatic external fun touch(
        actionMasked: Int,
        actionPointerId: Int,
        ids: IntArray,
        xs: FloatArray,
        ys: FloatArray,
        eventTimeMs: Long,
    ): Long

    /** Ha llegado el plazo de [touch] (toque largo, doble toque…). Devuelve el siguiente o -1. */
    @JvmStatic external fun touchTick(timeMs: Long): Long

    /** El cursor del anfitrión: `[changeSeq, x, y, visible (0/1), shapeId]` en px de la superficie, o `null`. */
    @JvmStatic external fun cursor(): LongArray?

    /** La forma `shapeId`: `[ancho, alto, hotX, hotY, escala×1000, píxeles ARGB…]`, o `null`. */
    @JvmStatic external fun cursorShape(shapeId: Long): IntArray?

    /** Una tecla (teclado o botón de mando). `true` si la usó. */
    @JvmStatic external fun key(
        deviceId: Int,
        keyCode: Int,
        scanCode: Int,
        down: Boolean,
        fromPad: Boolean,
        eventTimeMs: Long,
    ): Boolean

    /** Ejes de un mando: `axes[i]` = `getAxisValue(i)` para los 48 `AXIS_*`. `true` si los usó. */
    @JvmStatic external fun padMotion(deviceId: Int, axes: FloatArray, eventTimeMs: Long): Boolean

    /** Se conectó un mando. */
    @JvmStatic external fun padAdded(deviceId: Int, vendorId: Int, productId: Int, name: String, descriptor: String)

    /** Se desconectó un mando. */
    @JvmStatic external fun padRemoved(deviceId: Int)
}
