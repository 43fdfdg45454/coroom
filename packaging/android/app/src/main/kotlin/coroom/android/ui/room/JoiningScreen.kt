package coroom.android.ui.room

import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import coroom.android.R
import coroom.android.bridge.Account
import coroom.android.ui.components.CoroomIcon
import coroom.android.ui.components.PrimaryButton
import coroom.android.ui.components.SeatOccupant
import coroom.android.ui.components.SeatRow
import coroom.android.ui.components.SecondaryButton
import coroom.android.ui.components.SignageCode
import coroom.android.ui.theme.CoroomText
import coroom.android.ui.theme.CoroomTheme

/**
 * «Entrando en la sala de Bea»: a la izquierda, el código en señalética y la fila de butacas (la
 * del anfitrión y la propia, en contorno hasta que hay conexión con él y entonces encendida con
 * la lámpara de la ruta); a la derecha, los pasos. La sala va siempre en horizontal.
 */
@Composable
fun JoiningScreen(state: RoomUiState, onCancel: () -> Unit) {
    val colors = MaterialTheme.colorScheme
    val done = state.steps.values.count { it == StepState.Done }
    val active = state.steps.values.count { it == StepState.Active }
    val progress by animateFloatAsState(
        targetValue = (done + active * 0.5f) / JoinStep.entries.size,
        animationSpec = tween(300),
        label = "join-progress",
    )
    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(colors.background)
            .safeDrawingPadding()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 24.dp, vertical = 20.dp),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            modifier = Modifier.widthIn(max = 760.dp).fillMaxWidth().semantics { liveRegion = LiveRegionMode.Polite },
            verticalArrangement = Arrangement.spacedBy(20.dp),
        ) {
            Row(horizontalArrangement = Arrangement.spacedBy(32.dp)) {
                Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(16.dp)) {
                    val host = state.hostName
                    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        Text(
                            text = if (host != null) stringResource(R.string.joining_title_host, host) else stringResource(R.string.joining_title),
                            style = CoroomText.screenTitle,
                            color = colors.onSurface,
                        )
                        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                            Text(
                                text = stringResource(R.string.joining_room_label),
                                style = CoroomText.supporting,
                                color = colors.onSurfaceVariant,
                            )
                            SignageCode(code = state.code, style = CoroomText.signage, color = colors.onSurface)
                        }
                    }
                    // Aún no se sabe quién más hay en la sala: solo la butaca del anfitrión y la
                    // propia, en contorno hasta que hay conexión con él.
                    SeatRow(
                        occupants = joiningSeats(state),
                        seatSize = 48.dp,
                        ownSeat = !selfSeated(state),
                        ownSeatColor = colors.onSurfaceVariant,
                        lampRing = colors.background,
                    )
                }
                Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(14.dp)) {
                    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                        JoinStep.entries.forEach { step ->
                            StepRow(step, state.steps[step] ?: StepState.Pending, state.relayed)
                        }
                    }
                    LinearProgressIndicator(
                        progress = { progress },
                        modifier = Modifier.fillMaxWidth().height(4.dp),
                        color = colors.primary,
                        trackColor = colors.surfaceContainerHighest,
                        strokeCap = StrokeCap.Round,
                        gapSize = 0.dp,
                        drawStopIndicator = {},
                    )
                }
            }

            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Text(
                    text = stringResource(R.string.joining_rotate_hint),
                    style = CoroomText.supporting,
                    color = colors.onSurfaceVariant,
                    modifier = Modifier.weight(1f),
                )
                SecondaryButton(text = stringResource(R.string.action_cancel), onClick = onCancel)
            }
        }
    }
}

/**
 * Las butacas mientras se entra: la del anfitrión (el miembro 0 si ya llegó la lista; si no, la
 * cuenta con la que se abrió la sala desde Inicio, para que no cambie de color ni pierda la foto,
 * o solo su nombre) y la propia, que solo se ocupa cuando ya hay camino con el anfitrión, con la
 * lámpara verde de la ruta directa o la ámbar del relay.
 */
@Composable
private fun joiningSeats(state: RoomUiState): List<SeatOccupant> {
    val extra = CoroomTheme.colors
    val hostMember = state.members.firstOrNull { it.guest == 0 }
    val hostName = hostMember?.name?.takeIf { it.isNotBlank() } ?: state.hostName
    val host = hostName?.let { name ->
        val user = hostMember?.user ?: state.hostUser
        SeatOccupant(
            name = name,
            seed = user ?: name.hashCode().toLong(),
            user = user,
            avatar = if (hostMember != null) hostMember.avatar else state.hostAvatar,
            role = stringResource(R.string.seat_host),
        )
    }
    val self = state.self?.takeIf { selfSeated(state) }?.let { account ->
        SeatOccupant(
            name = account.shownName,
            seed = account.userId,
            user = account.userId,
            avatar = account.avatar,
            lamp = when (state.relayed) {
                true -> extra.cautionDot
                false -> extra.successDot
                null -> null
            },
            role = stringResource(R.string.seat_you),
        )
    }
    return listOfNotNull(host, self)
}

