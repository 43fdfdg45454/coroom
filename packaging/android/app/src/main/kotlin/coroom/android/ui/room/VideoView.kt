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
 */
@SuppressLint("ViewConstructor")
class VideoView(context: Context) : FrameLayout(context) {
    private val surface = SurfaceView(context)
    private val cursor = CursorView(context)
    val keyCapture = KeyCaptureView(context)

    /** Solo con la sala abierta (no durante «Entrando» ni con el error). */
    var inputEnabled = false

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
    }

    private fun onVideoTouch(view: View, event: MotionEvent): Boolean {
        if (!inputEnabled || !event.isFromSource(android.view.InputDevice.SOURCE_TOUCHSCREEN)) return false
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

    /** Empieza a seguir el cursor del anfitrión, un sondeo por fotograma. */
    fun startCursor() = cursor.start()

    fun stopCursor() = cursor.stop()

    fun release() {
        handler.removeCallbacksAndMessages(tickToken)
        cursor.stop()
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
        }
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
 * (que la actividad pasa a Rust en `dispatchKeyEvent`); lo que llegue como texto se convierte en
 * teclas con el mapa del teclado virtual.
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
                val events = text?.toString()?.toCharArray()?.let(keyMap::getEvents) ?: return true
                events.forEach(::sendKey)
                return true
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
