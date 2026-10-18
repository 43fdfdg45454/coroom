package coroom.android.ui.theme

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext

/**
 * Colores que Material 3 no tiene como rol: el verde de «conectado», el ámbar del aviso del
 * certificado y la pastilla de calidad, y los fondos translúcidos sobre el vídeo de la sala.
 */
@Immutable
data class CoroomColors(
    val success: Color,
    val successDot: Color,
    val warningContainer: Color,
    val onWarningContainer: Color,
    val warningAccent: Color,
    val onWarningAccent: Color,
    val warningCode: Color,
    val caution: Color,
    val overlay: Color,
    val overlayStrong: Color,
    val avatarPairs: List<Pair<Color, Color>>,
    val avatarOffline: Pair<Color, Color>,
)

private val LightExtra = CoroomColors(
    success = Color(0xFF1E6B3D),
    successDot = Color(0xFF1E7B45),
    warningContainer = Color(0xFFFFF4D6),
    onWarningContainer = Color(0xFF3D2D00),
    warningAccent = Color(0xFF5C4300),
    onWarningAccent = Color(0xFFFFFFFF),
    warningCode = Color(0xFFFFE7A8),
    caution = Color(0xFF8A6100),
    overlay = Color(0xC7111318),
    overlayStrong = Color(0xD1111318),
    avatarPairs = listOf(
        Color(0xFFE8590C) to Color(0xFFFFFFFF),
        Color(0xFFC2185B) to Color(0xFFFFFFFF),
        Color(0xFF00796B) to Color(0xFFFFFFFF),
        Color(0xFF5E35B1) to Color(0xFFFFFFFF),
        Color(0xFF1565C0) to Color(0xFFFFFFFF),
        Color(0xFF2E7D32) to Color(0xFFFFFFFF),
    ),
    avatarOffline = Color(0xFFC4C6D0) to Color(0xFF2D3038),
)

private val DarkExtra = CoroomColors(
    success = Color(0xFF6FD99A),
    successDot = Color(0xFF6FD99A),
    warningContainer = Color(0xFF3B2F00),
    onWarningContainer = Color(0xFFFFE08A),
    warningAccent = Color(0xFFF3C04E),
    onWarningAccent = Color(0xFF3D2D00),
    warningCode = Color(0xFF574400),
    caution = Color(0xFFF3C04E),
    overlay = Color(0xC7111318),
    overlayStrong = Color(0xD1111318),
    avatarPairs = listOf(
        Color(0xFFF4A261) to Color(0xFF3A1D00),
        Color(0xFFFFB0C8) to Color(0xFF5E1133),
        Color(0xFF82D5C8) to Color(0xFF00382F),
        Color(0xFFCFBCFF) to Color(0xFF381E72),
        Color(0xFFA8C8FF) to Color(0xFF002F65),
        Color(0xFF9AD79C) to Color(0xFF00390B),
    ),
    avatarOffline = Color(0xFF44474E) to Color(0xFFC4C6D0),
)

/** El esquema de los bocetos, para Android 11 y anteriores (sin color dinámico). */
private val FallbackLight: ColorScheme = lightColorScheme(
    primary = Color(0xFF2F62A8),
    onPrimary = Color(0xFFFFFFFF),
    primaryContainer = Color(0xFFD6E3FF),
    onPrimaryContainer = Color(0xFF001B3E),
    secondary = Color(0xFF555F71),
    onSecondary = Color(0xFFFFFFFF),
    secondaryContainer = Color(0xFFD9E3F8),
    onSecondaryContainer = Color(0xFF121C2B),
    background = Color(0xFFF9F9FF),
    onBackground = Color(0xFF1A1C20),
    surface = Color(0xFFF9F9FF),
    onSurface = Color(0xFF1A1C20),
    surfaceVariant = Color(0xFFE0E2EC),
    onSurfaceVariant = Color(0xFF44474E),
    surfaceContainerLowest = Color(0xFFFFFFFF),
    surfaceContainerLow = Color(0xFFF3F3FA),
    surfaceContainer = Color(0xFFEDEEF4),
    surfaceContainerHigh = Color(0xFFE7E8EE),
    surfaceContainerHighest = Color(0xFFE2E2E9),
    outline = Color(0xFF74777F),
    outlineVariant = Color(0xFFDFE2EB),
    error = Color(0xFFB3261E),
    onError = Color(0xFFFFFFFF),
    errorContainer = Color(0xFFFFDAD6),
    onErrorContainer = Color(0xFF410002),
)

private val FallbackDark: ColorScheme = darkColorScheme(
    primary = Color(0xFFA8C8FF),
    onPrimary = Color(0xFF002F65),
    primaryContainer = Color(0xFF1F477F),
    onPrimaryContainer = Color(0xFFD6E3FF),
    secondary = Color(0xFFBDC7DC),
    onSecondary = Color(0xFF273141),
    secondaryContainer = Color(0xFF3E4759),
    onSecondaryContainer = Color(0xFFD9E3F8),
    background = Color(0xFF111318),
    onBackground = Color(0xFFE2E2E9),
    surface = Color(0xFF111318),
    onSurface = Color(0xFFE2E2E9),
    surfaceVariant = Color(0xFF44474E),
    onSurfaceVariant = Color(0xFFC4C6D0),
    surfaceContainerLowest = Color(0xFF0C0E13),
    surfaceContainerLow = Color(0xFF1A1C20),
    surfaceContainer = Color(0xFF1D2024),
    surfaceContainerHigh = Color(0xFF282A2F),
    surfaceContainerHighest = Color(0xFF33353A),
    outline = Color(0xFF8E9099),
    outlineVariant = Color(0xFF44474E),
    error = Color(0xFFFFB4AB),
    onError = Color(0xFF690005),
    errorContainer = Color(0xFF93000A),
    onErrorContainer = Color(0xFFFFDAD6),
)

val LocalCoroomColors = staticCompositionLocalOf { LightExtra }

/** Atajo: `CoroomTheme.colors.success`. */
object CoroomTheme {
    val colors: CoroomColors
        @Composable get() = LocalCoroomColors.current
}

/**
 * El tema de coroom: el color dinámico del sistema (Android 12+) en claro u oscuro según el
 * sistema, y el de los bocetos en los anteriores. La sala va siempre en oscuro (`darkTheme = true`).
 */
@Composable
fun CoroomTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    dynamicColor: Boolean = true,
    content: @Composable () -> Unit,
) {
    val context = LocalContext.current
    val scheme = when {
        dynamicColor && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S ->
            if (darkTheme) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        darkTheme -> FallbackDark
        else -> FallbackLight
    }
    CompositionLocalProvider(LocalCoroomColors provides if (darkTheme) DarkExtra else LightExtra) {
        MaterialTheme(colorScheme = scheme, typography = CoroomTypography, content = content)
    }
}
