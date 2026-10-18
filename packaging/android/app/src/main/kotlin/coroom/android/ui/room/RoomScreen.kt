package coroom.android.ui.room

import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.ime
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.viewinterop.AndroidView

/**
 * La pantalla de la sala, en horizontal: el vídeo siempre debajo (así la superficie existe antes
 * de que llegue el primer fotograma) y encima «Entrando», la sala o el error.
 */
@Composable
fun RoomScreen(
    state: RoomUiState,
    video: VideoView,
    actions: RoomActions,
    onCancelJoin: () -> Unit,
    onRetry: () -> Unit,
    onClose: () -> Unit,
) {
    val phase = state.phase
    BackHandler {
        when {
            phase is RoomPhase.Failed -> onClose()
            phase == RoomPhase.Joining -> onCancelJoin()
            state.panel != Panel.None -> actions.onClosePanel()
            else -> actions.onAskLeave(true)
        }
    }

    // El teclado en pantalla: abrirlo o cerrarlo desde la barra, y saber si el usuario lo cerró.
    val imeVisible = WindowInsets.ime.getBottom(LocalDensity.current) > 0
    val currentKeyboard by rememberUpdatedState(state.keyboardVisible)
    LaunchedEffect(state.keyboardVisible) { video.showKeyboard(state.keyboardVisible) }
    LaunchedEffect(imeVisible) {
        if (!imeVisible && currentKeyboard && state.panel != Panel.Chat) actions.onToggleKeyboard()
    }

    Box(modifier = Modifier.fillMaxSize()) {
        AndroidView(factory = { video }, modifier = Modifier.fillMaxSize())
        AnimatedContent(
            targetState = phase::class,
            transitionSpec = { fadeIn() togetherWith fadeOut() },
            label = "room-phase",
        ) { phaseClass ->
            when (phaseClass) {
                RoomPhase.Joining::class -> JoiningScreen(state, onCancel = onCancelJoin)
                RoomPhase.InRoom::class -> RoomOverlay(state, actions)
                else -> (state.phase as? RoomPhase.Failed)?.let { failed ->
                    RoomErrorScreen(failed.error, failed.wasInRoom, onHome = onClose, onRetry = onRetry)
                }
            }
        }
    }
}
