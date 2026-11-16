package coroom.android.ui.components

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.util.LruCache
import androidx.compose.animation.animateColorAsState
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ColorFilter
import androidx.compose.ui.graphics.ColorMatrix
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalInspectionMode
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.coerceAtLeast
import androidx.compose.ui.unit.dp
import coroom.android.R
import coroom.android.bridge.BridgeException
import coroom.android.bridge.CoroomRepository
import coroom.android.container
import coroom.android.ui.theme.CoroomTheme
import coroom.android.ui.theme.SeatShape
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlin.math.absoluteValue
import kotlin.math.min

/**
 * La butaca de una persona: su avatar en toda la app, con el respaldo redondeado arriba y la base
 * casi recta ([SeatShape]). Lleva su imagen si tiene (`user` y la versión `avatar`, que lleva cada
 * dato que la muestra) y, mientras llega o si no se puede leer, la inicial del nombre sobre un
 * color estable por usuario (el mismo en todas las pantallas). `offline` la apaga, como en
 * «Desconectados».
 *
 * `lamp` enciende la lámpara de estado de la esquina inferior derecha (verde de conectado o de
 * ruta directa, ámbar de relay); `lampRing` es el color de lo que hay detrás, para separarla de
 * la butaca.
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
    lamp: Color? = null,
    lampRing: Color = MaterialTheme.colorScheme.surfaceContainerLow,
) {
    val colors = CoroomTheme.colors
    val (background, foreground) = if (offline) {
        colors.avatarOffline
    } else {
        colors.avatarPairs[(seed % colors.avatarPairs.size).toInt().absoluteValue]
    }
    val initial = name.trim().firstOrNull()?.uppercaseChar()?.toString() ?: "?"
    val density = LocalDensity.current
    // La inicial es parte del dibujo de la butaca: va en proporción a su lado, no al tamaño de
    // letra del sistema (si no, con la letra grande se saldría).
    val initialSize = with(density) { (size * INITIAL_SCALE).toSp() }
    // Las vistas previas no tienen la parte nativa: siempre la inicial.
    val image = if (user != null && avatar != null && !LocalInspectionMode.current) {
        val sidePx = with(density) { size.roundToPx() }.coerceAtLeast(1)
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
    Box(modifier = modifier.size(size)) {
        Box(
            modifier = Modifier.matchParentSize().clip(SeatShape).background(background),
            contentAlignment = Alignment.Center,
        ) {
            if (image != null) {
                Image(
                    bitmap = image,
                    contentDescription = null,
                    modifier = Modifier.matchParentSize(),
                    contentScale = ContentScale.Crop,
                    alpha = if (offline) OFFLINE_ALPHA else 1f,
                    colorFilter = if (offline) GRAYSCALE else null,
                )
            } else {
                Text(
                    text = initial,
                    color = foreground,
                    fontWeight = FontWeight.SemiBold,
                    fontSize = initialSize,
                    maxLines = 1,
                )
            }
        }
        if (lamp != null) {
            SeatLamp(
                color = lamp,
                ring = lampRing,
                seat = size,
                modifier = Modifier.align(Alignment.BottomEnd).offset(x = LAMP_OVERHANG, y = LAMP_OVERHANG),
            )
        }
    }
}

/** La lámpara de una butaca: un punto con un aro del color del fondo, y el cambio de color animado. */
@Composable
private fun SeatLamp(color: Color, ring: Color, seat: Dp, modifier: Modifier = Modifier) {
    val animated by animateColorAsState(targetValue = color, label = "seat-lamp")
    Box(
        modifier = modifier
            .size((seat * LAMP_SCALE).coerceAtLeast(LAMP_MIN))
            .background(ring, CircleShape)
            .padding(LAMP_RING)
            .background(animated, CircleShape),
    )
}

/**
 * Una butaca libre: el mismo contorno que la de una persona, sin relleno, con su número. Los
 * lectores de pantalla dicen «Butaca 3, libre».
 */
@Composable
fun EmptySeat(number: Int, modifier: Modifier = Modifier, size: Dp = 40.dp) {
    val colors = MaterialTheme.colorScheme
    val description = stringResource(R.string.seat_free, number)
    val numberSize = with(LocalDensity.current) { (size * NUMBER_SCALE).toSp() }
    Box(
        modifier = modifier
            .size(size)
            .border(EMPTY_SEAT_BORDER, colors.outline, SeatShape)
            .clearAndSetSemantics { contentDescription = description },
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = number.toString(),
            color = colors.onSurfaceVariant,
            fontWeight = FontWeight.Medium,
            fontSize = numberSize,
            maxLines = 1,
        )
    }
}

/** La inicial, en proporción al lado de la butaca. */
private const val INITIAL_SCALE = 0.4f

/** El número de una butaca libre, en proporción a su lado. */
private const val NUMBER_SCALE = 0.34f

/** La lámpara, en proporción al lado de la butaca, y su mínimo para que se vea en las pequeñas. */
private const val LAMP_SCALE = 0.3f
private val LAMP_MIN = 12.dp
private val LAMP_RING = 2.dp

/** Lo que la lámpara sobresale de la esquina de la butaca. */
private val LAMP_OVERHANG = 2.dp

private val EMPTY_SEAT_BORDER = 1.5.dp

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
