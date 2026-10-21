package coroom.android.ui.main

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import coroom.android.AppContainer
import coroom.android.R
import coroom.android.bridge.BridgeException
import coroom.android.bridge.ServerProbe
import coroom.android.ui.UiText
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** El certificado del servidor: si hace falta confiar en su huella y qué decidió el usuario. */
enum class TrustState { NotNeeded, Ask, Trusted, Rejected }

data class LoginUiState(
    val url: String = "",
    val probing: Boolean = false,
    val probe: ServerProbe? = null,
    val probeError: String? = null,
    val trust: TrustState = TrustState.NotNeeded,
    val username: String = "",
    val password: String = "",
    val passwordRepeat: String = "",
    val invitationMode: Boolean = false,
    val invitation: String = "",
    val displayName: String = "",
    val busy: Boolean = false,
    val error: UiText? = null,
) {
    /** Si con lo escrito ya se puede pulsar «Iniciar sesión» / «Crear la cuenta». */
    val canSubmit: Boolean
        get() = !busy && probe != null && (trust == TrustState.NotNeeded || trust == TrustState.Trusted) &&
            username.isNotBlank() && password.isNotEmpty() &&
            (!invitationMode || invitation.isNotBlank())
}

/** Pantalla Servidor: comprobar el servidor, confiar en su certificado e iniciar sesión. */
class LoginViewModel(private val container: AppContainer) : ViewModel() {
    private val repository = container.repository
    private val _state = MutableStateFlow(LoginUiState())
    val state: StateFlow<LoginUiState> = _state.asStateFlow()
    private var probeJob: Job? = null

    fun setUrl(url: String) {
        _state.update {
            it.copy(url = url, probe = null, probeError = null, trust = TrustState.NotNeeded, error = null)
        }
        probeJob?.cancel()
        // Mientras se escribe solo se comprueba lo que ya parece una dirección completa.
        val normalized = normalize(url, whileTyping = true) ?: return
        probeJob = viewModelScope.launch {
            delay(PROBE_DEBOUNCE_MS)
            probe(normalized)
        }
    }

    /** Comprobar ya (al pulsar «Ir» en el teclado o «Reintentar»). */
    fun probeNow() {
        val normalized = normalize(_state.value.url) ?: run {
            _state.update { it.copy(probeError = null, error = UiText.res(R.string.login_error_url)) }
            return
        }
        probeJob?.cancel()
        probeJob = viewModelScope.launch { probe(normalized) }
    }

    private suspend fun probe(url: String) {
        _state.update { it.copy(probing = true, probeError = null) }
        try {
            val probe = repository.probeServer(url)
            _state.update {
                it.copy(
                    probing = false,
                    probe = probe,
                    trust = if (probe.untrusted != null) TrustState.Ask else TrustState.NotNeeded,
                )
            }
            // La app quedó atrás del servidor: buscar ya la versión nueva para ofrecerla aquí (Rust
            // limita estas búsquedas fuera del intervalo a una por hora).
            if (probe.protocolGap?.clientOlder == true && repository.update.value?.version == null) {
                runCatching { repository.updateCheckExtra() }
            }
        } catch (error: BridgeException) {
            _state.update { it.copy(probing = false, probeError = error.message) }
        }
    }

    fun trust(accept: Boolean) {
        _state.update { it.copy(trust = if (accept) TrustState.Trusted else TrustState.Rejected) }
    }

    fun setUsername(value: String) = _state.update { it.copy(username = value.trim(), error = null) }
    fun setPassword(value: String) = _state.update { it.copy(password = value, error = null) }
    fun setPasswordRepeat(value: String) = _state.update { it.copy(passwordRepeat = value, error = null) }
    fun setInvitation(value: String) = _state.update { it.copy(invitation = value.trim(), error = null) }
    fun setDisplayName(value: String) = _state.update { it.copy(displayName = value, error = null) }

    fun toggleInvitationMode() = _state.update { it.copy(invitationMode = !it.invitationMode, error = null) }

    fun submit() {
        val form = _state.value
        if (!form.canSubmit) return
        val server = normalize(form.url) ?: return
        if (form.invitationMode) {
            if (form.password.length < MIN_PASSWORD) {
                _state.update { it.copy(error = UiText.res(R.string.login_error_password_short)) }
                return
            }
            if (form.password != form.passwordRepeat) {
                _state.update { it.copy(error = UiText.res(R.string.login_error_password_mismatch)) }
                return
            }
        }
        viewModelScope.launch {
            _state.update { it.copy(busy = true, error = null) }
            try {
                repository.signIn(
                    server = server,
                    username = form.username,
                    password = form.password,
                    invitation = form.invitation.takeIf { form.invitationMode && it.isNotBlank() },
                    displayName = form.displayName.trim().takeIf { form.invitationMode && it.isNotEmpty() },
                    trustFingerprint = form.probe?.fingerprint?.takeIf { form.trust == TrustState.Trusted },
                )
                _state.update { it.copy(busy = false, password = "", passwordRepeat = "") }
            } catch (error: BridgeException) {
                _state.update { it.copy(busy = false, error = UiText.Raw(error.message.orEmpty())) }
            }
        }
    }

    /** `coroom.example.com` → `https://coroom.example.com`; `null` si no hay servidor. */
    private fun normalize(url: String, whileTyping: Boolean = false): String? {
        val trimmed = url.trim().trimEnd('/')
        if (trimmed.isEmpty()) return null
        val withScheme = if ("://" in trimmed) trimmed else "https://$trimmed"
        val authority = withScheme.substringAfter("://").substringBefore('/')
        if (authority.isEmpty()) return null
        val looksComplete = '.' in authority || ':' in authority || authority.startsWith("localhost")
        return withScheme.takeIf { !whileTyping || looksComplete }
    }

    private companion object {
        const val PROBE_DEBOUNCE_MS = 700L
        const val MIN_PASSWORD = 12
    }
}
