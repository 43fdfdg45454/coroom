package coroom.android.ui.room

import android.Manifest
import android.annotation.SuppressLint
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.view.InputDevice
import android.view.KeyCharacterMap
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.core.content.ContextCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.repeatOnLifecycle
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import coroom.android.Native
import coroom.android.container
import coroom.android.platform.LogShare
import coroom.android.ui.theme.CoroomTheme
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

/**
 * La sala: en horizontal, a pantalla completa y con la pantalla encendida. Pasa a Rust los
 * toques del vídeo, el ratón, las teclas, el texto del teclado en pantalla y los mandos mientras
 * se está dentro.
 */
class RoomActivity : ComponentActivity() {
    private val room: RoomViewModel by viewModels {
        viewModelFactory {
            initializer {
                RoomViewModel(
                    container,
                    intent.getStringExtra(EXTRA_CODE).orEmpty(),
                    intent.getStringExtra(EXTRA_HOST_NAME),
                )
            }
        }
    }

    private lateinit var video: VideoView

    /** Qué hacer al contestar el permiso de micrófono: al entrar o desde la barra. */
    private var micForJoin = true

    private val micPermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (micForJoin) {
            lifecycleScope.launch { startJoin(granted) }
        } else {
            room.micPermissionResult(granted)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        window.attributes = window.attributes.apply {
            layoutInDisplayCutoutMode = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
            } else {
                WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES
            }
        }
        hideSystemBars()
        video = VideoView(this)

        setContent {
            CoroomTheme(darkTheme = true) {
                val state by room.state.collectAsStateWithLifecycle()
                SideEffect {
                    video.inputEnabled = state.phase == RoomPhase.InRoom
                    // Un panel, el diálogo de salir o perder el permiso sueltan el ratón capturado.
                    video.mouseAllowed = state.phase == RoomPhase.InRoom && state.panel == Panel.None &&
                        !state.confirmLeave && state.permissions?.keyboardMouse != false
                }
                RoomScreen(
                    state = state,
                    video = video,
                    actions = RoomActions(
                        onToggleMic = room::toggleMic,
                        onToggleKeyboard = { room.setKeyboardVisible(!room.state.value.keyboardVisible) },
                        onCycleTouchMode = room::cycleTouchMode,
                        onTogglePanel = room::togglePanel,
                        onClosePanel = room::closePanel,
                        onAskLeave = room::askLeave,
                        onLeave = room::leave,
                        onSendChat = room::sendChat,
                        onClearNotice = room::clearNotice,
                        onRevealOverlay = room::revealOverlay,
                        onShareLog = room::shareLog,
                        onToggleVideo = room::toggleVideo,
                    ),
                    onCancelJoin = room::leave,
                    onRetry = room::retry,
                    onClose = room::close,
                )
            }
        }

        lifecycleScope.launch {
            repeatOnLifecycle(Lifecycle.State.CREATED) {
                room.commands.collect { command ->
                    when (command) {
                        RoomCommand.Finish -> finish()
                        RoomCommand.RequestMic -> {
                            micForJoin = false
                            micPermission.launch(Manifest.permission.RECORD_AUDIO)
                        }
                        is RoomCommand.ShareLog -> shareLog(command.paths)
                    }
                }
            }
        }

