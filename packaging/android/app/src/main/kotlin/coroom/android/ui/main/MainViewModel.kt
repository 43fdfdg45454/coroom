package coroom.android.ui.main

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import coroom.android.AppContainer
import coroom.android.R
import coroom.android.Startup
import coroom.android.bridge.Account
import coroom.android.bridge.BridgeException
import coroom.android.bridge.BridgePad
import coroom.android.bridge.CoroomEvent
import coroom.android.bridge.Friend
import coroom.android.bridge.FriendRequest
import coroom.android.bridge.FriendsList
import coroom.android.bridge.Invitation
import coroom.android.bridge.OwnRoom
import coroom.android.bridge.RoomCode
import coroom.android.bridge.TouchMode
import coroom.android.bridge.UpdateState
import coroom.android.platform.Gamepad
import coroom.android.settings.Settings
import coroom.android.ui.UiText
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/** En qué punto está la app antes de enseñar Inicio. */
sealed interface MainPhase {
    data object Starting : MainPhase

    /** La parte nativa no arrancó (falta la biblioteca, el Keystore falló). */
    data class Failed(val message: String) : MainPhase

    data object Resuming : MainPhase

    /** Había sesión guardada pero no se pudo abrir (servidor caído, sin red…). */
    data class ResumeFailed(val message: String) : MainPhase

    /** Pantalla Servidor. `sessionLost`: el Keystore perdió la clave y hay que entrar otra vez. */
    data class SignedOut(val sessionLost: Boolean = false) : MainPhase

    data object SignedIn : MainPhase
}

data class FriendsUi(
    val loading: Boolean = true,
    val list: FriendsList? = null,
    val error: String? = null,
)

data class MainUiState(
    val phase: MainPhase = MainPhase.Starting,
    val account: Account? = null,
    val serverOnline: Boolean? = null,
    val invitations: List<Invitation> = emptyList(),
    /** Las salas de la cuenta abiertas en sus otros dispositivos. */
    val ownRooms: List<OwnRoom> = emptyList(),
    val friends: FriendsUi = FriendsUi(),
    val pads: List<Gamepad> = emptyList(),
    /** Los mandos tal como los ve Rust (`pads`); `null` si aún no contestó. */
    val bridgePads: List<BridgePad>? = null,
    val settings: Settings = Settings(),
    /** Amigos a los que se pidió entrar hace poco (se entra solo si invitan). */
    val askedToJoin: Set<Long> = emptySet(),
    val busy: Boolean = false,
    /** La hora para «hace N min», que avanza cada medio minuto. */
    val nowMs: Long = System.currentTimeMillis(),
    /** La versión nueva (`null` hasta que arranca la parte en Rust). */
    val update: UpdateState? = null,
    /** El servidor de la cuenta usa otro protocolo. */
    val protocolMismatch: CoroomEvent.ProtocolMismatch? = null,
    /** El APK que espera al instalador del sistema. */
    val pendingInstall: String? = null,
)

/** Entrar en una sala: lo abre la actividad de la sala. */
data class RoomRequest(val code: String, val hostName: String?)

class MainViewModel(private val container: AppContainer) : ViewModel() {
    private val repository = container.repository
    private val _state = MutableStateFlow(MainUiState())
    val state: StateFlow<MainUiState> = _state.asStateFlow()

    private val _messages = Channel<UiText>(Channel.BUFFERED)

    /** Avisos breves para la barra inferior (snackbar). */
    val messages: Flow<UiText> = _messages.receiveAsFlow()

    private val _rooms = Channel<RoomRequest>(Channel.BUFFERED)

    /** Salas que abrir. */
    val rooms: Flow<RoomRequest> = _rooms.receiveAsFlow()

    private val _logShares = Channel<List<String>>(Channel.BUFFERED)

    /** Los ficheros del registro que compartir («Compartir registro» de Ajustes; lo abre la pantalla). */
    val logShares: Flow<List<String>> = _logShares.receiveAsFlow()

