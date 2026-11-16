package coroom.android.ui.main

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ButtonColors
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleResumeEffect
import coroom.android.R
import coroom.android.bridge.ProtocolGap
import coroom.android.bridge.UpdateState
import coroom.android.platform.UpdateInstaller
import coroom.android.settings.UPDATE_INTERVALS
import coroom.android.ui.components.CoroomIcon
import coroom.android.ui.components.Notice
import coroom.android.ui.components.PrimaryButton
import coroom.android.ui.components.RowDivider
import coroom.android.ui.components.SecondaryButton
import coroom.android.ui.theme.CoroomText
import coroom.android.ui.theme.CoroomTheme
import coroom.android.ui.theme.Tone
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlin.math.roundToLong

/** Lo que hacen los botones de la versión nueva (Inicio, Ajustes y Servidor). */
data class UpdateActions(
    val checkNow: () -> Unit = {},
    val retry: () -> Unit = {},
    /** «Descargar y actualizar» (versiones distintas): descarga e instala al terminar. */
    val updateNow: () -> Unit = {},
    /** «Instalar ahora»: el APK al instalador del sistema. */
    val install: () -> Unit = {},
    val setCheck: (Boolean) -> Unit = {},
    val setAuto: (Boolean) -> Unit = {},
    val setInterval: (Int) -> Unit = {},
) {
    companion object {
        val None = UpdateActions()
    }
}

/** El error con la primera letra en mayúscula y sin el punto final (lo pone el texto de alrededor). */
private fun sentence(error: String?): String =
    error.orEmpty().trim().trimEnd('.').replaceFirstChar { it.uppercaseChar() }

/**
 * «Instalar ahora»: desactivado con una sala abierta, y TalkBack dice por qué (el texto del aviso
 * ya lo explica).
 */
@Composable
private fun RestartButton(
    text: String,
    update: UpdateState,
    onInstall: () -> Unit,
    colors: ButtonColors = ButtonDefaults.buttonColors(),
) {
    val reason = stringResource(R.string.update_close_room)
    PrimaryButton(
        text = text,
        onClick = onInstall,
        enabled = update.canInstall,
        icon = R.drawable.ic_rotate_ccw,
        colors = colors,
        modifier = if (update.canInstall) Modifier else Modifier.semantics { stateDescription = reason },
    )
}

/** «Novedades», la alternativa junto a «Instalar ahora»: con el color del aviso que la contiene. */
@Composable
private fun NewsButton(update: UpdateState, contentColor: Color) {
    val uri = LocalUriHandler.current
    SecondaryButton(
        text = stringResource(R.string.update_news),
        onClick = { runCatching { uri.openUri(update.url) } },
        contentColor = contentColor,
    )
}

@Composable
private fun SeeButton(update: UpdateState, colors: ButtonColors = ButtonDefaults.buttonColors()) {
    val uri = LocalUriHandler.current
    PrimaryButton(
        text = stringResource(R.string.update_see),
        onClick = { runCatching { uri.openUri(update.url) } },
        icon = R.drawable.ic_external_link,
        colors = colors,
    )
}

/**
 * El aviso de Inicio: lista para instalar, no se pudo descargar o, sin actualización
 * automática, que hay una versión nueva. Mientras descarga no enseña nada (está en Ajustes).
 */
@Composable
fun UpdateBanner(update: UpdateState?, actions: UpdateActions) {
    if (update == null || !update.enabled) return
    val version = update.version ?: return
    val colors = MaterialTheme.colorScheme
    when (update.stage) {
        "ready" -> Notice(
            icon = R.drawable.ic_download,
            title = stringResource(R.string.update_ready_title, version),
            body = if (update.roomOpen) {
                stringResource(R.string.update_ready_room)
            } else {
                stringResource(R.string.update_ready_body, update.current)
            },
            tone = Tone.Info,
            actions = {
                NewsButton(update, contentColor = colors.onPrimaryContainer)
                RestartButton(stringResource(R.string.update_restart), update, actions.install)
            },
        )
        "failed" -> Notice(
            icon = R.drawable.ic_circle_alert,
            title = stringResource(R.string.update_failed_title, version),
            body = stringResource(R.string.update_failed_body, sentence(update.error)),
            tone = Tone.Error,
            actions = {
                SecondaryButton(
                    text = stringResource(R.string.action_retry),
                    onClick = actions.retry,
                    contentColor = colors.onErrorContainer,
                )
            },
        )
        "notice" -> Notice(
            icon = R.drawable.ic_info,
            title = stringResource(R.string.update_available, version, update.current),
            body = "",
            tone = Tone.Info,
            actions = { SeeButton(update) },
        )
        else -> Unit
    }
}

