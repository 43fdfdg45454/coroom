package coroom.android.ui.preview

import coroom.android.bridge.Account
import coroom.android.bridge.Friend
import coroom.android.bridge.FriendRequest
import coroom.android.bridge.FriendsList
import coroom.android.bridge.Invitation
import coroom.android.platform.Gamepad
import coroom.android.ui.main.FriendsUi
import coroom.android.ui.main.MainPhase
import coroom.android.ui.main.MainUiState

/**
 * Datos de muestra para las vistas previas, con los nombres ficticios de los bocetos. Solo los
 * usan las `@Preview`; en la app todo llega del puente con Rust.
 */
internal object FakeData {
    private const val NOW = 1_700_000_000_000L

    val account = Account(
        server = "https://coroom.example.com",
        username = "ana",
        displayName = "Ana",
        userId = 1,
    )

    private val friends = FriendsList(
        friends = listOf(
            Friend(id = 2, username = "bea", displayName = "Bea", online = true, activity = "hosting"),
            Friend(id = 3, username = "carla", displayName = "Carla", online = true),
            Friend(id = 5, username = "eli", displayName = "Eli", online = false),
        ),
        incoming = listOf(
            FriendRequest(id = 9, user = Friend(id = 4, username = "dani", displayName = "Dani")),
        ),
    )

    val mainState = MainUiState(
        phase = MainPhase.SignedIn,
        account = account,
        serverOnline = true,
        invitations = listOf(Invitation(fromUser = 2, fromName = "Bea", code = "K7Q2MZ", receivedAtMs = NOW - 60_000)),
        friends = FriendsUi(loading = false, list = friends),
        pads = listOf(Gamepad(deviceId = 7, name = "Mando de ejemplo", canVibrate = true)),
        nowMs = NOW,
    )

    val emptyState = MainUiState(
        phase = MainPhase.SignedIn,
        account = account,
        serverOnline = true,
        friends = FriendsUi(loading = false, list = FriendsList()),
        nowMs = NOW,
    )
}