    private var pendingJoin: RoomRequest? = null
    private val askedAt = mutableMapOf<Long, Long>()

    init {
        viewModelScope.launch {
            container.startup.collect { startup ->
                when (startup) {
                    Startup.Starting -> Unit
                    is Startup.Failed -> _state.update { it.copy(phase = MainPhase.Failed(startup.message)) }
                    is Startup.Ready -> if (startup.sessionLost) {
                        _state.update { it.copy(phase = MainPhase.SignedOut(sessionLost = true)) }
                    } else if (repository.account.value == null) {
                        resume()
                    }
                }
            }
        }
        viewModelScope.launch {
            repository.account.collect { account ->
                _state.update { state ->
                    val phase = when {
                        account != null -> MainPhase.SignedIn
                        state.phase == MainPhase.SignedIn -> MainPhase.SignedOut()
                        else -> state.phase
                    }
                    state.copy(account = account, phase = phase)
                }
                if (account != null) {
                    container.settings.setHasSavedSession(true)
                    refreshFriends()
                    pendingJoin?.let {
                        pendingJoin = null
                        _rooms.send(it)
                    }
                }
            }
        }
        viewModelScope.launch {
            repository.serverStatus.collect { status ->
                val wasOffline = _state.value.serverOnline != true
                _state.update { it.copy(serverOnline = status?.online) }
                if (status?.online == true && wasOffline && _state.value.friends.error != null) refreshFriends()
            }
        }
        viewModelScope.launch {
            repository.invitations.collect { list ->
                _state.update { it.copy(invitations = list, nowMs = System.currentTimeMillis()) }
            }
        }
        viewModelScope.launch {
            repository.ownRooms.collect { rooms -> _state.update { it.copy(ownRooms = rooms) } }
        }
        viewModelScope.launch {
            container.gamepads.pads.collect { pads ->
                _state.update { it.copy(pads = pads) }
                refreshBridgePads()
            }
        }
        viewModelScope.launch {
            container.settings.settings.collect { settings -> _state.update { it.copy(settings = settings) } }
        }
        viewModelScope.launch { repository.events.collect(::onEvent) }
        viewModelScope.launch { repository.update.collect { update -> _state.update { it.copy(update = update) } } }
        viewModelScope.launch {
            repository.protocolMismatch.collect { mismatch -> _state.update { it.copy(protocolMismatch = mismatch) } }
        }
        viewModelScope.launch {
            repository.pendingInstall.collect { path -> _state.update { it.copy(pendingInstall = path) } }
        }
        viewModelScope.launch {
            repository.installErrors.collect { message ->
                _messages.send(
                    message?.let { UiText.res(R.string.update_install_failed, it) }
                        ?: UiText.res(R.string.update_install_failed_unknown),
                )
            }
        }
        // Las invitaciones dicen «hace N min»: se recompone cada medio minuto.
        viewModelScope.launch {
            while (isActive) {
                delay(30_000)
                _state.update { it.copy(nowMs = System.currentTimeMillis()) }
            }
        }
    }

    private suspend fun resume() {
        _state.update { it.copy(phase = MainPhase.Resuming) }
        try {
            repository.resume()
        } catch (error: BridgeException) {
            val saved = container.settings.settings.first().hasSavedSession
            val message = error.message.orEmpty()
            val phase = if (!saved || message == NO_SESSION) MainPhase.SignedOut() else MainPhase.ResumeFailed(message)
            _state.update { it.copy(phase = phase) }
        }
    }

    fun retryResume() {
        viewModelScope.launch { resume() }
    }

    /** «Iniciar sesión de nuevo» desde el error de la sesión guardada. */
    fun signInAgain() {
        _state.update { it.copy(phase = MainPhase.SignedOut()) }
    }

