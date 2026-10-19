package coroom.android.ui.room

import androidx.annotation.DrawableRes
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.displayCutout
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.Dp
import coroom.android.R
import coroom.android.bridge.ChatMessage
import coroom.android.bridge.Member
import coroom.android.bridge.RoomStats
import coroom.android.bridge.TouchMode
import coroom.android.ui.components.CoroomIcon
import coroom.android.ui.components.StatusDot
import coroom.android.ui.text
import coroom.android.ui.theme.CoroomText
import coroom.android.ui.theme.CoroomTheme
import kotlinx.coroutines.delay

/** Las acciones de la barra de la sala. */
class RoomActions(
    val onToggleMic: () -> Unit,
    val onToggleKeyboard: () -> Unit,
    val onCycleTouchMode: () -> Unit,
    val onTogglePanel: (Panel) -> Unit,
    val onClosePanel: () -> Unit,
    val onAskLeave: (Boolean) -> Unit,
    val onLeave: () -> Unit,
    val onSendChat: (String) -> Unit,
    val onClearNotice: (Int) -> Unit,
    /** Enseñar la barra escondida (toque en la píldora reducida). */
    val onRevealOverlay: () -> Unit,
    /** «Compartir registro» desde el panel de conexión. */
    val onShareLog: () -> Unit,
)

/**
 * Lo que va encima del vídeo con la sala abierta (bocetos «Sala», «Sala: conexión y calidad» y
 * «Sala: chat»): la píldora de conexión siempre visible, la barra lateral, el modo táctil, los
 * avisos y los paneles.
 *
 * La barra lateral y los chips se esconden solos a los [OVERLAY_HIDE_MS] sin tocarlos (con un
 * panel, el diálogo de salir, el teclado o una reconexión no se esconden): queda solo la píldora
 * reducida al punto de la calidad y la latencia, que siguen siempre a la vista, y tocarla vuelve
 * a enseñarlo todo. Los toques en el vídeo siguen yendo al juego.
 */
@Composable
fun RoomOverlay(state: RoomUiState, actions: RoomActions) {
    val panelOpen = state.panel != Panel.None
    val overlayVisible = state.overlayVisible
    Box(modifier = Modifier.fillMaxSize()) {
        // Con un panel abierto el vídeo se oscurece y tocarlo cierra el panel (no llega al juego).
        AnimatedVisibility(visible = panelOpen, enter = fadeIn(), exit = fadeOut()) {
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .background(Color.Black.copy(alpha = 0.35f))
                    .clickable(
                        interactionSource = remember { MutableInteractionSource() },
                        indication = null,
                        onClick = actions.onClosePanel,
                    ),
            )
        }

        Box(modifier = Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.displayCutout)) {
            Row(
                modifier = Modifier.align(Alignment.TopStart).padding(start = 14.dp, top = 12.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                ConnectionPill(
                    state,
                    expanded = overlayVisible,
                    onClick = { if (overlayVisible) actions.onTogglePanel(Panel.Connection) else actions.onRevealOverlay() },
                )
                val permissions = state.permissions
                AnimatedVisibility(visible = overlayVisible && permissions != null, enter = fadeIn(), exit = fadeOut()) {
                    permissions?.let { PermissionsChip(it) }
                }
            }

            AnimatedVisibility(
                visible = overlayVisible && !panelOpen,
                modifier = Modifier.align(Alignment.BottomStart).padding(start = 14.dp, bottom = 12.dp),
                enter = slideInVertically { it } + fadeIn(),
                exit = slideOutVertically { it } + fadeOut(),
            ) {
                TouchModeChip(state.touchMode)
            }
            AnimatedVisibility(
                visible = overlayVisible && !panelOpen,
                modifier = Modifier.align(Alignment.CenterEnd).padding(end = 12.dp, top = 12.dp, bottom = 12.dp),
                enter = slideInHorizontally { it } + fadeIn(),
                exit = slideOutHorizontally { it } + fadeOut(),
            ) {
                RoomRail(state, actions)
            }

            NoticeToast(state, actions.onClearNotice, Modifier.align(Alignment.TopCenter).padding(top = 12.dp))
        }

        AnimatedVisibility(
            visible = state.panel == Panel.Connection,
            modifier = Modifier.align(Alignment.CenterEnd),
            enter = slideInHorizontally { it } + fadeIn(),
            exit = slideOutHorizontally { it } + fadeOut(),
        ) {
            ConnectionPanel(state, onClose = actions.onClosePanel, onShareLog = actions.onShareLog)
        }
        AnimatedVisibility(
            visible = state.panel == Panel.Chat,
            modifier = Modifier.align(Alignment.CenterEnd),
            enter = slideInHorizontally { it } + fadeIn(),
            exit = slideOutHorizontally { it } + fadeOut(),
        ) {
            ChatPanel(state, onClose = actions.onClosePanel, onSend = actions.onSendChat)
        }
    }

    if (state.confirmLeave) {
        AlertDialog(
            onDismissRequest = { actions.onAskLeave(false) },
            title = { Text(stringResource(R.string.leave_title)) },
            text = { Text(stringResource(R.string.leave_body)) },
            confirmButton = {
                TextButton(onClick = actions.onLeave) {
                    Text(stringResource(R.string.leave_confirm), color = MaterialTheme.colorScheme.error)
                }
            },
            dismissButton = {
                TextButton(onClick = { actions.onAskLeave(false) }) { Text(stringResource(R.string.leave_stay)) }
            },
        )
    }
}

