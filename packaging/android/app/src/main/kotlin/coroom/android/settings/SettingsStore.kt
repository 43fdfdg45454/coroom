package coroom.android.settings

import android.content.Context
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.emptyPreferences
import androidx.datastore.preferences.core.intPreferencesKey
import androidx.datastore.preferences.core.longPreferencesKey
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import coroom.android.bridge.TouchMode
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.map
import java.io.IOException

/** Los ajustes que elige el usuario (pantalla Ajustes y barra de la sala). */
data class Settings(
    val touchMode: TouchMode = TouchMode.Trackpad,
    val gameVolume: Int = 100,
    val joinMuted: Boolean = false,
    val notifyInvitations: Boolean = true,
    /** Si hay una sesión guardada que `resume` debería abrir (para no enseñar el error la primera vez). */
    val hasSavedSession: Boolean = false,
    /** Si ya se pidió el permiso de notificaciones una vez. */
    val askedNotifications: Boolean = false,
    /** «Buscar versiones nuevas». */
    val updatesCheck: Boolean = true,
    /** «Actualizar automáticamente». */
    val updatesAuto: Boolean = true,
    /** Cada cuántas horas se busca (24 o 168). */
    val updatesIntervalHours: Int = 24,
    /** La última búsqueda que contestó, en segundos desde 1970. */
    val updatesLastCheck: Long? = null,
)

/** Las frecuencias de «Buscar versiones nuevas», en horas (una vez al día, una vez a la semana). */
val UPDATE_INTERVALS = listOf(24, 168)

private val Context.dataStore: DataStore<Preferences> by preferencesDataStore(name = "ajustes")

class SettingsStore(context: Context) {
    private val store = context.applicationContext.dataStore

    val settings: Flow<Settings> = store.data
        .catch { error -> if (error is IOException) emit(emptyPreferences()) else throw error }
        .map { prefs ->
            Settings(
                touchMode = TouchMode.fromWire(prefs[TOUCH_MODE]),
                gameVolume = (prefs[GAME_VOLUME] ?: 100).coerceIn(0, 200),
                joinMuted = prefs[JOIN_MUTED] ?: false,
                notifyInvitations = prefs[NOTIFY_INVITATIONS] ?: true,
                hasSavedSession = prefs[HAS_SAVED_SESSION] ?: false,
                askedNotifications = prefs[ASKED_NOTIFICATIONS] ?: false,
                updatesCheck = prefs[UPDATES_CHECK] ?: true,
                updatesAuto = prefs[UPDATES_AUTO] ?: true,
                updatesIntervalHours = (prefs[UPDATES_INTERVAL] ?: 24).takeIf { it in UPDATE_INTERVALS } ?: 24,
                updatesLastCheck = prefs[UPDATES_LAST_CHECK],
            )
        }

    suspend fun setTouchMode(mode: TouchMode) = store.edit { it[TOUCH_MODE] = mode.wire }
    suspend fun setGameVolume(percent: Int) = store.edit { it[GAME_VOLUME] = percent.coerceIn(0, 200) }
    suspend fun setJoinMuted(muted: Boolean) = store.edit { it[JOIN_MUTED] = muted }
    suspend fun setNotifyInvitations(notify: Boolean) = store.edit { it[NOTIFY_INVITATIONS] = notify }
    suspend fun setHasSavedSession(saved: Boolean) = store.edit { it[HAS_SAVED_SESSION] = saved }
    suspend fun setAskedNotifications() = store.edit { it[ASKED_NOTIFICATIONS] = true }
    suspend fun setUpdatesCheck(on: Boolean) = store.edit { it[UPDATES_CHECK] = on }
    suspend fun setUpdatesAuto(on: Boolean) = store.edit { it[UPDATES_AUTO] = on }
    suspend fun setUpdatesInterval(hours: Int) = store.edit { it[UPDATES_INTERVAL] = hours }
    suspend fun setUpdatesLastCheck(at: Long) = store.edit { it[UPDATES_LAST_CHECK] = at }

    private companion object {
        val TOUCH_MODE = stringPreferencesKey("touch_mode")
        val GAME_VOLUME = intPreferencesKey("game_volume")
        val JOIN_MUTED = booleanPreferencesKey("join_muted")
        val NOTIFY_INVITATIONS = booleanPreferencesKey("notify_invitations")
        val HAS_SAVED_SESSION = booleanPreferencesKey("has_saved_session")
        val ASKED_NOTIFICATIONS = booleanPreferencesKey("asked_notifications")
        val UPDATES_CHECK = booleanPreferencesKey("updates_check")
        val UPDATES_AUTO = booleanPreferencesKey("updates_auto")
        val UPDATES_INTERVAL = intPreferencesKey("updates_interval_hours")
        val UPDATES_LAST_CHECK = longPreferencesKey("updates_last_check")
    }
}
