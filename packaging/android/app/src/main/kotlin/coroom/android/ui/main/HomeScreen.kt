package coroom.android.ui.main

import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import coroom.android.R
import coroom.android.bridge.Account
import coroom.android.bridge.Friend
import coroom.android.bridge.FriendActivity
import coroom.android.bridge.Invitation
import coroom.android.bridge.OwnRoom
import coroom.android.bridge.RoomCode
import coroom.android.platform.Gamepad
import coroom.android.ui.components.Avatar
import coroom.android.ui.components.CoroomIcon
import coroom.android.ui.components.LabeledField
import coroom.android.ui.components.Lamp
import coroom.android.ui.components.ListCard
import coroom.android.ui.components.Notice
import coroom.android.ui.components.PersonRow
import coroom.android.ui.components.PrimaryButton
import coroom.android.ui.components.RoomCodeGrouping
import coroom.android.ui.components.RowDivider
import coroom.android.ui.components.SeatOccupant
import coroom.android.ui.components.SeatRow
import coroom.android.ui.components.SectionHeader
import coroom.android.ui.components.SignageCode
import coroom.android.ui.components.Ticket
import coroom.android.ui.components.TonalButton
import coroom.android.ui.components.TonalCard
import coroom.android.ui.components.relativeTime
import coroom.android.ui.components.serverDotColor
import coroom.android.ui.preview.FakeData
import coroom.android.ui.theme.CoroomText
import coroom.android.ui.theme.CoroomTheme

/**
 * Pestaña Inicio: unirse con un código, las salas propias y las invitaciones como entradas (con su
 * fila de butacas), y los amigos conectados. En ancho compacto va todo en una columna; desde [TWO_PANE_MIN_WIDTH], las
 * salas a la izquierda (3/5) y los amigos a la derecha (2/5).
 */
@Composable
fun HomeScreen(
    state: MainUiState,
    contentPadding: PaddingValues,
    onJoin: (code: String, hostName: String?) -> Unit,
    onAskToJoin: (Friend) -> Unit,
    onOpenAccount: () -> Unit,
    onOpenFriends: () -> Unit,
    /** Los botones de los avisos de la versión nueva y de versiones distintas. */
    updates: UpdateActions = UpdateActions.None,
) {
    val account = state.account ?: return
    var code by rememberSaveable { mutableStateOf("") }

    BoxWithConstraints(modifier = Modifier.fillMaxSize()) {
        val twoPane = maxWidth >= TWO_PANE_MIN_WIDTH
        Column(
            modifier = Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(contentPadding)
                .padding(start = 16.dp, end = 16.dp, bottom = 24.dp),
        ) {
            HomeTopBar(account, onOpenAccount)
            Greeting(account, state.serverOnline)
            Spacer(Modifier.height(24.dp))
            if (twoPane) {
                Row(horizontalArrangement = Arrangement.spacedBy(24.dp)) {
                    HomeRooms(
                        state = state,
                        account = account,
                        code = code,
                        onCode = { code = RoomCode.clean(it) },
                        onJoin = onJoin,
                        updates = updates,
                        modifier = Modifier.weight(3f),
                    )
                    HomeFriends(state, onAskToJoin, onOpenFriends, modifier = Modifier.weight(2f))
                }
            } else {
                HomeRooms(
                    state = state,
                    account = account,
                    code = code,
                    onCode = { code = RoomCode.clean(it) },
                    onJoin = onJoin,
                    updates = updates,
                    modifier = Modifier.fillMaxWidth(),
                )
                Spacer(Modifier.height(28.dp))
                HomeFriends(state, onAskToJoin, onOpenFriends, modifier = Modifier.fillMaxWidth())
            }
        }
    }
}

