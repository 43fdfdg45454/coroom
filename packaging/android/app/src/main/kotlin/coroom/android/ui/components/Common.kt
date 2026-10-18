package coroom.android.ui.components

import androidx.annotation.DrawableRes
import androidx.compose.animation.animateColorAsState
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import coroom.android.R
import coroom.android.ui.theme.CoroomText
import coroom.android.ui.theme.CoroomTheme

/** Un icono de Lucide de `res/drawable`, teñido. */
@Composable
fun CoroomIcon(
    @DrawableRes icon: Int,
    contentDescription: String?,
    modifier: Modifier = Modifier,
    tint: Color = MaterialTheme.colorScheme.onSurfaceVariant,
    size: Dp = 22.dp,
) {
    Icon(
        painter = painterResource(icon),
        contentDescription = contentDescription,
        modifier = modifier.size(size),
        tint = tint,
    )
}

/** Cabecera de sección («Invitaciones», «Amigos conectados»). */
@Composable
fun SectionHeader(
    text: String,
    modifier: Modifier = Modifier,
    color: Color = MaterialTheme.colorScheme.onSurfaceVariant,
) {
    Text(
        text = text,
        style = CoroomText.section,
        color = color,
        modifier = modifier
            .padding(start = 4.dp, end = 4.dp, top = 4.dp, bottom = 4.dp)
            .semantics { heading() },
    )
}

/**
 * Tarjeta de lista de los bocetos: fondo de superficie más clara, borde fino y radio 16, con una
 * línea entre filas.
 */
@Composable
fun ListCard(
    modifier: Modifier = Modifier,
    content: @Composable ColumnScope.() -> Unit,
) {
    val shape = RoundedCornerShape(16.dp)
    Column(
        modifier = modifier
            .fillMaxWidth()
            .clip(shape)
            .background(MaterialTheme.colorScheme.surfaceContainerLowest)
            .border(1.dp, MaterialTheme.colorScheme.outlineVariant, shape),
        content = content,
    )
}

/** La línea entre dos filas de una [ListCard]. */
@Composable
fun RowDivider() {
    HorizontalDivider(thickness = 1.dp, color = MaterialTheme.colorScheme.surfaceContainerHigh)
}

/**
 * Una fila de persona: avatar, nombre, segunda línea y la acción a la derecha. `user` y `avatar`
 * (su versión) ponen la imagen del avatar en lugar de la inicial.
 */
@Composable
fun PersonRow(
    name: String,
    seed: Long,
    supporting: String,
    modifier: Modifier = Modifier,
    supportingColor: Color = MaterialTheme.colorScheme.onSurfaceVariant,
    offline: Boolean = false,
    user: Long? = null,
    avatar: Long? = null,
    trailing: @Composable RowScope.() -> Unit = {},
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .heightIn(min = 64.dp)
            .padding(start = 14.dp, end = 12.dp, top = 12.dp, bottom = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Avatar(name = name, seed = seed, offline = offline, user = user, avatar = avatar)
        Column(modifier = Modifier.weight(1f)) {
            Text(text = name, style = CoroomText.itemTitle, color = MaterialTheme.colorScheme.onSurface, maxLines = 1)
            Text(text = supporting, style = CoroomText.supporting, color = supportingColor, maxLines = 2)
        }
        trailing()
    }
}

/** Botón relleno en forma de píldora (alto 40–52 según el boceto). */
@Composable
fun PillButton(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    height: Dp = 40.dp,
    enabled: Boolean = true,
    @DrawableRes icon: Int? = null,
    containerColor: Color = MaterialTheme.colorScheme.primary,
    contentColor: Color = MaterialTheme.colorScheme.onPrimary,
) {
    Button(
        onClick = onClick,
        modifier = modifier.height(height),
        enabled = enabled,
        shape = RoundedCornerShape(height / 2),
        contentPadding = PaddingValues(horizontal = 18.dp),
        colors = ButtonDefaults.buttonColors(containerColor = containerColor, contentColor = contentColor),
    ) {
        if (icon != null) {
            Icon(painterResource(icon), contentDescription = null, modifier = Modifier.size(18.dp))
            Box(Modifier.size(8.dp))
        }
        Text(text = text, style = CoroomText.button)
    }
}

/** Botón con borde en forma de píldora («Pedir entrar», «Rechazar», «Cancelar»). */
@Composable
fun PillOutlinedButton(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    height: Dp = 40.dp,
    enabled: Boolean = true,
    contentColor: Color = MaterialTheme.colorScheme.primary,
) {
    OutlinedButton(
        onClick = onClick,
        modifier = modifier.height(height),
        enabled = enabled,
        shape = RoundedCornerShape(height / 2),
        contentPadding = PaddingValues(horizontal = 14.dp),
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outline),
        colors = ButtonDefaults.outlinedButtonColors(contentColor = contentColor),
    ) {
        Text(text = text, style = CoroomText.button)
    }
}

/** El punto de estado (conectado, conectando, sin conexión), con el cambio de color animado. */
@Composable
fun StatusDot(color: Color, modifier: Modifier = Modifier, size: Dp = 8.dp) {
    val animated by animateColorAsState(targetValue = color, label = "status-dot")
    Box(modifier = modifier.size(size).background(animated, CircleShape))
}

/**
 * Un estado vacío o un error explicado: icono, título y texto, con una acción opcional. Nunca
 * una pantalla en blanco.
 */
@Composable
fun NoticeCard(
    title: String,
    body: String,
    modifier: Modifier = Modifier,
    @DrawableRes icon: Int = R.drawable.ic_info,
    containerColor: Color = MaterialTheme.colorScheme.surfaceContainerLow,
    contentColor: Color = MaterialTheme.colorScheme.onSurfaceVariant,
    action: (@Composable () -> Unit)? = null,
) {
    Column(
        modifier = modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(16.dp))
            .background(containerColor)
            .padding(horizontal = 16.dp, vertical = 14.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            CoroomIcon(icon, contentDescription = null, tint = contentColor, size = 20.dp)
            Text(text = title, style = CoroomText.itemTitle, color = contentColor)
        }
        Text(text = body, style = CoroomText.supporting, color = contentColor)
        if (action != null) {
            Box(modifier = Modifier.fillMaxWidth(), contentAlignment = Alignment.CenterEnd) { action() }
        }
    }
}

/** «ahora», «hace 3 min», «hace 2 h». */
@Composable
fun relativeTime(atMs: Long, nowMs: Long): String {
    val minutes = ((nowMs - atMs).coerceAtLeast(0) / 60_000).toInt()
    return when {
        minutes < 1 -> stringResource(R.string.time_now)
        minutes < 60 -> stringResource(R.string.time_minutes_ago, minutes)
        else -> stringResource(R.string.time_hours_ago, minutes / 60)
    }
}

/** El color del punto según el estado de la conexión con el servidor. */
@Composable
fun serverDotColor(online: Boolean?): Color = when (online) {
    true -> CoroomTheme.colors.successDot
    false -> MaterialTheme.colorScheme.error
    null -> MaterialTheme.colorScheme.outline
}
