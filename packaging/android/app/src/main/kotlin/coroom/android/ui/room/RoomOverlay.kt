package coroom.android.ui.room

import androidx.annotation.DrawableRes
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.LocalIndication
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.indication
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.displayCutout
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonColors
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.LocalContentColor
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
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.coerceAtLeast
import androidx.compose.ui.unit.dp
import coroom.android.R
import coroom.android.bridge.ChatMessage
import coroom.android.bridge.Member
import coroom.android.bridge.RoomStats
import coroom.android.bridge.TouchMode
import coroom.android.ui.components.CoroomIcon
import coroom.android.ui.components.Lamp
import coroom.android.ui.components.PrimaryButton
import coroom.android.ui.components.SeatRow
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
    /** Apagar o encender el vídeo. */
    val onToggleVideo: () -> Unit = {},
)

/**
 * Lo que va encima del vídeo con la sala abierta: la píldora de conexión siempre visible, la barra
 * lateral, el modo táctil, los avisos y los paneles.
 *
 * La barra lateral y los chips se esconden solos a los [OVERLAY_HIDE_MS] sin tocarlos (con un
 * panel, el diálogo de salir, el teclado o una reconexión no se esconden): queda solo la píldora
 * reducida a la lámpara de la calidad y la latencia, que siguen siempre a la vista, y tocarla
 * vuelve a enseñarlo todo. Los toques en el vídeo siguen yendo al juego.
 */