/** El aviso de versiones distintas con el servidor `host`. */
@Composable
fun ProtocolBanner(host: String, gap: ProtocolGap, update: UpdateState?, actions: UpdateActions) {
    if (!gap.clientOlder) {
        Notice(
            icon = R.drawable.ic_server,
            title = stringResource(R.string.protocol_server_older_title),
            body = stringResource(R.string.protocol_server_older_body, host, gap.server, gap.client),
            tone = Tone.Error,
        )
        return
    }
    val known = update?.takeIf { it.version != null }
    Notice(
        icon = R.drawable.ic_triangle_alert,
        title = stringResource(R.string.protocol_client_older_title),
        body = stringResource(R.string.protocol_client_older_body, host, gap.server, gap.client),
        tone = Tone.Warning,
        actions = known?.let { { ProtocolUpdateAction(it, actions) } },
    )
}

/** Lo que se puede hacer con la versión nueva desde el aviso de versiones distintas. */
@Composable
private fun ProtocolUpdateAction(known: UpdateState, actions: UpdateActions) {
    val colors = MaterialTheme.colorScheme
    val extra = CoroomTheme.colors
    // Sobre el ámbar del aviso, los botones rellenos van con su acento, no con el primario.
    val warningButton = ButtonDefaults.buttonColors(
        containerColor = extra.warningAccent,
        contentColor = extra.onWarningAccent,
    )
    when {
        !known.auto -> SeeButton(known, colors = warningButton)
        known.stage == "ready" -> RestartButton(
            stringResource(R.string.protocol_update_restart),
            known,
            actions.install,
            colors = warningButton,
        )
        known.stage == "downloading" || known.stage == "paused" -> Row(
            modifier = Modifier.heightIn(min = 40.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            LinearProgressIndicator(
                progress = { known.percent / 100f },
                modifier = Modifier.width(140.dp),
                color = if (known.stage == "paused") colors.outline else extra.warningAccent,
            )
            Text(
                stringResource(R.string.protocol_percent, known.percent),
                style = CoroomText.figure,
                color = extra.onWarningContainer,
            )
        }
        else -> PrimaryButton(
            text = stringResource(R.string.protocol_download_update),
            onClick = actions.updateNow,
            enabled = !known.roomOpen,
            icon = R.drawable.ic_download,
            colors = warningButton,
        )
    }
}

/** «hace 3 h», como en el escritorio. */
@Composable
private fun ago(lastCheckSeconds: Long): String {
    val seconds = (System.currentTimeMillis() / 1000 - lastCheckSeconds).coerceAtLeast(0)
    return when {
        seconds < 60 -> stringResource(R.string.time_now)
        seconds < 3_600 -> stringResource(R.string.time_minutes_ago, (seconds / 60).toInt())
        seconds < 86_400 -> stringResource(R.string.time_hours_ago, (seconds / 3_600).toInt())
        else -> stringResource(R.string.time_days_ago, (seconds / 86_400).toInt())
    }
}

/** Ajustes → Actualizaciones: la versión nueva, «Actualizar automáticamente» y la búsqueda. */
@Composable
fun UpdateSettings(update: UpdateState?, intervalHours: Int, actions: UpdateActions) {
    val colors = MaterialTheme.colorScheme
    val state = update ?: UpdateState()
    val version = state.version.orEmpty()
    Column(
        modifier = Modifier.fillMaxWidth().padding(start = 16.dp, end = 12.dp, top = 12.dp, bottom = 8.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text(stringResource(R.string.update_status_title), style = CoroomText.itemBody, color = colors.onSurface)
        val text = when {
            state.checking -> stringResource(R.string.update_checking)
            state.stage == "downloading" -> state.totalBytes?.let {
                stringResource(R.string.update_downloading, version, state.percent, (it / 1e6).roundToLong().toInt())
            } ?: stringResource(R.string.update_downloading_no_size, version, state.percent)
            state.stage == "paused" -> stringResource(
                if (state.waiting == "metered") R.string.update_paused_metered else R.string.update_paused,
                version,
                state.percent,
            )
            state.stage == "ready" -> stringResource(R.string.update_ready_about, version)
            state.stage == "failed" -> stringResource(R.string.update_failed_about, version, state.error.orEmpty().trimEnd('.'))
            state.stage == "notice" && state.version != null ->
                stringResource(R.string.update_notice_about, version, state.current)
            state.checkError != null -> stringResource(R.string.update_check_failed, state.checkError.trimEnd('.'))
            state.lastCheck != null -> stringResource(R.string.update_up_to_date, ago(state.lastCheck))
            else -> stringResource(R.string.update_never_checked)
        }
        Text(text, style = CoroomText.supporting, color = colors.onSurfaceVariant)
        Box(modifier = Modifier.fillMaxWidth(), contentAlignment = Alignment.CenterEnd) {
            when {
                state.checking -> SecondaryButton(
                    text = stringResource(R.string.update_checking_button),
                    onClick = {},
                    enabled = false,
                )
                state.stage == "downloading" || state.stage == "paused" -> LinearProgressIndicator(
                    progress = { state.percent / 100f },
                    modifier = Modifier.padding(vertical = 18.dp).width(160.dp),
                    color = if (state.stage == "paused") colors.outline else colors.primary,
                )
                state.stage == "ready" -> Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    NewsButton(state, contentColor = colors.primary)
                    RestartButton(stringResource(R.string.update_restart), state, actions.install)
                }
                state.stage == "failed" -> SecondaryButton(
                    text = stringResource(R.string.action_retry),
                    onClick = actions.retry,
                )
                state.stage == "notice" && state.version != null -> SeeButton(state)
                else -> SecondaryButton(
                    text = stringResource(R.string.update_check_now),
                    onClick = actions.checkNow,
                    enabled = state.enabled,
                )
            }
        }
    }
    RowDivider()
    ToggleRow(
        title = stringResource(R.string.update_auto),
        body = stringResource(R.string.update_auto_body),
        checked = state.auto && state.enabled,
        enabled = state.enabled,
        onChange = actions.setAuto,
    )
    RowDivider()
    ToggleRow(
        title = stringResource(R.string.update_check),
        body = stringResource(R.string.update_check_body),
        checked = state.enabled,
        enabled = true,
        onChange = actions.setCheck,
    ) {
        if (state.enabled) IntervalChoice(intervalHours, actions.setInterval)
    }
}

@Composable
private fun ToggleRow(
    title: String,
    body: String,
    checked: Boolean,
    enabled: Boolean,
    onChange: (Boolean) -> Unit,
    below: @Composable () -> Unit = {},
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .toggleable(value = checked, enabled = enabled, onValueChange = onChange, role = Role.Switch)
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(16.dp)) {
            Column(modifier = Modifier.weight(1f)) {
                Text(title, style = CoroomText.itemBody, color = MaterialTheme.colorScheme.onSurface)
                Text(body, style = CoroomText.supporting, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            Switch(checked = checked, onCheckedChange = null, enabled = enabled)
        }
        below()
    }
}

/** «Una vez al día», con la flecha del desplegable. */
@Composable
private fun IntervalChoice(hours: Int, onChoose: (Int) -> Unit) {
    var open by remember { mutableStateOf(false) }
    val label = @Composable { value: Int ->
        stringResource(if (value == 24) R.string.update_daily else R.string.update_weekly)
    }
    Box {
        SecondaryButton(text = label(hours), onClick = { open = true }, trailingIcon = R.drawable.ic_chevron_down)
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            UPDATE_INTERVALS.forEach { value ->
                DropdownMenuItem(
                    text = { Text(label(value)) },
                    onClick = {
                        open = false
                        onChoose(value)
                    },
                )
            }
        }
    }
}

