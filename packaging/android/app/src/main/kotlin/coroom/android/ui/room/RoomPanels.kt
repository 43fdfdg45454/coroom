package coroom.android.ui.room

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.VerticalDivider
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import coroom.android.R
import coroom.android.bridge.ChatMessage
import coroom.android.bridge.Member
import coroom.android.platform.NetworkKind
import coroom.android.ui.components.Avatar
import coroom.android.ui.components.CoroomIcon
import coroom.android.ui.components.PillTextField
import coroom.android.ui.components.SeatOccupant
import coroom.android.ui.components.SeatRow
import coroom.android.ui.components.StatusPill
import coroom.android.ui.theme.CoroomText
import coroom.android.ui.theme.CoroomTheme
import coroom.android.ui.theme.Tone
import java.util.Locale

private val Spanish: Locale = Locale.forLanguageTag("es")

/** Números con coma decimal («21,4 Mbit/s»). */
fun formatNumber(value: Double, decimals: Int): String = String.format(Spanish, "%.${decimals}f", value)

/**
 * Quién está en la sala, como butacas: el anfitrión primero y los invitados en su orden. La
 * propia lleva la lámpara de la ruta de este teléfono (verde directa, ámbar por relay o
 * reconectando); de los demás no se sabe su enlace, así que van sin lámpara.
 */
@Composable
fun roomSeats(state: RoomUiState): List<SeatOccupant> {
    val extra = CoroomTheme.colors
    val ownLamp = when {
        state.reconnecting != null -> extra.cautionDot
        state.shownRelayed == true -> extra.cautionDot
        state.shownRelayed == false -> extra.successDot
        else -> null
    }
    val host = stringResource(R.string.seat_host)
    val you = stringResource(R.string.seat_you)
    return state.members.sortedBy { it.guest }.map { member ->
        SeatOccupant(
            name = member.name,
            seed = member.user,
            user = member.user,
            avatar = member.avatar,
            lamp = if (member.you) ownLamp else null,
            role = when {
                member.you -> you
                member.guest == 0 -> host
                else -> null
            },
        )
    }
}

/** Panel «Conexión y calidad». `onShareLog`: «Compartir registro», para mandar lo que pasó si algo va mal. */
@Composable
fun ConnectionPanel(state: RoomUiState, onClose: () -> Unit, onShareLog: () -> Unit) {
    val colors = MaterialTheme.colorScheme
    val stats = state.stats
    val none = stringResource(R.string.no_value)
    fun ms(value: Double?) = value?.let { formatNumber(it, if (it < 10) 1 else 0) }
    SidePanel(
        title = stringResource(R.string.connection_title),
        width = 360.dp,
        closeDescription = stringResource(R.string.action_close),
        onClose = onClose,
    ) {
        Column(modifier = Modifier.fillMaxSize(), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            // Las tres cifras del enlace, en un solo bloque partido en columnas.
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(IntrinsicSize.Min)
                    .clip(MaterialTheme.shapes.medium)
                    .background(colors.surfaceContainerHigh),
            ) {
                Metric(
                    stringResource(R.string.connection_rtt),
                    ms(state.shownRttMs)?.let { stringResource(R.string.unit_ms, it) } ?: none,
                    Modifier.weight(1f),
                )
                VerticalDivider(modifier = Modifier.fillMaxHeight().padding(vertical = 10.dp), color = colors.outlineVariant)
                Metric(
                    stringResource(R.string.connection_e2e),
                    ms(stats?.latencyMs)?.let { stringResource(R.string.unit_ms, it) } ?: none,
                    Modifier.weight(1f),
                )
                VerticalDivider(modifier = Modifier.fillMaxHeight().padding(vertical = 10.dp), color = colors.outlineVariant)
                Metric(
                    stringResource(R.string.connection_loss),
                    stats?.lossPercent?.let { stringResource(R.string.unit_percent, formatNumber(it, 1)) } ?: none,
                    Modifier.weight(1f),
                )
            }
            Column(
                modifier = Modifier.weight(1f).verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                if (state.members.isNotEmpty()) {
                    PanelSection(stringResource(R.string.room_seats))
                    SeatRow(
                        occupants = roomSeats(state),
                        modifier = Modifier.padding(top = 4.dp, bottom = 2.dp),
                        lampRing = colors.surfaceContainer,
                    )
                }
                PanelSection(stringResource(R.string.connection_video))
                StatRow(stringResource(R.string.connection_fps), stats?.fps?.let { formatNumber(it, 0) } ?: none)
                StatRow(
                    stringResource(R.string.connection_bitrate),
                    stats?.kbps?.let { stringResource(R.string.unit_mbits, formatNumber(it / 1000.0, 1)) } ?: none,
                )
                stats?.targetKbps?.let {
                    StatRow(stringResource(R.string.connection_target), stringResource(R.string.unit_mbits, formatNumber(it / 1000.0, 1)))
                }
                StatRow(
                    stringResource(R.string.connection_decoder),
                    state.codecDetail?.takeIf { it.isNotBlank() } ?: state.codec ?: none,
                )
                val p50 = ms(stats?.latencyMs)
                val p95 = ms(stats?.latencyP95Ms)
                if (p50 != null && p95 != null) {
                    StatRow(stringResource(R.string.connection_latency_pair), stringResource(R.string.unit_ms_pair, p50, p95))
                }
                PanelSection(stringResource(R.string.connection_link))
                RouteRow(state.shownRelayed)
                StatRow(stringResource(R.string.connection_network), networkText(state.network))
                PanelSection(stringResource(R.string.connection_input))
                StatRow(stringResource(R.string.connection_permissions), permissionsText(state.permissions))
                state.safetyNumber?.let {
                    PanelSection(stringResource(R.string.connection_security))
                    Text(stringResource(R.string.connection_safety_number), style = CoroomText.supporting, color = colors.onSurfaceVariant)
                    Text(it, style = CoroomText.mono, color = colors.onSurface)
                }
            }
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    text = stringResource(R.string.connection_footer),
                    style = CoroomText.label,
                    color = colors.onSurfaceVariant,
                    modifier = Modifier.weight(1f),
                )
                TextButton(onClick = onShareLog) { Text(stringResource(R.string.settings_log)) }
            }
        }
    }
}

