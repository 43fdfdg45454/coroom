package coroom.android.ui.room

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Matrix
import android.graphics.Paint
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.text.InputType
import android.view.Choreographer
import android.view.InputDevice
import android.view.KeyCharacterMap
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.ViewGroup
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import android.widget.FrameLayout
import coroom.android.Native

/**
 * El vídeo de la sala: la [SurfaceView] en la que Rust decodifica con MediaCodec, el cursor del
 * anfitrión dibujado encima ([CursorView]) y una vista invisible que recibe el teclado en pantalla
 * ([KeyCaptureView]). Los toques sobre el vídeo van tal cual a `Native.touch`.
 *
 * Un ratón o un touchpad conectado al teléfono captura el puntero con un clic en el vídeo (como el
 * modo inmersivo del escritorio; ese clic no llega al juego): desde entonces su movimiento
 * relativo, sus botones y su rueda van a `Native.mouse*` y no pasan por el tacto. La captura se
 * suelta al abrir un panel, al salir o al perder el foco la ventana, y los botones pulsados se
 * sueltan con ella (`Native.mouseRelease`).
 */
@SuppressLint("ViewConstructor")
class VideoView(context: Context) : FrameLayout(context) {
    private val surface = SurfaceView(context)
    private val cursor = CursorView(context)
    val keyCapture = KeyCaptureView(context)

    /** Solo con la sala abierta (no durante «Entrando» ni con el error). */
    var inputEnabled = false

    /**
     * Si el ratón puede capturar el puntero ahora: dentro de la sala, sin panel ni diálogo abierto
     * y con permiso de teclado y ratón. Al pasar a `false` se suelta la captura.
     */
    var mouseAllowed = false
        set(value) {
            val was = field
            field = value
            if (was && !value) releaseMouse()
        }

    /** Los botones del ratón (`MotionEvent.BUTTON_*`) tal como los vio la última vez la captura. */
    private var mouseButtons = 0

    /** El dedo que mueve el puntero en un touchpad capturado (`SOURCE_TOUCHPAD`, absoluto). */
    private var padPointer = -1
    private var padX = 0f
    private var padY = 0f

    private val handler = Handler(Looper.getMainLooper())
    private val tickToken = Any()

    init {
        setBackgroundColor(android.graphics.Color.BLACK)
        val fill = LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT)
        addView(surface, fill)
        addView(cursor, LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
        addView(keyCapture, LayoutParams(1, 1))
        surface.holder.addCallback(object : SurfaceHolder.Callback {
            override fun surfaceCreated(holder: SurfaceHolder) = Unit

            override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
                Native.setSurface(holder.surface, width, height)
            }

            override fun surfaceDestroyed(holder: SurfaceHolder) {
                // Dentro del callback: Rust suelta la ventana antes de que Android la destruya.
                Native.setSurface(null, 0, 0)
            }
        })
        setOnTouchListener { view, event -> onVideoTouch(view, event) }