    private suspend fun onEvent(event: CoroomEvent) {
        when (event) {
            is CoroomEvent.Presence -> updateFriend(event.user) {
                it.copy(online = event.online, activity = if (event.online) it.activity else null)
            }
            is CoroomEvent.ActivityChanged -> updateFriend(event.user) { it.copy(activity = event.activity) }
            is CoroomEvent.AvatarChanged -> updateFriend(event.user, inRequests = true) { it.copy(avatar = event.avatar) }
            is CoroomEvent.FriendRequested -> {
                refreshFriends()
                _messages.send(UiText.res(R.string.message_friend_requested, event.fromName))
            }
            is CoroomEvent.JoinDeclined -> {
                forgetAsk(event.fromUser)
                _messages.send(UiText.res(R.string.message_join_declined, event.fromName))
            }
            is CoroomEvent.JoinRequestFailed -> _messages.send(
                event.message?.let { UiText.Raw(it) } ?: UiText.res(R.string.message_join_request_offline),
            )
            is CoroomEvent.Invited -> {
                val asked = askedAt[event.fromUser]
                if (asked != null && System.currentTimeMillis() - asked < ASK_WINDOW_MS) {
                    forgetAsk(event.fromUser)
                    join(event.code, event.fromName)
                }
            }
            else -> Unit
        }
    }

    /** Cambia al amigo `id` en la lista y, con `inRequests`, también en las solicitudes. */
    private fun updateFriend(id: Long, inRequests: Boolean = false, change: (Friend) -> Friend) {
        _state.update { state ->
            val list = state.friends.list ?: return@update state
            fun requests(requests: List<FriendRequest>) = if (inRequests) {
                requests.map { request ->
                    val user = request.user
                    if (user != null && user.id == id) request.copy(user = change(user)) else request
                }
            } else {
                requests
            }
            state.copy(
                friends = state.friends.copy(
                    list = list.copy(
                        friends = list.friends.map { if (it.id == id) change(it) else it },
                        incoming = requests(list.incoming),
                        outgoing = requests(list.outgoing),
                    ),
                ),
            )
        }
    }

    fun refreshFriends() {
        if (repository.account.value == null) return
        viewModelScope.launch {
            _state.update { it.copy(friends = it.friends.copy(loading = true)) }
            try {
                val list = repository.friends()
                _state.update { it.copy(friends = FriendsUi(loading = false, list = list)) }
                // Hay servidor: los avatares que no llegaron antes (el propio también) se vuelven a pedir.
                repository.retryAvatars()
            } catch (error: BridgeException) {
                _state.update { it.copy(friends = it.friends.copy(loading = false, error = error.message)) }
            }
        }
    }

    private fun refreshBridgePads() {
        viewModelScope.launch {
            val pads = runCatching { repository.pads() }.getOrNull()
            if (pads != null) _state.update { it.copy(bridgePads = pads) }
        }
    }

    // --- Salas --------------------------------------------------------------------------

    fun join(code: String, hostName: String? = null) {
        val clean = RoomCode.clean(code)
        if (!RoomCode.isValid(clean)) {
            _messages.trySend(UiText.res(R.string.message_invalid_code))
            return
        }
        val request = RoomRequest(clean, hostName)
        if (_state.value.phase == MainPhase.SignedIn) _rooms.trySend(request) else pendingJoin = request
    }

    fun dismissInvitation(code: String) = repository.dismissInvitation(code)

    fun askToJoin(friend: Friend) {
        viewModelScope.launch {
            try {
                repository.askToJoin(friend.id)
                askedAt[friend.id] = System.currentTimeMillis()
                _state.update { it.copy(askedToJoin = askedAt.keys.toSet()) }
                _messages.send(UiText.res(R.string.message_asked_to_join, friend.shownName))
                delay(ASK_WINDOW_MS)
                forgetAsk(friend.id)
            } catch (error: BridgeException) {
                _messages.send(UiText.Raw(error.message.orEmpty()))
            }
        }
    }

    private fun forgetAsk(user: Long) {
        askedAt.remove(user)
        _state.update { it.copy(askedToJoin = askedAt.keys.toSet()) }
    }

    // --- Amigos -------------------------------------------------------------------------

    fun requestFriend(username: String) = friendAction { repository.requestFriend(username.trim().removePrefix("@")) }