        // Una vez por sala: el ViewModel sobrevive a los cambios de configuración.
        if (!room.started) {
            lifecycleScope.launch {
                val joinMuted = container.settings.settings.first().joinMuted
                when {
                    hasMicPermission() -> startJoin(true)
                    // Quien entra en silencio no necesita el permiso ahora: se pide al activar el micro.
                    joinMuted -> startJoin(false)
                    else -> {
                        micForJoin = true
                        micPermission.launch(Manifest.permission.RECORD_AUDIO)
                    }
                }
            }
        }
    }

    private suspend fun startJoin(mic: Boolean) {
        val joinMuted = container.settings.settings.first().joinMuted
        room.start(mic = mic, joinMuted = joinMuted)
    }

    /** La hoja de compartir con el registro (la sala sigue abierta detrás). */
    private fun shareLog(paths: List<String>) {
        val chooser = LogShare.chooser(this, paths)
        val shown = chooser != null && runCatching { startActivity(chooser) }.isSuccess
        if (!shown) room.logShareFailed(empty = chooser == null)
    }

    private fun hasMicPermission(): Boolean =
        ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED

    private fun hideSystemBars() {
        WindowCompat.getInsetsController(window, window.decorView).apply {
            systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            hide(WindowInsetsCompat.Type.systemBars())
        }
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) hideSystemBars() else video.releaseMouse()
    }

    override fun onResume() {
        super.onResume()
        video.startCursor()
    }

    override fun onPause() {
        video.stopCursor()
        video.releaseMouse()
        super.onPause()
    }

    override fun onDestroy() {
        video.release()
        super.onDestroy()
    }

    // --- Teclas y mandos -----------------------------------------------------------------

    private val inRoom: Boolean get() = room.state.value.phase == RoomPhase.InRoom

    /** Con el chat abierto el teclado escribe en el chat; los mandos siguen yendo al juego. */
    private val keyboardToGame: Boolean get() = room.state.value.panel != Panel.Chat

    // Es Activity.dispatchKeyEvent, público; lint lo marca porque androidx.core.app.ComponentActivity
    // lo vuelve a declarar con @RestrictTo para su propio uso.
    @SuppressLint("RestrictedApi")
    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        if (inRoom && event.keyCode !in SYSTEM_KEYS) {
            // Los botones laterales de un ratón también llegan como Atrás y Adelante: no abren
            // «¿Salir de la sala?» ni van como teclas (con el puntero capturado ya van al juego
            // como botones). Con un panel abierto, Atrás lo cierra como siempre.
            if (fromMouse(event) && mouseKeyIsButton(event.keyCode)) return true
            val fromPad = event.isFromSource(InputDevice.SOURCE_GAMEPAD) || event.isFromSource(InputDevice.SOURCE_JOYSTICK)
            // Atrás del sistema (gesto o tecla del teléfono) abre «¿Salir de la sala?»; el de un mando va al juego.
            val systemBack = event.keyCode == KeyEvent.KEYCODE_BACK && !fromPad
            if (!systemBack && (fromPad || keyboardToGame)) {
                val down = event.action == KeyEvent.ACTION_DOWN
                if (event.action == KeyEvent.ACTION_DOWN || event.action == KeyEvent.ACTION_UP) {
                    // Las repeticiones automáticas no se mandan: el anfitrión repite él solo.
                    if (down && event.repeatCount > 0) return true
                    val used = runCatching {
                        Native.key(event.deviceId, event.keyCode, event.scanCode, down, fromPad, event.eventTime)
                    }.getOrDefault(false)
                    if (used) return true
                    // Una tecla sin equivalente que escribe un carácter (ñ, ç…): va como texto.
                    if (down && !fromPad && sendChar(event.unicodeChar)) return true
                } else if (event.action == KeyEvent.ACTION_MULTIPLE && event.keyCode == KeyEvent.KEYCODE_UNKNOWN) {
                    // El texto que el teclado en pantalla confirma de una vez (vista con TYPE_NULL).
                    val text = multipleCharacters(event)
                    if (!text.isNullOrEmpty() && runCatching { Native.text(text) }.getOrDefault(false)) return true
                }
            }
        }
        return super.dispatchKeyEvent(event)
    }

    private fun mouseKeyIsButton(keyCode: Int): Boolean =
        keyCode == KeyEvent.KEYCODE_FORWARD || keyCode == KeyEvent.KEYCODE_BACK && room.state.value.panel == Panel.None

    private fun fromMouse(event: KeyEvent): Boolean =
        event.isFromSource(InputDevice.SOURCE_MOUSE) || event.isFromSource(InputDevice.SOURCE_MOUSE_RELATIVE)

    /** Un carácter de `KeyEvent.getUnicodeChar()` como texto; no los de control ni los acentos muertos. */
    private fun sendChar(unicodeChar: Int): Boolean {
        if (unicodeChar == 0 || unicodeChar and KeyCharacterMap.COMBINING_ACCENT != 0) return false
        if (!Character.isValidCodePoint(unicodeChar) || Character.isISOControl(unicodeChar)) return false
        return runCatching { Native.text(String(Character.toChars(unicodeChar))) }.getOrDefault(false)
    }

    // `getCharacters()` está en desuso desde la API 29 porque el sistema ya no lo usa, pero es lo que
    // manda BaseInputConnection cuando la vista de TYPE_NULL recibe texto que no son teclas.
    @Suppress("DEPRECATION")
    private fun multipleCharacters(event: KeyEvent): String? = event.characters

    override fun dispatchGenericMotionEvent(event: MotionEvent): Boolean {
        if (inRoom && event.isFromSource(InputDevice.SOURCE_JOYSTICK) && event.actionMasked == MotionEvent.ACTION_MOVE) {
            val axes = FloatArray(AXIS_COUNT) { event.getAxisValue(it) }
            val used = runCatching { Native.padMotion(event.deviceId, axes, event.eventTime) }.getOrDefault(false)
            if (used) return true
        }
        return super.dispatchGenericMotionEvent(event)
    }

    companion object {
        private const val EXTRA_CODE = "coroom.android.extra.ROOM_CODE"
        private const val EXTRA_HOST_NAME = "coroom.android.extra.ROOM_HOST"
        private const val AXIS_COUNT = 48

        /** Teclas que siguen siendo del teléfono dentro de la sala. */
        private val SYSTEM_KEYS = setOf(
            KeyEvent.KEYCODE_VOLUME_UP,
            KeyEvent.KEYCODE_VOLUME_DOWN,
            KeyEvent.KEYCODE_VOLUME_MUTE,
            KeyEvent.KEYCODE_POWER,
            KeyEvent.KEYCODE_HOME,
            KeyEvent.KEYCODE_APP_SWITCH,
            KeyEvent.KEYCODE_ASSIST,
            KeyEvent.KEYCODE_VOICE_ASSIST,
            KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE,
        )

        fun intent(context: Context, code: String, hostName: String?): Intent =
            Intent(context, RoomActivity::class.java)
                .putExtra(EXTRA_CODE, code)
                .putExtra(EXTRA_HOST_NAME, hostName)
    }
}