        // Los eventos capturados van a la vista con el foco: esta o, con el teclado en pantalla
        // abierto, la que lo recibe.
        isFocusable = true
        isFocusableInTouchMode = true
        setDefaultFocusHighlightEnabled(false)
        setOnCapturedPointerListener { _, event -> onCapturedPointer(event) }
        keyCapture.setOnCapturedPointerListener { _, event -> onCapturedPointer(event) }
    }

    private fun onVideoTouch(view: View, event: MotionEvent): Boolean {
        if (inputEnabled && event.isFromSource(InputDevice.SOURCE_MOUSE)) {
            // El ratón sin capturar no mueve nada: el primer clic captura el puntero.
            if (event.actionMasked == MotionEvent.ACTION_DOWN) requestMouseCapture(event)
            return true
        }
        if (!inputEnabled || !event.isFromSource(InputDevice.SOURCE_TOUCHSCREEN)) return false
        val count = event.pointerCount
        val ids = IntArray(count) { event.getPointerId(it) }
        val xs = FloatArray(count) { event.getX(it) }
        val ys = FloatArray(count) { event.getY(it) }
        val masked = event.actionMasked
        val actionPointer = if (masked == MotionEvent.ACTION_POINTER_DOWN || masked == MotionEvent.ACTION_POINTER_UP) {
            event.getPointerId(event.actionIndex)
        } else {
            -1
        }
        schedule(Native.touch(masked, actionPointer, ids, xs, ys, event.eventTime))
        if (masked == MotionEvent.ACTION_UP) view.performClick()
        return true
    }

    /** Los gestos con tiempo (toque largo, doble toque) piden que se les avise en un plazo. */
    private fun schedule(deadline: Long) {
        handler.removeCallbacksAndMessages(tickToken)
        if (deadline < 0) return
        handler.postAtTime({ schedule(Native.touchTick(SystemClock.uptimeMillis())) }, tickToken, deadline)
    }

    override fun performClick(): Boolean = super.performClick()

    // --- Ratón y touchpad capturados --------------------------------------------------------

    private fun requestMouseCapture(event: MotionEvent) {
        if (!mouseAllowed || hasPointerCapture()) return
        // Sin el foco los eventos capturados no llegarían a nadie y el puntero se quedaría oculto.
        if (!hasFocus() && !requestFocus()) return
        // Los botones de este clic no son del juego: soltarlos luego no manda nada.
        mouseButtons = event.buttonState and MOUSE_BUTTONS
        padPointer = -1
        requestPointerCapture()
    }

    private fun onCapturedPointer(event: MotionEvent): Boolean {
        if (!mouseAllowed) {
            releaseMouse()
            return true
        }
        when {
            event.isFromSource(InputDevice.SOURCE_MOUSE_RELATIVE) -> relativeMotion(event)
            event.isFromSource(InputDevice.SOURCE_TOUCHPAD) -> touchpadMotion(event)
        }
        syncButtons(event)
        if (event.actionMasked == MotionEvent.ACTION_SCROLL) {
            val h = event.getAxisValue(MotionEvent.AXIS_HSCROLL)
            val v = event.getAxisValue(MotionEvent.AXIS_VSCROLL)
            if (h != 0f || v != 0f) runCatching { Native.mouseWheel(h, v) }
        }
        return true
    }

    /** Un ratón capturado da desplazamientos; los movimientos agrupados van en el historial. */
    private fun relativeMotion(event: MotionEvent) {
        val masked = event.actionMasked
        if (masked != MotionEvent.ACTION_MOVE && masked != MotionEvent.ACTION_HOVER_MOVE) return
        var dx = event.x
        var dy = event.y
        for (i in 0 until event.historySize) {
            dx += event.getHistoricalX(i)
            dy += event.getHistoricalY(i)
        }
        if (dx != 0f || dy != 0f) runCatching { Native.mouseMove(dx, dy) }
    }

    /**
     * Un touchpad capturado da la posición absoluta de cada dedo en su superficie: el primero que
     * se apoya mueve el puntero con su desplazamiento, pasado a milímetros con la resolución del
     * touchpad y a unidades de ratón con [TOUCHPAD_COUNTS_PER_MM].
     */
    private fun touchpadMotion(event: MotionEvent) {
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> trackPadPointer(event, 0)
            MotionEvent.ACTION_POINTER_UP -> if (event.getPointerId(event.actionIndex) == padPointer) {
                trackPadPointer(event, if (event.actionIndex == 0) 1 else 0)
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> padPointer = -1
            MotionEvent.ACTION_MOVE -> {
                val index = event.findPointerIndex(padPointer)
                if (index < 0) {
                    trackPadPointer(event, 0)
                    return
                }
                var dx = 0f
                var dy = 0f
                var lastX = padX
                var lastY = padY
                for (i in 0 until event.historySize) {
                    val x = event.getHistoricalX(index, i)
                    val y = event.getHistoricalY(index, i)
                    dx += x - lastX
                    dy += y - lastY
                    lastX = x
                    lastY = y
                }
                padX = event.getX(index)
                padY = event.getY(index)
                dx += padX - lastX
                dy += padY - lastY
                val scaleX = touchpadScale(event, MotionEvent.AXIS_X)
                val scaleY = touchpadScale(event, MotionEvent.AXIS_Y)
                if (dx != 0f || dy != 0f) runCatching { Native.mouseMove(dx * scaleX, dy * scaleY) }
            }
        }
    }

    private fun trackPadPointer(event: MotionEvent, index: Int) {
        if (index >= event.pointerCount) {
            padPointer = -1
            return
        }
        padPointer = event.getPointerId(index)
        padX = event.getX(index)
        padY = event.getY(index)
    }

    /** Unidades de ratón por unidad del touchpad (1 si el touchpad no dice su resolución). */
    private fun touchpadScale(event: MotionEvent, axis: Int): Float {
        val resolution = event.device?.getMotionRange(axis, InputDevice.SOURCE_TOUCHPAD)?.resolution ?: 0f
        return if (resolution > 0f) TOUCHPAD_COUNTS_PER_MM / resolution else 1f
    }

    /** Los botones que cambiaron desde el último evento, uno a uno (Rust no repite los que ya están). */
    private fun syncButtons(event: MotionEvent) {
        val now = when (event.actionMasked) {
            MotionEvent.ACTION_BUTTON_PRESS -> mouseButtons or event.actionButton
            MotionEvent.ACTION_BUTTON_RELEASE -> mouseButtons and event.actionButton.inv()
            else -> event.buttonState
        } and MOUSE_BUTTONS
        val changed = now xor mouseButtons
        mouseButtons = now
        if (changed == 0) return
        for (button in MOUSE_BUTTON_LIST) {
            if (changed and button != 0) runCatching { Native.mouseButton(button, now and button != 0) }
        }
    }

    override fun onPointerCaptureChange(hasCapture: Boolean) {
        super.onPointerCaptureChange(hasCapture)
        if (!hasCapture) forgetMouse()
    }

    /** Suelta la captura del puntero y los botones del ratón (al abrir un panel, al salir…). */
    fun releaseMouse() {
        if (hasPointerCapture()) releasePointerCapture()
        forgetMouse()
    }

    private fun forgetMouse() {
        mouseButtons = 0
        padPointer = -1
        runCatching { Native.mouseRelease() }
    }

    /** Empieza a seguir el cursor del anfitrión, un sondeo por fotograma. */
    fun startCursor() = cursor.start()

    fun stopCursor() = cursor.stop()

    fun release() {
        handler.removeCallbacksAndMessages(tickToken)
        cursor.stop()
        releaseMouse()
    }

    /** Enseña u oculta el teclado en pantalla. */
    fun showKeyboard(show: Boolean) {
        val imm = context.getSystemService(InputMethodManager::class.java)
        if (show) {
            keyCapture.requestFocus()
            imm.showSoftInput(keyCapture, 0)
        } else {
            imm.hideSoftInputFromWindow(keyCapture.windowToken, 0)
            keyCapture.clearFocus()
            // Con el puntero capturado, sus eventos siguen necesitando una vista con el foco.
            if (hasPointerCapture()) requestFocus()
        }
    }

    private companion object {
        /** Los botones de ratón que entiende el anfitrión. */
        val MOUSE_BUTTON_LIST = intArrayOf(
            MotionEvent.BUTTON_PRIMARY,
            MotionEvent.BUTTON_SECONDARY,
            MotionEvent.BUTTON_TERTIARY,
            MotionEvent.BUTTON_BACK,
            MotionEvent.BUTTON_FORWARD,
        )
        val MOUSE_BUTTONS = MOUSE_BUTTON_LIST.fold(0) { all, button -> all or button }

        /** Unidades de ratón por milímetro del touchpad (un ratón de unos 400 ppp). */
        const val TOUCHPAD_COUNTS_PER_MM = 16f
    }
}

