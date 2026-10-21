package coroom.android.ui.main

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.animateContentSize
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import coroom.android.R
import coroom.android.bridge.ServerProbe
import coroom.android.bridge.UpdateState
import coroom.android.bridge.displayHost
import coroom.android.bridge.formatFingerprint
import coroom.android.ui.components.CoroomIcon
import coroom.android.ui.components.LabeledField
import coroom.android.ui.components.NoticeCard
import coroom.android.ui.components.PillButton
import coroom.android.ui.text
import coroom.android.ui.theme.CoroomText
import coroom.android.ui.theme.CoroomTheme

/** Primera vez: el servidor del grupo, su certificado y la cuenta (boceto «Servidor»). */
@Composable
fun ServerScreen(
    state: LoginUiState,
    sessionLost: Boolean,
    onUrl: (String) -> Unit,
    onProbeNow: () -> Unit,
    onTrust: (Boolean) -> Unit,
    onUsername: (String) -> Unit,
    onPassword: (String) -> Unit,
    onPasswordRepeat: (String) -> Unit,
    onInvitation: (String) -> Unit,
    onDisplayName: (String) -> Unit,
    onToggleInvitation: () -> Unit,
    onSubmit: () -> Unit,
    update: UpdateState? = null,
    updates: UpdateActions = UpdateActions.None,
) {
    val colors = MaterialTheme.colorScheme
    Surface(color = colors.background, modifier = Modifier.fillMaxSize()) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .safeDrawingPadding()
                .imePadding()
                .verticalScroll(rememberScrollState())
                .padding(start = 20.dp, end = 20.dp, top = 40.dp, bottom = 24.dp)
                .widthIn(max = 560.dp)
                .animateContentSize(),
            verticalArrangement = Arrangement.spacedBy(18.dp),
        ) {
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                CoroomIcon(R.drawable.ic_radio, contentDescription = null, tint = colors.primary, size = 40.dp)
                Text(stringResource(R.string.login_title), style = CoroomText.greeting, color = colors.onSurface)
                Text(stringResource(R.string.login_intro), style = CoroomText.body, color = colors.onSurfaceVariant)
            }

            if (sessionLost) {
                NoticeCard(
                    title = stringResource(R.string.login_session_lost_title),
                    body = stringResource(R.string.login_session_lost_body),
                    icon = R.drawable.ic_shield_alert,
                )
            }

            LabeledFieldServer(state, onUrl, onProbeNow)

            // La app o el servidor quedaron atrás: quién y qué hacer (boceto «versiones distintas»).
            state.probe?.protocolGap?.let { ProtocolBanner(displayHost(state.url), it, update, updates) }

            AnimatedVisibility(
                visible = state.probe != null && state.trust != TrustState.NotNeeded,
                enter = fadeIn() + expandVertically(),
                exit = fadeOut() + shrinkVertically(),
            ) {
                state.probe?.let { TrustCard(it, state.trust, onTrust) }
            }

            LabeledField(
                label = stringResource(R.string.login_username),
                value = state.username,
                onValueChange = onUsername,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Ascii, imeAction = ImeAction.Next),
            )
            LabeledField(
                label = stringResource(if (state.invitationMode) R.string.login_password_new else R.string.login_password),
                value = state.password,
                onValueChange = onPassword,
                supporting = if (state.invitationMode) stringResource(R.string.login_password_hint) else null,
                visualTransformation = PasswordVisualTransformation(),
                keyboardOptions = KeyboardOptions(
                    keyboardType = KeyboardType.Password,
                    imeAction = if (state.invitationMode) ImeAction.Next else ImeAction.Done,
                ),
                keyboardActions = KeyboardActions(onDone = { onSubmit() }),
            )
            AnimatedVisibility(visible = state.invitationMode) {
                Column(verticalArrangement = Arrangement.spacedBy(18.dp)) {
                    LabeledField(
                        label = stringResource(R.string.login_password_repeat),
                        value = state.passwordRepeat,
                        onValueChange = onPasswordRepeat,
                        visualTransformation = PasswordVisualTransformation(),
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, imeAction = ImeAction.Next),
                    )
                    LabeledField(
                        label = stringResource(R.string.login_invitation),
                        value = state.invitation,
                        onValueChange = onInvitation,
                        supporting = stringResource(R.string.login_invitation_hint),
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Ascii, imeAction = ImeAction.Next),
                    )
                    LabeledField(
                        label = stringResource(R.string.login_display_name),
                        value = state.displayName,
                        onValueChange = onDisplayName,
                        supporting = stringResource(R.string.login_display_name_hint),
                        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
                        keyboardActions = KeyboardActions(onDone = { onSubmit() }),
                    )
                }
            }

            state.error?.let { error ->
                NoticeCard(
                    title = stringResource(R.string.login_error_title),
                    body = error.text(),
                    icon = R.drawable.ic_triangle_alert,
                    containerColor = colors.errorContainer,
                    contentColor = colors.onErrorContainer,
                    modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
                )
            }

            Spacer(Modifier.height(4.dp))

            PillButton(
                text = stringResource(
                    when {
                        state.busy && state.invitationMode -> R.string.login_creating
                        state.busy -> R.string.login_signing_in
                        state.invitationMode -> R.string.login_create_account
                        else -> R.string.login_sign_in
                    },
                ),
                onClick = onSubmit,
                enabled = state.canSubmit,
                height = 52.dp,
                modifier = Modifier.fillMaxWidth(),
            )
            TextButton(
                onClick = onToggleInvitation,
                modifier = Modifier.fillMaxWidth().height(44.dp),
            ) {
                Text(
                    stringResource(if (state.invitationMode) R.string.login_have_account else R.string.login_have_invitation),
                    style = CoroomText.button.copy(fontSize = MaterialTheme.typography.titleSmall.fontSize),
                )
            }
        }
    }
}

