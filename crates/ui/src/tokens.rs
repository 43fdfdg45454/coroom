//! Tokens del sistema de diseño: espaciado, radios, tipografía, tamaños y duraciones.
//!
//! Los valores son los de GNOME (libadwaita) para que la aplicación se sienta del escritorio:
//! espaciado en múltiplos de 4, radios de 8 (controles), 12 (tarjetas) y 16 (diálogos), y la escala
//! de tipo de los títulos y el cuerpo. Los colores no están aquí: van en [`crate::theme`].

use std::time::Duration;

/// Espaciado (píxeles lógicos).
pub mod space {
    /// 4.
    pub const XXS: f32 = 4.0;
    /// 8.
    pub const XS: f32 = 8.0;
    /// 12.
    pub const S: f32 = 12.0;
    /// 16.
    pub const M: f32 = 16.0;
    /// 24.
    pub const L: f32 = 24.0;
    /// 32.
    pub const XL: f32 = 32.0;

    /// La escala completa, de menor a mayor.
    pub const SCALE: [f32; 6] = [XXS, XS, S, M, L, XL];
}

/// Radios de las esquinas.
pub mod radius {
    /// Controles: botones, campos, interruptores, selectores.
    pub const CONTROL: f32 = 8.0;
    /// Tarjetas, filas agrupadas, avisos.
    pub const CARD: f32 = 12.0;
    /// Diálogos y paneles flotantes.
    pub const DIALOG: f32 = 16.0;
    /// Píldoras (insignias, avatares): la mitad de la altura siempre alcanza.
    pub const PILL: f32 = 999.0;
}

/// Tipografía: tamaños en píxeles lógicos (los pesos los da [`crate::font::Fonts`]).
pub mod text {
    /// Título de página (24/600).
    pub const TITLE: f32 = 24.0;
    /// Encabezado de sección (18/600).
    pub const HEADING: f32 = 18.0;
    /// Cuerpo (14/400).
    pub const BODY: f32 = 14.0;
    /// Secundario (13/400).
    pub const SECONDARY: f32 = 13.0;
    /// Pie y etiquetas de teclas (12).
    pub const CAPTION: f32 = 12.0;
    /// Código grande de la sala (monoespaciada).
    pub const CODE: f32 = 40.0;
    /// Cifra destacada (RTT de la estimación de calidad).
    pub const DISPLAY: f32 = 48.0;
}

/// Tamaños de los elementos.
pub mod size {
    /// Ancho de la barra lateral.
    pub const SIDEBAR: f32 = 232.0;
    /// Ancho máximo del contenido de las páginas.
    pub const CONTENT: f32 = 880.0;
    /// Ancho de las tarjetas de inicio de sesión y de los diálogos.
    pub const NARROW: f32 = 440.0;
    /// Altura de los controles (botones, campos).
    pub const CONTROL: f32 = 36.0;
    /// Lado de los iconos en botones y filas.
    pub const ICON: f32 = 16.0;
    /// Lado de los iconos grandes (tarjetas de inicio, estados vacíos).
    pub const ICON_LARGE: f32 = 32.0;
    /// Avatar en las filas.
    pub const AVATAR: f32 = 36.0;
    /// Punto de las insignias de estado.
    pub const DOT: f32 = 8.0;
    /// Ancho del panel de estadísticas sobre el vídeo.
    pub const STATS_PANEL: f32 = 340.0;
    /// Altura de la barra superior del vídeo.
    pub const VIDEO_BAR: f32 = 52.0;
}

/// Duraciones de las animaciones (150-250 ms, `ease-out`; ninguna bloquea la entrada).
pub mod motion {
    use super::Duration;

    /// Cambios de estado (botones, interruptores).
    pub const QUICK: Duration = Duration::from_millis(150);
    /// Aparición de paneles y toasts.
    pub const PANEL: Duration = Duration::from_millis(200);
    /// Barras que se ocultan solas y diálogos.
    pub const SLOW: Duration = Duration::from_millis(250);
    /// Lo que dura un toast en pantalla.
    pub const TOAST: Duration = Duration::from_secs(4);
    /// Lo que tarda en ocultarse la barra del vídeo sin mover el ratón.
    pub const BAR_HIDE: Duration = Duration::from_millis(2500);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spacing_is_the_four_point_scale() {
        assert_eq!(space::SCALE, [4.0, 8.0, 12.0, 16.0, 24.0, 32.0]);
        assert!(space::SCALE.iter().all(|step| step % 4.0 == 0.0));
        assert!(space::SCALE.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn radii_and_type_follow_the_contract() {
        assert_eq!(
            (radius::CONTROL, radius::CARD, radius::DIALOG),
            (8.0, 12.0, 16.0)
        );
        assert_eq!(
            (text::TITLE, text::HEADING, text::BODY, text::SECONDARY),
            (24.0, 18.0, 14.0, 13.0)
        );
        for duration in [motion::QUICK, motion::PANEL, motion::SLOW] {
            assert!((150..=250).contains(&duration.as_millis()), "{duration:?}");
        }
    }
}