@Composable
private fun qualityColor(state: RoomUiState): Color = when {
    state.reconnecting != null -> CoroomTheme.colors.caution
    else -> when (state.quality) {
        LinkQuality.Excellent, LinkQuality.Good -> CoroomTheme.colors.successDot
        LinkQuality.Acceptable -> CoroomTheme.colors.caution
        LinkQuality.Poor -> MaterialTheme.colorScheme.error
        null -> MaterialTheme.colorScheme.outline
    }
}

@Composable
private fun routeText(relayed: Boolean?): String = stringResource(
    when (relayed) {
        true -> R.string.route_relay_short
        false -> R.string.route_direct_short
        null -> R.string.route_unknown_short
    },
)

/**
 * «● 14 ms · directa · AV1»: la latencia siempre a la vista; abre el panel de conexión. Con la
 * barra escondida (`expanded` en falso) se queda en «● 14 ms» (y el aviso de chat sin leer), y
 * tocarla enseña la barra.
 */
@Composable
fun ConnectionPill(state: RoomUiState, onClick: () -> Unit, modifier: Modifier = Modifier, expanded: Boolean = true) {
    val colors = MaterialTheme.colorScheme
    val rtt = state.shownRttMs?.let { stringResource(R.string.unit_ms, formatNumber(it, 0)) } ?: stringResource(R.string.no_value)
    val route = routeText(state.shownRelayed)
    val detail = listOfNotNull(route, state.codec?.takeIf { it.isNotBlank() }).joinToString(" · ")
    val quality = stringResource(
        when {
            state.reconnecting != null -> R.string.quality_reconnecting
            else -> when (state.quality) {
                LinkQuality.Excellent -> R.string.quality_excellent
                LinkQuality.Good -> R.string.quality_good
                LinkQuality.Acceptable -> R.string.quality_acceptable
                LinkQuality.Poor -> R.string.quality_poor
                null -> R.string.quality_unknown
            }
        },
    )
    val description = when {
        expanded -> stringResource(R.string.pill_description, quality, rtt, route)
        state.unreadChat -> stringResource(R.string.pill_description_collapsed_unread, quality, rtt)
        else -> stringResource(R.string.pill_description_collapsed, quality, rtt)
    }
    Row(
        modifier = modifier
            .height(34.dp)
            .clip(RoundedCornerShape(17.dp))
            .background(CoroomTheme.colors.overlay)
            .clickable(role = Role.Button, onClick = onClick)
            .semantics(mergeDescendants = true) { contentDescription = description }
            .padding(start = 10.dp, end = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        StatusDot(qualityColor(state))
        Text(
            text = if (state.reconnecting != null) stringResource(R.string.quality_reconnecting) else rtt,
            style = CoroomText.supporting.copy(fontWeight = FontWeight.SemiBold),
            color = colors.onSurface,
        )
        AnimatedVisibility(visible = expanded) {
            Text(text = detail, style = CoroomText.supporting, color = colors.onSurfaceVariant)
        }
        // Con la barra escondida no se ve el aviso del chat de la barra lateral: va aquí.
        AnimatedVisibility(visible = !expanded && state.unreadChat, enter = fadeIn(), exit = fadeOut()) {
            Box(Modifier.size(8.dp).background(colors.error, CircleShape))
        }
    }
}

@Composable
private fun PermissionsChip(permissions: InputPermissions) {
    val text = when {
        permissions.gamepadSlot != null && permissions.keyboardMouse ->
            stringResource(R.string.permissions_all, permissions.gamepadSlot)
        permissions.gamepadSlot != null -> stringResource(R.string.permissions_pad, permissions.gamepadSlot)
        permissions.keyboardMouse -> stringResource(R.string.permissions_keyboard)
        else -> stringResource(R.string.permissions_watch)
    }
    val icon = when {
        permissions.gamepadSlot != null -> R.drawable.ic_gamepad_2
        permissions.keyboardMouse -> R.drawable.ic_keyboard
        else -> R.drawable.ic_info
    }
    OverlayChip(icon = icon, text = text)
}

@Composable
private fun TouchModeChip(mode: TouchMode, modifier: Modifier = Modifier) {
    OverlayChip(
        icon = if (mode == TouchMode.Trackpad) R.drawable.ic_trackpad else R.drawable.ic_pointer,
        text = stringResource(if (mode == TouchMode.Trackpad) R.string.touch_trackpad else R.string.touch_direct),
        modifier = modifier,
    )
}

@Composable
private fun OverlayChip(@DrawableRes icon: Int, text: String, modifier: Modifier = Modifier) {
    Row(
        modifier = modifier
            .height(32.dp)
            .clip(RoundedCornerShape(16.dp))
            .background(CoroomTheme.colors.overlay)
            .padding(start = 10.dp, end = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        CoroomIcon(icon, contentDescription = null, size = 18.dp)
        Text(text = text, style = CoroomText.supporting, color = MaterialTheme.colorScheme.onSurface)
    }
}

/** La barra vertical de la derecha: micrófono, teclado, modo táctil, chat, conexión y salir. */
@Composable
private fun RoomRail(state: RoomUiState, actions: RoomActions, modifier: Modifier = Modifier) {
    val colors = MaterialTheme.colorScheme
    Column(
        modifier = modifier
            .fillMaxHeight()
            .width(60.dp)
            .clip(RoundedCornerShape(30.dp))
            .background(CoroomTheme.colors.overlayStrong)
            .padding(vertical = 8.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.SpaceBetween,
    ) {
        RailButton(
            icon = if (state.micMuted) R.drawable.ic_mic_off else R.drawable.ic_mic,
            description = stringResource(if (state.micMuted) R.string.rail_mic_unmute else R.string.rail_mic_mute),
            active = !state.micMuted,
            onClick = actions.onToggleMic,
        )
        RailButton(
            icon = R.drawable.ic_keyboard,
            description = stringResource(if (state.keyboardVisible) R.string.rail_keyboard_hide else R.string.rail_keyboard_show),
            active = state.keyboardVisible,
            onClick = actions.onToggleKeyboard,
        )
        RailButton(
            icon = if (state.touchMode == TouchMode.Trackpad) R.drawable.ic_trackpad else R.drawable.ic_pointer,
            description = stringResource(R.string.rail_touch_mode),
            onClick = actions.onCycleTouchMode,
        )
        Box {
            RailButton(
                icon = R.drawable.ic_message_square,
                description = stringResource(if (state.unreadChat) R.string.rail_chat_unread else R.string.rail_chat),
                onClick = { actions.onTogglePanel(Panel.Chat) },
            )
            if (state.unreadChat) {
                Box(
                    Modifier
                        .align(Alignment.TopEnd)
                        .padding(top = 6.dp, end = 6.dp)
                        .size(10.dp)
                        .background(colors.error, CircleShape),
                )
            }
        }
        RailButton(
            icon = R.drawable.ic_activity,
            description = stringResource(R.string.rail_connection),
            onClick = { actions.onTogglePanel(Panel.Connection) },
        )
        RailButton(
            icon = R.drawable.ic_log_out,
            description = stringResource(R.string.rail_leave),
            onClick = { actions.onAskLeave(true) },
            container = colors.errorContainer,
            tint = colors.onErrorContainer,
        )
    }
}

@Composable
private fun RailButton(
    @DrawableRes icon: Int,
    description: String,
    onClick: () -> Unit,
    active: Boolean = false,
    container: Color? = null,
    tint: Color? = null,
) {
    val colors = MaterialTheme.colorScheme
    val background = container ?: if (active) colors.primaryContainer else Color.Transparent
    val iconTint = tint ?: if (active) colors.onPrimaryContainer else colors.onSurface
    Box(
        modifier = Modifier
            .size(44.dp)
            .clip(CircleShape)
            .background(background)
            .clickable(role = Role.Button, onClick = onClick)
            .semantics { contentDescription = description },
        contentAlignment = Alignment.Center,
    ) {
        CoroomIcon(icon, contentDescription = null, tint = iconTint)
    }
}

/** Avisos breves arriba en el centro, y la reconexión mientras dure. */
@Composable
private fun NoticeToast(state: RoomUiState, onClear: (Int) -> Unit, modifier: Modifier = Modifier) {
    val reconnecting = state.reconnecting
    val notice = state.notice
    LaunchedEffect(state.noticeSeq) {
        if (notice != null) {
            delay(NOTICE_MS)
            onClear(state.noticeSeq)
        }
    }
    val text = when {
        reconnecting != null && reconnecting.retryInMs > 0 -> stringResource(
            R.string.room_reconnecting_in,
            ((reconnecting.retryInMs + 999) / 1000).toInt(),
            reconnecting.attempt,
            reconnecting.of,
        )
        reconnecting != null -> stringResource(R.string.room_reconnecting, reconnecting.attempt, reconnecting.of)
        notice != null -> notice.text()
        else -> null
    }
    AnimatedVisibility(
        visible = text != null,
        modifier = modifier,
        enter = slideInVertically { -it } + fadeIn(),
        exit = slideOutVertically { -it } + fadeOut(),
    ) {
        Column(
            modifier = Modifier
                .clip(RoundedCornerShape(17.dp))
                .background(CoroomTheme.colors.overlay)
                .padding(horizontal = 14.dp, vertical = 8.dp)
                .semantics { liveRegion = LiveRegionMode.Polite },
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                CoroomIcon(
                    if (reconnecting != null) R.drawable.ic_wifi_off else R.drawable.ic_info,
                    contentDescription = null,
                    tint = if (reconnecting != null) CoroomTheme.colors.caution else CoroomTheme.colors.success,
                    size = 18.dp,
                )
                Text(text = text.orEmpty(), style = CoroomText.supporting, color = MaterialTheme.colorScheme.onSurface)
            }
            if (reconnecting != null && reconnecting.reason.isNotBlank()) {
                Text(
                    text = reconnecting.reason,
                    style = CoroomText.label,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

/** El panel lateral de los bocetos: fondo de superficie, esquinas izquierdas redondeadas. */
@Composable
fun SidePanel(
    title: String,
    width: Dp,
    closeDescription: String,
    onClose: () -> Unit,
    content: @Composable BoxScope.() -> Unit,
) {
    val colors = MaterialTheme.colorScheme
    Column(
        modifier = Modifier
            .fillMaxHeight()
            .width(width)
            .clip(RoundedCornerShape(topStart = 24.dp, bottomStart = 24.dp))
            .background(colors.surfaceContainer)
            // Los toques dentro del panel no lo atraviesan hasta el fondo (que lo cerraría).
            .pointerInput(Unit) { detectTapGestures { } }
            .windowInsetsPadding(WindowInsets.displayCutout)
            .padding(start = 20.dp, end = 18.dp, top = 14.dp, bottom = 14.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                text = title,
                style = CoroomText.cardTitle.copy(fontWeight = FontWeight.SemiBold),
                color = colors.onSurface,
                modifier = Modifier.weight(1f),
            )
            Box(
                modifier = Modifier
                    .size(44.dp)
                    .clip(CircleShape)
                    .clickable(role = Role.Button, onClick = onClose)
                    .semantics { contentDescription = closeDescription },
                contentAlignment = Alignment.Center,
            ) {
                CoroomIcon(R.drawable.ic_x, contentDescription = null)
            }
        }
        Box(modifier = Modifier.weight(1f), content = content)
    }
}

private const val NOTICE_MS = 4_000L

@Preview(name = "Sala", widthDp = 844, heightDp = 390)
@Composable
private fun RoomOverlayPreview() {
    CoroomTheme(darkTheme = true, dynamicColor = false) {
        Box(Modifier.fillMaxSize().background(Color(0xFF3D8B4A))) {
            RoomOverlay(PreviewRoom.state, PreviewRoom.actions)
        }
    }
}

@Preview(name = "Sala: barra escondida", widthDp = 844, heightDp = 390)
@Composable
private fun RoomOverlayHiddenPreview() {
    CoroomTheme(darkTheme = true, dynamicColor = false) {
        Box(Modifier.fillMaxSize().background(Color(0xFF3D8B4A))) {
            RoomOverlay(PreviewRoom.state.copy(overlayShown = false), PreviewRoom.actions)
        }
    }
}

@Preview(name = "Sala: conexión y calidad", widthDp = 844, heightDp = 390)
@Composable
private fun RoomConnectionPreview() {
    CoroomTheme(darkTheme = true, dynamicColor = false) {
        Box(Modifier.fillMaxSize().background(Color(0xFF3D8B4A))) {
            RoomOverlay(PreviewRoom.state.copy(panel = Panel.Connection), PreviewRoom.actions)
        }
    }
}

@Preview(name = "Sala: chat", widthDp = 844, heightDp = 390)
@Composable
private fun RoomChatPreview() {
    CoroomTheme(darkTheme = true, dynamicColor = false) {
        Box(Modifier.fillMaxSize().background(Color(0xFF3D8B4A))) {
            RoomOverlay(PreviewRoom.state.copy(panel = Panel.Chat, unreadChat = false), PreviewRoom.actions)
        }
    }
}

/** Datos de muestra para las vistas previas de la sala (nombres ficticios). */
internal object PreviewRoom {
    val state = RoomUiState(
        code = "K7Q2MZ",
        hostName = "Bea",
        phase = RoomPhase.InRoom,
        relayed = false,
        quality = LinkQuality.Excellent,
        rttMs = 14.0,
        codec = "AV1",
        codecDetail = "AV1 por hardware, baja latencia",
        safetyNumber = "12345 67890 13579 24680",
        members = listOf(
            Member(guest = 0, user = 2, name = "Bea"),
            Member(guest = 1, user = 3, name = "Carla"),
            Member(guest = 2, user = 1, name = "Ana", you = true),
        ),
        chat = listOf(
            ChatMessage(guest = 0, name = "Bea", text = "¿Probamos el nivel 3?"),
            ChatMessage(guest = 1, name = "Carla", text = "Dale, yo voy con el mando 2"),
            ChatMessage(guest = 2, name = "Ana", text = "Listo, entro", own = true),
        ),
        unreadChat = true,
        micMuted = false,
        micAvailable = true,
        stats = RoomStats(
            rttMs = 14.0,
            lossPercent = 0.1,
            relayed = false,
            fps = 60.0,
            kbps = 21_400.0,
            targetKbps = 25_000.0,
            latencyMs = 31.0,
            latencyP95Ms = 44.0,
        ),
        permissions = InputPermissions(keyboardMouse = false, gamepadSlot = 1),
        noticeSeq = 1,
    )
    val actions = RoomActions({}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {})
}
