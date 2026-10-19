package coroom.android.platform

import android.content.ClipData
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.core.content.FileProvider
import coroom.android.BuildConfig
import coroom.android.R
import java.io.File

/**
 * «Compartir registro» de Ajustes: los ficheros del registro que escribe la parte en Rust
 * (`filesDir/logs`, ver `apps/android/src/log.rs`) a la app que elija el usuario (correo,
 * mensajería…) por la hoja de compartir del sistema. Van por el FileProvider de
 * `res/xml/log_paths.xml`: solo esa carpeta, solo lectura y solo para la app elegida.
 */
object LogShare {
    /** La hoja de compartir con los registros `paths` (el de ahora primero), o `null` si no queda ninguno. */
    fun chooser(context: Context, paths: List<String>): Intent? {
        val authority = context.packageName + AUTHORITY_SUFFIX
        val uris: List<Uri> = paths
            .map(::File)
            .filter { it.isFile && it.length() > 0 }
            // Fuera de `logs/` el FileProvider no da URI (no debería pasar): se deja fuera.
            .mapNotNull { runCatching { FileProvider.getUriForFile(context, authority, it) }.getOrNull() }
        if (uris.isEmpty()) return null
        val subject = context.getString(R.string.log_share_subject, BuildConfig.VERSION_NAME)
        val send = if (uris.size == 1) {
            Intent(Intent.ACTION_SEND).putExtra(Intent.EXTRA_STREAM, uris.first())
        } else {
            Intent(Intent.ACTION_SEND_MULTIPLE).putParcelableArrayListExtra(Intent.EXTRA_STREAM, ArrayList(uris))
        }
        send.type = MIME
        send.putExtra(Intent.EXTRA_SUBJECT, subject)
        // El permiso de lectura viaja con el ClipData: así llega a la app que se elija en la hoja.
        send.clipData = ClipData(subject, arrayOf(MIME), ClipData.Item(uris.first())).apply {
            uris.drop(1).forEach { addItem(ClipData.Item(it)) }
        }
        send.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        return Intent.createChooser(send, context.getString(R.string.log_share_title))
    }

    /** La autoridad del `<provider>` del manifiesto es `${applicationId}.logs`. */
    private const val AUTHORITY_SUFFIX = ".logs"
    private const val MIME = "text/plain"
}
