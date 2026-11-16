package coroom.android.ui.components

import androidx.annotation.DrawableRes
import androidx.compose.animation.animateColorAsState
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonColors
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
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
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import coroom.android.R
import coroom.android.ui.theme.CoroomText
import coroom.android.ui.theme.CoroomTheme
import coroom.android.ui.theme.Tone
import coroom.android.ui.theme.colors

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
            .padding(horizontal = 4.dp)
            .semantics { heading() },
    )
}

/**
 * Tarjeta tonal: una superficie de contenedor de Material con un filete fino, sin sombra. Es el
 * bloque de todas las pantallas; nunca va una dentro de otra.
 */
@Composable
fun TonalCard(
    modifier: Modifier = Modifier,
    containerColor: Color = MaterialTheme.colorScheme.surfaceContainerLow,
    contentColor: Color = MaterialTheme.colorScheme.onSurface,
    outlined: Boolean = true,
    contentPadding: PaddingValues = PaddingValues(16.dp),
    content: @Composable ColumnScope.() -> Unit,
) {
    Surface(
        modifier = modifier.fillMaxWidth(),
        shape = MaterialTheme.shapes.large,
        color = containerColor,
        contentColor = contentColor,
        border = if (outlined) BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant) else null,
    ) {
        Column(modifier = Modifier.padding(contentPadding), content = content)
    }
}

/** Lista agrupada: las filas de un grupo en una sola tarjeta tonal, con una línea entre ellas. */
@Composable
fun ListCard(
    modifier: Modifier = Modifier,
    content: @Composable ColumnScope.() -> Unit,
) {
    TonalCard(modifier = modifier, contentPadding = PaddingValues(0.dp), content = content)
}

/** La línea entre dos filas de una [ListCard]. */
@Composable
fun RowDivider() {
    HorizontalDivider(
        modifier = Modifier.padding(horizontal = 16.dp),
        thickness = 1.dp,
        color = MaterialTheme.colorScheme.outlineVariant,
    )
}

/**
 * Una fila de persona: su butaca, nombre, segunda línea y la acción a la derecha. `user` y
 * `avatar` (su versión) ponen la imagen en lugar de la inicial. `lamp` enciende la lámpara de la
 * butaca y `supportingLamp` pone un punto delante de la segunda línea (el piloto de «en vivo»).
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
    lamp: Color? = null,
    supportingLamp: Color? = null,
    trailing: @Composable RowScope.() -> Unit = {},
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .heightIn(min = 72.dp)
            .padding(start = 16.dp, end = 12.dp, top = 12.dp, bottom = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Avatar(name = name, seed = seed, offline = offline, user = user, avatar = avatar, lamp = lamp)
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = name,
                style = CoroomText.itemTitle,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                if (supportingLamp != null) Lamp(supportingLamp)
                Text(
                    text = supporting,
                    style = CoroomText.supporting,
                    color = supportingColor,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        trailing()
    }
}

/**
 * Los botones, con una sola jerarquía en toda la app:
 * - [PrimaryButton] (relleno): la acción principal de la pantalla o del aviso; una por bloque.
 * - [TonalButton] (tonal): la acción de una fila o de un estado vacío.
 * - [SecondaryButton] (con borde): la alternativa junto a una de las anteriores, y lo destructivo.
 * - `TextButton` de Material: lo de menor peso y los botones de los diálogos.
 */
@Composable
fun PrimaryButton(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    @DrawableRes icon: Int? = null,
    colors: ButtonColors = ButtonDefaults.buttonColors(),
) {
    Button(onClick = onClick, modifier = modifier, enabled = enabled, colors = colors) {
        ButtonContent(text = text, icon = icon, trailingIcon = null)
    }
}

/** Botón tonal: la acción de una fila («Pedir entrar», «Aceptar»). Ver [PrimaryButton]. */
@Composable
fun TonalButton(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    @DrawableRes icon: Int? = null,
) {
    FilledTonalButton(onClick = onClick, modifier = modifier, enabled = enabled) {
        ButtonContent(text = text, icon = icon, trailingIcon = null)
    }
}

