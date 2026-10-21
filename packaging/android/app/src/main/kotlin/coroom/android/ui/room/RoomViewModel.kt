package coroom.android.ui.room

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import coroom.android.AppContainer
import coroom.android.R
import coroom.android.bridge.BridgeException
import coroom.android.bridge.ChatMessage
import coroom.android.bridge.CoroomEvent
import coroom.android.bridge.Member
import coroom.android.bridge.RoomStats
import coroom.android.bridge.TouchMode
import coroom.android.bridge.isRoomEvent
import coroom.android.platform.NetworkKind
import coroom.android.ui.UiText
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.filterIsInstance
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/** Los pasos de «Entrando en la sala» (`join_progress`). */
enum class JoinStep(val wire: String) {
    Server("server"),
    Path("path"),
    Probe("probe"),
    Keys("keys"),
}

enum class StepState { Pending, Active, Done }

/** Calidad del enlace (`quality.level`). */
enum class LinkQuality {
    Excellent,
    Good,
    Acceptable,
    Poor,
    ;

    companion object {
        fun parse(level: String): LinkQuality? = when (level.lowercase()) {
            "excellent" -> Excellent
            "good" -> Good
            "acceptable" -> Acceptable
            "poor" -> Poor
            else -> null
        }
    }
}

sealed interface RoomPhase {
    data object Joining : RoomPhase
    data object InRoom : RoomPhase

    /** No se pudo entrar o se cortó. `wasInRoom`: llegó a estar dentro. */
    data class Failed(val error: String, val wasInRoom: Boolean) : RoomPhase
}

enum class Panel { None, Connection, Chat }

/** Lo que el anfitrión deja hacer (`permissions`). */
data class InputPermissions(val keyboardMouse: Boolean, val gamepadSlot: Int?)

data class RoomUiState(
    val code: String,
    val hostName: String? = null,
    val phase: RoomPhase = RoomPhase.Joining,
    val steps: Map<JoinStep, StepState> = JoinStep.entries.associateWith { StepState.Pending },
    val relayed: Boolean? = null,
    val quality: LinkQuality? = null,
    val rttMs: Double? = null,
    val codec: String? = null,
    val codecDetail: String? = null,
    val safetyNumber: String? = null,
    val members: List<Member> = emptyList(),
    val chat: List<ChatMessage> = emptyList(),
    val unreadChat: Boolean = false,
    val chatError: String? = null,
    val reconnecting: CoroomEvent.Reconnecting? = null,
    val notice: UiText? = null,
    val noticeSeq: Int = 0,
    val micMuted: Boolean = true,
    /**
     * El vídeo apagado: el anfitrión no lo manda, el tacto, el teclado, el ratón y los mandos no
     * le llegan y la barra queda fija. No se recuerda: cada sala arranca con el vídeo encendido.
     */
    val videoOff: Boolean = false,
    /** Si se entró con micrófono (permiso concedido). */
    val micAvailable: Boolean = false,
    val touchMode: TouchMode = TouchMode.Trackpad,
    val panel: Panel = Panel.None,
    val keyboardVisible: Boolean = false,
    val stats: RoomStats? = null,
    val network: NetworkKind = NetworkKind.Unknown,
    val permissions: InputPermissions? = null,
    val confirmLeave: Boolean = false,
    /**
     * Si la barra de la sala (la lateral, los chips y la píldora entera) está a la vista: se
     * esconde sola a los [OVERLAY_HIDE_MS] sin tocarla, como la del escritorio a pantalla completa.
     */
    val overlayShown: Boolean = true,
) {
    /** La latencia de la píldora: la de `stats` si hay, si no la de `quality`. */
    val shownRttMs: Double? get() = stats?.rttMs ?: rttMs

    val shownRelayed: Boolean? get() = stats?.relayed ?: relayed

    /**
     * Lo que no deja esconder la barra: un panel (el chat) o el diálogo de salir abiertos, el
     * teclado en pantalla en uso, una reconexión en curso o el vídeo apagado (no hay imagen que
     * tapar).
     */
    val overlayPinned: Boolean
        get() = panel != Panel.None || confirmLeave || keyboardVisible || reconnecting != null || videoOff

    /** La barra se dibuja entera (si no, solo la píldora reducida a punto y latencia). */
    val overlayVisible: Boolean get() = overlayShown || overlayPinned
}

/**
 * Lo que tarda en esconderse la barra de la sala sin tocarla: el mismo tiempo que la del vídeo en
 * el escritorio (`coroom_ui::tokens::motion::BAR_HIDE`).
 */
