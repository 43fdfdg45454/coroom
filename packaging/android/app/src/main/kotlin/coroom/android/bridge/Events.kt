package coroom.android.bridge

import kotlinx.serialization.KSerializer
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

/** Un evento de `Native.nextEvent` (`type` en snake_case). Los tipos que no se conocen se ignoran. */
sealed interface CoroomEvent {
    @Serializable
    data class ServerStatus(
        val online: Boolean = false,
        @SerialName("rtt_ms") val rttMs: Double? = null,
    ) : CoroomEvent

    @Serializable
    data class Invited(
        @SerialName("from_user") val fromUser: Long = 0,
        @SerialName("from_name") val fromName: String = "",
        val code: String = "",
    ) : CoroomEvent

    @Serializable
    data class Presence(val user: Long = 0, val online: Boolean = false) : CoroomEvent

    @Serializable
    data class ActivityChanged(val user: Long = 0, val activity: String? = null) : CoroomEvent

    @Serializable
    data class AvatarChanged(val user: Long = 0, val avatar: Long? = null) : CoroomEvent

    /** Las salas abiertas con la propia cuenta en otros dispositivos (vacía: ninguna). */
    @Serializable
    data class OwnRooms(val rooms: List<OwnRoom> = emptyList()) : CoroomEvent

    @Serializable
    data class JoinRequested(
        @SerialName("from_user") val fromUser: Long = 0,
        @SerialName("from_name") val fromName: String = "",
    ) : CoroomEvent

    @Serializable
    data class JoinDeclined(
        @SerialName("from_user") val fromUser: Long = 0,
        @SerialName("from_name") val fromName: String = "",
    ) : CoroomEvent

    @Serializable
    data class FriendRequested(
        @SerialName("from_user") val fromUser: Long = 0,
        @SerialName("from_name") val fromName: String = "",
    ) : CoroomEvent

    /** `message` `null`: no había conexión con el servidor (la interfaz pone su texto). */
    @Serializable
    data class JoinRequestFailed(val message: String? = null) : CoroomEvent

    @Serializable
    data class Joined(
        @SerialName("host_name") val hostName: String = "",
        @SerialName("safety_number") val safetyNumber: String = "",
    ) : CoroomEvent

    @Serializable
    data class Connected(val relayed: Boolean = false) : CoroomEvent

    @Serializable
    data class Quality(
        @SerialName("rtt_ms") val rttMs: Double = 0.0,
        val level: String = "",
    ) : CoroomEvent

    @Serializable
    data class Reconnecting(
        val attempt: Int = 0,
        val of: Int = 0,
        @SerialName("retry_in_ms") val retryInMs: Long = 0,
        val reason: String = "",
    ) : CoroomEvent

    @Serializable
    data class Reconnected(val relayed: Boolean = false) : CoroomEvent

    @Serializable
    data class VideoCodec(val codec: String = "", val detail: String = "") : CoroomEvent

    @Serializable
    data class Members(val members: List<Member> = emptyList()) : CoroomEvent

    @Serializable
    data class Chat(
        val guest: Int = 0,
        val name: String = "",
        val text: String = "",
        @SerialName("at_unix_ms") val atUnixMs: Long = 0,
        val own: Boolean = false,
    ) : CoroomEvent {
        val message: ChatMessage get() = ChatMessage(guest, name, text, atUnixMs, own)
    }

    @Serializable
    data class ChatHistory(val messages: List<ChatMessage> = emptyList()) : CoroomEvent

    data object ChatCleared : CoroomEvent

    @Serializable
    data class ChatRejected(val reason: String = "") : CoroomEvent

    @Serializable
    data class MoveToRoom(
        val code: String = "",
        @SerialName("host_name") val hostName: String = "",
    ) : CoroomEvent

    @Serializable
    data class Warning(val message: String = "") : CoroomEvent

    /** `error` `null`: se salió sin error (vuelta a Inicio). */
    @Serializable
    data class Ended(val error: String? = null) : CoroomEvent

    /** Mantener estos niveles de los motores (0..65535) hasta el siguiente `rumble`; 0/0 para. */
    @Serializable
    data class Rumble(
        @SerialName("device_id") val deviceId: Int = 0,
        val slot: Int? = null,
        val strong: Int = 0,
        val weak: Int = 0,
    ) : CoroomEvent

