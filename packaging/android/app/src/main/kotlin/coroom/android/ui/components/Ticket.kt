package coroom.android.ui.components

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.OffsetMapping
import androidx.compose.ui.text.input.TransformedText
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import coroom.android.ui.theme.CoroomText

/**
 * Una entrada: lo que dice (quién invita, a qué sala) a la izquierda y, tras una línea de puntos,
 * el talón con su acción («Entrar»). El talón entero se puede pulsar.
 */
@Composable
fun Ticket(
    stubLabel: String,
    onStub: () -> Unit,
    modifier: Modifier = Modifier,
    containerColor: Color = MaterialTheme.colorScheme.primaryContainer,
    contentColor: Color = MaterialTheme.colorScheme.onPrimaryContainer,
    content: @Composable RowScope.() -> Unit,
) {
    Surface(
        modifier = modifier.fillMaxWidth(),
        shape = MaterialTheme.shapes.large,
        color = containerColor,
        contentColor = contentColor,
    ) {
        Row(modifier = Modifier.height(IntrinsicSize.Min), verticalAlignment = Alignment.CenterVertically) {
            Row(
                modifier = Modifier
                    .weight(1f)
                    .semantics(mergeDescendants = true) {}
                    .padding(start = 16.dp, end = 12.dp, top = 14.dp, bottom = 14.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
                content = content,
            )
            Perforation(color = contentColor.copy(alpha = PERFORATION_ALPHA))
            Box(
                modifier = Modifier
                    .fillMaxHeight()
                    .widthIn(min = STUB_MIN_WIDTH)
                    .clickable(role = Role.Button, onClick = onStub)
                    .padding(horizontal = 16.dp),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    text = stubLabel,
                    style = CoroomText.button.copy(fontWeight = FontWeight.SemiBold),
                    color = contentColor,
                    maxLines = 1,
                )
            }
        }
    }
}

/** La línea de puntos vertical que separa el talón de una entrada. */
@Composable
private fun Perforation(color: Color) {
    Box(
        modifier = Modifier
            .fillMaxHeight()
            .width(PERFORATION_DOT)
            .drawBehind {
                val dot = PERFORATION_DOT.toPx()
                val inset = PERFORATION_INSET.toPx()
                drawLine(
                    color = color,
                    start = Offset(size.width / 2f, inset),
                    end = Offset(size.width / 2f, size.height - inset),
                    strokeWidth = dot,
                    cap = StrokeCap.Round,
                    // Trazos casi nulos con el extremo redondo: una fila de puntos.
                    pathEffect = PathEffect.dashPathEffect(floatArrayOf(0.1f, dot * 3f), 0f),
                )
            },
    )
}

/**
 * Un código de sala en señalética: seminegrita, cada carácter en su celda de ancho fijo y de tres
 * en tres («K7Q 2MZ»). Los lectores de pantalla lo deletrean.
 */
@Composable
fun SignageCode(
    code: String,
    modifier: Modifier = Modifier,
    style: TextStyle = CoroomText.signageSmall,
    color: Color = LocalContentColor.current,
) {
    val cell = with(LocalDensity.current) { (style.fontSize * SIGNAGE_CELL_EM).toDp() }
    val spelled = code.toList().joinToString(" ")
    Row(
        modifier = modifier.clearAndSetSemantics { contentDescription = spelled },
        verticalAlignment = Alignment.CenterVertically,
    ) {
        code.forEachIndexed { index, char ->
            if (index > 0 && index % SIGNAGE_GROUP == 0) Spacer(Modifier.width(cell * SIGNAGE_GAP_CELLS))
            Box(modifier = Modifier.width(cell), contentAlignment = Alignment.Center) {
                Text(text = char.toString(), style = style, color = color, maxLines = 1, softWrap = false)
            }
        }
    }
}

/**
 * Enseña el código de sala de tres en tres mientras se escribe en su campo («K7Q2MZ» se ve
 * «K7Q 2MZ»). Solo cambia lo que se ve: el valor del campo sigue sin espacios.
 */
object RoomCodeGrouping : VisualTransformation {
    override fun filter(text: AnnotatedString): TransformedText {
        val raw = text.text
        if (raw.length <= SIGNAGE_GROUP) return TransformedText(text, OffsetMapping.Identity)
        val grouped = raw.chunked(SIGNAGE_GROUP).joinToString(" ")
        val mapping = object : OffsetMapping {
            // Delante de cada posición hay un espacio por cada grupo completo que queda atrás; el
            // cursor al final de un grupo se queda pegado a él, antes del espacio.
            override fun originalToTransformed(offset: Int): Int =
                (offset + (offset - 1).coerceAtLeast(0) / SIGNAGE_GROUP).coerceIn(0, grouped.length)

            override fun transformedToOriginal(offset: Int): Int =
                (offset - offset / (SIGNAGE_GROUP + 1)).coerceIn(0, raw.length)
        }
        return TransformedText(AnnotatedString(grouped), mapping)
    }
}

/** Los caracteres de cada grupo de un código. */
private const val SIGNAGE_GROUP = 3

/** El ancho de la celda de un carácter, en proporción al tamaño de la letra. */
private const val SIGNAGE_CELL_EM = 0.9f

/** El hueco entre dos grupos, en celdas. */
private const val SIGNAGE_GAP_CELLS = 0.5f

private val STUB_MIN_WIDTH = 80.dp
private val PERFORATION_DOT = 2.dp
private val PERFORATION_INSET = 10.dp
private const val PERFORATION_ALPHA = 0.5f
