package coroom.android.ui.theme

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Shapes
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.unit.dp

/** Las formas de Material con los radios de coroom: de los menús (6) a los diálogos (28). */
val CoroomShapes = Shapes(
    extraSmall = RoundedCornerShape(6.dp),
    small = RoundedCornerShape(10.dp),
    medium = RoundedCornerShape(14.dp),
    large = RoundedCornerShape(20.dp),
    extraLarge = RoundedCornerShape(28.dp),
)

/**
 * La butaca: la forma de los avatares en toda la app. Respaldo redondeado arriba y base casi
 * recta, en proporción al lado (42 % arriba, 18 % abajo), así vale para cualquier tamaño.
 */
val SeatShape: Shape = RoundedCornerShape(
    topStartPercent = 42,
    topEndPercent = 42,
    bottomEndPercent = 18,
    bottomStartPercent = 18,
)
