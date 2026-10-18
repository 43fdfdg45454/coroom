package coroom.android.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import coroom.android.ui.theme.CoroomTheme
import kotlin.math.absoluteValue

/**
 * Avatar con la inicial del nombre sobre un color estable por usuario (el mismo en todas las
 * pantallas). `offline` lo apaga, como en «Desconectados».
 */
@Composable
fun Avatar(
    name: String,
    seed: Long,
    modifier: Modifier = Modifier,
    size: Dp = 40.dp,
    offline: Boolean = false,
) {
    val colors = CoroomTheme.colors
    val (background, foreground) = if (offline) {
        colors.avatarOffline
    } else {
        colors.avatarPairs[(seed % colors.avatarPairs.size).toInt().absoluteValue]
    }
    val initial = name.trim().firstOrNull()?.uppercaseChar()?.toString() ?: "?"
    Box(
        modifier = modifier.size(size).background(background, CircleShape),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = initial,
            color = foreground,
            fontWeight = FontWeight.SemiBold,
            fontSize = (size.value * 0.4f).sp,
        )
    }
}
