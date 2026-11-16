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
import androidx.compose.ui.graphics.compositeOver
import androidx.compose.ui.platform.LocalContext

/**
 * Colores que Material 3 no tiene como rol: las lámparas de estado (verde de conectado y de la
 * ruta directa, ámbar del relay y de los avisos, piloto rojo de «en vivo»), cada una con su par de
 * contenedor, los fondos translúcidos sobre el vídeo de la sala y el color de cada persona.
 */
@Immutable
data class CoroomColors(
    /** Si es el juego del tema oscuro (la sala lo es siempre). */
    val dark: Boolean,
    /** Texto en verde sobre cualquier superficie. */
    val success: Color,
    /** La lámpara verde: conectado, ruta directa. */
    val successDot: Color,
    val successContainer: Color,
    val onSuccessContainer: Color,
    val warningContainer: Color,
    val onWarningContainer: Color,
    val warningAccent: Color,
    val onWarningAccent: Color,
    /** Fondo de la huella dentro de un aviso ámbar. */
    val warningCode: Color,
    /** Texto en ámbar sobre cualquier superficie. */
    val caution: Color,
    /** La lámpara ámbar: relay, calidad justa, reconectando. */
    val cautionDot: Color,
    /** El piloto rojo de «en vivo»: se está compartiendo. Nunca un aviso. */
    val live: Color,
    val liveContainer: Color,
    val onLiveContainer: Color,
    /** Píldoras y avisos sobre el vídeo. */
    val overlay: Color,
    /** La barra lateral de la sala, sobre el vídeo. */
    val overlayStrong: Color,
    /** Lo que oscurece el vídeo bajo un panel abierto. */
    val scrim: Color,
    /** Lo que se ve en lugar del vídeo cuando está apagado. */
    val videoBackdrop: Color,
    /** El color de cada persona (fondo y letra de su butaca), estable por usuario. */
    val avatarPairs: List<Pair<Color, Color>>,
    val avatarOffline: Pair<Color, Color>,
)

private val LightExtra = CoroomColors(
    dark = false,
    success = Color(0xFF00632A),
    successDot = Color(0xFF037E38),
    successContainer = Color(0xFFCBF2D1),
    onSuccessContainer = Color(0xFF012A0F),
    warningContainer = Color(0xFFFFE5BA),
    onWarningContainer = Color(0xFF312103),
    warningAccent = Color(0xFF5E4205),
    onWarningAccent = Color(0xFFFFFFFF),
    warningCode = Color(0xFFF7D293),
    caution = Color(0xFF835C01),
    cautionDot = Color(0xFF9C6F02),
    live = Color(0xFFDA2A32),
    liveContainer = Color(0xFFFEE2DF),
    onLiveContainer = Color(0xFF520D0F),
    overlay = Color(0xCC0F141A),
    overlayStrong = Color(0xDB0F141A),
    scrim = Color(0x660A0E14),
    videoBackdrop = Color(0xFF000000),
    avatarPairs = listOf(
        Color(0xFFBE4500) to Color(0xFFFFFFFF),
        Color(0xFFC2185B) to Color(0xFFFFFFFF),
        Color(0xFF00796B) to Color(0xFFFFFFFF),
        Color(0xFF5E35B1) to Color(0xFFFFFFFF),
        Color(0xFF1565C0) to Color(0xFFFFFFFF),
        Color(0xFF2E7D32) to Color(0xFFFFFFFF),
    ),
    avatarOffline = Color(0xFFBEC7D4) to Color(0xFF2E353F),
)

private val DarkExtra = CoroomColors(
    dark = true,
    success = Color(0xFF80DA95),
    successDot = Color(0xFF5ED27D),
    successContainer = Color(0xFF184123),
    onSuccessContainer = Color(0xFFBAEFC4),
    warningContainer = Color(0xFF433215),
    onWarningContainer = Color(0xFFFFDEA8),
    warningAccent = Color(0xFFF4BE5C),
    onWarningAccent = Color(0xFF39290B),
    warningCode = Color(0xFF58431E),
    caution = Color(0xFFF4BE5C),
    cautionDot = Color(0xFFF1B74A),
    live = Color(0xFFFF766F),
    liveContainer = Color(0xFF5F2826),
    onLiveContainer = Color(0xFFFEDAD7),
    overlay = Color(0xCC0F141A),
    overlayStrong = Color(0xDB0F141A),
    scrim = Color(0x660A0E14),
    videoBackdrop = Color(0xFF000000),
    avatarPairs = listOf(
        Color(0xFFF4A261) to Color(0xFF3A1D00),
        Color(0xFFFFB0C8) to Color(0xFF5E1133),
        Color(0xFF82D5C8) to Color(0xFF00382F),
        Color(0xFFCFBCFF) to Color(0xFF381E72),
        Color(0xFFA8C8FF) to Color(0xFF002F65),
        Color(0xFF9AD79C) to Color(0xFF00390B),
    ),
    avatarOffline = Color(0xFF404752) to Color(0xFFBEC7D4),
)

/**
 * El esquema propio, para Android 11 y anteriores (sin color dinámico): el azul del icono como
 * primario y todos los neutros teñidos de ese mismo tono, sin ningún gris puro.
 */