/**
 * El cursor del anfitrión (`Native.cursor`), redibujado solo cuando cambia. Las formas llegan
 * una vez (`Native.cursorShape`) y se guardan por su id.
 */
class CursorView(context: Context) : View(context) {
    private class Shape(val bitmap: Bitmap, val hotX: Int, val hotY: Int, val scale: Float)

    private val shapes = HashMap<Long, Shape?>()
    private val paint = Paint(Paint.FILTER_BITMAP_FLAG)
    private val matrix = Matrix()
    private var lastSeq = Long.MIN_VALUE
    private var shape: Shape? = null
    private var x = 0f
    private var y = 0f
    private var visible = false
    private var running = false

    private val frame = object : Choreographer.FrameCallback {
        override fun doFrame(frameTimeNanos: Long) {
            if (!running) return
            poll()
            Choreographer.getInstance().postFrameCallback(this)
        }
    }

    init {
        isClickable = false
        isFocusable = false
        importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_NO
    }

    fun start() {
        if (running) return
        running = true
        Choreographer.getInstance().postFrameCallback(frame)
    }

    fun stop() {
        running = false
        Choreographer.getInstance().removeFrameCallback(frame)
        if (visible) {
            visible = false
            invalidate()
        }
        lastSeq = Long.MIN_VALUE
    }

