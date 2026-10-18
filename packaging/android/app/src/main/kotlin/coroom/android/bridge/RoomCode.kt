package coroom.android.bridge

/** Los códigos de sala: 6 caracteres sin los que se confunden (sin 0, O, 1 ni I), como en `proto::signal`. */
object RoomCode {
    const val LENGTH = 6
    private const val ALPHABET = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789"

    /** Lo que se escribe, en mayúsculas y solo con caracteres válidos, hasta 6. */
    fun clean(input: String): String =
        input.uppercase().filter { it in ALPHABET }.take(LENGTH)

    fun isValid(code: String): Boolean = code.length == LENGTH && code.all { it in ALPHABET }
}