@Composable
private fun networkText(kind: NetworkKind): String = when (kind) {
    is NetworkKind.Wifi -> kind.bandGhz?.let { stringResource(R.string.network_wifi_band, it) } ?: stringResource(R.string.network_wifi)
    NetworkKind.Cellular -> stringResource(R.string.network_cellular)
    NetworkKind.Ethernet -> stringResource(R.string.network_ethernet)
    NetworkKind.Vpn -> stringResource(R.string.network_vpn)
    NetworkKind.None -> stringResource(R.string.network_none)
    NetworkKind.Unknown -> stringResource(R.string.no_value)
}

@Composable
private fun permissionsText(permissions: InputPermissions?): String = when {
    permissions == null -> stringResource(R.string.no_value)
    permissions.gamepadSlot != null && permissions.keyboardMouse ->
        stringResource(R.string.permissions_all_long, permissions.gamepadSlot)
    permissions.gamepadSlot != null -> stringResource(R.string.permissions_pad_long, permissions.gamepadSlot)
    permissions.keyboardMouse -> stringResource(R.string.permissions_keyboard_long)
    else -> stringResource(R.string.permissions_watch_long)
}

/** Una cifra destacada del enlace, con su nombre encima. */
@Composable
private fun Metric(label: String, value: String, modifier: Modifier = Modifier) {
    val colors = MaterialTheme.colorScheme
    Column(modifier = modifier.padding(horizontal = 12.dp, vertical = 10.dp)) {
        Text(label, style = CoroomText.label, color = colors.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
        Text(value, style = CoroomText.metric, color = colors.onSurface, maxLines = 1)
    }
}

@Composable
private fun PanelSection(title: String) {
    Text(
        text = title,
        style = CoroomText.section,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(top = 10.dp).semantics { heading() },
    )
}

/** Una fila de la tabla: el nombre a la izquierda y la cifra a la derecha, con las cifras alineadas. */
@Composable
private fun StatRow(label: String, value: String) {
    Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Text(label, style = CoroomText.supporting, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.weight(1f))
        Text(value, style = CoroomText.figure, color = MaterialTheme.colorScheme.onSurface, textAlign = TextAlign.End)
    }
}

/** La ruta del enlace, con su lámpara: verde si es directa, ámbar si va por el relay. */
@Composable
private fun RouteRow(relayed: Boolean?) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(
            stringResource(R.string.connection_route),
            style = CoroomText.supporting,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.weight(1f),
        )
        StatusPill(
            text = stringResource(
                when (relayed) {
                    true -> R.string.route_relay
                    false -> R.string.route_direct
                    null -> R.string.route_unknown
                },
            ),
            tone = when (relayed) {
                true -> Tone.Warning
                false -> Tone.Success
                null -> Tone.Neutral
            },
        )
    }
}

