package coroom.android.ui.components

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.util.LruCache
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.ColorFilter
import androidx.compose.ui.graphics.ColorMatrix
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalInspectionMode
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import coroom.android.bridge.BridgeException
import coroom.android.bridge.CoroomRepository
import coroom.android.container
import coroom.android.ui.theme.CoroomTheme
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlin.math.absoluteValue
import kotlin.math.min

/**
 * Avatar de una persona: su imagen si tiene (`user` y la versión `avatar`, que lleva cada dato que
 * la muestra) y, mientras llega o si no se puede leer, la inicial del nombre sobre un color estable
 * por usuario (el mismo en todas las pantallas). `offline` lo apaga, como en «Desconectados».
 */
@Composable
fun Avatar(
    name: String,
    seed: Long,
    modifier: Modifier = Modifier,
    size: Dp = 40.dp,
    offline: Boolean = false,
    user: Long? = null,
    avatar: Long? = null,
) {
    val colors = CoroomTheme.colors
    val (background, foreground) = if (offline) {
        colors.avatarOffline
    } else {
        colors.avatarPairs[(seed % colors.avatarPairs.size).toInt().absoluteValue]
    }
    val initial = name.trim().firstOrNull()?.uppercaseChar()?.toString() ?: "?"
    // Las vistas previas no tienen la parte nativa: siempre la inicial.
    val image = if (user != null && avatar != null && !LocalInspectionMode.current) {
        val sidePx = with(LocalDensity.current) { size.roundToPx() }.coerceAtLeast(1)
        val repository = LocalContext.current.container.repository
        // Si no llegó por la red (sin servidor justo al arrancar, por ejemplo), se vuelve a pedir
        // cuando el repositorio dice que hay servidor otra vez; el propio, que está siempre a la
        // vista en Inicio, si no, se quedaba en la inicial.
        val retry by repository.avatarRetry.collectAsState()
        val loaded by produceState(AvatarImages.cached(user, avatar, sidePx), user, avatar, sidePx, retry) {
            if (value == null) value = AvatarImages.load(repository, user, avatar, sidePx)
        }
        loaded
    } else {
        null
    }
    Box(
        modifier = modifier.size(size).background(background, CircleShape),
        contentAlignment = Alignment.Center,
    ) {
        if (image != null) {
            Image(
                bitmap = image,
                contentDescription = null,
                modifier = Modifier.matchParentSize().clip(CircleShape),
                contentScale = ContentScale.Crop,
                alpha = if (offline) OFFLINE_ALPHA else 1f,
                colorFilter = if (offline) GRAYSCALE else null,
            )
        } else {
            Text(
                text = initial,
                color = foreground,
                fontWeight = FontWeight.SemiBold,
                fontSize = (size.value * 0.4f).sp,
            )
        }
    }
}

private const val OFFLINE_ALPHA = 0.6f

private val GRAYSCALE = ColorFilter.colorMatrix(ColorMatrix().apply { setToSaturation(0f) })

/**
 * Las imágenes de los avatares ya decodificadas al lado en que se ven, por usuario, versión y
 * lado. El PNG lo da Rust (`avatar`: de su caché de disco o descargado); aquí se decodifica fuera
 * del hilo principal con el submuestreo que pide ese lado.
 */
private object AvatarImages {
    /** Bytes como mucho en memoria (un avatar de 40 dp a 3× ocupa unos 57 KiB). */
    private const val MAX_BYTES = 4 * 1024 * 1024

    private val images = object : LruCache<String, ImageBitmap>(MAX_BYTES) {
        override fun sizeOf(key: String, value: ImageBitmap): Int = value.width * value.height * 4
    }

    /** Versiones que el servidor dice que ya no tiene: no se vuelven a pedir. */
    private val missing = LruCache<String, Boolean>(64)

    private fun key(user: Long, version: Long, sidePx: Int) = "$user-$version-$sidePx"

    fun cached(user: Long, version: Long, sidePx: Int): ImageBitmap? = images.get(key(user, version, sidePx))

    /** La imagen, o `null` si no tiene o no se pudo traer (se ven las iniciales). */
    suspend fun load(repository: CoroomRepository, user: Long, version: Long, sidePx: Int): ImageBitmap? {
        val key = key(user, version, sidePx)
        images.get(key)?.let { return it }
        if (missing.get("$user-$version") != null) return null
        val path = try {
            repository.avatarPath(user, version)
        } catch (_: BridgeException) {
            // Sin red o sin sesión: se vuelve a intentar la próxima vez que se muestre.
            return null
        }
        if (path == null) {
            missing.put("$user-$version", true)
            return null
        }
        val image = withContext(Dispatchers.IO) { decode(path, sidePx) } ?: return null
        images.put(key, image)
        return image
    }

    private fun decode(path: String, sidePx: Int): ImageBitmap? {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeFile(path, bounds)
        val shortSide = min(bounds.outWidth, bounds.outHeight)
        if (shortSide <= 0) return null
        var sample = 1
        while (shortSide / (sample * 2) >= sidePx) sample *= 2
        val decoded = BitmapFactory.decodeFile(path, BitmapFactory.Options().apply { inSampleSize = sample })
            ?: return null
        val side = min(decoded.width, decoded.height)
        val square = if (decoded.width == decoded.height) {
            decoded
        } else {
            Bitmap.createBitmap(decoded, (decoded.width - side) / 2, (decoded.height - side) / 2, side, side)
        }
        val scaled = if (side == sidePx) square else Bitmap.createScaledBitmap(square, sidePx, sidePx, true)
        return scaled.asImageBitmap()
    }
}