@Composable
private fun LabeledFieldServer(state: LoginUiState, onUrl: (String) -> Unit, onProbeNow: () -> Unit) {
    val probe = state.probe
    val supporting = when {
        state.probing -> stringResource(R.string.login_probing)
        probe != null && probe.udpReachable == false -> stringResource(R.string.login_probe_no_udp_server)
        probe != null && !probe.http3 -> stringResource(R.string.login_probe_tcp_only)
        probe != null -> stringResource(R.string.login_probe_ok, probe.name, probe.version)
        else -> stringResource(R.string.login_server_hint)
    }
    LabeledField(
        label = stringResource(R.string.login_server),
        value = state.url,
        onValueChange = onUrl,
        placeholder = stringResource(R.string.login_server_placeholder),
        supporting = supporting,
        error = state.probeError?.let { stringResource(R.string.login_probe_failed, it) },
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Go),
        keyboardActions = KeyboardActions(onGo = { onProbeNow() }),
        trailing = when {
            state.probing -> {
                { CircularProgressIndicator(modifier = Modifier.size(20.dp), strokeWidth = 2.dp) }
            }
            probe != null && state.trust != TrustState.Rejected -> {
                {
                    CoroomIcon(
                        R.drawable.ic_shield_check,
                        contentDescription = stringResource(R.string.login_server_ok),
                        tint = CoroomTheme.colors.success,
                    )
                }
            }
            else -> null
        },
    )
}

/** «¿Confías en este certificado?» con la huella para compararla (boceto «Servidor»). */
@Composable
private fun TrustCard(probe: ServerProbe, trust: TrustState, onTrust: (Boolean) -> Unit) {
    val extra = CoroomTheme.colors
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(16.dp))
            .background(extra.warningContainer)
            .padding(14.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            CoroomIcon(R.drawable.ic_shield_alert, contentDescription = null, tint = extra.warningAccent)
            Text(
                text = stringResource(
                    when (trust) {
                        TrustState.Trusted -> R.string.trust_title_trusted
                        TrustState.Rejected -> R.string.trust_title_rejected
                        else -> R.string.trust_title
                    },
                ),
                style = CoroomText.itemTitle.copy(fontWeight = CoroomText.section.fontWeight),
                color = extra.onWarningContainer,
            )
        }
        Text(
            text = stringResource(
                when (trust) {
                    TrustState.Trusted -> R.string.trust_body_trusted
                    TrustState.Rejected -> R.string.trust_body_rejected
                    else -> R.string.trust_body
                },
            ),
            style = CoroomText.body.copy(fontSize = MaterialTheme.typography.bodyMedium.fontSize),
            color = extra.onWarningContainer,
        )
        probe.untrusted?.takeIf { it.isNotBlank() && trust == TrustState.Ask }?.let { reason ->
            Text(
                text = stringResource(R.string.trust_reason, reason),
                style = CoroomText.supporting,
                color = extra.onWarningContainer,
            )
        }
        Text(
            text = formatFingerprint(probe.fingerprint),
            style = CoroomText.mono,
            color = extra.onWarningContainer,
            modifier = Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(10.dp))
                .background(extra.warningCode)
                .padding(horizontal = 10.dp, vertical = 8.dp),
        )
        if (trust == TrustState.Ask) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
            ) {
                TextButton(onClick = { onTrust(false) }) {
                    Text(stringResource(R.string.trust_reject), style = CoroomText.button, color = extra.warningAccent)
                }
                PillButton(
                    text = stringResource(R.string.trust_accept),
                    onClick = { onTrust(true) },
                    containerColor = extra.warningAccent,
                    contentColor = extra.onWarningAccent,
                )
            }
        } else if (trust == TrustState.Rejected) {
            Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                TextButton(onClick = { onTrust(true) }) {
                    Text(stringResource(R.string.trust_accept_anyway), style = CoroomText.button, color = extra.warningAccent)
                }
            }
        }
    }
}

@Preview(name = "Servidor", widthDp = 390, heightDp = 844)
@Composable
private fun ServerScreenPreview() {
    CoroomTheme(dynamicColor = false) {
        ServerScreen(
            state = LoginUiState(
                url = "https://coroom.example.com",
                probe = ServerProbe(
                    name = "coroom-server",
                    version = "1.0.0",
                    http3 = true,
                    udpReachable = true,
                    fingerprint = "3FA10C77B29D41E8056CD32A9F107BC4",
                    untrusted = "autofirmado",
                ),
                trust = TrustState.Ask,
                username = "ana",
                password = "change-me",
            ),
            sessionLost = false,
            onUrl = {}, onProbeNow = {}, onTrust = {}, onUsername = {}, onPassword = {},
            onPasswordRepeat = {}, onInvitation = {}, onDisplayName = {}, onToggleInvitation = {}, onSubmit = {},
        )
    }
}
