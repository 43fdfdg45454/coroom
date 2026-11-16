package coroom.android.ui.theme

import androidx.compose.material3.Typography
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.LineHeightStyle
import androidx.compose.ui.unit.sp

/** La escala de Material 3 tal cual (Roboto del sistema, en sp). */
private val Base = Typography()

/**
 * La escala de coroom: la de Material 3 con los titulares y los títulos en seminegrita. Los
 * tamaños no se tocan: siguen el ajuste de tamaño de letra del sistema.
 */
val CoroomTypography = Typography(
    headlineLarge = Base.headlineLarge.copy(fontWeight = FontWeight.SemiBold),
    headlineMedium = Base.headlineMedium.copy(fontWeight = FontWeight.SemiBold),
    headlineSmall = Base.headlineSmall.copy(fontWeight = FontWeight.SemiBold),
    titleLarge = Base.titleLarge.copy(fontWeight = FontWeight.SemiBold),
    titleSmall = Base.titleSmall.copy(fontWeight = FontWeight.SemiBold),
)

/** Cifras de ancho fijo (`tnum`): las columnas de números no bailan al cambiar de valor. */
private const val TABULAR_FIGURES = "tnum"

/**
 * Los estilos por su papel, todos sacados de [CoroomTypography]. Sin color: lo pone cada pantalla
 * o el contenedor.
 */
object CoroomText {
    private val scale = CoroomTypography

    /** La marca «coroom» en la cabecera de Inicio. */
    val wordmark = scale.titleLarge

    /** Título de pantalla: «Amigos», «Ajustes», «Entrando en la sala de Bea». */
    val screenTitle = scale.headlineMedium

    /** «Hola, Ana», «Bienvenido a coroom». */
    val greeting = scale.headlineMedium

    /** Título de un bloque («Unirme a una sala») o de un panel de la sala. */
    val cardTitle = scale.titleLarge

    /** Cabecera de sección («Invitaciones», «Conectados · 2»). */
    val section = scale.titleSmall

    /** Nombre en una fila. */
    val itemTitle = scale.titleMedium

    /** Texto principal de una fila de ajustes. */
    val itemBody = scale.bodyLarge

    /** Segunda línea de una fila. */
    val supporting = scale.bodyMedium

    /**
     * El texto de una píldora («14 ms», los chips de la sala), con la letra centrada en el alto de
     * la línea (no repartido según el ascendente y el descendente), para que las cifras y las
     * mayúsculas queden en el medio de la píldora.
     */
    val pill = scale.labelLarge.copy(
        fontFeatureSettings = TABULAR_FIGURES,
        lineHeightStyle = LineHeightStyle(LineHeightStyle.Alignment.Center, LineHeightStyle.Trim.None),
    )

    /** Etiquetas pequeñas: el nombre bajo una butaca, la hora de un mensaje, las notas al pie. */
    val label = scale.bodySmall

    /** Párrafo explicativo. */
    val body = scale.bodyLarge

    /** Botones. */
    val button = scale.labelLarge

    /**
     * Señalética: el código de sala y las cifras grandes. Seminegrita grande; cada carácter va en
     * su celda de ancho fijo y de tres en tres (lo hace `SignageCode`).
     */
    val signage = scale.headlineSmall.copy(fontFeatureSettings = TABULAR_FIGURES)

    /** La señalética dentro de una fila o de una entrada. */
    val signageSmall = scale.titleMedium.copy(
        fontWeight = FontWeight.SemiBold,
        fontFeatureSettings = TABULAR_FIGURES,
    )

    /** El código de sala mientras se escribe en su campo: la señalética, con los caracteres separados. */
    val roomCode = signage.copy(letterSpacing = 3.sp)

    /** Una cifra destacada del panel de conexión («14 ms»). */
    val metric = scale.titleLarge.copy(fontFeatureSettings = TABULAR_FIGURES)

    /** Una cifra en una tabla de estadísticas, alineada a la derecha. */
    val figure = scale.bodyMedium.copy(fontFeatureSettings = TABULAR_FIGURES)

    /** Solo para datos técnicos: huellas, números de seguridad y direcciones. */
    val mono = scale.bodyMedium.copy(fontFamily = FontFamily.Monospace)
}
