package coroom.android

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.repeatOnLifecycle
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import coroom.android.ui.main.LoginViewModel
import coroom.android.ui.main.MainApp
import coroom.android.ui.main.MainPhase
import coroom.android.ui.main.MainViewModel
import coroom.android.ui.room.RoomActivity
import coroom.android.ui.theme.CoroomTheme
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

/** Inicio, Amigos, Ajustes y la pantalla Servidor, en vertical. La sala va en [RoomActivity]. */
class MainActivity : ComponentActivity() {
    private val main: MainViewModel by viewModels {
        viewModelFactory { initializer { MainViewModel(container) } }
    }
    private val login: LoginViewModel by viewModels {
        viewModelFactory { initializer { LoginViewModel(container) } }
    }

    private val notificationPermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) main.setNotifyInvitations(true) else main.notificationsDenied()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        if (savedInstanceState == null) handleIntent(intent)

        setContent {
            CoroomTheme {
                MainApp(main = main, login = login, onEnableNotifications = ::enableNotifications)
            }
        }

        lifecycleScope.launch {
            repeatOnLifecycle(Lifecycle.State.STARTED) {
                main.rooms.collect { request ->
                    startActivity(RoomActivity.intent(this@MainActivity, request.code, request.hostName))
                }
            }
        }
        // La primera vez que hay sesión, el permiso de notificaciones para «Avisarme cuando me invitan».
        lifecycleScope.launch {
            main.state.first { it.phase == MainPhase.SignedIn }
            val settings = main.state.value.settings
            if (settings.notifyInvitations && !settings.askedNotifications && needsNotificationPermission()) {
                main.markAskedNotifications()
                notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleIntent(intent)
    }

    private fun handleIntent(intent: Intent?) {
        if (intent?.action != ACTION_JOIN) return
        val code = intent.getStringExtra(EXTRA_CODE) ?: return
        main.join(code, intent.getStringExtra(EXTRA_HOST_NAME))
    }

    private fun enableNotifications() {
        if (needsNotificationPermission()) {
            notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
        } else {
            main.setNotifyInvitations(true)
        }
    }

    private fun needsNotificationPermission(): Boolean =
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED

    companion object {
        const val ACTION_JOIN = "coroom.android.action.JOIN"
        const val EXTRA_CODE = "coroom.android.extra.CODE"
        const val EXTRA_HOST_NAME = "coroom.android.extra.HOST_NAME"
    }
}