    fun acceptRequest(id: Long) = friendAction { repository.acceptRequest(id) }

    fun declineRequest(id: Long) = friendAction { repository.declineRequest(id) }

    fun removeFriend(id: Long) = friendAction { repository.removeFriend(id) }

    private fun friendAction(action: suspend () -> String) {
        viewModelScope.launch {
            _state.update { it.copy(busy = true) }
            try {
                val text = action()
                if (text.isNotBlank()) _messages.send(UiText.Raw(text))
                refreshFriends()
            } catch (error: BridgeException) {
                _messages.send(UiText.Raw(error.message.orEmpty()))
            } finally {
                _state.update { it.copy(busy = false) }
            }
        }
    }

    // --- Ajustes ------------------------------------------------------------------------

    fun setTouchMode(mode: TouchMode) {
        viewModelScope.launch { container.settings.setTouchMode(mode) }
    }

    fun setGameVolume(percent: Int) {
        viewModelScope.launch { container.settings.setGameVolume(percent) }
    }

    fun setJoinMuted(muted: Boolean) {
        viewModelScope.launch { container.settings.setJoinMuted(muted) }
    }

    fun setNotifyInvitations(notify: Boolean) {
        viewModelScope.launch { container.settings.setNotifyInvitations(notify) }
    }

    fun markAskedNotifications() {
        viewModelScope.launch { container.settings.setAskedNotifications() }
    }

    fun notificationsDenied() {
        viewModelScope.launch {
            container.settings.setNotifyInvitations(false)
            _messages.send(UiText.res(R.string.message_notifications_denied))
        }
    }

    // --- Versión nueva ------------------------------------------------------------------

    /** Los botones de la versión nueva (Inicio, Ajustes y Servidor). */
    val updateActions = UpdateActions(
        checkNow = { updateCall { repository.updateCheck(manual = true) } },
        retry = { updateCall { repository.updateRetry() } },
        updateNow = { updateCall { repository.updateNow() } },
        install = { updateCall { repository.updateInstall() } },
        setCheck = { on ->
            updateCall {
                // A Rust primero (la búsqueda de después ya la ve encendida); luego se guarda.
                repository.updateSetCheck(on)
                container.settings.setUpdatesCheck(on)
                // Al encenderla se busca enseguida.
                if (on) repository.updateCheck(manual = true)
            }
        },
        setAuto = { on ->
            updateCall {
                repository.updateSetAuto(on)
                container.settings.setUpdatesAuto(on)
            }
        },
        setInterval = { hours -> viewModelScope.launch { container.settings.setUpdatesInterval(hours) } },
    )

    private fun updateCall(action: suspend () -> Unit) {
        viewModelScope.launch {
            try {
                action()
            } catch (error: BridgeException) {
                _messages.send(UiText.Raw(error.message.orEmpty()))
            }
        }
    }

    /** El instalador ya tiene el APK (o se dejó para después). */
    fun installHandled() = repository.installHandled()

    fun installFailed(message: String?) = repository.installFailed(message)

    /** Comparte el registro: los ficheros que haya o, si todavía no hay nada escrito, se dice. */
    fun shareLog() {
        viewModelScope.launch {
            try {
                val files = repository.logFiles()
                if (files.isEmpty()) _messages.send(UiText.res(R.string.log_share_empty)) else _logShares.send(files)
            } catch (error: BridgeException) {
                _messages.send(UiText.Raw(error.message.orEmpty()))
            }
        }
    }

    fun logout() {
        viewModelScope.launch {
            try {
                repository.logout()
                container.settings.setHasSavedSession(false)
            } catch (error: BridgeException) {
                _messages.send(UiText.Raw(error.message.orEmpty()))
            }
        }
    }

    private companion object {
        /** El texto de `resume` cuando nunca se inició sesión (contrato con apps/android). */
        const val NO_SESSION = "todavía no has iniciado sesión en ningún servidor"
        const val ASK_WINDOW_MS = 120_000L
    }
}
