package coroom.android.bridge

import android.content.Context
import coroom.android.Native
import coroom.android.R
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.serialization.KSerializer
import kotlinx.serialization.SerializationException
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObjectBuilder
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put
import kotlin.concurrent.thread

/** Un error de la parte en Rust (o del puente), ya en español. */
class BridgeException(message: String) : Exception(message)

/** Una invitación recibida (`invited`), con la hora a la que llegó. */
data class Invitation(
    val fromUser: Long,
    val fromName: String,
    val code: String,
    val receivedAtMs: Long,
)

/**
 * Las operaciones de `Native.call` como funciones `suspend` en `Dispatchers.IO`, y los eventos de
 * `Native.nextEvent` como un flujo. Guarda además el estado que necesita más de una pantalla: la
 * cuenta abierta, el estado del servidor y las invitaciones pendientes.
 */
class CoroomRepository(
    private val context: Context,
    private val scope: CoroutineScope,
) {
    @Volatile
    private var ready = false

    private val _events = MutableSharedFlow<CoroomEvent>(extraBufferCapacity = 512)

    /** Todos los eventos, en el orden en que llegan. */
    val events: SharedFlow<CoroomEvent> = _events.asSharedFlow()

    private val _account = MutableStateFlow<Account?>(null)

    /** La cuenta con la sesión abierta, o `null`. */
    val account: StateFlow<Account?> = _account.asStateFlow()

    private val _serverStatus = MutableStateFlow<CoroomEvent.ServerStatus?>(null)

    /** El último `server_status` (`null`: la señalización está cerrada o aún no contestó). */
    val serverStatus: StateFlow<CoroomEvent.ServerStatus?> = _serverStatus.asStateFlow()

    private val _invitations = MutableStateFlow<List<Invitation>>(emptyList())

    /** Invitaciones recibidas, la más nueva primero. */
    val invitations: StateFlow<List<Invitation>> = _invitations.asStateFlow()

    private val _inRoom = MutableStateFlow(false)

    /** Si hay una pantalla de sala abierta (la sala de espera se cierra mientras tanto). */
    val inRoom: StateFlow<Boolean> = _inRoom.asStateFlow()

    private val lobbyMutex = Mutex()
    private var lobbyServer: String? = null

    /** Lo llama el arranque cuando `Native.init` fue bien. */
    internal fun markReady() {
        ready = true
    }

    /** El bucle de eventos: un hilo propio porque `nextEvent` bloquea. */
    internal fun startEventLoop() {
        thread(name = "coroom-events", isDaemon = true) {
            while (true) {
                val text = try {
                    Native.nextEvent(EVENT_WAIT_MS)
                } catch (_: Throwable) {
                    null
                } ?: continue
                val event = parseEvent(text) ?: continue
                remember(event)
                if (!_events.tryEmit(event)) {
                    // El búfer está lleno: se espera a que lo lean antes de perder un evento.
                    runBlocking { _events.emit(event) }
                }
            }
        }
    }

    private fun remember(event: CoroomEvent) {
        when (event) {
            is CoroomEvent.ServerStatus -> _serverStatus.value = event
            // El avatar propio cambiado desde otro dispositivo.
            is CoroomEvent.AvatarChanged -> _account.update { account ->
                if (account != null && account.userId == event.user) account.copy(avatar = event.avatar) else account
            }
            is CoroomEvent.Invited -> _invitations.update { list ->
                val now = System.currentTimeMillis()
                val fresh = list.filter { it.code != event.code && now - it.receivedAtMs < INVITATION_TTL_MS }
                (listOf(Invitation(event.fromUser, event.fromName, event.code, now)) + fresh)
                    .take(MAX_INVITATIONS)
            }
            else -> Unit
        }
    }

    /** Quita una invitación (al entrar con ella o descartarla). */
    fun dismissInvitation(code: String) {
        _invitations.update { list -> list.filterNot { it.code == code } }
    }

    fun setInRoom(inRoom: Boolean) {
        _inRoom.value = inRoom
    }

    // --- Cuenta ---------------------------------------------------------------------------

    suspend fun resume(server: String? = null): Account {
        val account = callAs("resume", Account.serializer()) { put("server", server) }
        _account.value = account
        return account
    }

    suspend fun probeServer(url: String, tcpPort: Int? = null): ServerProbe =
        callAs("probe_server", ServerProbe.serializer()) {
            put("url", url)
            put("tcp_port", tcpPort)
        }

    suspend fun signIn(
        server: String,
        username: String,
        password: String,
        invitation: String? = null,
        displayName: String? = null,
        trustFingerprint: String? = null,
        tcpPort: Int? = null,
    ): Account {
        val account = callAs("sign_in", Account.serializer()) {
            put("server", server)
            put("username", username)
            put("password", password)
            put("invitation", invitation)
            put("display_name", displayName)
            put("trust_fingerprint", trustFingerprint)
            put("tcp_port", tcpPort)
        }
        _account.value = account
        return account
    }

    suspend fun logout(): String {
        val text = callText("logout") { put("server", _account.value?.server) }
        lobbyMutex.withLock { lobbyServer = null }
        _serverStatus.value = null
        _invitations.value = emptyList()
        _account.value = null
        return text
    }

    // --- Amigos ---------------------------------------------------------------------------

    suspend fun friends(): FriendsList =
        callAs("friends", FriendsList.serializer()) { put("server", _account.value?.server) }

    suspend fun requestFriend(username: String): String = callText("request_friend") { put("username", username) }

    suspend fun acceptRequest(id: Long): String = callText("accept_request") { put("id", id) }

    suspend fun declineRequest(id: Long): String = callText("decline_request") { put("id", id) }

    suspend fun removeFriend(id: Long): String = callText("remove_friend") { put("id", id) }

    /**
     * La ruta del PNG de la versión `version` del avatar de `user` (de la caché de disco de Rust o
     * descargado del servidor), o `null` si no tiene avatar.
     */
    suspend fun avatarPath(user: Long, version: Long): String? {
        val ok = call("avatar") {
            put("user", user)
            put("version", version)
            put("server", _account.value?.server)
        }
        return (ok as? JsonPrimitive)?.contentOrNull
    }

    // --- Sala de espera (señalización) ----------------------------------------------------

    /** Abre la señalización con el servidor de la cuenta, si no estaba ya abierta con él. */
    suspend fun lobbyStart() {
        val server = _account.value?.server ?: return
        lobbyMutex.withLock {
            if (lobbyServer == server) return
            call("lobby_start") { put("server", server) }
            lobbyServer = server
        }
    }

    suspend fun lobbyStop() {
        lobbyMutex.withLock {
            if (lobbyServer == null) return
            lobbyServer = null
            _serverStatus.value = null
            runCatching { call("lobby_stop") }
        }
    }

    suspend fun askToJoin(user: Long) {
        call("ask_to_join") { put("user", user) }
    }

    // --- Sala -----------------------------------------------------------------------------

    /**
     * Entra en la sala `code`: vuelve enseguida y el avance llega como eventos. `mic`: solo si hay
     * permiso de micrófono y el usuario no lo quitó.
     */
    suspend fun join(code: String, mic: Boolean) {
        dismissInvitation(code)
        call("join") {
            put("code", code)
            put("server", _account.value?.server)
            put("mic", mic)
        }
    }

    suspend fun leave() {
        runCatching { call("leave") }
    }

    suspend fun sendChat(text: String) {
        call("send_chat") { put("text", text) }
    }

    suspend fun setTouchMode(mode: TouchMode) {
        call("set_touch_mode") { put("mode", mode.wire) }
    }

    suspend fun setGameVolume(percent: Int) {
        call("set_game_volume") { put("percent", percent.coerceIn(0, 200)) }
    }

    suspend fun setMicMuted(muted: Boolean) {
        call("set_mic_muted") { put("muted", muted) }
    }

    suspend fun setJoinMuted(muted: Boolean) {
        call("set_join_muted") { put("muted", muted) }
    }

    /** `null` fuera de una sala. */
    suspend fun stats(): RoomStats? {
        val ok = call("stats")
        if (ok is JsonNull) return null
        return decode(ok, RoomStats.serializer())
    }

    suspend fun pads(): List<BridgePad> = callAs("pads", ListSerializer(BridgePad.serializer()))

    // --- Plataforma -----------------------------------------------------------------------

    /** La ruta por defecto de la red actual (IPv4 de la puerta de enlace y MTU), o `null`. */
    fun setRoute(gateway: String?, mtu: Int?) {
        if (!ready) return
        scope.launch {
            runCatching {
                call("set_route") {
                    put("gateway", gateway)
                    put("mtu", mtu)
                }
            }
        }
    }

    /** Los umbrales de gesto del sistema, para que el trackpad se sienta como el resto de Android. */
    suspend fun setTouchConfig(touchSlop: Float, tapTimeoutMs: Int, longPressTimeoutMs: Int, doubleTapTimeoutMs: Int) {
        call("set_touch_config") {
            put("touch_slop", touchSlop)
            put("tap_timeout_ms", tapTimeoutMs)
            put("long_press_timeout_ms", longPressTimeoutMs)
            put("double_tap_timeout_ms", doubleTapTimeoutMs)
        }
    }

    /** Los decodificadores de vídeo del teléfono: antes de entrar en ninguna sala. */
    suspend fun setDecoders(decoders: List<DecoderInfo>) {
        call("set_decoders") {
            put("decoders", BridgeJson.encodeToJsonElement(ListSerializer(DecoderInfo.serializer()), decoders))
        }
    }

    /** Las raíces de confianza del sistema y del usuario, en PEM. */
    suspend fun setRoots(pem: String) {
        call("set_roots") { put("pem", pem) }
    }

    // --- Llamada genérica -----------------------------------------------------------------

    private suspend fun call(method: String, args: JsonObjectBuilder.() -> Unit = {}): JsonElement {
        if (!ready) throw BridgeException(context.getString(R.string.error_bridge_not_ready))
        val request = buildJsonObject(args).toString()
        return withContext(Dispatchers.IO) {
            val answer = try {
                Native.call(method, request)
            } catch (error: Throwable) {
                // UnsatisfiedLinkError o una excepción de JNI: se explica en vez de cerrar la app.
                throw BridgeException(context.getString(R.string.error_bridge_call, error.message.orEmpty()))
            }
            val json = try {
                BridgeJson.parseToJsonElement(answer).jsonObject
            } catch (_: IllegalArgumentException) {
                throw BridgeException(context.getString(R.string.error_bridge_answer))
            }
            json["error"]?.let { error ->
                val message = (error as? JsonPrimitive)?.contentOrNull ?: error.toString()
                throw BridgeException(message)
            }
            json["ok"] ?: JsonNull
        }
    }

    private suspend fun <T> callAs(
        method: String,
        serializer: KSerializer<T>,
        args: JsonObjectBuilder.() -> Unit = {},
    ): T {
        return decode(call(method, args), serializer)
    }

    private fun <T> decode(ok: JsonElement, serializer: KSerializer<T>): T {
        return try {
            BridgeJson.decodeFromJsonElement(serializer, ok)
        } catch (_: SerializationException) {
            throw BridgeException(context.getString(R.string.error_bridge_answer))
        } catch (_: IllegalArgumentException) {
            throw BridgeException(context.getString(R.string.error_bridge_answer))
        }
    }

    /** Las operaciones que devuelven un texto para el usuario (o nada). */
    private suspend fun callText(method: String, args: JsonObjectBuilder.() -> Unit = {}): String =
        (call(method, args) as? JsonPrimitive)?.contentOrNull.orEmpty()

    private companion object {
        const val EVENT_WAIT_MS = 1_000L
        const val MAX_INVITATIONS = 5
        const val INVITATION_TTL_MS = 30 * 60 * 1_000L
    }
}