    private fun poll() {
        val state = runCatching { Native.cursor() }.getOrNull()
        if (state == null || state.size < 5) {
            if (visible) {
                visible = false
                invalidate()
            }
            return
        }
        if (state[0] == lastSeq) return
        lastSeq = state[0]
        x = state[1].toFloat()
        y = state[2].toFloat()
        visible = state[3] != 0L
        shape = shapes.getOrPut(state[4]) { loadShape(state[4]) }
        invalidate()
    }

    private fun loadShape(id: Long): Shape? {
        val data = runCatching { Native.cursorShape(id) }.getOrNull() ?: return null
        if (data.size < 5) return null
        val width = data[0]
        val height = data[1]
        if (width <= 0 || height <= 0 || data.size < 5 + width * height) return null
        val bitmap = Bitmap.createBitmap(data, 5, width, width, height, Bitmap.Config.ARGB_8888)
        return Shape(bitmap, data[2], data[3], (data[4] / 1000f).takeIf { it > 0f } ?: 1f)
    }

    override fun onDraw(canvas: Canvas) {
        val current = shape ?: return
        if (!visible) return
        matrix.reset()
        matrix.postScale(current.scale, current.scale)
        matrix.postTranslate(x - current.hotX * current.scale, y - current.hotY * current.scale)
        canvas.drawBitmap(current.bitmap, matrix, paint)
    }
}

/**
 * Recibe el teclado en pantalla sin enseñar ningún campo: con `TYPE_NULL` el teclado manda teclas
 * (que la actividad pasa a Rust en `dispatchKeyEvent`). Lo que llegue como texto (`commitText`):
 * las letras sin acento, las cifras, el espacio, Intro y el tabulador como sus teclas, con el mapa
 * del teclado virtual (así los juegos que leen teclas las ven); el resto (ñ, á, signos que dependen
 * de la distribución del anfitrión, emoji) como texto con `Native.text`, en el mismo orden.
 */
class KeyCaptureView(context: Context) : View(context) {
    init {
        isFocusable = true
        isFocusableInTouchMode = true
        importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_NO
    }

    override fun onCheckIsTextEditor(): Boolean = true

    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
        outAttrs.inputType = InputType.TYPE_NULL
        outAttrs.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN or EditorInfo.IME_FLAG_NO_EXTRACT_UI or
            EditorInfo.IME_ACTION_NONE
        return object : BaseInputConnection(this, false) {
            private val keyMap = KeyCharacterMap.load(KeyCharacterMap.VIRTUAL_KEYBOARD)

            override fun commitText(text: CharSequence?, newCursorPosition: Int): Boolean {
                sendText(text?.toString().orEmpty())
                return true
            }

            private fun sendText(text: String) {
                val pending = StringBuilder()
                var i = 0
                while (i < text.length) {
                    val codePoint = text.codePointAt(i)
                    val events = if (codePoint.asKey()) keyMap.getEvents(charArrayOf(text[i])) else null
                    if (events != null) {
                        flushText(pending)
                        events.forEach(::sendKey)
                    } else {
                        pending.appendCodePoint(codePoint)
                    }
                    i += Character.charCount(codePoint)
                }
                flushText(pending)
            }

            private fun Int.asKey(): Boolean =
                this in 'a'.code..'z'.code || this in 'A'.code..'Z'.code || this in '0'.code..'9'.code ||
                    this == ' '.code || this == '\n'.code || this == '\t'.code

            private fun flushText(pending: StringBuilder) {
                if (pending.isEmpty()) return
                val text = pending.toString()
                pending.setLength(0)
                runCatching { Native.text(text) }
            }

            override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
                repeat(beforeLength.coerceIn(0, 64)) {
                    val now = SystemClock.uptimeMillis()
                    sendKey(KeyEvent(now, now, KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_DEL, 0))
                    sendKey(KeyEvent(now, now, KeyEvent.ACTION_UP, KeyEvent.KEYCODE_DEL, 0))
                }
                return true
            }

            private fun sendKey(event: KeyEvent) {
                if (event.action != KeyEvent.ACTION_DOWN && event.action != KeyEvent.ACTION_UP) return
                runCatching {
                    Native.key(
                        KeyCharacterMap.VIRTUAL_KEYBOARD,
                        event.keyCode,
                        event.scanCode,
                        event.action == KeyEvent.ACTION_DOWN,
                        false,
                        event.eventTime,
                    )
                }
            }
        }
    }
}