@Composable
fun RoomOverlay(state: RoomUiState, actions: RoomActions) {
    val panelOpen = state.panel != Panel.None
    val overlayVisible = state.overlayVisible
    Box(modifier = Modifier.fillMaxSize()) {
        // Con el vídeo apagado, su vista tapa la imagen (y los toques no llegan al juego).
        AnimatedVisibility(visible = state.videoOff, enter = fadeIn(), exit = fadeOut()) {
            VideoOffView(state, onTurnOn = actions.onToggleVideo)
        }
        // Con un panel abierto el vídeo se oscurece y tocarlo cierra el panel (no llega al juego).
        AnimatedVisibility(visible = panelOpen, enter = fadeIn(), exit = fadeOut()) {
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .background(CoroomTheme.colors.scrim)
                    .clickable(
                        interactionSource = remember { MutableInteractionSource() },
                        indication = null,
                        onClick = actions.onClosePanel,
                    ),
            )
        }

        Box(modifier = Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.displayCutout)) {
            // La píldora ocupa su zona de toque de 48 dp: con 6 de margen, se ve a 12 del borde.
            Row(
                modifier = Modifier.align(Alignment.TopStart).padding(start = 14.dp, top = 6.dp),
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
                visible = overlayVisible && !panelOpen && !state.videoOff,
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

/** La lámpara de la calidad: verde si va bien, ámbar si va justa o reconecta, rojo si va mal. */
@Composable
private fun qualityColor(state: RoomUiState): Color = when {
    state.reconnecting != null -> CoroomTheme.colors.cautionDot
    else -> when (state.quality) {
        LinkQuality.Excellent, LinkQuality.Good -> CoroomTheme.colors.successDot
        LinkQuality.Acceptable -> CoroomTheme.colors.cautionDot
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
 * «14 ms · directa · AV1» con la lámpara de la calidad: la latencia siempre a la vista; abre el
 * panel de conexión. Con la barra escondida (`expanded` en falso) se queda en la lámpara y
 * «14 ms» (y el aviso de chat sin leer), y tocarla enseña la barra. La píldora se ve baja, pero
 * su zona de toque mide 48 dp.
 */
@Composable
fun ConnectionPill(state: RoomUiState, onClick: () -> Unit, modifier: Modifier = Modifier, expanded: Boolean = true) {
    val colors = MaterialTheme.colorScheme
    val rtt = state.shownRttMs?.let { stringResource(R.string.unit_ms, formatNumber(it, 0)) } ?: stringResource(R.string.no_value)
    val route = routeText(state.shownRelayed)
    val media = if (state.videoOff) stringResource(R.string.room_voice_only) else state.codec?.takeIf { it.isNotBlank() }
    val detail = listOfNotNull(route, media).joinToString(" · ")
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
    val interaction = remember { MutableInteractionSource() }
    Box(
        modifier = modifier
            .defaultMinSize(minHeight = TOUCH_TARGET)
            .clickable(interactionSource = interaction, indication = null, role = Role.Button, onClick = onClick)
            .semantics(mergeDescendants = true) { contentDescription = description },
        contentAlignment = Alignment.Center,
    ) {
        Row(
            modifier = Modifier
                .heightIn(min = PILL_HEIGHT)
                .clip(CircleShape)
                .background(CoroomTheme.colors.overlay)
                // La respuesta al toque se dibuja en la píldora, no en toda la zona de toque.
                .indication(interaction, LocalIndication.current)
                // El mismo margen a los dos lados: el contenido, centrado en la píldora.
                .padding(horizontal = 12.dp, vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Lamp(qualityColor(state))
            Text(
                text = if (state.reconnecting != null) stringResource(R.string.quality_reconnecting) else rtt,
                style = CoroomText.pill.copy(fontWeight = FontWeight.SemiBold),
                color = colors.onSurface,
            )
            AnimatedVisibility(visible = expanded) {
                Text(text = detail, style = CoroomText.pill, color = colors.onSurfaceVariant)
            }
            // Con la barra escondida no se ve el aviso del chat de la barra lateral: va aquí.
            AnimatedVisibility(visible = !expanded && state.unreadChat, enter = fadeIn(), exit = fadeOut()) {
                Lamp(colors.error)
            }
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

/** Un chip informativo sobre el vídeo (no se pulsa): icono y texto en una píldora. */
@Composable
private fun OverlayChip(@DrawableRes icon: Int, text: String, modifier: Modifier = Modifier) {
    Row(
        modifier = modifier
            .heightIn(min = PILL_HEIGHT)
            .clip(CircleShape)
            .background(CoroomTheme.colors.overlay)
            .padding(horizontal = 12.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        CoroomIcon(icon, contentDescription = null, size = 18.dp)
        Text(text = text, style = CoroomText.pill, color = MaterialTheme.colorScheme.onSurface)
    }
}

/** La barra vertical de la derecha: micrófono, vídeo, teclado, modo táctil, chat, conexión y salir. */
@Composable
private fun RoomRail(state: RoomUiState, actions: RoomActions, modifier: Modifier = Modifier) {
    val colors = MaterialTheme.colorScheme
    BoxWithConstraints(
        modifier = modifier
            .fillMaxHeight()
            .width(RAIL_WIDTH)
            .clip(MaterialTheme.shapes.extraLarge)
            .background(CoroomTheme.colors.overlayStrong),
    ) {
        // Los botones repartidos a lo alto; en un teléfono bajo, en horizontal, no caben con su
        // tamaño de toque y la barra se desliza en lugar de encogerlos.
        val gap = ((maxHeight - RAIL_PADDING * 2 - TOUCH_TARGET * RAIL_BUTTONS) / (RAIL_BUTTONS - 1)).coerceAtLeast(0.dp)
        Column(
            modifier = Modifier
                .fillMaxHeight()
                .verticalScroll(rememberScrollState())
                .padding(vertical = RAIL_PADDING),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(gap),
        ) {
            RailButton(
                icon = if (state.micMuted) R.drawable.ic_mic_off else R.drawable.ic_mic,
                description = stringResource(if (state.micMuted) R.string.rail_mic_unmute else R.string.rail_mic_mute),
                active = !state.micMuted,
                onClick = actions.onToggleMic,
            )
            RailButton(
                icon = if (state.videoOff) R.drawable.ic_monitor_off else R.drawable.ic_monitor,
                description = stringResource(if (state.videoOff) R.string.rail_video_on else R.string.rail_video_off),
                active = state.videoOff,
                onClick = actions.onToggleVideo,
            )
            // El teclado y el modo táctil, en pausa con el vídeo apagado.
            val paused = stringResource(R.string.rail_paused_without_video)
            RailButton(
                icon = R.drawable.ic_keyboard,
                description = if (state.videoOff) paused else stringResource(if (state.keyboardVisible) R.string.rail_keyboard_hide else R.string.rail_keyboard_show),
                active = state.keyboardVisible,
                onClick = actions.onToggleKeyboard,
                enabled = !state.videoOff,
            )
            RailButton(
                icon = if (state.touchMode == TouchMode.Trackpad) R.drawable.ic_trackpad else R.drawable.ic_pointer,
                description = if (state.videoOff) paused else stringResource(R.string.rail_touch_mode),
                onClick = actions.onCycleTouchMode,
                enabled = !state.videoOff,
            )
            Box {
                RailButton(
                    icon = R.drawable.ic_message_square,
                    description = stringResource(if (state.unreadChat) R.string.rail_chat_unread else R.string.rail_chat),
                    onClick = { actions.onTogglePanel(Panel.Chat) },
                )
                if (state.unreadChat) {
                    Lamp(
                        color = colors.error,
                        modifier = Modifier.align(Alignment.TopEnd).padding(top = 8.dp, end = 8.dp),
                        size = 10.dp,
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
                colors = IconButtonDefaults.filledIconButtonColors(
                    containerColor = colors.errorContainer,
                    contentColor = colors.onErrorContainer,
                ),
            )
        }
    }
}

/**
 * Un botón de la barra lateral: el botón de icono de Material, con su zona de toque de 48 dp.
 * Encendido (`active`) va relleno con el contenedor del primario; apagado, solo el icono.
 */
@Composable
private fun RailButton(
    @DrawableRes icon: Int,
    description: String,
    onClick: () -> Unit,
    active: Boolean = false,
    enabled: Boolean = true,
    colors: IconButtonColors? = null,
) {
    val scheme = MaterialTheme.colorScheme
    val buttonColors = colors ?: if (active) {
        IconButtonDefaults.filledIconButtonColors(
            containerColor = scheme.primaryContainer,
            contentColor = scheme.onPrimaryContainer,
        )
    } else {
        IconButtonDefaults.iconButtonColors(contentColor = scheme.onSurface)
    }
    IconButton(onClick = onClick, enabled = enabled, colors = buttonColors) {
        CoroomIcon(icon, contentDescription = description, tint = LocalContentColor.current)
    }
}

/** Botones de la barra lateral: micrófono, vídeo, teclado, modo táctil, chat, conexión y salir. */
private const val RAIL_BUTTONS = 7

/** La zona de toque mínima de Material: la de cada botón de la barra y la de la píldora. */
private val TOUCH_TARGET = 48.dp

/** El alto con que se ven las píldoras sobre el vídeo. */
private val PILL_HEIGHT = 36.dp

private val RAIL_WIDTH = 64.dp

/** Margen de arriba y abajo de la barra lateral. */
private val RAIL_PADDING = 8.dp

/**
 * Lo que se ve en lugar del vídeo cuando el invitado lo apagó: qué sigue y qué no, quién está en
 * la sala (sus butacas) y el botón para encenderlo. Tapa la imagen y se queda con los toques (no
 * llegan al juego).
 */
@Composable
private fun VideoOffView(state: RoomUiState, onTurnOn: () -> Unit) {
    val colors = MaterialTheme.colorScheme
    val backdrop = CoroomTheme.colors.videoBackdrop
    val hostName = state.hostName
    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(backdrop)
            .pointerInput(Unit) { detectTapGestures { } }
            .semantics { liveRegion = LiveRegionMode.Polite },
        contentAlignment = Alignment.Center,
    ) {
        Column(
            modifier = Modifier
                .widthIn(max = 440.dp)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 24.dp, vertical = 16.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (state.members.isNotEmpty()) {
                SeatRow(occupants = roomSeats(state), modifier = Modifier.padding(bottom = 8.dp), lampRing = backdrop)
            } else {
                Box(
                    modifier = Modifier.size(56.dp).background(colors.surfaceContainerHigh, MaterialTheme.shapes.large),
                    contentAlignment = Alignment.Center,
                ) {
                    CoroomIcon(R.drawable.ic_monitor_off, contentDescription = null, tint = colors.onSurfaceVariant, size = 28.dp)
                }
            }
            Text(
                text = stringResource(R.string.room_video_off_title),
                style = CoroomText.cardTitle,
                color = colors.onSurface,
            )
            Text(
                text = hostName?.let { stringResource(R.string.room_video_off_body, it) }
                    ?: stringResource(R.string.room_video_off_body_no_name),
                style = CoroomText.supporting,
                color = colors.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
            PrimaryButton(
                text = stringResource(R.string.room_video_on),
                onClick = onTurnOn,
                modifier = Modifier.padding(top = 8.dp),
                icon = R.drawable.ic_monitor,
            )
        }
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
                .clip(MaterialTheme.shapes.large)
                .background(CoroomTheme.colors.overlay)
                .padding(horizontal = 16.dp, vertical = 8.dp)
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

/** El panel lateral de la sala: una superficie de contenedor con las esquinas del lado del vídeo redondeadas. */
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
            .clip(RoundedCornerShape(topStart = 28.dp, bottomStart = 28.dp))
            .background(colors.surfaceContainer)
            // Los toques dentro del panel no lo atraviesan hasta el fondo (que lo cerraría).
            .pointerInput(Unit) { detectTapGestures { } }
            .windowInsetsPadding(WindowInsets.displayCutout)
            .padding(start = 20.dp, end = 12.dp, top = 8.dp, bottom = 12.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                text = title,
                style = CoroomText.cardTitle,
                color = colors.onSurface,
                modifier = Modifier.weight(1f),
            )
            IconButton(onClick = onClose) {
                CoroomIcon(R.drawable.ic_x, contentDescription = closeDescription)
            }
        }
        Box(modifier = Modifier.weight(1f).padding(end = 8.dp), content = content)
    }
}

private const val NOTICE_MS = 4_000L

@Preview(name = "Sala", widthDp = 844, heightDp = 390)
@Composable
private fun RoomOverlayPreview() {
    CoroomTheme(darkTheme = true, dynamicColor = false) {
        Box(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.tertiaryContainer)) {
            RoomOverlay(PreviewRoom.state, PreviewRoom.actions)
        }
    }
}

@Preview(name = "Sala: barra escondida", widthDp = 844, heightDp = 390)
@Composable
private fun RoomOverlayHiddenPreview() {
    CoroomTheme(darkTheme = true, dynamicColor = false) {
        Box(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.tertiaryContainer)) {
            RoomOverlay(PreviewRoom.state.copy(overlayShown = false), PreviewRoom.actions)
        }
    }
}

@Preview(name = "Sala: vídeo apagado", widthDp = 844, heightDp = 390)
@Composable
private fun RoomVideoOffPreview() {
    CoroomTheme(darkTheme = true, dynamicColor = false) {
        Box(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.tertiaryContainer)) {
            RoomOverlay(PreviewRoom.state.copy(videoOff = true), PreviewRoom.actions)
        }
    }
}

@Preview(name = "Sala: conexión y calidad", widthDp = 844, heightDp = 390)
@Composable
private fun RoomConnectionPreview() {
    CoroomTheme(darkTheme = true, dynamicColor = false) {
        Box(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.tertiaryContainer)) {
            RoomOverlay(PreviewRoom.state.copy(panel = Panel.Connection), PreviewRoom.actions)
        }
    }
}

@Preview(name = "Sala: chat", widthDp = 844, heightDp = 390)
@Composable
private fun RoomChatPreview() {
    CoroomTheme(darkTheme = true, dynamicColor = false) {
        Box(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.tertiaryContainer)) {
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