const val OVERLAY_HIDE_MS = 2_500L

/** Lo que la actividad tiene que hacer por la sala. */
sealed interface RoomCommand {
    data object Finish : RoomCommand
    data object RequestMic : RoomCommand

    /** Abrir la hoja de compartir con estos ficheros del registro. */
    data class ShareLog(val paths: List<String>) : RoomCommand
}

class RoomViewModel(
    private val container: AppContainer,
    code: String,
    hostName: String?,
) : ViewModel() {
    private val repository = container.repository
    private val _state = MutableStateFlow(RoomUiState(code = code, hostName = hostName))
    val state: StateFlow<RoomUiState> = _state.asStateFlow()

    private val _commands = Channel<RoomCommand>(Channel.BUFFERED)
    val commands: Flow<RoomCommand> = _commands.receiveAsFlow()

    /** Si ya se pidió entrar (la actividad no vuelve a pedirlo al recrearse). */
    var started = false
        private set
    private var left = false
    private var micAllowed = false
    private var statsJob: Job? = null
    private var overlayJob: Job? = null

    init {
        repository.setInRoom(true)
        viewModelScope.launch {
            repository.events.filter { it.isRoomEvent }.collect(::onEvent)
        }
        // Alguien de la sala cambió su avatar: el chat enseña el nuevo.
        viewModelScope.launch {
            repository.events.filterIsInstance<CoroomEvent.AvatarChanged>().collect { event ->
                _state.update { state ->
                    state.copy(
                        members = state.members.map { if (it.user == event.user) it.copy(avatar = event.avatar) else it },
                    )
                }
            }
        }
        viewModelScope.launch {
            container.settings.settings.collect { settings ->
                _state.update { it.copy(touchMode = settings.touchMode) }
            }
        }
        viewModelScope.launch {
            container.network.kind.collect { kind -> _state.update { it.copy(network = kind) } }
        }
    }

    /** Entra (una vez por pantalla: girarla no vuelve a entrar). `mic`: hay permiso de micrófono. */
    fun start(mic: Boolean, joinMuted: Boolean) {
        if (started) return
        started = true
        micAllowed = mic
        _state.update { it.copy(micAvailable = mic, micMuted = joinMuted || !mic) }
        join()
    }

    private fun join() {
        val code = _state.value.code
        viewModelScope.launch {
            repository.lobbyStop()
            try {
                repository.join(code, micAllowed)
            } catch (error: BridgeException) {
                _state.update { it.copy(phase = RoomPhase.Failed(error.message.orEmpty(), wasInRoom = false)) }
            }
        }
    }

    fun retry() {
        _state.update {
            it.copy(
                phase = RoomPhase.Joining,
                steps = JoinStep.entries.associateWith { StepState.Pending },
                reconnecting = null,
                stats = null,
                videoOff = false,
            )
        }
        join()
    }

    private suspend fun onEvent(event: CoroomEvent) {
        when (event) {
            is CoroomEvent.JoinProgress -> {
                val step = JoinStep.entries.firstOrNull { it.wire == event.step } ?: return
                _state.update { state ->
                    val steps = state.steps.toMutableMap()
                    // Un paso que avanza da por hechos los anteriores.
                    JoinStep.entries.filter { it.ordinal < step.ordinal }.forEach { steps[it] = StepState.Done }
                    steps[step] = if (event.done) StepState.Done else StepState.Active
                    state.copy(steps = steps)
                }
            }
            is CoroomEvent.Joined -> {
                _state.update {
                    it.copy(
                        phase = RoomPhase.InRoom,
                        hostName = event.hostName.ifBlank { it.hostName },
                        safetyNumber = event.safetyNumber.ifBlank { null },
                        steps = JoinStep.entries.associateWith { StepState.Done },
                    )
                }
                startStats()
                // Al entrar se ve la barra (y luego se esconde sola), como al empezar el vídeo en el escritorio.
                revealOverlay()
            }
            is CoroomEvent.Connected -> _state.update { it.copy(relayed = event.relayed) }
            is CoroomEvent.Quality -> _state.update {
                it.copy(quality = LinkQuality.parse(event.level) ?: it.quality, rttMs = event.rttMs)
            }
            is CoroomEvent.Reconnecting -> _state.update { it.copy(reconnecting = event) }
            is CoroomEvent.Reconnected -> {
                _state.update { it.copy(reconnecting = null, relayed = event.relayed) }
                // Que se vea un momento que volvió la conexión antes de esconderse.
                revealOverlay()
            }
            is CoroomEvent.VideoCodec -> _state.update { it.copy(codec = event.codec, codecDetail = event.detail) }
            is CoroomEvent.Members -> _state.update { state ->
                val host = event.members.firstOrNull { it.guest == 0 }?.name
                state.copy(members = event.members, hostName = state.hostName ?: host)
            }
            is CoroomEvent.Chat -> _state.update { state ->
                val message = event.message
                state.copy(
                    chat = (state.chat + message).takeLast(MAX_CHAT),
                    unreadChat = state.unreadChat || (state.panel != Panel.Chat && !message.own),
                    chatError = if (message.own) null else state.chatError,
                )
            }
            is CoroomEvent.ChatHistory -> _state.update { it.copy(chat = event.messages.takeLast(MAX_CHAT)) }
            CoroomEvent.ChatCleared -> _state.update { it.copy(chat = emptyList(), unreadChat = false) }
            is CoroomEvent.ChatRejected -> _state.update { it.copy(chatError = event.reason) }
            is CoroomEvent.Permissions -> onPermissions(event)
            is CoroomEvent.Warning -> showNotice(UiText.Raw(event.message))
            is CoroomEvent.MoveToRoom -> {
                _state.update {
                    it.copy(
                        code = event.code,
                        hostName = event.hostName.ifBlank { it.hostName },
                        phase = RoomPhase.Joining,
                        steps = JoinStep.entries.associateWith { StepState.Pending },
                        reconnecting = null,
                        chat = emptyList(),
                        members = emptyList(),
                        permissions = null,
                    )
                }
                join()
            }
            is CoroomEvent.Ended -> {
                statsJob?.cancel()
                val error = event.error
                if (error == null) {
                    left = true
                    _commands.send(RoomCommand.Finish)
                } else {
                    _state.update {
                        it.copy(
                            phase = RoomPhase.Failed(error, wasInRoom = it.phase == RoomPhase.InRoom),
                            panel = Panel.None,
                            reconnecting = null,
                            keyboardVisible = false,
                        )
                    }
                }
            }
            else -> Unit
        }
    }

    private fun onPermissions(event: CoroomEvent.Permissions) {
        val before = _state.value.permissions
        val now = InputPermissions(event.keyboardMouse, event.gamepadSlot)
        _state.update { it.copy(permissions = now) }
        val notice = when {
            now.gamepadSlot != null && now.gamepadSlot != before?.gamepadSlot ->
                UiText.res(R.string.room_notice_pad_given, now.gamepadSlot)
            now.gamepadSlot == null && before?.gamepadSlot != null -> UiText.res(R.string.room_notice_pad_taken)
            now.keyboardMouse && before?.keyboardMouse != true -> UiText.res(R.string.room_notice_keyboard_given)
            !now.keyboardMouse && before?.keyboardMouse == true -> UiText.res(R.string.room_notice_keyboard_taken)
            else -> null
        }
        notice?.let(::showNotice)
    }

    private fun showNotice(text: UiText) {
        _state.update { it.copy(notice = text, noticeSeq = it.noticeSeq + 1) }
    }

    /** El aviso se va solo a los pocos segundos (si no lo sustituyó otro). */
    fun clearNotice(seq: Int) {
        _state.update { if (it.noticeSeq == seq) it.copy(notice = null) else it }
    }

    private fun startStats() {
        statsJob?.cancel()
        statsJob = viewModelScope.launch {
            while (isActive) {
                val stats = runCatching { repository.stats() }.getOrNull()
                _state.update { it.copy(stats = stats ?: it.stats) }
                delay(STATS_PERIOD_MS)
            }
        }
    }

    // --- Barra de la sala ----------------------------------------------------------------

    /**
     * Enseña la barra de la sala y vuelve a contar [OVERLAY_HIDE_MS] para esconderla: al entrar,
     * con cada acción de la barra y al tocar la píldora reducida. Lo que se manda al juego (tacto,
     * ratón, teclado y mandos) no la enseña. Mientras [RoomUiState.overlayPinned] se sigue viendo
     * aunque haya pasado el tiempo.
     */
    fun revealOverlay() {
        _state.update { it.copy(overlayShown = true) }
        overlayJob?.cancel()
        overlayJob = viewModelScope.launch {
            delay(OVERLAY_HIDE_MS)
            _state.update { it.copy(overlayShown = false) }
        }
    }

    fun toggleMic() {
        revealOverlay()
        val state = _state.value
        if (state.micMuted && !micAllowed) {
            _commands.trySend(RoomCommand.RequestMic)
            return
        }
        setMicMuted(!state.micMuted)
    }

    /** Respuesta al permiso de micrófono pedido desde la barra. */
    fun micPermissionResult(granted: Boolean) {
        if (granted) {
            micAllowed = true
            _state.update { it.copy(micAvailable = true) }
            setMicMuted(false)
        } else {
            showNotice(UiText.res(R.string.room_mic_denied))
        }
    }

    private fun setMicMuted(muted: Boolean) {
        _state.update { it.copy(micMuted = muted) }
        viewModelScope.launch {
            try {
                repository.setMicMuted(muted)
            } catch (error: BridgeException) {
                _state.update { it.copy(micMuted = !muted) }
                showNotice(UiText.Raw(error.message.orEmpty()))
            }
        }
    }

    /** Apaga o enciende el vídeo (botón de la barra o de la vista del vídeo apagado). */
    fun toggleVideo() {
        revealOverlay()
        val off = !_state.value.videoOff
        // Sin imagen, el teclado en pantalla no tiene a quién escribir.
        _state.update { it.copy(videoOff = off, keyboardVisible = if (off) false else it.keyboardVisible) }
        viewModelScope.launch {
            try {
                repository.setVideoPaused(off)
            } catch (error: BridgeException) {
                // Solo falla sin sala (la sesión no cambió): se deshace, salvo que ya se haya
                // vuelto a pulsar.
                _state.update { if (it.videoOff == off) it.copy(videoOff = !off) else it }
                showNotice(UiText.Raw(error.message.orEmpty()))
            }
        }
    }

    fun cycleTouchMode() {
        revealOverlay()
        val next = if (_state.value.touchMode == TouchMode.Trackpad) TouchMode.Direct else TouchMode.Trackpad
        _state.update { it.copy(touchMode = next) }
        viewModelScope.launch { container.settings.setTouchMode(next) }
    }

    fun setKeyboardVisible(visible: Boolean) {
        _state.update { it.copy(keyboardVisible = visible) }
        revealOverlay()
    }

    fun togglePanel(panel: Panel) {
        revealOverlay()
        _state.update {
            val next = if (it.panel == panel) Panel.None else panel
            it.copy(panel = next, unreadChat = if (next == Panel.Chat) false else it.unreadChat)
        }
    }

    fun closePanel() {
        _state.update { it.copy(panel = Panel.None) }
        revealOverlay()
    }

    fun sendChat(text: String) {
        val clean = text.trim()
        if (clean.isEmpty()) return
        viewModelScope.launch {
            try {
                repository.sendChat(clean)
                _state.update { it.copy(chatError = null) }
            } catch (error: BridgeException) {
                _state.update { it.copy(chatError = error.message) }
            }
        }
    }

    /** «Compartir registro» del panel de conexión: la actividad abre la hoja de compartir. */
    fun shareLog() {
        viewModelScope.launch {
            try {
                val files = repository.logFiles()
                if (files.isEmpty()) showNotice(UiText.res(R.string.log_share_empty)) else _commands.send(RoomCommand.ShareLog(files))
            } catch (error: BridgeException) {
                showNotice(UiText.Raw(error.message.orEmpty()))
            }
        }
    }

    /** No se pudo abrir la hoja de compartir: sin ficheros o sin ninguna app que los reciba. */
    fun logShareFailed(empty: Boolean) {
        showNotice(UiText.res(if (empty) R.string.log_share_empty else R.string.log_share_failed))
    }

    fun askLeave(ask: Boolean) {
        _state.update { it.copy(confirmLeave = ask) }
        revealOverlay()
    }

    fun leave() {
        left = true
        _state.update { it.copy(confirmLeave = false) }
        statsJob?.cancel()
        viewModelScope.launch {
            repository.leave()
            _commands.send(RoomCommand.Finish)
        }
    }

    /** Sin salir de la sala (por ejemplo tras un error): solo cerrar la pantalla. */
    fun close() {
        left = true
        viewModelScope.launch { _commands.send(RoomCommand.Finish) }
    }

    override fun onCleared() {
        if (!left) {
            // La pantalla se cerró sin «Salir» (el sistema la quitó): se sale igual.
            container.scope.launch { repository.leave() }
        }
        repository.setInRoom(false)
    }

    private companion object {
        const val MAX_CHAT = 200
        const val STATS_PERIOD_MS = 1_000L
    }
}
