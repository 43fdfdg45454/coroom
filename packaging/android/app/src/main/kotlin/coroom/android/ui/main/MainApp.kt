package coroom.android.ui.main

import androidx.annotation.DrawableRes
import androidx.annotation.StringRes
import androidx.compose.animation.Crossfade
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.NavigationBarItemDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.navigation.NavDestination.Companion.hasRoute
import androidx.navigation.NavDestination.Companion.hierarchy
import androidx.navigation.NavGraph.Companion.findStartDestination
import androidx.navigation.NavHostController
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import coroom.android.R
import coroom.android.ui.components.CoroomIcon
import coroom.android.ui.components.NoticeCard
import coroom.android.ui.components.PillButton
import coroom.android.ui.components.PillOutlinedButton
import coroom.android.ui.theme.CoroomText
import kotlinx.serialization.Serializable

@Serializable
data object HomeRoute

@Serializable
data object FriendsRoute

@Serializable
data object SettingsRoute

private data class Tab(val route: Any, @StringRes val label: Int, @DrawableRes val icon: Int)

private val Tabs = listOf(
    Tab(HomeRoute, R.string.tab_home, R.drawable.ic_house),
    Tab(FriendsRoute, R.string.tab_friends, R.drawable.ic_users),
    Tab(SettingsRoute, R.string.tab_settings, R.drawable.ic_settings),
)

/** La raíz de la actividad principal: el arranque, la pantalla Servidor o las tres pestañas. */
@Composable
fun MainApp(
    main: MainViewModel,
    login: LoginViewModel,
    onEnableNotifications: () -> Unit,
) {
    val state by main.state.collectAsStateWithLifecycle()
    Crossfade(targetState = state.phase::class, label = "phase") { phaseClass ->
        when (phaseClass) {
            MainPhase.SignedIn::class -> SignedInApp(main, state, onEnableNotifications)
            MainPhase.SignedOut::class -> {
                val form by login.state.collectAsStateWithLifecycle()
                ServerScreen(
                    state = form,
                    sessionLost = (state.phase as? MainPhase.SignedOut)?.sessionLost == true,
                    onUrl = login::setUrl,
                    onProbeNow = login::probeNow,
                    onTrust = login::trust,
                    onUsername = login::setUsername,
                    onPassword = login::setPassword,
                    onPasswordRepeat = login::setPasswordRepeat,
                    onInvitation = login::setInvitation,
                    onDisplayName = login::setDisplayName,
                    onToggleInvitation = login::toggleInvitationMode,
                    onSubmit = login::submit,
                )
            }
            else -> StartupScreen(state.phase, onRetry = main::retryResume, onSignInAgain = main::signInAgain)
        }
    }
}

