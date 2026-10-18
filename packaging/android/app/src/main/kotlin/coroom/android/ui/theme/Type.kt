package coroom.android.ui.theme

import androidx.compose.material3.Typography
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp

/** La tipografía del sistema (Roboto): los tamaños de Material 3 tal cual. */
val CoroomTypography = Typography()

/** Los estilos de los bocetos, por su papel. Sin color: lo pone cada pantalla. */
object CoroomText {
    /** «coroom», «Amigos», «Ajustes». */
    val screenTitle = TextStyle(fontSize = 22.sp, fontWeight = FontWeight.SemiBold, letterSpacing = (-0.2).sp)

    /** «Hola, Ana», «Bienvenido a coroom». */
    val greeting = TextStyle(fontSize = 28.sp, fontWeight = FontWeight.Medium, lineHeight = 34.sp)

    /** Título de una tarjeta («Unirme a una sala»). */
    val cardTitle = TextStyle(fontSize = 18.sp, fontWeight = FontWeight.Medium)

    /** Cabecera de sección («Invitaciones», «Conectados · 2»). */
    val section = TextStyle(fontSize = 14.sp, fontWeight = FontWeight.SemiBold)

    /** Nombre en una fila. */
    val itemTitle = TextStyle(fontSize = 16.sp, fontWeight = FontWeight.Medium)

    /** Texto principal de una fila de ajustes. */
    val itemBody = TextStyle(fontSize = 16.sp)

    /** Segunda línea de una fila. */
    val supporting = TextStyle(fontSize = 13.sp, lineHeight = 18.sp)

    /** Etiqueta de un campo. */
    val label = TextStyle(fontSize = 12.sp)

    /** Párrafo explicativo. */
    val body = TextStyle(fontSize = 15.sp, lineHeight = 22.sp)

    /** Botones. */
    val button = TextStyle(fontSize = 14.sp, fontWeight = FontWeight.SemiBold)

    /** Código de sala en el campo: monoespaciada y espaciada. */
    val roomCode = TextStyle(
        fontFamily = FontFamily.Monospace,
        fontWeight = FontWeight.Medium,
        fontSize = 17.sp,
        letterSpacing = 2.sp,
    )

    /** Huellas, números de seguridad y códigos en línea. */
    val mono = TextStyle(fontFamily = FontFamily.Monospace, fontSize = 13.sp, lineHeight = 20.sp)
}
