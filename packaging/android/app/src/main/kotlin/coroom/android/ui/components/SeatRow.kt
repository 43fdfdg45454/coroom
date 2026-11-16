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
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
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
) {
    /** Quién es, para saber si una butaca cambia de persona o solo de estado. */
    val identity: Any get() = user ?: name
}

/**
 * La fila de butacas de una sala: sus integrantes en el orden en que llegan en `occupants` (el
 * anfitrión primero). Solo se dibuja a quien está: el aforo lo fija el servidor y aquí no se
 * conoce, así que no hay butacas libres.
 *
 * Con `ownSeat`, detrás va la butaca que espera a quien va a entrar, en contorno y sin número
 * (color `ownSeatColor`). Cuando esa persona entra y pasa a `occupants`, su butaca cambia del
 * contorno a ocupada con un fundido corto; con «Quitar animaciones» del sistema el cambio es
 * inmediato. Si no caben, la fila se desliza.
 *
 * Sin `showLabels` van solo las butacas, sin el nombre debajo (cuando ya lo dice el texto de al lado).
 */
@Composable
fun SeatRow(
    occupants: List<SeatOccupant>,
    modifier: Modifier = Modifier,
    seatSize: Dp = 44.dp,
    ownSeat: Boolean = false,
    ownSeatColor: Color = LocalContentColor.current,
    showLabels: Boolean = true,
    lampRing: Color = MaterialTheme.colorScheme.surfaceContainerLow,
) {
    val seats = if (ownSeat) occupants.size + 1 else occupants.size
    Row(
        // El margen del final deja sitio a la lámpara de la última butaca, que sobresale un poco.
        modifier = modifier.horizontalScroll(rememberScrollState()).padding(end = SEAT_ROW_END),
        horizontalArrangement = Arrangement.spacedBy(SEAT_GAP),
        verticalAlignment = Alignment.Top,
    ) {
        repeat(seats) { index ->
            Seat(
                occupant = occupants.getOrNull(index),
                size = seatSize,
                ownSeatColor = ownSeatColor,
                showLabel = showLabels,
                lampRing = lampRing,
            )
        }
    }
}

/** Una butaca de la fila: la de `occupant` o, si es `null`, la que espera a quien va a entrar. */
@Composable
private fun Seat(occupant: SeatOccupant?, size: Dp, ownSeatColor: Color, showLabel: Boolean, lampRing: Color) {
    val colors = MaterialTheme.colorScheme
    // El fundido va por quién ocupa la butaca, no por su estado: si solo cambia la lámpara o la
    // foto, la butaca se actualiza sin fundirse otra vez.
    val current by rememberUpdatedState(occupant)
    val shownIdentity: Any = occupant?.identity ?: OwnSeatIdentity
    // Lo que dicen los lectores de pantalla: con etiquetas, «Bea, Anfitrión» de una vez; sin
    // ellas, nada (el nombre ya lo lee el texto de al lado). La butaca en contorno dice la suya.
    val description = if (occupant != null && showLabel) {
        listOfNotNull(occupant.name, occupant.role).joinToString(", ")
    } else {
        null
    }
    val seatModifier = Modifier.width(if (showLabel) size + SEAT_LABEL_MARGIN else size)
    Column(
        modifier = when {
            occupant == null -> seatModifier
            description != null -> seatModifier.clearAndSetSemantics { contentDescription = description }
            else -> seatModifier.clearAndSetSemantics { }
        },
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Crossfade(targetState = shownIdentity, animationSpec = tween(SEAT_FADE_MS), label = "seat") { identity ->
            val seated = current
            if (identity == OwnSeatIdentity) {
                OwnSeat(size = size, color = ownSeatColor)
            } else if (seated != null && seated.identity == identity) {
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

/** La identidad de la butaca en contorno, la de quien todavía no ha entrado. */
private object OwnSeatIdentity

private val SEAT_GAP = 8.dp
private val SEAT_ROW_END = 2.dp

/** Lo que el nombre puede sobresalir a los lados de su butaca. */
private val SEAT_LABEL_MARGIN = 20.dp

/** El fundido con que se enciende una butaca al ocuparse. */
private const val SEAT_FADE_MS = 220