/** Arrancando, recuperando la sesión o un error que impide seguir, siempre explicado. */
@Composable
private fun StartupScreen(phase: MainPhase, onRetry: () -> Unit, onSignInAgain: () -> Unit) {
    Surface(color = MaterialTheme.colorScheme.background, modifier = Modifier.fillMaxSize()) {
        Box(modifier = Modifier.fillMaxSize().safeDrawingPadding().padding(24.dp), contentAlignment = Alignment.Center) {
            when (phase) {
                is MainPhase.Failed -> NoticeCard(
                    title = stringResource(R.string.startup_failed_title),
                    body = phase.message,
                    icon = R.drawable.ic_triangle_alert,
                    containerColor = MaterialTheme.colorScheme.errorContainer,
                    contentColor = MaterialTheme.colorScheme.onErrorContainer,
                    modifier = Modifier.widthIn(max = 480.dp),
                )
                is MainPhase.ResumeFailed -> Column(
                    modifier = Modifier.widthIn(max = 480.dp),
                    verticalArrangement = Arrangement.spacedBy(16.dp),
                ) {
                    NoticeCard(
                        title = stringResource(R.string.resume_failed_title),
                        body = stringResource(R.string.resume_failed_body, phase.message),
                        icon = R.drawable.ic_wifi_off,
                    )
                    Box(Modifier.align(Alignment.End)) {
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            PillOutlinedButton(stringResource(R.string.resume_other_account), onSignInAgain)
                            PillButton(stringResource(R.string.action_retry), onRetry, icon = R.drawable.ic_refresh_cw)
                        }
                    }
                }
                else -> Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(16.dp)) {
                    CircularProgressIndicator()
                    Text(
                        text = stringResource(if (phase == MainPhase.Resuming) R.string.startup_resuming else R.string.startup_starting),
                        style = CoroomText.body,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

@Composable
private fun SignedInApp(main: MainViewModel, state: MainUiState, onEnableNotifications: () -> Unit) {
    val nav = rememberNavController()
    val snackbar = remember { SnackbarHostState() }
    val context = LocalContext.current

    LaunchedEffect(main) {
        main.messages.collect { snackbar.showSnackbar(it.resolve(context)) }
    }
    // Al volver a la app, la lista de amigos al día.
    LifecycleResumeEffect(main) {
        main.refreshFriends()
        onPauseOrDispose { }
    }

    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        bottomBar = { BottomBar(nav, requests = state.friends.list?.incoming?.size ?: 0) },
        containerColor = MaterialTheme.colorScheme.background,
    ) { padding ->
        NavHost(
            navController = nav,
            startDestination = HomeRoute,
            enterTransition = { fadeIn() },
            exitTransition = { fadeOut() },
        ) {
            composable<HomeRoute> {
                HomeScreen(
                    state = state,
                    contentPadding = padding,
                    onJoin = main::join,
                    onAskToJoin = main::askToJoin,
                    onOpenAccount = { nav.selectTab(SettingsRoute) },
                    onOpenFriends = { nav.selectTab(FriendsRoute) },
                )
            }
            composable<FriendsRoute> {
                FriendsScreen(
                    state = state,
                    contentPadding = padding,
                    onRefresh = main::refreshFriends,
                    onAskToJoin = main::askToJoin,
                    onAccept = main::acceptRequest,
                    onDecline = main::declineRequest,
                    onRemove = main::removeFriend,
                    onAdd = main::requestFriend,
                )
            }
            composable<SettingsRoute> {
                SettingsScreen(
                    state = state,
                    contentPadding = padding,
                    onTouchMode = main::setTouchMode,
                    onGameVolume = main::setGameVolume,
                    onJoinMuted = main::setJoinMuted,
                    onNotify = main::setNotifyInvitations,
                    onEnableNotifications = onEnableNotifications,
                    onLogout = main::logout,
                )
            }
        }
    }
}

private fun NavHostController.selectTab(route: Any) {
    navigate(route) {
        popUpTo(graph.findStartDestination().id) { saveState = true }
        launchSingleTop = true
        restoreState = true
    }
}

@Composable
private fun BottomBar(nav: NavHostController, requests: Int) {
    val entry by nav.currentBackStackEntryAsState()
    val destination = entry?.destination
    val colors = MaterialTheme.colorScheme
    NavigationBar(containerColor = colors.surfaceContainer) {
        Tabs.forEach { tab ->
            val selected = destination?.hierarchy?.any { it.hasRoute(tab.route::class) } == true
            NavigationBarItem(
                selected = selected,
                onClick = { nav.selectTab(tab.route) },
                icon = {
                    if (tab.route == FriendsRoute && requests > 0) {
                        BadgedBox(badge = { Badge { Text(requests.toString()) } }) {
                            CoroomIcon(tab.icon, contentDescription = null, tint = LocalContentColor.current)
                        }
                    } else {
                        CoroomIcon(tab.icon, contentDescription = null, tint = LocalContentColor.current)
                    }
                },
                label = {
                    Text(
                        stringResource(tab.label),
                        fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Medium,
                    )
                },
                colors = NavigationBarItemDefaults.colors(
                    indicatorColor = colors.primaryContainer,
                    selectedIconColor = colors.onPrimaryContainer,
                    selectedTextColor = colors.onSurface,
                    unselectedIconColor = colors.onSurfaceVariant,
                    unselectedTextColor = colors.onSurfaceVariant,
                ),
            )
        }
    }
}
