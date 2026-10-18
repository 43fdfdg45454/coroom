package coroom.android.ui.main

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.background
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
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import coroom.android.R
import coroom.android.bridge.Friend
import coroom.android.bridge.FriendActivity
import coroom.android.bridge.FriendRequest
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
import coroom.android.ui.preview.FakeData
import coroom.android.ui.theme.CoroomText
import coroom.android.ui.theme.CoroomTheme

/** Pestaña Amigos (boceto «Amigos»): solicitudes, conectados, desconectados y «Agregar amigo». */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FriendsScreen(
    state: MainUiState,
    contentPadding: PaddingValues,
    onRefresh: () -> Unit,
    onAskToJoin: (Friend) -> Unit,
    onAccept: (Long) -> Unit,
    onDecline: (Long) -> Unit,
    onRemove: (Long) -> Unit,
    onAdd: (String) -> Unit,
) {
    val colors = MaterialTheme.colorScheme
    var searching by rememberSaveable { mutableStateOf(false) }
    var query by rememberSaveable { mutableStateOf("") }
    var adding by rememberSaveable { mutableStateOf(false) }
    var removing by remember { mutableStateOf<Friend?>(null) }

    val list = state.friends.list
    val matches: (String, String) -> Boolean = { name, user ->
        query.isBlank() || name.contains(query.trim(), ignoreCase = true) || user.contains(query.trim(), ignoreCase = true)
    }
    val friends = list?.friends.orEmpty().filter { matches(it.shownName, it.username) }
    val online = friends.filter { it.online }
        .sortedWith(compareBy<Friend> { it.activityKind != FriendActivity.Hosting }.thenBy { it.shownName.lowercase() })
    val offline = friends.filterNot { it.online }.sortedBy { it.shownName.lowercase() }
    val incoming = list?.incoming.orEmpty()
    val outgoing = list?.outgoing.orEmpty()

    Box(modifier = Modifier.fillMaxSize().padding(contentPadding)) {
        PullToRefreshBox(
            isRefreshing = state.friends.loading && list != null,
            onRefresh = onRefresh,
            modifier = Modifier.fillMaxSize(),
        ) {
            Column(modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                Row(
                    modifier = Modifier.padding(start = 20.dp, end = 16.dp, top = 20.dp, bottom = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        text = stringResource(R.string.friends_title),
                        style = CoroomText.screenTitle,
                        color = colors.onSurface,
                        modifier = Modifier.weight(1f),
                    )
                    IconButton(onClick = {
                        searching = !searching
                        if (!searching) query = ""
                    }) {
                        CoroomIcon(
                            if (searching) R.drawable.ic_x else R.drawable.ic_search,
                            contentDescription = stringResource(if (searching) R.string.friends_search_close else R.string.friends_search),
                        )
                    }
                }

                Column(
                    modifier = Modifier
                        .padding(start = 16.dp, end = 16.dp, top = 4.dp, bottom = 96.dp)
                        .animateContentSize(),
                    verticalArrangement = Arrangement.spacedBy(16.dp),
                ) {
                    AnimatedVisibility(visible = searching) {
                        LabeledField(
                            label = stringResource(R.string.friends_search_label),
                            value = query,
                            onValueChange = { query = it },
                            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                        )
                    }

                    when {
                        list == null && state.friends.error != null -> NoticeCard(
                            title = stringResource(R.string.friends_error_title),
                            body = state.friends.error,
                            icon = R.drawable.ic_triangle_alert,
                            action = { PillOutlinedButton(stringResource(R.string.action_retry), onRefresh) },
                        )
                        list == null -> Text(
                            text = stringResource(R.string.friends_loading),
                            style = CoroomText.supporting,
                            color = colors.onSurfaceVariant,
                            modifier = Modifier.padding(horizontal = 4.dp),
                        )
                        list.friends.isEmpty() && incoming.isEmpty() && outgoing.isEmpty() -> NoticeCard(
                            title = stringResource(R.string.friends_empty_title),
                            body = stringResource(R.string.friends_empty_body),
                            icon = R.drawable.ic_users,
                        )
                        else -> {
                            if (incoming.isNotEmpty()) Requests(incoming, state.busy, onAccept, onDecline)
                            if (online.isNotEmpty()) {
                                FriendGroup(stringResource(R.string.friends_online_count, online.size)) {
                                    online.forEachIndexed { index, friend ->
                                        if (index > 0) RowDivider()
                                        FriendRow(friend, asked = friend.id in state.askedToJoin, onAskToJoin = onAskToJoin) {
                                            FriendMenu(friend) { removing = it }
                                        }
                                    }
                                }
                            }
                            if (offline.isNotEmpty()) {
                                FriendGroup(stringResource(R.string.friends_offline_count, offline.size)) {
                                    offline.forEachIndexed { index, friend ->
                                        if (index > 0) RowDivider()
                                        FriendRow(friend, asked = false, onAskToJoin = onAskToJoin) {
                                            FriendMenu(friend) { removing = it }
                                        }
                                    }
                                }
                            }
                            if (outgoing.isNotEmpty()) Outgoing(outgoing, state.busy, onDecline)
                            if (friends.isEmpty() && query.isNotBlank()) {
                                Text(
                                    text = stringResource(R.string.friends_search_empty, query.trim()),
                                    style = CoroomText.supporting,
                                    color = colors.onSurfaceVariant,
                                    modifier = Modifier.padding(horizontal = 4.dp),
                                )
                            }
                        }
                    }
                }
            }
        }

        ExtendedFloatingActionButton(
            onClick = { adding = true },
            modifier = Modifier.align(Alignment.BottomEnd).padding(16.dp),
            shape = RoundedCornerShape(16.dp),
            containerColor = colors.primaryContainer,
            contentColor = colors.onPrimaryContainer,
            icon = { CoroomIcon(R.drawable.ic_user_plus, contentDescription = null, tint = colors.onPrimaryContainer) },
            text = { Text(stringResource(R.string.friends_add), style = CoroomText.button) },
        )
    }

    if (adding) {
        AddFriendDialog(
            onDismiss = { adding = false },
            onAdd = {
                adding = false
                onAdd(it)
            },
        )
    }
    removing?.let { friend ->
        AlertDialog(
            onDismissRequest = { removing = null },
            title = { Text(stringResource(R.string.friends_remove_title, friend.shownName)) },
            text = { Text(stringResource(R.string.friends_remove_body)) },
            confirmButton = {
                TextButton(onClick = {
                    removing = null
                    onRemove(friend.id)
                }) { Text(stringResource(R.string.friends_remove_confirm), color = colors.error) }
            },
            dismissButton = {
                TextButton(onClick = { removing = null }) { Text(stringResource(R.string.action_cancel)) }
            },
        )
    }
}

