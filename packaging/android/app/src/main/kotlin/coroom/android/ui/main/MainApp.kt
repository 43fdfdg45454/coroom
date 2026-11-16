package coroom.android.ui.main

import androidx.annotation.DrawableRes
import androidx.annotation.StringRes
import androidx.compose.animation.Crossfade
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.displayCutout
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.systemBars
import androidx.compose.foundation.layout.union
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.NavigationBarItemDefaults
import androidx.compose.material3.NavigationRail
import androidx.compose.material3.NavigationRailItem
import androidx.compose.material3.NavigationRailItemDefaults
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
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.navigation.NavDestination
import androidx.navigation.NavDestination.Companion.hasRoute
import androidx.navigation.NavDestination.Companion.hierarchy
import androidx.navigation.NavGraph.Companion.findStartDestination
import androidx.navigation.NavHostController
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import coroom.android.R
import coroom.android.platform.LogShare
import coroom.android.ui.UiText
import coroom.android.ui.components.CoroomIcon
import coroom.android.ui.components.Notice
import coroom.android.ui.components.PrimaryButton
import coroom.android.ui.components.SecondaryButton
import coroom.android.ui.theme.CoroomText
import coroom.android.ui.theme.CoroomTheme
import coroom.android.ui.theme.Tone
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
                    update = state.update,
                    updates = main.updateActions,
                )
            }
            else -> StartupScreen(state.phase, onRetry = main::retryResume, onSignInAgain = main::signInAgain)
        }
    }
    // El APK nuevo al instalador del sistema, en cualquier fase (también antes de entrar).
    UpdateInstallHost(state.pendingInstall, onHandled = main::installHandled, onFailed = main::installFailed)
}

/** Arrancando, recuperando la sesión o un error que impide seguir, siempre explicado. */
@Composable
private fun StartupScreen(phase: MainPhase, onRetry: () -> Unit, onSignInAgain: () -> Unit) {
    Surface(color = MaterialTheme.colorScheme.background, modifier = Modifier.fillMaxSize()) {
        Box(modifier = Modifier.fillMaxSize().safeDrawingPadding().padding(24.dp), contentAlignment = Alignment.Center) {
            when (phase) {
                is MainPhase.Failed -> Notice(
                    title = stringResource(R.string.startup_failed_title),
                    body = phase.message,
                    icon = R.drawable.ic_triangle_alert,
                    tone = Tone.Error,
                    modifier = Modifier.widthIn(max = 480.dp),
                )
                is MainPhase.ResumeFailed -> Notice(
                    title = stringResource(R.string.resume_failed_title),
                    body = stringResource(R.string.resume_failed_body, phase.message),
                    icon = R.drawable.ic_wifi_off,
                    modifier = Modifier.widthIn(max = 480.dp),
                    actions = {
                        SecondaryButton(stringResource(R.string.resume_other_account), onSignInAgain)
                        PrimaryButton(stringResource(R.string.action_retry), onRetry, icon = R.drawable.ic_refresh_cw)
                    },
                )
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
    // «Compartir registro» de Ajustes: la hoja de compartir del sistema con los ficheros.
    LaunchedEffect(main) {
        main.logShares.collect { paths ->
            val chooser = LogShare.chooser(context, paths)
            val shown = chooser != null && runCatching { context.startActivity(chooser) }.isSuccess
            if (!shown) {
                snackbar.showSnackbar(
                    UiText.res(if (chooser == null) R.string.log_share_empty else R.string.log_share_failed).resolve(context),
                )
            }
        }
    }
    // Al volver a la app, la lista de amigos al día.
    LifecycleResumeEffect(main) {
        main.refreshFriends()
        onPauseOrDispose { }
    }

    val requests = state.friends.list?.incoming?.size ?: 0
    // La navegación de Material según el ancho: barra inferior en compacto y riel lateral desde
    // el ancho expandido (tabletas, plegables abiertos y teléfonos en horizontal).
    BoxWithConstraints(modifier = Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background)) {
        val useRail = maxWidth >= RAIL_MIN_WIDTH
        val insets = WindowInsets.systemBars.union(WindowInsets.displayCutout)
        Row(modifier = Modifier.fillMaxSize()) {
            if (useRail) {
                SideRail(nav, requests, insets.only(WindowInsetsSides.Vertical + WindowInsetsSides.Start))
            }
            Scaffold(
                // El riel ya se queda con el margen del borde inicial: el contenido no lo repite.
                modifier = if (useRail) {
                    Modifier.weight(1f).consumeWindowInsets(insets.only(WindowInsetsSides.Start))
                } else {
                    Modifier.weight(1f)
                },
                snackbarHost = { SnackbarHost(snackbar) },
                bottomBar = { if (!useRail) BottomBar(nav, requests) },
                containerColor = MaterialTheme.colorScheme.background,
                contentWindowInsets = insets,
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
                            updates = main.updateActions,
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
                            onShareLog = main::shareLog,
                            updates = main.updateActions,
                        )
                    }
                }
            }
        }
    }
}

/** El ancho desde el que la navegación pasa de la barra inferior al riel (ancho expandido de Material). */
private val RAIL_MIN_WIDTH = 840.dp

private fun NavHostController.selectTab(route: Any) {
    navigate(route) {
        popUpTo(graph.findStartDestination().id) { saveState = true }
        launchSingleTop = true
        restoreState = true
    }
}

private fun NavDestination?.isOn(tab: Tab): Boolean =
    this?.hierarchy?.any { it.hasRoute(tab.route::class) } == true

/** El icono de una pestaña; la de Amigos lleva el número de solicitudes pendientes. */
@Composable
private fun TabIcon(tab: Tab, requests: Int) {
    if (tab.route == FriendsRoute && requests > 0) {
        BadgedBox(badge = { Badge { Text(requests.toString()) } }) {
            CoroomIcon(tab.icon, contentDescription = null, tint = LocalContentColor.current)
        }
    } else {
        CoroomIcon(tab.icon, contentDescription = null, tint = LocalContentColor.current)
    }
}

/** La barra de navegación inferior (ancho compacto): el plano más teñido de la ventana. */
@Composable
private fun BottomBar(nav: NavHostController, requests: Int) {
    val entry by nav.currentBackStackEntryAsState()
    val destination = entry?.destination
    val colors = MaterialTheme.colorScheme
    NavigationBar(containerColor = CoroomTheme.navigationContainer) {
        Tabs.forEach { tab ->
            NavigationBarItem(
                selected = destination.isOn(tab),
                onClick = { nav.selectTab(tab.route) },
                icon = { TabIcon(tab, requests) },
                label = { Text(stringResource(tab.label)) },
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

/** El riel de navegación lateral (ancho expandido), con la marca arriba. */
@Composable
private fun SideRail(nav: NavHostController, requests: Int, insets: WindowInsets) {
    val entry by nav.currentBackStackEntryAsState()
    val destination = entry?.destination
    val colors = MaterialTheme.colorScheme
    NavigationRail(
        containerColor = CoroomTheme.navigationContainer,
        header = {
            CoroomIcon(
                R.drawable.ic_radio,
                contentDescription = null,
                modifier = Modifier.padding(vertical = 12.dp),
                tint = colors.primary,
                size = 28.dp,
            )
        },
        windowInsets = insets,
    ) {
        Tabs.forEach { tab ->
            NavigationRailItem(
                selected = destination.isOn(tab),
                onClick = { nav.selectTab(tab.route) },
                icon = { TabIcon(tab, requests) },
                label = { Text(stringResource(tab.label)) },
                colors = NavigationRailItemDefaults.colors(
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
