package coroom.android.ui.main

import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
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
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import coroom.android.R
import coroom.android.bridge.Account
import coroom.android.bridge.Friend
import coroom.android.bridge.FriendActivity
import coroom.android.bridge.Invitation
import coroom.android.bridge.RoomCode
import coroom.android.platform.Gamepad
import coroom.android.ui.components.Avatar
import coroom.android.ui.components.CoroomIcon
import coroom.android.ui.components.LabeledField
import coroom.android.ui.components.ListCard
import coroom.android.ui.components.NoticeCard
import coroom.android.ui.components.PersonRow
import coroom.android.ui.components.PillButton
import coroom.android.ui.components.PillOutlinedButton
import coroom.android.ui.components.RowDivider
import coroom.android.ui.components.SectionHeader
import coroom.android.ui.components.StatusDot
import coroom.android.ui.components.relativeTime
import coroom.android.ui.components.serverDotColor
import coroom.android.ui.preview.FakeData
import coroom.android.ui.theme.CoroomText
import coroom.android.ui.theme.CoroomTheme

/** Pestaña Inicio (boceto «Inicio»): unirse con un código, invitaciones y amigos conectados. */
@Composable
fun HomeScreen(
    state: MainUiState,
    contentPadding: PaddingValues,
    onJoin: (code: String, hostName: String?) -> Unit,
    onAskToJoin: (Friend) -> Unit,
    onOpenAccount: () -> Unit,
    onOpenFriends: () -> Unit,
) {
    val account = state.account ?: return
    val colors = MaterialTheme.colorScheme
    var code by rememberSaveable { mutableStateOf("") }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(contentPadding),
    ) {
        Row(
            modifier = Modifier.padding(start = 20.dp, end = 16.dp, top = 20.dp, bottom = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            CoroomIcon(R.drawable.ic_radio, contentDescription = null, tint = colors.primary, size = 26.dp)
            Text(
                text = stringResource(R.string.app_name),
                style = CoroomText.screenTitle,
                color = colors.onSurface,
                modifier = Modifier.weight(1f),
            )
            val accountLabel = stringResource(R.string.home_account_button, account.shownName)
            Box(
                modifier = Modifier
                    .size(48.dp)
                    .clip(CircleShape)
                    .clickable(onClick = onOpenAccount)
                    .semantics { contentDescription = accountLabel },
                contentAlignment = Alignment.Center,
            ) {
                Avatar(name = account.shownName, seed = account.userId, user = account.userId, avatar = account.avatar)
            }
        }

        Column(
            modifier = Modifier
                .padding(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 16.dp)
                .animateContentSize(),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Greeting(account, state.serverOnline)
            JoinCard(
                code = code,
                onCode = { code = RoomCode.clean(it) },
                onJoin = { onJoin(code, null) },
            )
            Invitations(state.invitations, state.friends.list?.friends.orEmpty(), state.nowMs, onJoin)
            OnlineFriends(state, onAskToJoin, onOpenFriends)
            state.pads.firstOrNull()?.let { PadHint(it) }
        }
    }
}