/**
 * Da al instalador del sistema el APK que espera (`pendingInstall`). Sin permiso para instalar
 * apps, primero explica por qué hace falta y lleva a «Instalar apps desconocidas»; al volver con
 * el permiso, instala.
 */
@Composable
fun UpdateInstallHost(pendingInstall: String?, onHandled: () -> Unit, onFailed: (String?) -> Unit) {
    val context = LocalContext.current
    var allowed by remember { mutableStateOf(UpdateInstaller.canInstall(context)) }
    // Se mandó a «Instalar apps desconocidas»: al volver se instala o, sin permiso, se deja.
    var sentToSettings by rememberSaveable { mutableStateOf(false) }
    val pending by rememberUpdatedState(pendingInstall)
    LifecycleResumeEffect(Unit) {
        allowed = UpdateInstaller.canInstall(context)
        if (!allowed && sentToSettings && pending != null) {
            sentToSettings = false
            onHandled()
        }
        onPauseOrDispose { }
    }
    LaunchedEffect(pendingInstall, allowed) {
        val path = pendingInstall ?: return@LaunchedEffect
        if (!allowed) return@LaunchedEffect
        sentToSettings = false
        val result = withContext(Dispatchers.IO) { runCatching { UpdateInstaller.install(context, path) } }
        onHandled()
        result.exceptionOrNull()?.let { onFailed(it.message) }
    }
    if (pendingInstall != null && !allowed && !sentToSettings) {
        AlertDialog(
            onDismissRequest = onHandled,
            icon = { CoroomIcon(R.drawable.ic_shield_check, contentDescription = null, tint = MaterialTheme.colorScheme.primary) },
            title = { Text(stringResource(R.string.update_unknown_sources_title)) },
            text = { Text(stringResource(R.string.update_unknown_sources_body)) },
            confirmButton = {
                TextButton(onClick = {
                    sentToSettings = true
                    if (runCatching { context.startActivity(UpdateInstaller.unknownSourcesIntent(context)) }.isFailure) {
                        sentToSettings = false
                        onHandled()
                    }
                }) { Text(stringResource(R.string.update_unknown_sources_open)) }
            },
            dismissButton = {
                TextButton(onClick = onHandled) { Text(stringResource(R.string.update_unknown_sources_later)) }
            },
        )
    }
}
