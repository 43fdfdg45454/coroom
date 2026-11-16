package coroom.android.ui.components

import androidx.compose.animation.Crossfade
import androidx.compose.animation.core.tween
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import coroom.android.ui.theme.CoroomText

/**
 * Quien ocupa una butaca de la fila: su nombre y color (`seed`), su imagen (`user` y la versión
 * `avatar`), la lámpara de su enlace si se sabe (`lamp`) y lo que es en la sala («Anfitrión», «Tú»).
 */
@Immutable
data class SeatOccupant(
    val name: String,
    val seed: Long,
    val user: Long? = null,
    val avatar: Long? = null,
    val lamp: Color? = null,
    val role: String? = null,
)

/**
 * La fila de butacas de una sala: sus integrantes en el orden en que llegan en `occupants` (el
 * anfitrión primero) y, detrás, butacas libres en contorno y numeradas hasta completar `minSeats`.
 * Cuando alguien entra, su butaca pasa de libre a ocupada con un fundido corto; con «Quitar
 * animaciones» del sistema el cambio es inmediato. Si no caben, la fila se desliza.
 *
 * Sin `showLabels` van solo las butacas, sin el nombre debajo (cuando ya lo dice el texto de al lado).
 */
@Composable
fun SeatRow(
    occupants: List<SeatOccupant>,
    modifier: Modifier = Modifier,
    seatSize: Dp = 44.dp,
    minSeats: Int = MIN_SEATS,
    showLabels: Boolean = true,
    lampRing: Color = MaterialTheme.colorScheme.surfaceContainerLow,
) {
    val seats = maxOf(occupants.size, minSeats)
    Row(
        // El margen del final deja sitio a la lámpara de la última butaca, que sobresale un poco.
        modifier = modifier.horizontalScroll(rememberScrollState()).padding(end = SEAT_ROW_END),
        horizontalArrangement = Arrangement.spacedBy(SEAT_GAP),
        verticalAlignment = Alignment.Top,
    ) {
        repeat(seats) { index ->
            Seat(
                occupant = occupants.getOrNull(index),
                number = index + 1,
                size = seatSize,
                showLabel = showLabels,
                lampRing = lampRing,
            )
        }
    }
}

@Composable
private fun Seat(occupant: SeatOccupant?, number: Int, size: Dp, showLabel: Boolean, lampRing: Color) {
    val colors = MaterialTheme.colorScheme
    Column(
        modifier = Modifier
            .width(if (showLabel) size + SEAT_LABEL_MARGIN else size)
            .semantics(mergeDescendants = true) {},
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Crossfade(targetState = occupant, animationSpec = tween(SEAT_FADE_MS), label = "seat") { seated ->
            if (seated == null) {
                EmptySeat(number = number, size = size)
            } else {
                Avatar(
                    name = seated.name,
                    seed = seated.seed,
                    size = size,
                    user = seated.user,
                    avatar = seated.avatar,
                    lamp = seated.lamp,
                    lampRing = lampRing,
                )
            }
        }
        if (occupant != null && showLabel) {
            Column(horizontalAlignment = Alignment.CenterHorizontally) {
                Text(
                    text = occupant.name,
                    style = CoroomText.label,
                    color = colors.onSurface,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    textAlign = TextAlign.Center,
                )
                if (occupant.role != null) {
                    Text(
                        text = occupant.role,
                        style = CoroomText.label,
                        color = colors.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        textAlign = TextAlign.Center,
                    )
                }
            }
        }
    }
}

/** Las butacas que se dibujan como mínimo: las que falten por ocupar van libres. */
const val MIN_SEATS = 4

private val SEAT_GAP = 8.dp
private val SEAT_ROW_END = 2.dp

/** Lo que el nombre puede sobresalir a los lados de su butaca. */
private val SEAT_LABEL_MARGIN = 20.dp

/** El fundido con que se enciende una butaca al ocuparse. */
private const val SEAT_FADE_MS = 220