@Composable
private fun FriendGroup(title: String, content: @Composable () -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        SectionHeader(title)
        ListCard { content() }
    }
}

@Composable
private fun FriendMenu(friend: Friend, onRemove: (Friend) -> Unit) {
    var open by remember { mutableStateOf(false) }
    Box {
        IconButton(onClick = { open = true }) {
            CoroomIcon(
                R.drawable.ic_ellipsis_vertical,
                contentDescription = stringResource(R.string.friends_more, friend.shownName),
                size = 20.dp,
            )
        }
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            DropdownMenuItem(
                text = { Text(stringResource(R.string.friends_remove)) },
                onClick = {
                    open = false
                    onRemove(friend)
                },
            )
        }
    }
}

@Composable
private fun Requests(
    requests: List<FriendRequest>,
    busy: Boolean,
    onAccept: (Long) -> Unit,
    onDecline: (Long) -> Unit,
) {
    val colors = MaterialTheme.colorScheme
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        SectionHeader(stringResource(R.string.friends_requests))
        requests.forEach { request ->
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(16.dp))
                    .background(colors.surfaceContainer)
                    .padding(14.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    Avatar(name = request.shownName, seed = request.user?.id ?: request.id)
                    Column(modifier = Modifier.weight(1f)) {
                        Text(request.shownName, style = CoroomText.itemTitle, color = colors.onSurface)
                        Text(
                            stringResource(R.string.friends_request_incoming, request.username),
                            style = CoroomText.supporting,
                            color = colors.onSurfaceVariant,
                        )
                    }
                }
                Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End)) {
                    PillOutlinedButton(stringResource(R.string.friends_decline), { onDecline(request.id) }, enabled = !busy)
                    PillButton(stringResource(R.string.friends_accept), { onAccept(request.id) }, enabled = !busy)
                }
            }
        }
    }
}

@Composable
private fun Outgoing(requests: List<FriendRequest>, busy: Boolean, onCancel: (Long) -> Unit) {
    FriendGroup(stringResource(R.string.friends_outgoing)) {
        requests.forEachIndexed { index, request ->
            if (index > 0) RowDivider()
            PersonRow(
                name = request.shownName,
                seed = request.user?.id ?: request.id,
                supporting = stringResource(R.string.friends_request_outgoing, request.username),
            ) {
                PillOutlinedButton(stringResource(R.string.action_cancel), { onCancel(request.id) }, enabled = !busy)
            }
        }
    }
}

@Composable
private fun AddFriendDialog(onDismiss: () -> Unit, onAdd: (String) -> Unit) {
    var username by rememberSaveable { mutableStateOf("") }
    val valid = username.trim().removePrefix("@").isNotEmpty()
    AlertDialog(
        onDismissRequest = onDismiss,
        icon = { CoroomIcon(R.drawable.ic_user_plus, contentDescription = null, tint = MaterialTheme.colorScheme.primary) },
        title = { Text(stringResource(R.string.friends_add_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text(stringResource(R.string.friends_add_body), style = CoroomText.supporting)
                LabeledField(
                    label = stringResource(R.string.login_username),
                    value = username,
                    onValueChange = { username = it },
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Ascii, imeAction = ImeAction.Send),
                    keyboardActions = KeyboardActions(onSend = { if (valid) onAdd(username) }),
                )
            }
        },
        confirmButton = {
            TextButton(onClick = { onAdd(username) }, enabled = valid) { Text(stringResource(R.string.friends_add_send)) }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.action_cancel)) } },
    )
}

@Preview(name = "Amigos", widthDp = 390, heightDp = 844)
@Composable
private fun FriendsPreview() {
    CoroomTheme(dynamicColor = false) {
        Box(Modifier.background(MaterialTheme.colorScheme.background).size(390.dp, 844.dp)) {
            FriendsScreen(FakeData.mainState, PaddingValues(), {}, {}, {}, {}, {}, {})
        }
    }
}

@Preview(name = "Amigos vacío", widthDp = 390, heightDp = 844)
@Composable
private fun FriendsEmptyPreview() {
    CoroomTheme(dynamicColor = false) {
        Box(Modifier.background(MaterialTheme.colorScheme.background).size(390.dp, 844.dp)) {
            FriendsScreen(FakeData.emptyState, PaddingValues(), {}, {}, {}, {}, {}, {})
        }
    }
}