/**
 * Botón con borde: la alternativa («Rechazar», «Cancelar», «Novedades»). `contentColor` lo pone en
 * rojo para lo destructivo o en el color del aviso que lo contiene. Ver [PrimaryButton].
 */
@Composable
fun SecondaryButton(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    @DrawableRes icon: Int? = null,
    @DrawableRes trailingIcon: Int? = null,
    contentColor: Color = MaterialTheme.colorScheme.primary,
) {
    OutlinedButton(
        onClick = onClick,
        modifier = modifier,
        enabled = enabled,
        colors = ButtonDefaults.outlinedButtonColors(contentColor = contentColor),
    ) {
        ButtonContent(text = text, icon = icon, trailingIcon = trailingIcon)
    }
}

@Composable
private fun ButtonContent(text: String, @DrawableRes icon: Int?, @DrawableRes trailingIcon: Int?) {
    if (icon != null) {
        Icon(painterResource(icon), contentDescription = null, modifier = Modifier.size(ButtonDefaults.IconSize))
        Spacer(Modifier.width(ButtonDefaults.IconSpacing))
    }
    Text(text = text, maxLines = 1)
    if (trailingIcon != null) {
        Spacer(Modifier.width(ButtonDefaults.IconSpacing))
        Icon(painterResource(trailingIcon), contentDescription = null, modifier = Modifier.size(ButtonDefaults.IconSize))
    }
}

/** La lámpara: el punto de estado (conectado, relay, en vivo, sin conexión), con el cambio de color animado. */
@Composable
fun Lamp(color: Color, modifier: Modifier = Modifier, size: Dp = 8.dp) {
    val animated by animateColorAsState(targetValue = color, label = "lamp")
    Box(modifier = modifier.size(size).background(animated, CircleShape))
}

/** Píldora de estado: la lámpara del tono y su texto («directa», «por el relay del servidor»). */
@Composable
fun StatusPill(text: String, tone: Tone, modifier: Modifier = Modifier) {
    val colors = tone.colors()
    Row(
        modifier = modifier
            .clip(CircleShape)
            .background(colors.container)
            .padding(horizontal = 10.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Lamp(colors.lamp)
        Text(text = text, style = CoroomText.pill, color = colors.content, maxLines = 1)
    }
}

/**
 * Un aviso, un estado vacío o un error explicado: icono, título y texto con el color de su tono, y
 * sus acciones debajo, a la derecha. Nunca una pantalla en blanco.
 */
@Composable
fun Notice(
    title: String,
    body: String,
    modifier: Modifier = Modifier,
    @DrawableRes icon: Int = R.drawable.ic_info,
    tone: Tone = Tone.Neutral,
    actions: (@Composable () -> Unit)? = null,
) {
    val colors = tone.colors()
    Surface(
        modifier = modifier.fillMaxWidth(),
        shape = MaterialTheme.shapes.large,
        color = colors.container,
        contentColor = colors.content,
    ) {
        Column(
            modifier = Modifier.padding(start = 16.dp, end = 16.dp, top = 14.dp, bottom = 14.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                CoroomIcon(icon, contentDescription = null, tint = colors.content)
                Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    Text(text = title, style = CoroomText.itemTitle, color = colors.content)
                    if (body.isNotEmpty()) Text(text = body, style = CoroomText.supporting, color = colors.supporting)
                }
            }
            if (actions != null) {
                FlowRow(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                ) { actions() }
            }
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

/** El color de la lámpara según el estado de la conexión con el servidor. */
@Composable
fun serverDotColor(online: Boolean?): Color = when (online) {
    true -> CoroomTheme.colors.successDot
    false -> MaterialTheme.colorScheme.error
    null -> MaterialTheme.colorScheme.outline
}