@Composable
private fun Greeting(account: Account, online: Boolean?) {
    val colors = MaterialTheme.colorScheme
    Column(modifier = Modifier.padding(start = 4.dp, end = 4.dp, top = 4.dp)) {
        Text(
            text = stringResource(R.string.home_greeting, account.shownName),
            style = CoroomText.greeting,
            color = colors.onSurface,
        )
        Row(
            modifier = Modifier.padding(top = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            StatusDot(serverDotColor(online))
            Text(
                text = stringResource(
                    when (online) {
                        true -> R.string.home_server_online
                        false -> R.string.home_server_offline
                        null -> R.string.home_server_connecting
                    },
                    account.serverHost,
                ),
                style = CoroomText.supporting.copy(fontSize = MaterialTheme.typography.bodyMedium.fontSize),
                color = colors.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun JoinCard(code: String, onCode: (String) -> Unit, onJoin: () -> Unit) {
    val colors = MaterialTheme.colorScheme
    val valid = RoomCode.isValid(code)
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(20.dp))
            .background(colors.surfaceContainer)
            .padding(start = 16.dp, end = 16.dp, top = 18.dp, bottom = 16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            CoroomIcon(R.drawable.ic_log_in, contentDescription = null, tint = colors.primary)
            Text(stringResource(R.string.home_join_title), style = CoroomText.cardTitle, color = colors.onSurface)
        }
        Row(verticalAlignment = Alignment.Bottom, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            LabeledField(
                label = stringResource(R.string.home_join_code),
                value = code,
                onValueChange = onCode,
                textStyle = CoroomText.roomCode.copy(color = colors.onSurface),
                placeholder = stringResource(R.string.home_join_placeholder),
                modifier = Modifier.weight(1f),
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.Characters,
                    keyboardType = KeyboardType.Ascii,
                    autoCorrectEnabled = false,
                    imeAction = ImeAction.Go,
                ),
                keyboardActions = KeyboardActions(onGo = { if (valid) onJoin() }),
            )
            PillButton(
                text = stringResource(R.string.action_join),
                onClick = onJoin,
                enabled = valid,
                height = 56.dp,
            )
        }
    }
}

@Composable
private fun Invitations(
    invitations: List<Invitation>,
    friends: List<Friend>,
    nowMs: Long,
    onJoin: (code: String, hostName: String?) -> Unit,
) {
    val colors = MaterialTheme.colorScheme
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        SectionHeader(stringResource(R.string.home_invitations))
        if (invitations.isEmpty()) {
            Text(
                text = stringResource(R.string.home_invitations_empty),
                style = CoroomText.supporting,
                color = colors.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 4.dp),
            )
        }
        invitations.forEach { invitation ->
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(16.dp))
                    .background(colors.primaryContainer)
                    .padding(start = 14.dp, end = 12.dp, top = 12.dp, bottom = 12.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                // La invitación no lleva el avatar: el del amigo que invita, si está en la lista.
                val avatar = friends.firstOrNull { it.id == invitation.fromUser }?.avatar
                Avatar(name = invitation.fromName, seed = invitation.fromUser, user = invitation.fromUser, avatar = avatar)
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = stringResource(R.string.home_invitation_title, invitation.fromName),
                        style = CoroomText.itemTitle,
                        color = colors.onPrimaryContainer,
                    )
                    Text(
                        text = stringResource(
                            R.string.home_invitation_detail,
                            invitation.code,
                            relativeTime(invitation.receivedAtMs, nowMs),
                        ),
                        style = CoroomText.supporting,
                        color = colors.onPrimaryContainer.copy(alpha = 0.8f),
                    )
                }
                PillButton(
                    text = stringResource(R.string.action_join),
                    onClick = { onJoin(invitation.code, invitation.fromName) },
                    height = 44.dp,
                )
            }
        }
    }
}

@Composable
private fun OnlineFriends(state: MainUiState, onAskToJoin: (Friend) -> Unit, onOpenFriends: () -> Unit) {
    val colors = MaterialTheme.colorScheme
    val all = state.friends.list?.friends.orEmpty()
    val online = all.filter { it.online }
        .sortedWith(compareBy<Friend> { it.activityKind != FriendActivity.Hosting }.thenBy { it.shownName.lowercase() })
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        SectionHeader(stringResource(R.string.home_friends_online))
        when {
            state.serverOnline == false -> NoticeCard(
                title = stringResource(R.string.home_offline_title),
                body = stringResource(R.string.home_offline_body),
                icon = R.drawable.ic_wifi_off,
            )
            state.friends.list == null && state.friends.error != null -> NoticeCard(
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
            all.isEmpty() -> NoticeCard(
                title = stringResource(R.string.friends_empty_title),
                body = stringResource(R.string.home_friends_none_body),
                icon = R.drawable.ic_user_plus,
                action = {
                    PillOutlinedButton(text = stringResource(R.string.friends_add), onClick = onOpenFriends)
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
    }
}

/** Fila de amigo conectado; con «Pedir entrar» si tiene una sala abierta. */
@Composable
fun FriendRow(
    friend: Friend,
    asked: Boolean,
    onAskToJoin: (Friend) -> Unit,
    trailing: @Composable () -> Unit = {},
) {
    PersonRow(
        name = friend.shownName,
        seed = friend.id,
        supporting = friendStatus(friend),
        supportingColor = if (friend.online) CoroomTheme.colors.success else MaterialTheme.colorScheme.onSurfaceVariant,
        offline = !friend.online,
        user = friend.id,
        avatar = friend.avatar,
    ) {
        if (friend.activityKind == FriendActivity.Hosting) {
            PillOutlinedButton(
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

@Composable
private fun PadHint(pad: Gamepad) {
    val colors = MaterialTheme.colorScheme
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(colors.surfaceContainerLow)
            .padding(horizontal = 14.dp, vertical = 10.dp),
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
            color = colors.onSurfaceVariant,
        )
    }
}

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

@Preview(name = "Inicio sin amigos", widthDp = 390, heightDp = 844)
@Composable
private fun HomeEmptyPreview() {
    CoroomTheme(dynamicColor = false) {
        Box(Modifier.background(MaterialTheme.colorScheme.background)) {
            HomeScreen(FakeData.emptyState, PaddingValues(), { _, _ -> }, {}, {}, {})
        }
    }
}
