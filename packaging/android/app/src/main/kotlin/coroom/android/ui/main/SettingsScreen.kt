package coroom.android.ui.main

import android.content.Intent
import android.provider.Settings as AndroidSettings
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Slider
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalInspectionMode
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.core.net.toUri
import coroom.android.BuildConfig
import coroom.android.R
import coroom.android.bridge.TouchMode
import coroom.android.ui.components.Avatar
import coroom.android.ui.components.CoroomIcon
import coroom.android.ui.components.ListCard
import coroom.android.ui.components.PillOutlinedButton
import coroom.android.ui.components.RowDivider
import coroom.android.ui.components.SectionHeader
import coroom.android.ui.preview.FakeData
import coroom.android.ui.theme.CoroomText
import coroom.android.ui.theme.CoroomTheme
import kotlin.math.roundToInt

/** Pestaña Ajustes (boceto «Ajustes»). */
@Composable
fun SettingsScreen(
    state: MainUiState,
    contentPadding: PaddingValues,
    onTouchMode: (TouchMode) -> Unit,
    onGameVolume: (Int) -> Unit,
    onJoinMuted: (Boolean) -> Unit,
    onNotify: (Boolean) -> Unit,
    /** Activar los avisos: pide el permiso de notificaciones si hace falta (lo hace la actividad). */
    onEnableNotifications: () -> Unit,
    onLogout: () -> Unit,
    /** «Compartir registro»: los ficheros del registro por la hoja de compartir del sistema. */
    onShareLog: () -> Unit,
) {
    val colors = MaterialTheme.colorScheme
    val context = LocalContext.current
    val settings = state.settings
    var confirmLogout by rememberSaveable { mutableStateOf(false) }
    var showLicenses by rememberSaveable { mutableStateOf(false) }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(contentPadding),
    ) {
        Text(
            text = stringResource(R.string.settings_title),
            style = CoroomText.screenTitle,
            color = colors.onSurface,
            modifier = Modifier.padding(start = 20.dp, end = 20.dp, top = 20.dp, bottom = 8.dp),
        )
        Column(
            modifier = Modifier.padding(start = 12.dp, end = 12.dp, top = 4.dp, bottom = 12.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            SettingsHeader(stringResource(R.string.settings_touch))
            ListCard(modifier = Modifier.selectableGroup()) {
                TouchOption(
                    title = stringResource(R.string.touch_trackpad),
                    body = stringResource(R.string.touch_trackpad_body),
                    selected = settings.touchMode == TouchMode.Trackpad,
                    onSelect = { onTouchMode(TouchMode.Trackpad) },
                )
                RowDivider()
                TouchOption(
                    title = stringResource(R.string.touch_direct),
                    body = stringResource(R.string.touch_direct_body),
                    selected = settings.touchMode == TouchMode.Direct,
                    onSelect = { onTouchMode(TouchMode.Direct) },
                )
            }

            SettingsHeader(stringResource(R.string.settings_pads))
            ListCard {
                val names = state.bridgePads?.map { it.deviceId to it.name } ?: state.pads.map { it.deviceId to it.name }
                if (names.isEmpty()) {
                    SettingsRow(
                        title = stringResource(R.string.pads_empty_title),
                        body = stringResource(R.string.pads_empty_body),
                        icon = R.drawable.ic_gamepad_2,
                    )
                }
                names.forEachIndexed { index, (deviceId, name) ->
                    if (index > 0) RowDivider()
                    val vibrates = state.pads.firstOrNull { it.deviceId == deviceId }?.canVibrate ?: true
                    SettingsRow(
                        title = name.ifBlank { stringResource(R.string.pads_unnamed) },
                        body = stringResource(if (vibrates) R.string.pads_as_xbox else R.string.pads_as_xbox_no_rumble),
                        icon = R.drawable.ic_gamepad_2,
                    )
                }
            }

            SettingsHeader(stringResource(R.string.settings_video_audio))
            ListCard {
                // La calidad la decide Rust según el enlace; aún no hay opciones que elegir aquí.
                SettingsRow(
                    title = stringResource(R.string.settings_quality),
                    body = stringResource(R.string.settings_quality_auto),
                )
                RowDivider()
                VolumeRow(settings.gameVolume, onGameVolume)
                RowDivider()
                SwitchRow(
                    title = stringResource(R.string.settings_join_muted),
                    body = null,
                    checked = settings.joinMuted,
                    onChange = onJoinMuted,
                )
            }

            SettingsHeader(stringResource(R.string.settings_notices))
            ListCard {
                SwitchRow(
                    title = stringResource(R.string.settings_notify),
                    body = stringResource(R.string.settings_notify_body),
                    checked = settings.notifyInvitations,
                    onChange = { on -> if (on) onEnableNotifications() else onNotify(false) },
                )
            }

            SettingsHeader(stringResource(R.string.settings_account))
            state.account?.let { account ->
                ListCard {
                    Row(
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 12.dp),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        Avatar(name = account.shownName, seed = account.userId, user = account.userId, avatar = account.avatar)
                        Column(modifier = Modifier.weight(1f)) {
                            Text(account.shownName, style = CoroomText.itemBody, color = colors.onSurface)
                            Text(
                                stringResource(R.string.settings_account_detail, account.username, account.serverHost),
                                style = CoroomText.supporting,
                                color = colors.onSurfaceVariant,
                            )
                        }
                        PillOutlinedButton(
                            text = stringResource(R.string.settings_logout),
                            onClick = { confirmLogout = true },
                            contentColor = colors.error,
                        )
                    }
                }
            }

            SettingsHeader(stringResource(R.string.settings_help))
            ListCard {
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable(role = Role.Button, onClick = onShareLog)
                        .heightIn(min = 56.dp)
                        .padding(horizontal = 14.dp, vertical = 12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(14.dp),
                ) {
                    CoroomIcon(R.drawable.ic_file_text, contentDescription = null)
                    Column(modifier = Modifier.weight(1f)) {
                        Text(stringResource(R.string.settings_log), style = CoroomText.itemBody, color = colors.onSurface)
                        Text(
                            stringResource(R.string.settings_log_body),
                            style = CoroomText.supporting,
                            color = colors.onSurfaceVariant,
                        )
                    }
                }
            }

            Row(
                modifier = Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = stringResource(R.string.settings_version, BuildConfig.VERSION_NAME),
                    style = CoroomText.supporting,
                    color = colors.onSurfaceVariant,
                    modifier = Modifier.weight(1f),
                )
                TextButton(onClick = { showLicenses = true }) { Text(stringResource(R.string.settings_licenses)) }
                TextButton(onClick = {
                    context.startActivity(
                        Intent(AndroidSettings.ACTION_APPLICATION_DETAILS_SETTINGS, "package:${context.packageName}".toUri()),
                    )
                }) { Text(stringResource(R.string.settings_system)) }
            }
        }
    }

    if (confirmLogout) {
        AlertDialog(
            onDismissRequest = { confirmLogout = false },
            title = { Text(stringResource(R.string.logout_title)) },
            text = { Text(stringResource(R.string.logout_body)) },
            confirmButton = {
                TextButton(onClick = {
                    confirmLogout = false
                    onLogout()
                }) { Text(stringResource(R.string.settings_logout), color = colors.error) }
            },
            dismissButton = { TextButton(onClick = { confirmLogout = false }) { Text(stringResource(R.string.action_cancel)) } },
        )
    }
    if (showLicenses) LicensesDialog(onDismiss = { showLicenses = false })
}

