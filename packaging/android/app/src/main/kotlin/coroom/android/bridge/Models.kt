package coroom.android.bridge

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * JSON del puente. Se lee con tolerancia: los campos que la parte en Rust añada más adelante se
 * ignoran y los que falten toman su valor por defecto.
 */
internal val BridgeJson = Json {
    ignoreUnknownKeys = true
    coerceInputValues = true
    isLenient = true
}

/** La cuenta con la sesión abierta (`resume` y `sign_in`). */
@Serializable
data class Account(
    val server: String = "",
    val username: String = "",
    @SerialName("display_name") val displayName: String = "",
    @SerialName("user_id") val userId: Long = 0,
    val avatar: Long? = null,
    val fingerprint: String? = null,
) {
    /** El nombre que ven los amigos (el de usuario si no tiene otro). */
    val shownName: String get() = displayName.ifBlank { username }

    /** El servidor sin el esquema ni la barra final, para enseñarlo. */
    val serverHost: String get() = displayHost(server)
}

/** Lo que contesta `probe_server`. */
@Serializable
data class ServerProbe(
    val name: String = "",
    val version: String = "",
    val protocol: Int = 0,
    val http3: Boolean = false,
    @SerialName("udp_reachable") val udpReachable: Boolean? = null,
    val fingerprint: String = "",
    /** `null` si las raíces del sistema validan el certificado; si no, por qué. */
    val untrusted: String? = null,
)

/** Qué hace un amigo conectado. */
enum class FriendActivity {
    Idle,
    Hosting,
    Playing,
    ;

    companion object {
        /** Acepta `hosting` y `Hosting` (la API lo manda con mayúscula, los eventos en minúscula). */
        fun parse(text: String?): FriendActivity = when (text?.lowercase()) {
            "hosting" -> Hosting
            "playing" -> Playing
            else -> Idle
        }
    }
}

@Serializable
data class Friend(
    val id: Long,
    val username: String = "",
    @SerialName("display_name") val displayName: String = "",
    val online: Boolean = false,
    val activity: String? = null,
    val avatar: Long? = null,
) {
    val shownName: String get() = displayName.ifBlank { username }
    val activityKind: FriendActivity get() = if (online) FriendActivity.parse(activity) else FriendActivity.Idle
}

/** Solicitud de amistad: `user` es el otro (quien la envió o a quien se envió). */
@Serializable
data class FriendRequest(
    val id: Long,
    val user: Friend? = null,
    @SerialName("created_at") val createdAt: Long = 0,
) {
    val shownName: String get() = user?.shownName.orEmpty()
    val username: String get() = user?.username.orEmpty()
}

/** Una sala abierta con la propia cuenta en otro dispositivo (`own_rooms`). */
@Serializable
data class OwnRoom(
    val code: String = "",
    val device: String = "",
)

@Serializable
data class FriendsList(
    val friends: List<Friend> = emptyList(),
    val incoming: List<FriendRequest> = emptyList(),
    val outgoing: List<FriendRequest> = emptyList(),
)

/** `stats` para la píldora y el panel de conexión. Lo que no llegue queda en `null` y se enseña «—». */
@Serializable
data class RoomStats(
    @SerialName("rtt_ms") val rttMs: Double? = null,
    @SerialName("loss_percent") val lossPercent: Double? = null,
    @SerialName("target_kbps") val targetKbps: Double? = null,
    val relayed: Boolean? = null,
    val fps: Double? = null,
    val kbps: Double? = null,
    /** De punta a punta (captura en el anfitrión → pantalla aquí), p50 y p95. */
    @SerialName("latency_ms") val latencyMs: Double? = null,
    @SerialName("latency_p95_ms") val latencyP95Ms: Double? = null,
)

/** Un mando del teléfono tal como lo ve Rust (`pads`). */
@Serializable
data class BridgePad(
    @SerialName("device_id") val deviceId: Int = 0,
    val name: String = "",
    val vendor: Int = 0,
    val product: Int = 0,
)

/** Un decodificador de vídeo del teléfono (`set_decoders`). */
@Serializable
data class DecoderInfo(
    val name: String,
    val mime: String,
    val hardware: Boolean,
    @SerialName("secure_required") val secureRequired: Boolean,
    @SerialName("low_latency") val lowLatency: Boolean,
    @SerialName("adaptive_playback") val adaptivePlayback: Boolean,
    @SerialName("max_width") val maxWidth: Int,
    @SerialName("max_height") val maxHeight: Int,
    @SerialName("max_fps") val maxFps: Int,
    @SerialName("achievable_fps") val achievableFps: Double? = null,
    val main10: Boolean,
    val yuv444: Boolean,
)

/** Alguien de la sala (`members`). `guest` 0 es el anfitrión. */
@Serializable
data class Member(
    val guest: Int = 0,
    val user: Long = 0,
    val name: String = "",
    val avatar: Long? = null,
    val you: Boolean = false,
)

/** Un mensaje del chat de la sala. */
@Serializable
data class ChatMessage(
    val guest: Int = 0,
    val name: String = "",
    val text: String = "",
    @SerialName("at_unix_ms") val atUnixMs: Long = 0,
    val own: Boolean = false,
)

/** Modo del control táctil (`set_touch_mode`). */
enum class TouchMode(val wire: String) {
    Trackpad("trackpad"),
    Direct("direct"),
    ;

    companion object {
        fun fromWire(text: String?): TouchMode = entries.firstOrNull { it.wire == text } ?: Trackpad
    }
}

/** `https://coroom.example.com/` → `coroom.example.com`. */
fun displayHost(server: String): String =
    server.trim().substringAfter("://").trimEnd('/')

/**
 * La huella de un certificado en grupos de dos cifras hexadecimales, ocho por línea, como la
 * imprime el servidor al arrancar (acepta `AB:CD…`, `abcd…` o `sha256:…`).
 */
fun formatFingerprint(fingerprint: String): String {
    val hex = fingerprint.substringAfter("sha256:", fingerprint)
        .filter { it.isDigit() || it.lowercaseChar() in 'a'..'f' }
        .uppercase()
    if (hex.isEmpty()) return fingerprint
    return hex.chunked(2).chunked(8).joinToString("\n") { it.joinToString(" ") }
}