private val FallbackLight: ColorScheme = lightColorScheme(
    primary = Color(0xFF0F5CB7),
    onPrimary = Color(0xFFFFFFFF),
    primaryContainer = Color(0xFFD1E4FF),
    onPrimaryContainer = Color(0xFF001B3F),
    inversePrimary = Color(0xFFA2C9FE),
    secondary = Color(0xFF525F72),
    onSecondary = Color(0xFFFFFFFF),
    secondaryContainer = Color(0xFFD4E4FA),
    onSecondaryContainer = Color(0xFF111C2C),
    tertiary = Color(0xFF725186),
    onTertiary = Color(0xFFFFFFFF),
    tertiaryContainer = Color(0xFFF0DBFE),
    onTertiaryContainer = Color(0xFF2C0C3C),
    background = Color(0xFFF6FAFF),
    onBackground = Color(0xFF171C22),
    surface = Color(0xFFF6FAFF),
    onSurface = Color(0xFF171C22),
    surfaceVariant = Color(0xFFD5E4F8),
    onSurfaceVariant = Color(0xFF3C4858),
    surfaceTint = Color(0xFF0F5CB7),
    inverseSurface = Color(0xFF2C3138),
    inverseOnSurface = Color(0xFFEBF1FA),
    error = Color(0xFFB91C1E),
    onError = Color(0xFFFFFFFF),
    errorContainer = Color(0xFFFEDBD6),
    onErrorContainer = Color(0xFF410002),
    outline = Color(0xFF6B788A),
    outlineVariant = Color(0xFFB9C8DC),
    scrim = Color(0xFF000000),
    surfaceBright = Color(0xFFF6FAFF),
    surfaceDim = Color(0xFFD4DBE3),
    surfaceContainerLowest = Color(0xFFFFFFFF),
    surfaceContainerLow = Color(0xFFEEF4FD),
    surfaceContainer = Color(0xFFE8EEF7),
    surfaceContainerHigh = Color(0xFFE2E9F2),
    surfaceContainerHighest = Color(0xFFDDE3EC),
)

private val FallbackDark: ColorScheme = darkColorScheme(
    primary = Color(0xFFA2C9FE),
    onPrimary = Color(0xFF012F66),
    primaryContainer = Color(0xFF014590),
    onPrimaryContainer = Color(0xFFD1E4FF),
    inversePrimary = Color(0xFF0F5CB7),
    secondary = Color(0xFFB8C8DE),
    onSecondary = Color(0xFF253142),
    secondaryContainer = Color(0xFF3B485A),
    onSecondaryContainer = Color(0xFFD4E4FA),
    tertiary = Color(0xFFDDB9F5),
    onTertiary = Color(0xFF422354),
    tertiaryContainer = Color(0xFF593A6D),
    onTertiaryContainer = Color(0xFFF0DBFE),
    background = Color(0xFF0F141A),
    onBackground = Color(0xFFDDE3EC),
    surface = Color(0xFF0F141A),
    onSurface = Color(0xFFDDE3EC),
    surfaceVariant = Color(0xFF3C4858),
    onSurfaceVariant = Color(0xFFB9C8DC),
    surfaceTint = Color(0xFFA2C9FE),
    inverseSurface = Color(0xFFDDE3EC),
    inverseOnSurface = Color(0xFF2C3138),
    error = Color(0xFFFEB4AB),
    onError = Color(0xFF680106),
    errorContainer = Color(0xFF93000C),
    onErrorContainer = Color(0xFFFEDBD6),
    outline = Color(0xFF8492A4),
    outlineVariant = Color(0xFF3C4858),
    scrim = Color(0xFF000000),
    surfaceBright = Color(0xFF353A41),
    surfaceDim = Color(0xFF0F141A),
    surfaceContainerLowest = Color(0xFF0A0E14),
    surfaceContainerLow = Color(0xFF171C22),
    surfaceContainer = Color(0xFF1B2026),
    surfaceContainerHigh = Color(0xFF262A31),
    surfaceContainerHighest = Color(0xFF30353C),
)

val LocalCoroomColors = staticCompositionLocalOf { LightExtra }

/** Atajos: `CoroomTheme.colors.success`, `CoroomTheme.navigationContainer`. */
object CoroomTheme {
    val colors: CoroomColors
        @Composable get() = LocalCoroomColors.current

    /**
     * El plano de la navegación (barra inferior o riel): el más teñido de la ventana. En claro, la
     * superficie de contenedor con una parte del primario; en oscuro, la más honda con algo menos.
     * Sale del esquema en uso, así que sigue al color dinámico del sistema.
     */
    val navigationContainer: Color
        @Composable get() {
            val scheme = MaterialTheme.colorScheme
            return if (LocalCoroomColors.current.dark) {
                scheme.primary.copy(alpha = NAVIGATION_TINT_DARK).compositeOver(scheme.surfaceContainerLowest)
            } else {
                scheme.primary.copy(alpha = NAVIGATION_TINT_LIGHT).compositeOver(scheme.surfaceContainer)
            }
        }
}

private const val NAVIGATION_TINT_LIGHT = 0.08f
private const val NAVIGATION_TINT_DARK = 0.07f

/**
 * El tema de coroom: el color dinámico del sistema (Android 12+) en claro u oscuro según el
 * sistema, y el esquema propio en los anteriores. La sala va siempre en oscuro (`darkTheme = true`).
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
        MaterialTheme(
            colorScheme = scheme,
            shapes = CoroomShapes,
            typography = CoroomTypography,
            content = content,
        )
    }
}