/** La marca y, a la derecha, la butaca de la cuenta (abre Ajustes). */
@Composable
private fun HomeTopBar(account: Account, onOpenAccount: () -> Unit) {
    val colors = MaterialTheme.colorScheme
    Row(
        modifier = Modifier.fillMaxWidth().padding(top = 12.dp, bottom = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        CoroomIcon(R.drawable.ic_radio, contentDescription = null, tint = colors.primary, size = 26.dp)
        Text(
            text = stringResource(R.string.app_name),
            style = CoroomText.wordmark,
            color = colors.onSurface,
            modifier = Modifier.weight(1f),
        )
        val accountLabel = stringResource(R.string.home_account_button, account.shownName)
        Box(
            modifier = Modifier
                .size(48.dp)
                .clip(MaterialTheme.shapes.medium)
                .clickable(role = Role.Button, onClick = onOpenAccount)
                .semantics { contentDescription = accountLabel },
            contentAlignment = Alignment.Center,
        ) {
            Avatar(name = account.shownName, seed = account.userId, user = account.userId, avatar = account.avatar)
        }
    }
}

/** El saludo y, debajo, la lámpara del servidor con su estado dicho en palabras. */
@Composable
private fun Greeting(account: Account, online: Boolean?) {
    val colors = MaterialTheme.colorScheme
    Column(modifier = Modifier.padding(horizontal = 4.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(
            text = stringResource(R.string.home_greeting, account.shownName),
            style = CoroomText.greeting,
            color = colors.onSurface,
        )
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Lamp(serverDotColor(online))
            Text(
                text = stringResource(
                    when (online) {
                        true -> R.string.home_server_online
                        false -> R.string.home_server_offline
                        null -> R.string.home_server_connecting
                    },
                    account.serverHost,
                ),
                style = CoroomText.supporting,
                color = colors.onSurfaceVariant,
            )
        }
    }
}

/** Lo que tiene que ver con entrar en una sala: los avisos, el código, las salas propias y las invitaciones. */
@Composable
private fun HomeRooms(
    state: MainUiState,
    account: Account,
    code: String,
    onCode: (String) -> Unit,
    onJoin: (code: String, hostName: String?) -> Unit,
    updates: UpdateActions,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.animateContentSize(), verticalArrangement = Arrangement.spacedBy(16.dp)) {
        // Versiones distintas con el servidor; si la app es la vieja, su aviso ya lleva la
        // acción de la versión nueva (un fallo sí se explica aparte).
        val mismatch = state.protocolMismatch
        mismatch?.let { ProtocolBanner(it.host, it.gap, state.update, updates) }
        if (mismatch?.gap?.clientOlder != true || state.update?.stage == "failed") {
            UpdateBanner(state.update, updates)
        }
        JoinCard(
            code = code,
            onCode = onCode,
            onJoin = { onJoin(code, null) },
        )
        if (state.ownRooms.isNotEmpty()) OwnRooms(state.ownRooms, account, onJoin)
        Invitations(state.invitations, state.friends.list?.friends.orEmpty(), state.nowMs, onJoin)
    }
}

/** «Unirme a una sala»: el código en señalética, en un campo de texto normal, y «Entrar». */
@Composable
private fun JoinCard(code: String, onCode: (String) -> Unit, onJoin: () -> Unit) {
    val colors = MaterialTheme.colorScheme
    val valid = RoomCode.isValid(code)
    TonalCard(contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 18.dp, bottom = 16.dp)) {
        Text(stringResource(R.string.home_join_title), style = CoroomText.cardTitle, color = colors.onSurface)
        Spacer(Modifier.height(12.dp))
        // El botón toma el alto del campo (sin el hueco que la etiqueta flotante deja arriba).
        Row(modifier = Modifier.height(IntrinsicSize.Min), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            LabeledField(
                label = stringResource(R.string.home_join_code),
                value = code,
                onValueChange = onCode,
                textStyle = CoroomText.roomCode,
                placeholder = stringResource(R.string.home_join_placeholder),
                modifier = Modifier.weight(1f),
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.Characters,
                    keyboardType = KeyboardType.Ascii,
                    autoCorrectEnabled = false,
                    imeAction = ImeAction.Go,
                ),
                keyboardActions = KeyboardActions(onGo = { if (valid) onJoin() }),
                visualTransformation = RoomCodeGrouping,
            )
            PrimaryButton(
                text = stringResource(R.string.action_join),
                onClick = onJoin,
                enabled = valid,
                icon = R.drawable.ic_log_in,
                modifier = Modifier.fillMaxHeight().padding(top = FIELD_LABEL_INSET),
            )
        }
        Spacer(Modifier.height(8.dp))
        Text(stringResource(R.string.home_join_hint), style = CoroomText.supporting, color = colors.onSurfaceVariant)
    }
}

/**
 * Las salas abiertas con la propia cuenta en otro dispositivo: una entrada cada una, con la butaca
 * propia (la del anfitrión) y la que se ocuparía desde aquí. Se entra con un toque.
 */
