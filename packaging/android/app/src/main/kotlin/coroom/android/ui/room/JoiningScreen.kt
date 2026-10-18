package coroom.android.ui.room

import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
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
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import coroom.android.R
import coroom.android.ui.components.Avatar
import coroom.android.ui.components.CoroomIcon
import coroom.android.ui.components.PillButton
import coroom.android.ui.components.PillOutlinedButton
import coroom.android.ui.theme.CoroomText
import coroom.android.ui.theme.CoroomTheme

/** «Entrando en la sala de Bea» con la lista de pasos (boceto «Entrando»). */
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
        modifier = Modifier.fillMaxSize().background(colors.background).safeDrawingPadding().padding(24.dp),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            modifier = Modifier.widthIn(max = 440.dp).fillMaxWidth().semantics { liveRegion = LiveRegionMode.Polite },
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                val host = state.hostName
                Avatar(name = host ?: state.code, seed = (host ?: state.code).hashCode().toLong(), size = 44.dp)
                Column {
                    Text(
                        text = if (host != null) stringResource(R.string.joining_title_host, host) else stringResource(R.string.joining_title),
                        style = CoroomText.screenTitle.copy(fontWeight = CoroomText.cardTitle.fontWeight),
                        color = colors.onSurface,
                    )
                    val label = stringResource(R.string.joining_room_label)
                    Text(
                        text = buildAnnotatedString {
                            append(label)
                            append(" ")
                            withStyle(SpanStyle(fontFamily = FontFamily.Monospace)) { append(state.code) }
                        },
                        style = CoroomText.supporting.copy(fontSize = 14.sp),
                        color = colors.onSurfaceVariant,
                    )
                }
            }

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
                PillOutlinedButton(text = stringResource(R.string.action_cancel), onClick = onCancel, height = 44.dp)
            }
        }
    }
}

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
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
        when (stepState) {
            StepState.Done -> CoroomIcon(R.drawable.ic_check, contentDescription = null, tint = CoroomTheme.colors.success, size = 20.dp)
            StepState.Active -> Spinner()
            StepState.Pending -> CoroomIcon(R.drawable.ic_circle, contentDescription = null, tint = colors.outline, size = 20.dp)
        }
        Text(
            text = text,
            style = CoroomText.body,
            color = if (stepState == StepState.Pending) colors.outline else colors.onSurface,
        )
    }
}

@Composable
private fun Spinner() {
    val transition = rememberInfiniteTransition(label = "spinner")
    val angle by transition.animateFloat(
        initialValue = 0f,
        targetValue = 360f,
        animationSpec = infiniteRepeatable(tween(900, easing = LinearEasing), RepeatMode.Restart),
        label = "spinner-angle",
    )
    CoroomIcon(
        R.drawable.ic_loader_circle,
        contentDescription = null,
        tint = MaterialTheme.colorScheme.primary,
        size = 20.dp,
        modifier = Modifier.rotate(angle),
    )
}

/** «No se pudo entrar en la sala» con el motivo (boceto «No se pudo entrar»). */
@Composable
fun RoomErrorScreen(error: String, wasInRoom: Boolean, onHome: () -> Unit, onRetry: () -> Unit) {
    val colors = MaterialTheme.colorScheme
    Box(
        modifier = Modifier.fillMaxSize().background(colors.background).safeDrawingPadding().padding(24.dp),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            modifier = Modifier.widthIn(max = 460.dp).fillMaxWidth().semantics { liveRegion = LiveRegionMode.Assertive },
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Box(
                modifier = Modifier.size(48.dp).background(colors.errorContainer, CircleShape),
                contentAlignment = Alignment.Center,
            ) {
                CoroomIcon(R.drawable.ic_triangle_alert, contentDescription = null, tint = colors.onErrorContainer, size = 24.dp)
            }
            Text(
                text = stringResource(if (wasInRoom) R.string.room_error_title_ended else R.string.room_error_title),
                style = CoroomText.screenTitle.copy(fontWeight = CoroomText.cardTitle.fontWeight),
                color = colors.onSurface,
            )
            Text(text = error, style = CoroomText.body, color = colors.onSurfaceVariant)
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
            ) {
                PillOutlinedButton(text = stringResource(R.string.room_error_home), onClick = onHome, height = 44.dp)
                PillButton(
                    text = stringResource(if (wasInRoom) R.string.room_error_rejoin else R.string.action_retry),
                    onClick = onRetry,
                    height = 44.dp,
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
