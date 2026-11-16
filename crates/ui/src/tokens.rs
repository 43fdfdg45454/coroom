//! Tokens del sistema de diseño: espaciado, radios, tipografía, tamaños y movimiento.
//!
//! Los valores son los de GNOME (libadwaita) para que la aplicación se sienta del escritorio:
//! espaciado en múltiplos de 4, radios de 8 (controles), 12 (tarjetas) y 16 (diálogos), y la escala
//! de tipo de los títulos y el cuerpo. Lo propio de coroom son la butaca ([`seat`]: la silueta de
//! las personas) y la señalética ([`sign`]: los códigos de sala, cada carácter en su celda). Los
//! colores no están aquí: van en [`crate::theme`].

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
    /// Píldoras (insignias, puntos de estado): la mitad de la altura siempre alcanza.
    pub const PILL: f32 = 999.0;
    /// Celda pequeña de señalética y etiquetas de tecla.
    pub const CELL: f32 = 6.0;
}

/// Tipografía: tamaños en píxeles lógicos (los pesos los da [`crate::font::Fonts`]).
pub mod text {
    /// Título de página (26/600).
    pub const TITLE: f32 = 26.0;
    /// Encabezado de sección (18/600).
    pub const HEADING: f32 = 18.0;
    /// Cuerpo (14/400).
    pub const BODY: f32 = 14.0;
    /// Secundario (13/400).
    pub const SECONDARY: f32 = 13.0;
    /// Pie y etiquetas de teclas (12).
    pub const CAPTION: f32 = 12.0;
    /// Código grande de la sala escrito de corrido (en celdas, [`super::sign`]).
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
    /// Butaca (avatar) en las filas de lista.
    pub const AVATAR: f32 = 36.0;
    /// Punto de las insignias de estado.
    pub const DOT: f32 = 8.0;
    /// Ancho del panel de estadísticas sobre el vídeo.
    pub const STATS_PANEL: f32 = 340.0;
    /// Altura de la barra superior del vídeo.
    pub const VIDEO_BAR: f32 = 52.0;
}

/// La butaca: la silueta de las personas en toda la aplicación. Un respaldo redondeado arriba y
/// una base casi recta, con todo en proporción al lado para que se reconozca igual a 20 px que a
/// 72.
pub mod seat {
    /// Radio de las esquinas de arriba (el respaldo), en fracción del lado.
    pub const BACK: f32 = 0.42;
    /// Radio de las esquinas de abajo (la base), en fracción del lado.
    pub const BASE: f32 = 0.18;
    /// Tamaño de las iniciales, en fracción del lado.
    pub const INITIALS: f32 = 0.4;
    /// Tamaño del número de una butaca libre, en fracción del lado.
    pub const NUMBER: f32 = 0.36;
    /// Grosor del contorno de una butaca libre.
    pub const OUTLINE: f32 = 1.5;
    /// Diámetro de la lámpara de estado, en fracción del lado...
    pub const LAMP: f32 = 0.28;
    /// ...sin bajar de este diámetro, para que se vea en las butacas pequeñas.
    pub const LAMP_MIN: f32 = 8.0;
    /// Filete del color del fondo alrededor de la lámpara, que la separa de la butaca.
    pub const LAMP_GAP: f32 = 2.0;
    /// Lado de las butacas de una fila de sala.
    pub const ROW: f32 = 44.0;
    /// Lado de las butacas de una fila compacta (el panel sobre el vídeo, una entrada).
    pub const ROW_SMALL: f32 = 32.0;
    /// Butacas de una fila de sala como poco: las que falten se ven libres.
    pub const ROW_MIN: usize = 4;
    /// Ancho de la ranura de cada butaca cuando lleva el nombre debajo.
    pub const LABEL: f32 = 68.0;

    /// Radios de una butaca de lado `side`: `(arriba, abajo)`.
    pub fn radii(side: f32) -> (f32, f32) {
        (side * BACK, side * BASE)
    }

    /// Diámetro de la lámpara de una butaca de lado `side`.
    pub fn lamp(side: f32) -> f32 {
        (side * LAMP).round().max(LAMP_MIN)
    }
}

/// Señalética: los códigos de sala, cada carácter en su celda de ancho fijo y agrupados de tres
/// en tres.
pub mod sign {
    /// Caracteres por grupo.
    pub const GROUP: usize = 3;

    /// Medidas de un tamaño de celda.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct Cell {
        /// Ancho de la celda.
        pub width: f32,
        /// Alto de la celda.
        pub height: f32,
        /// Tamaño del carácter.
        pub text: f32,
        /// Hueco entre las celdas de un grupo.
        pub gap: f32,
        /// Hueco entre grupos.
        pub group_gap: f32,
        /// Radio de la celda.
        pub radius: f32,
    }

    /// Celda grande: el código de la sala abierta.
    pub const LARGE: Cell = Cell {
        width: 40.0,
        height: 52.0,
        text: 30.0,
        gap: 4.0,
        group_gap: 16.0,
        radius: super::radius::CONTROL,
    };

    /// Celda pequeña: el código dentro de una invitación.
    pub const SMALL: Cell = Cell {
        width: 20.0,
        height: 26.0,
        text: 15.0,
        gap: 2.0,
        group_gap: 8.0,
        radius: super::radius::CELL,
    };
}