@Composable
private fun OwnRooms(rooms: List<OwnRoom>, account: Account, onJoin: (code: String, hostName: String?) -> Unit) {
    val colors = MaterialTheme.colorScheme
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        rooms.forEach { room ->
            Ticket(
                stubLabel = stringResource(R.string.action_join),
                onStub = { onJoin(room.code, account.shownName) },
                containerColor = colors.secondaryContainer,
                contentColor = colors.onSecondaryContainer,
            ) {
                TicketBody(
                    host = SeatOccupant(
                        name = account.shownName,
                        seed = account.userId,
                        user = account.userId,
                        avatar = account.avatar,
                    ),
                    title = stringResource(R.string.home_own_room_title, room.device),
                    code = room.code,
                    lampRing = colors.secondaryContainer,
                    modifier = Modifier.weight(1f),
                ) {
                    // La sala está abierta: se está compartiendo desde el otro dispositivo.
                    Lamp(CoroomTheme.colors.live)
                    Text(
                        text = stringResource(R.string.room_live),
                        style = CoroomText.pill,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}

/** Las invitaciones recibidas, como entradas: la butaca de quien invita, el código y el talón «Entrar». */
@Composable
private fun Invitations(
    invitations: List<Invitation>,
    friends: List<Friend>,
    nowMs: Long,
    onJoin: (code: String, hostName: String?) -> Unit,
) {
    val colors = MaterialTheme.colorScheme
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        SectionHeader(stringResource(R.string.home_invitations), modifier = Modifier.padding(top = 8.dp))
        if (invitations.isEmpty()) {
            Text(
                text = stringResource(R.string.home_invitations_empty),
                style = CoroomText.supporting,
                color = colors.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 4.dp),
            )
        }
        invitations.forEach { invitation ->
            // La invitación no lleva el avatar ni el estado: los del amigo que invita, si está en la lista.
            val friend = friends.firstOrNull { it.id == invitation.fromUser }
            Ticket(
                stubLabel = stringResource(R.string.action_join),
                onStub = { onJoin(invitation.code, invitation.fromName) },
            ) {
                TicketBody(
                    host = SeatOccupant(
                        name = invitation.fromName,
                        seed = invitation.fromUser,
                        user = invitation.fromUser,
                        avatar = friend?.avatar,
                        lamp = if (friend?.online == true) CoroomTheme.colors.successDot else null,
                    ),
                    title = stringResource(R.string.home_invitation_title, invitation.fromName),
                    code = invitation.code,
                    lampRing = colors.primaryContainer,
                    modifier = Modifier.weight(1f),
                ) {
                    Text(
                        text = relativeTime(invitation.receivedAtMs, nowMs),
                        style = CoroomText.label,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}

/**
 * Lo que dice una entrada de sala: arriba, la fila de butacas (la del anfitrión y la libre que
 * espera a quien entra; de los demás integrantes no se sabe hasta estar dentro) con el título al
 * lado; debajo, el código en señalética y, tras él, `detail` (hace cuánto llegó, «En vivo»).
 */
@Composable
private fun TicketBody(
    host: SeatOccupant,
    title: String,
    code: String,
    lampRing: Color,
    modifier: Modifier = Modifier,
    detail: @Composable RowScope.() -> Unit,
) {
    Column(modifier = modifier, verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            SeatRow(
                occupants = listOf(host),
                seatSize = 40.dp,
                minSeats = TICKET_SEATS,
                showLabels = false,
                lampRing = lampRing,
            )
            Text(text = title, style = CoroomText.itemTitle, modifier = Modifier.weight(1f))
        }
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            SignageCode(code, modifier = Modifier.padding(end = 2.dp))
            detail()
        }
    }
}

/** Las butacas de una entrada: la del anfitrión y la que se va a ocupar. */
private const val TICKET_SEATS = 2

/** Los amigos conectados, en una lista agrupada con sus butacas, y el mando conectado al pie. */
@Composable
private fun HomeFriends(
    state: MainUiState,
    onAskToJoin: (Friend) -> Unit,
    onOpenFriends: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = MaterialTheme.colorScheme
    val all = state.friends.list?.friends.orEmpty()
    val online = all.filter { it.online }
        .sortedWith(compareBy<Friend> { it.activityKind != FriendActivity.Hosting }.thenBy { it.shownName.lowercase() })
    Column(modifier = modifier, verticalArrangement = Arrangement.spacedBy(8.dp)) {
        SectionHeader(stringResource(R.string.home_friends_online))
        when {
            state.serverOnline == false -> Notice(
                title = stringResource(R.string.home_offline_title),
                body = stringResource(R.string.home_offline_body),
                icon = R.drawable.ic_wifi_off,
            )
            state.friends.list == null && state.friends.error != null -> Notice(
                title = stringResource(R.string.friends_error_title),
                body = state.friends.error,
                icon = R.drawable.ic_triangle_alert,
            )
            state.friends.list == null -> Text(
                text = stringResource(R.string.friends_loading),
                style = CoroomText.supporting,
                color = colors.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 4.dp),
            )
            all.isEmpty() -> Notice(
                title = stringResource(R.string.friends_empty_title),
                body = stringResource(R.string.home_friends_none_body),
                icon = R.drawable.ic_user_plus,
                actions = {
                    TonalButton(text = stringResource(R.string.friends_add), onClick = onOpenFriends)
                },
            )
            online.isEmpty() -> Text(
                text = stringResource(R.string.home_friends_online_empty),
                style = CoroomText.supporting,
                color = colors.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 4.dp),
            )
            else -> ListCard {
                online.forEachIndexed { index, friend ->
                    if (index > 0) RowDivider()
                    FriendRow(friend, asked = friend.id in state.askedToJoin, onAskToJoin = onAskToJoin)
                }
            }
        }
        state.pads.firstOrNull()?.let { PadHint(it) }
    }
}

/**
 * Fila de amigo: su butaca con la lámpara verde si está conectado y, si tiene una sala abierta, el
 * piloto de «en vivo» delante de su estado y «Pedir entrar».
 */
@Composable
fun FriendRow(
    friend: Friend,
    asked: Boolean,
    onAskToJoin: (Friend) -> Unit,
    trailing: @Composable () -> Unit = {},
) {
    val hosting = friend.activityKind == FriendActivity.Hosting
    PersonRow(
        name = friend.shownName,
        seed = friend.id,
        supporting = friendStatus(friend),
        offline = !friend.online,
        user = friend.id,
        avatar = friend.avatar,
        lamp = if (friend.online) CoroomTheme.colors.successDot else null,
        supportingLamp = if (hosting) CoroomTheme.colors.live else null,
    ) {
        if (hosting) {
            TonalButton(
                text = stringResource(if (asked) R.string.friends_asked else R.string.friends_ask_to_join),
                onClick = { onAskToJoin(friend) },
                enabled = !asked,
            )
        } else {
            trailing()
        }
    }
}

@Composable
fun friendStatus(friend: Friend): String = stringResource(
    when {
        !friend.online -> R.string.friend_offline
        friend.activityKind == FriendActivity.Hosting -> R.string.friend_hosting
        friend.activityKind == FriendActivity.Playing -> R.string.friend_playing
        else -> R.string.friend_online
    },
)

/** El mando conectado al teléfono, dicho en una línea. */
@Composable
private fun PadHint(pad: Gamepad) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(start = 4.dp, end = 4.dp, top = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        CoroomIcon(R.drawable.ic_gamepad_2, contentDescription = null, size = 20.dp)
        Text(
            text = stringResource(
                if (pad.canVibrate) R.string.home_pad_hint else R.string.home_pad_hint_no_rumble,
                pad.name,
            ),
            style = CoroomText.supporting,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** El hueco que la etiqueta flotante de un campo con borde deja encima de su caja. */
private val FIELD_LABEL_INSET = 8.dp

/** El ancho desde el que Inicio pasa a dos columnas. */
private val TWO_PANE_MIN_WIDTH = 720.dp

@Preview(name = "Inicio", widthDp = 390, heightDp = 844)
@Composable
private fun HomePreview() {
    CoroomTheme(dynamicColor = false, darkTheme = false) {
        Box(Modifier.background(MaterialTheme.colorScheme.background)) {
            HomeScreen(FakeData.mainState, PaddingValues(), { _, _ -> }, {}, {}, {})
        }
    }
}

@Preview(name = "Inicio, tema oscuro", widthDp = 390, heightDp = 844)
@Composable
private fun HomeDarkPreview() {
    CoroomTheme(dynamicColor = false, darkTheme = true) {
        Box(Modifier.background(MaterialTheme.colorScheme.background)) {
            HomeScreen(FakeData.mainState, PaddingValues(), { _, _ -> }, {}, {}, {})
        }
    }
}

@Preview(name = "Inicio, ancho expandido", widthDp = 900, heightDp = 600)
@Composable
private fun HomeWidePreview() {
    CoroomTheme(dynamicColor = false) {
        Box(Modifier.background(MaterialTheme.colorScheme.background)) {
            HomeScreen(FakeData.mainState, PaddingValues(), { _, _ -> }, {}, {}, {})
        }
    }
}

@Preview(name = "Inicio sin amigos", widthDp = 390, heightDp = 844)
@Composable
private fun HomeEmptyPreview() {
    CoroomTheme(dynamicColor = false) {
        Box(Modifier.background(MaterialTheme.colorScheme.background)) {
            HomeScreen(FakeData.emptyState, PaddingValues(), { _, _ -> }, {}, {}, {})
        }
    }
}