/** Si la butaca propia ya está ocupada: hay cuenta y ya hay camino con el anfitrión. */
private fun selfSeated(state: RoomUiState): Boolean =
    state.self != null && state.steps[JoinStep.Path] == StepState.Done

@Composable
private fun StepRow(step: JoinStep, stepState: StepState, relayed: Boolean?) {
    val colors = MaterialTheme.colorScheme
    val text = stringResource(
        when (step) {
            JoinStep.Server -> if (stepState == StepState.Done) R.string.step_server_done else R.string.step_server
            JoinStep.Path -> when {
                stepState != StepState.Done -> R.string.step_path
                relayed == true -> R.string.step_path_relay
                else -> R.string.step_path_direct
            }
            JoinStep.Probe -> if (stepState == StepState.Done) R.string.step_probe_done else R.string.step_probe
            JoinStep.Keys -> if (stepState == StepState.Done) R.string.step_keys_done else R.string.step_keys
        },
    )
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        when (stepState) {
            StepState.Done -> CoroomIcon(R.drawable.ic_check, contentDescription = null, tint = CoroomTheme.colors.success, size = 20.dp)
            StepState.Active -> CircularProgressIndicator(modifier = Modifier.size(20.dp), strokeWidth = 2.dp)
            StepState.Pending -> CoroomIcon(R.drawable.ic_circle, contentDescription = null, tint = colors.outline, size = 20.dp)
        }
        Text(
            text = text,
            style = CoroomText.body,
            color = if (stepState == StepState.Pending) colors.onSurfaceVariant else colors.onSurface,
        )
    }
}

/** «No se pudo entrar en la sala» con el motivo, y qué hacer. */
@Composable
fun RoomErrorScreen(error: String, wasInRoom: Boolean, onHome: () -> Unit, onRetry: () -> Unit) {
    val colors = MaterialTheme.colorScheme
    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(colors.background)
            .safeDrawingPadding()
            .verticalScroll(rememberScrollState())
            .padding(24.dp),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            modifier = Modifier.widthIn(max = 480.dp).fillMaxWidth().semantics { liveRegion = LiveRegionMode.Assertive },
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Box(
                modifier = Modifier.size(48.dp).background(colors.errorContainer, MaterialTheme.shapes.medium),
                contentAlignment = Alignment.Center,
            ) {
                CoroomIcon(R.drawable.ic_triangle_alert, contentDescription = null, tint = colors.onErrorContainer, size = 24.dp)
            }
            Text(
                text = stringResource(if (wasInRoom) R.string.room_error_title_ended else R.string.room_error_title),
                style = CoroomText.screenTitle,
                color = colors.onSurface,
            )
            Text(text = error, style = CoroomText.body, color = colors.onSurfaceVariant)
            Row(
                modifier = Modifier.fillMaxWidth().padding(top = 4.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
            ) {
                SecondaryButton(text = stringResource(R.string.room_error_home), onClick = onHome)
                PrimaryButton(
                    text = stringResource(if (wasInRoom) R.string.room_error_rejoin else R.string.action_retry),
                    onClick = onRetry,
                    icon = R.drawable.ic_refresh_cw,
                )
            }
        }
    }
}

@Preview(name = "Entrando", widthDp = 844, heightDp = 390)
@Composable
private fun JoiningPreview() {
    CoroomTheme(darkTheme = true, dynamicColor = false) {
        JoiningScreen(
            state = RoomUiState(
                code = "K7Q2MZ",
                hostName = "Bea",
                self = Account(username = "ana", displayName = "Ana", userId = 1),
                relayed = false,
                steps = mapOf(
                    JoinStep.Server to StepState.Done,
                    JoinStep.Path to StepState.Done,
                    JoinStep.Probe to StepState.Active,
                    JoinStep.Keys to StepState.Pending,
                ),
            ),
            onCancel = {},
        )
    }
}

@Preview(name = "No se pudo entrar", widthDp = 844, heightDp = 390)
@Composable
private fun RoomErrorPreview() {
    CoroomTheme(darkTheme = true, dynamicColor = false) {
        RoomErrorScreen(
            error = "No hay camino directo con el anfitrión y el servidor no tiene relay activado. " +
                "Pide a quien administra el servidor que lo active, o prueba desde otra red.",
            wasInRoom = false,
            onHome = {},
            onRetry = {},
        )
    }
}