@Composable
private fun SettingsHeader(text: String) {
    SectionHeader(
        text = text,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 4.dp, top = 6.dp),
    )
}

@Composable
private fun SettingsRow(title: String, body: String?, icon: Int? = null) {
    Row(
        modifier = Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(horizontal = 14.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        if (icon != null) CoroomIcon(icon, contentDescription = null)
        Column(modifier = Modifier.weight(1f)) {
            Text(title, style = CoroomText.itemBody, color = MaterialTheme.colorScheme.onSurface)
            if (body != null) Text(body, style = CoroomText.supporting, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
private fun TouchOption(title: String, body: String, selected: Boolean, onSelect: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .selectable(selected = selected, onClick = onSelect, role = Role.RadioButton)
            .padding(horizontal = 14.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        RadioButton(selected = selected, onClick = null, modifier = Modifier.size(20.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(title, style = CoroomText.itemBody, color = MaterialTheme.colorScheme.onSurface)
            Text(body, style = CoroomText.supporting, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
private fun SwitchRow(title: String, body: String?, checked: Boolean, onChange: (Boolean) -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .toggleable(value = checked, onValueChange = onChange, role = Role.Switch)
            .padding(horizontal = 14.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(title, style = CoroomText.itemBody, color = MaterialTheme.colorScheme.onSurface)
            if (body != null) Text(body, style = CoroomText.supporting, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        Switch(checked = checked, onCheckedChange = null)
    }
}

@Composable
private fun VolumeRow(volume: Int, onVolume: (Int) -> Unit) {
    // El deslizador va suelto mientras se arrastra; el valor se guarda (y llega a Rust) al soltar.
    var dragging by remember { mutableStateOf(false) }
    var value by remember { mutableFloatStateOf(volume.toFloat()) }
    if (!dragging && value.roundToInt() != volume) value = volume.toFloat()
    Column(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 12.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Text(
            stringResource(R.string.settings_volume, value.roundToInt()),
            style = CoroomText.itemBody,
            color = MaterialTheme.colorScheme.onSurface,
        )
        Slider(
            value = value,
            onValueChange = {
                dragging = true
                value = (it / 5f).roundToInt() * 5f
            },
            onValueChangeFinished = {
                dragging = false
                onVolume(value.roundToInt())
            },
            valueRange = 0f..200f,
        )
    }
}

@Composable
private fun LicensesDialog(onDismiss: () -> Unit) {
    val context = LocalContext.current
    val inPreview = LocalInspectionMode.current
    val lucide = remember {
        if (inPreview) "" else context.resources.openRawResource(R.raw.lucide_license).bufferedReader().use { it.readText() }
    }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.licenses_title)) },
        text = {
            Column(modifier = Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text(stringResource(R.string.licenses_app), style = CoroomText.supporting)
                Text(stringResource(R.string.licenses_lucide), style = CoroomText.section)
                Box(
                    Modifier
                        .background(MaterialTheme.colorScheme.surfaceContainerHigh)
                        .padding(8.dp),
                ) {
                    Text(lucide, style = CoroomText.mono)
                }
            }
        },
        confirmButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.action_close)) } },
    )
}

@Preview(name = "Ajustes", widthDp = 390, heightDp = 844)
@Composable
private fun SettingsPreview() {
    CoroomTheme(dynamicColor = false) {
        Box(Modifier.background(MaterialTheme.colorScheme.background)) {
            SettingsScreen(FakeData.mainState, PaddingValues(), {}, {}, {}, {}, {}, {}, {})
        }
    }
}