/// La entrada: el contenedor con cuerpo y talón separados por un filete punteado.
pub mod ticket {
    /// Diámetro de cada punto del filete.
    pub const DOT: f32 = 2.0;
    /// Distancia entre el comienzo de un punto y el del siguiente.
    pub const PITCH: f32 = 6.0;
}

/// Movimiento: duraciones, distancias y escalas de las animaciones. Cortas, que salen rápido y
/// frenan suave, y ninguna bloquea la entrada: nada espera a que termine una animación para
/// aceptar un clic. Las curvas y las preferencias del sistema (movimiento reducido, factor de
/// duración) las aplica [`crate::motion`].
pub mod motion {
    use super::Duration;

    /// Color y fondo al pasar el ratón (lineal).
    pub const HOVER: Duration = Duration::from_millis(90);
    /// Lo que tarda un botón en hundirse al pulsarlo (vuelve con resorte).
    pub const PRESS: Duration = Duration::from_millis(60);
    /// Pulsar, interruptores, insignias (`ease-out-expo`).
    pub const QUICK: Duration = Duration::from_millis(140);
    /// Paneles, desplegables, toasts, diálogos, filas que se abren (`ease-out-expo`; resorte en
    /// interruptores y toasts).
    pub const PANEL: Duration = Duration::from_millis(200);
    /// Cambio de pantalla: la nueva entra deslizándose [`SLIDE`] y aparece (`ease-out-expo`).
    pub const SCREEN: Duration = Duration::from_millis(260);
    /// Momentos (abrir la sala, entrar en una, pasarla), con escalonado (`ease-out-expo`).
    pub const MOMENT: Duration = Duration::from_millis(420);
    /// Lo que dura una salida respecto de su entrada, en por ciento (`ease-in-out`): todo lo que
    /// sale lo hace más rápido de lo que entró.
    pub const EXIT_PERCENT: u32 = 70;
    /// Escalonado entre los bloques de una pantalla y los elementos de una lista.
    pub const STAGGER: Duration = Duration::from_millis(30);
    /// Escalonado entre las opciones de un desplegable.
    pub const STAGGER_MENU: Duration = Duration::from_millis(15);
    /// Distancia que se desliza una pantalla al entrar o salir (píxeles lógicos).
    pub const SLIDE: f32 = 36.0;
    /// Escala de un botón pulsado.
    pub const PRESS_SCALE: f32 = 0.96;
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
            (26.0, 18.0, 14.0, 13.0)
        );
    }

    #[test]
    fn the_seat_keeps_its_proportions_at_any_side() {
        assert_eq!((seat::BACK, seat::BASE), (0.42, 0.18));
        for side in [20.0_f32, 22.0, 32.0, 36.0, 44.0, 72.0, 96.0] {
            let (back, base) = seat::radii(side);
            assert!((back / side - 0.42).abs() < 1e-6, "{side}");
            assert!((base / side - 0.18).abs() < 1e-6, "{side}");
            // El respaldo se redondea más que la base y ninguno llega al medio lado: no es un
            // círculo ni una píldora.
            assert!(back > 2.0 * base && back < side / 2.0, "{side}");
            // La lámpara cabe en la esquina sin tapar las iniciales.
            let lamp = seat::lamp(side) + 2.0 * seat::LAMP_GAP;
            assert!(seat::lamp(side) >= seat::LAMP_MIN, "{side}");
            assert!(lamp <= side * 0.62, "{side}: {lamp}");
        }
        // La ranura con el nombre debajo es más ancha que la butaca de una fila.
        assert_eq!(
            (seat::ROW, seat::ROW_SMALL, seat::LABEL),
            (44.0, 32.0, 68.0)
        );
        assert_eq!(seat::ROW_MIN, 4);
    }

    #[test]
    fn sign_cells_come_in_two_sizes_grouped_by_three() {
        assert_eq!(sign::GROUP, 3);
        for cell in [sign::LARGE, sign::SMALL] {
            assert!(cell.width < cell.height, "celdas más altas que anchas");
            assert!(cell.text < cell.height && cell.text <= cell.width);
            assert!(
                cell.group_gap >= 3.0 * cell.gap,
                "el hueco entre grupos se distingue del de las celdas"
            );
            assert_eq!(cell.gap % 2.0, 0.0);
        }
        assert_eq!(sign::LARGE.width, 2.0 * sign::SMALL.width);
        assert_eq!(sign::LARGE.height, 2.0 * sign::SMALL.height);
        // El código de seis en grande cabe en media página.
        let six = 6.0 * sign::LARGE.width + 4.0 * sign::LARGE.gap + sign::LARGE.group_gap;
        assert!(six <= size::NARROW - 2.0 * space::L, "{six}");
    }

    #[test]
    fn motion_follows_the_contract() {
        let entries = [
            motion::HOVER,
            motion::QUICK,
            motion::PANEL,
            motion::SCREEN,
            motion::MOMENT,
        ];
        assert_eq!(
            entries.map(|duration| duration.as_millis()),
            [90, 140, 200, 260, 420]
        );
        assert!(entries.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(motion::PRESS < motion::HOVER);
        assert_eq!(motion::EXIT_PERCENT, 70);
        assert_eq!(
            (
                motion::STAGGER.as_millis(),
                motion::STAGGER_MENU.as_millis()
            ),
            (30, 15)
        );
        assert_eq!((motion::SLIDE, motion::PRESS_SCALE), (36.0, 0.96));
    }
}