/** Panel «Chat» de la sala. */
@Composable
fun ChatPanel(state: RoomUiState, onClose: () -> Unit, onSend: (String) -> Unit) {
    val colors = MaterialTheme.colorScheme
    var draft by rememberSaveable { mutableStateOf("") }
    val listState = rememberLazyListState()
    LaunchedEffect(state.chat.size) {
        if (state.chat.isNotEmpty()) listState.animateScrollToItem(0)
    }
    val send = {
        if (draft.isNotBlank()) {
            onSend(draft)
            draft = ""
        }
    }
    Box(modifier = Modifier.imePadding()) {
        SidePanel(
            title = stringResource(R.string.chat_title),
            width = 340.dp,
            closeDescription = stringResource(R.string.chat_close),
            onClose = onClose,
        ) {
            Column(modifier = Modifier.fillMaxSize(), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                if (state.chat.isEmpty()) {
                    Box(modifier = Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
                        Text(
                            text = stringResource(R.string.chat_empty),
                            style = CoroomText.supporting,
                            color = colors.onSurfaceVariant,
                            textAlign = TextAlign.Center,
                        )
                    }
                } else {
                    LazyColumn(
                        state = listState,
                        modifier = Modifier.weight(1f).fillMaxWidth(),
                        reverseLayout = true,
                        verticalArrangement = Arrangement.spacedBy(10.dp, Alignment.Bottom),
                    ) {
                        items(state.chat.asReversed()) { message ->
                            ChatBubble(message, state.members.firstOrNull { it.guest == message.guest })
                        }
                    }
                }
                state.chatError?.let {
                    Text(it, style = CoroomText.label, color = colors.error)
                }
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    PillTextField(
                        value = draft,
                        onValueChange = { draft = it },
                        placeholder = stringResource(R.string.chat_placeholder),
                        modifier = Modifier.weight(1f),
                        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                        keyboardActions = KeyboardActions(onSend = { send() }),
                    )
                    FilledIconButton(onClick = send, enabled = draft.isNotBlank()) {
                        CoroomIcon(
                            R.drawable.ic_send,
                            contentDescription = stringResource(R.string.chat_send),
                            tint = LocalContentColor.current,
                            size = 20.dp,
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun ChatBubble(message: ChatMessage, author: Member?) {
    val colors = MaterialTheme.colorScheme
    if (message.own) {
        Box(modifier = Modifier.fillMaxWidth(), contentAlignment = Alignment.CenterEnd) {
            Text(
                text = message.text,
                style = CoroomText.supporting,
                color = colors.onPrimaryContainer,
                modifier = Modifier
                    .widthIn(max = 240.dp)
                    .clip(OwnBubble)
                    .background(colors.primaryContainer)
                    .padding(horizontal = 12.dp, vertical = 8.dp),
            )
        }
    } else {
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.Top) {
            // El mensaje dice quién lo escribió por su id en la sala; la cuenta y el avatar, los miembros.
            Avatar(
                name = message.name,
                seed = author?.user ?: message.name.hashCode().toLong(),
                size = 28.dp,
                user = author?.user,
                avatar = author?.avatar,
            )
            Column {
                Text(
                    text = if (message.guest == 0) stringResource(R.string.chat_host_name, message.name) else message.name,
                    style = CoroomText.label,
                    color = colors.onSurfaceVariant,
                )
                Text(
                    text = message.text,
                    style = CoroomText.supporting,
                    color = colors.onSurface,
                    modifier = Modifier
                        .padding(top = 2.dp)
                        .clip(OtherBubble)
                        .background(colors.surfaceContainerHighest)
                        .padding(horizontal = 12.dp, vertical = 8.dp),
                )
            }
        }
    }
}

/** El globo de un mensaje propio: la esquina pegada al borde derecho, casi recta. */
private val OwnBubble = RoundedCornerShape(topStart = 14.dp, topEnd = 4.dp, bottomEnd = 14.dp, bottomStart = 14.dp)

/** El globo de un mensaje de otra persona: la esquina junto a su butaca, casi recta. */
private val OtherBubble = RoundedCornerShape(topStart = 4.dp, topEnd = 14.dp, bottomEnd = 14.dp, bottomStart = 14.dp)
