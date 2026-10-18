package coroom.android.platform

import android.content.Context
import android.os.Build
import android.provider.Settings

/** El nombre del teléfono que verá la cuenta en su lista de dispositivos. */
object DeviceName {
    fun get(context: Context): String {
        val named = Settings.Global.getString(context.contentResolver, Settings.Global.DEVICE_NAME)
        return named?.trim()?.takeIf { it.isNotEmpty() } ?: Build.MODEL.orEmpty().ifBlank { "Android" }
    }
}
