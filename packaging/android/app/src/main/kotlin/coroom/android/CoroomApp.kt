package coroom.android

import android.app.Application
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.security.KeyChain
import android.view.ViewConfiguration
import androidx.core.content.ContextCompat
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import coroom.android.bridge.CoroomEvent
import coroom.android.bridge.CoroomRepository
import coroom.android.platform.CaRoots
import coroom.android.platform.Decoders
import coroom.android.platform.DeviceName
import coroom.android.platform.Gamepads
import coroom.android.platform.NetworkWatcher
import coroom.android.platform.Notifier
import coroom.android.platform.Rumble
import coroom.android.platform.SealSecret
import coroom.android.settings.SettingsStore
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

class CoroomApp : Application() {
    lateinit var container: AppContainer
        private set

    override fun onCreate() {
        super.onCreate()
        container = AppContainer(this)
        container.start()
    }
}

/** El contenedor de la app es el del proceso: lo comparten las dos actividades. */
val Context.container: AppContainer get() = (applicationContext as CoroomApp).container

/** Cómo fue el arranque de la parte en Rust. */
sealed interface Startup {
    data object Starting : Startup

    /** Lista. `sessionLost`: el secreto del Keystore se perdió y hay que iniciar sesión otra vez. */
    data class Ready(val sessionLost: Boolean) : Startup

    data class Failed(val message: String) : Startup
}

/**
 * Lo que vive mientras vive el proceso: el puente con Rust, los ajustes y lo que se vigila del
 * sistema (red, mandos, raíces de confianza, si la app está delante).
 */
class AppContainer(private val app: Application) {
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    val settings = SettingsStore(app)
    val repository = CoroomRepository(app, scope)
    val gamepads = Gamepads(app)
    val network = NetworkWatcher(app) { gateway, mtu -> repository.setRoute(gateway, mtu) }
    private val rumble = Rumble(app)
    private val notifier = Notifier(app)

    private val _startup = MutableStateFlow<Startup>(Startup.Starting)
    val startup: StateFlow<Startup> = _startup.asStateFlow()

    private val _foreground = MutableStateFlow(false)
    val foreground: StateFlow<Boolean> = _foreground.asStateFlow()

    fun start() {
        notifier.createChannels()
        ProcessLifecycleOwner.get().lifecycle.addObserver(object : DefaultLifecycleObserver {
            override fun onStart(owner: LifecycleOwner) {
                _foreground.value = true
                notifier.cancelInvitations()
            }

            override fun onStop(owner: LifecycleOwner) {
                _foreground.value = false
            }
        })
        scope.launch(Dispatchers.IO) { boot() }
    }

    private suspend fun boot() {
        val loaded = try {
            SealSecret.load(app)
        } catch (error: Exception) {
            _startup.value = Startup.Failed(app.getString(R.string.error_keystore, error.message.orEmpty()))
            return
        }
        val error = try {
            Native.init(app.filesDir.absolutePath, loaded.secret, DeviceName.get(app))
        } catch (_: UnsatisfiedLinkError) {
            app.getString(R.string.error_native_missing)
        } catch (_: ExceptionInInitializerError) {
            app.getString(R.string.error_native_missing)
        } catch (_: NoClassDefFoundError) {
            app.getString(R.string.error_native_missing)
        } finally {
            loaded.secret.fill(0)
        }
        if (error != null) {
            _startup.value = Startup.Failed(error)
            return
        }
        repository.markReady()
        repository.startEventLoop()
        refreshRoots()
        // Antes de cualquier sala: qué decodificadores hay y los umbrales de gesto del sistema.
        runCatching { repository.setDecoders(Decoders.list()) }
        val gestures = ViewConfiguration.get(app)
        runCatching {
            repository.setTouchConfig(
                touchSlop = gestures.scaledTouchSlop.toFloat(),
                tapTimeoutMs = ViewConfiguration.getTapTimeout(),
                longPressTimeoutMs = ViewConfiguration.getLongPressTimeout(),
                doubleTapTimeoutMs = ViewConfiguration.getDoubleTapTimeout(),
            )
        }
        withContext(Dispatchers.Main) {
            network.start()
            gamepads.start()
            ContextCompat.registerReceiver(
                app,
                object : BroadcastReceiver() {
                    override fun onReceive(context: Context, intent: Intent) {
                        scope.launch(Dispatchers.IO) { refreshRoots() }
                    }
                },
                IntentFilter(KeyChain.ACTION_TRUST_STORE_CHANGED),
                ContextCompat.RECEIVER_NOT_EXPORTED,
            )
        }
        pushSettings()
        watchEvents()
        manageLobby()
        if (loaded.replacedLost) settings.setHasSavedSession(false)
        _startup.value = Startup.Ready(sessionLost = loaded.replacedLost)
    }

    private suspend fun refreshRoots() {
        runCatching { repository.setRoots(CaRoots.pem()) }
    }

    /** Cada ajuste que cambia llega a Rust, venga de Ajustes o de la barra de la sala. */
    private fun pushSettings() {
        scope.launch {
            settings.settings.map { it.touchMode }.distinctUntilChanged().collect {
                runCatching { repository.setTouchMode(it) }
            }
        }
        scope.launch {
            settings.settings.map { it.gameVolume }.distinctUntilChanged().collect {
                runCatching { repository.setGameVolume(it) }
            }
        }
        scope.launch {
            settings.settings.map { it.joinMuted }.distinctUntilChanged().collect {
                runCatching { repository.setJoinMuted(it) }
            }
        }
    }

    /** Lo que se hace con los eventos aunque no haya ninguna pantalla abierta. */
    private fun watchEvents() {
        scope.launch {
            repository.events.collect { event ->
                when (event) {
                    is CoroomEvent.Rumble -> withContext(Dispatchers.Main) {
                        rumble.play(event.deviceId, event.strong, event.weak)
                    }
                    is CoroomEvent.Invited -> {
                        val notify = settings.settings.first().notifyInvitations
                        if (notify && !_foreground.value) notifier.showInvitation(event.fromName, event.code)
                    }
                    else -> Unit
                }
            }
        }
    }

    /**
     * La sala de espera (señalización) abierta mientras haya sesión y no se esté en una sala.
     * Con la app detrás se deja abierta si el usuario quiere avisos de invitaciones (hasta que el
     * sistema congele el proceso); si no, se cierra al minuto.
     */
    private fun manageLobby() {
        scope.launch {
            combine(
                repository.account,
                repository.inRoom,
                _foreground,
                settings.settings.map { it.notifyInvitations }.distinctUntilChanged(),
            ) { account, inRoom, foreground, notify ->
                when {
                    account == null || inRoom -> LobbyWish.Closed
                    foreground || notify -> LobbyWish.Open
                    else -> LobbyWish.CloseSoon
                }
            }.distinctUntilChanged().collectLatest { wish ->
                when (wish) {
                    LobbyWish.Open -> runCatching { repository.lobbyStart() }
                    LobbyWish.Closed -> repository.lobbyStop()
                    LobbyWish.CloseSoon -> {
                        delay(BACKGROUND_LOBBY_MS)
                        repository.lobbyStop()
                    }
                }
            }
        }
    }

    private enum class LobbyWish { Open, Closed, CloseSoon }

    private companion object {
        const val BACKGROUND_LOBBY_MS = 60_000L
    }
}
