package coroom.android.ui.theme

import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.graphics.Color

/**
 * El tono de un aviso o de una píldora de estado. Cada uno dice una cosa: neutro (estado vacío,
 * explicación), información (acento), bien (verde: conectado, directo), aviso (ámbar: relay,
 * calidad justa), en vivo (piloto rojo: se está compartiendo) y error (rojo: falló, destructivo).
 */
enum class Tone { Neutral, Info, Success, Warning, Live, Error }

/** Los colores de un [Tone]: su contenedor, lo que va encima y su lámpara. */
@Immutable
data class ToneColors(
    val container: Color,
    /** Título e icono sobre el contenedor. */
    val content: Color,
    /** Texto secundario sobre el contenedor. */
    val supporting: Color,
    /** El punto de estado, que se ve también sobre las superficies normales. */
    val lamp: Color,
)

/** Los colores del tono con el esquema en uso (el dinámico del sistema o el propio). */
@Composable
fun Tone.colors(): ToneColors {
    val scheme = MaterialTheme.colorScheme
    val extra = CoroomTheme.colors
    return when (this) {
        Tone.Neutral -> ToneColors(scheme.surfaceContainerHigh, scheme.onSurface, scheme.onSurfaceVariant, scheme.outline)
        Tone.Info -> ToneColors(scheme.primaryContainer, scheme.onPrimaryContainer, scheme.onPrimaryContainer, scheme.primary)
        Tone.Success -> ToneColors(
            extra.successContainer,
            extra.onSuccessContainer,
            extra.onSuccessContainer,
            extra.successDot,
        )
        Tone.Warning -> ToneColors(
            extra.warningContainer,
            extra.onWarningContainer,
            extra.onWarningContainer,
            extra.cautionDot,
        )
        Tone.Live -> ToneColors(extra.liveContainer, extra.onLiveContainer, extra.onLiveContainer, extra.live)
        Tone.Error -> ToneColors(scheme.errorContainer, scheme.onErrorContainer, scheme.onErrorContainer, scheme.error)
    }
}