    /** Qué deja hacer el anfitrión: teclado y ratón, y la ranura del mando (`null`: ninguna). */
    @Serializable
    data class Permissions(
        @SerialName("keyboard_mouse") val keyboardMouse: Boolean = false,
        @SerialName("gamepad_slot") val gamepadSlot: Int? = null,
    ) : CoroomEvent

    /** Paso de la lista de «Entrando en la sala»: `server`, `path`, `probe` o `keys`. */
    @Serializable
    data class JoinProgress(val step: String = "", val done: Boolean = false) : CoroomEvent
}

/** Los eventos que solo importan dentro de una sala. */
val CoroomEvent.isRoomEvent: Boolean
    get() = when (this) {
        is CoroomEvent.Joined, is CoroomEvent.Connected, is CoroomEvent.Quality,
        is CoroomEvent.Reconnecting, is CoroomEvent.Reconnected, is CoroomEvent.VideoCodec,
        is CoroomEvent.Members, is CoroomEvent.Chat, is CoroomEvent.ChatHistory,
        CoroomEvent.ChatCleared, is CoroomEvent.ChatRejected, is CoroomEvent.MoveToRoom,
        is CoroomEvent.Warning, is CoroomEvent.Ended, is CoroomEvent.JoinProgress,
        is CoroomEvent.Permissions,
        -> true
        else -> false
    }

/** Lee un evento; `null` si no es JSON, no tiene `type` o el tipo no se conoce. */
fun parseEvent(text: String): CoroomEvent? = try {
    val json = BridgeJson.parseToJsonElement(text).jsonObject
    when (json["type"]?.jsonPrimitive?.contentOrNull) {
        "server_status" -> json.decode(CoroomEvent.ServerStatus.serializer())
        "invited" -> json.decode(CoroomEvent.Invited.serializer())
        "presence" -> json.decode(CoroomEvent.Presence.serializer())
        "activity" -> json.decode(CoroomEvent.ActivityChanged.serializer())
        "avatar" -> json.decode(CoroomEvent.AvatarChanged.serializer())
        "own_rooms" -> json.decode(CoroomEvent.OwnRooms.serializer())
        "join_requested" -> json.decode(CoroomEvent.JoinRequested.serializer())
        "join_declined" -> json.decode(CoroomEvent.JoinDeclined.serializer())
        "friend_requested" -> json.decode(CoroomEvent.FriendRequested.serializer())
        "join_request_failed" -> json.decode(CoroomEvent.JoinRequestFailed.serializer())
        "joined" -> json.decode(CoroomEvent.Joined.serializer())
        "connected" -> json.decode(CoroomEvent.Connected.serializer())
        "quality" -> json.decode(CoroomEvent.Quality.serializer())
        "reconnecting" -> json.decode(CoroomEvent.Reconnecting.serializer())
        "reconnected" -> json.decode(CoroomEvent.Reconnected.serializer())
        "video_codec" -> json.decode(CoroomEvent.VideoCodec.serializer())
        "members" -> json.decode(CoroomEvent.Members.serializer())
        "chat" -> json.decode(CoroomEvent.Chat.serializer())
        "chat_history" -> json.decode(CoroomEvent.ChatHistory.serializer())
        "chat_cleared" -> CoroomEvent.ChatCleared
        "chat_rejected" -> json.decode(CoroomEvent.ChatRejected.serializer())
        "move_to_room" -> json.decode(CoroomEvent.MoveToRoom.serializer())
        "warning" -> json.decode(CoroomEvent.Warning.serializer())
        "ended" -> json.decode(CoroomEvent.Ended.serializer())
        "rumble" -> json.decode(CoroomEvent.Rumble.serializer())
        "join_progress" -> json.decode(CoroomEvent.JoinProgress.serializer())
        "permissions" -> json.decode(CoroomEvent.Permissions.serializer())
        else -> null
    }
} catch (_: IllegalArgumentException) {
    // También SerializationException, que la extiende: un evento mal formado no tumba el bucle.
    null
}

private fun <T> JsonObject.decode(serializer: KSerializer<T>): T =
    BridgeJson.decodeFromJsonElement(serializer, this)
